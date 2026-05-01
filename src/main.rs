/*
agz v1.0.0
Licensed under the MIT License.

MIT License

Copyright (c) 2026 Herman Nythe

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

---------------------------------------------------------------------------
DISCLAIMER OF WARRANTY & SCOPE
---------------------------------------------------------------------------

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. 

IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, 
DAMAGES (INCLUDING, BUT NOT LIMITED TO, LOSS OF DATA OR CRYPTOGRAPHIC 
FAILURE), OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR 
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE 
USE OR OTHER DEALINGS IN THE SOFTWARE.
*/

use rayon::prelude::*;
use std::env;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write, Result, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;
use walkdir::WalkDir;
use zstd::stream::{encode_all, decode_all};

const AGZ_MAGIC: [u8; 4] = [b'A', b'G', b'Z', 16];

#[derive(Debug, PartialEq, Clone, Copy)]
enum EngineType {
    ZstdOnly = 0,
    Agz1D = 1,
    Agz2D = 2,
}

struct FileRecord {
    rel_path: String,
    engine: EngineType,
    channels: usize,
    row_stride: usize,
    orig_size: u64,
    compressed: Vec<u8>,
}

fn calculate_shannon_entropy(data: &[u8]) -> f64 {
    let mut counts = [0usize; 256];
    for &byte in data { counts[byte as usize] += 1; }
    let mut entropy = 0.0;
    let len = data.len() as f64;
    for &count in &counts {
        if count > 0 { let p = count as f64 / len; entropy -= p * p.log2(); }
    }
    entropy.max(0.0)
}

fn process_1d_forward_multi(channel: &[u8]) -> Vec<u8> {
    if channel.is_empty() { return vec![0]; }
    let mut best_out = Vec::new();
    let mut best_ent = f64::MAX;
    
    for mode in 0..4u8 {
        let mut out = Vec::with_capacity(channel.len() + 1);
        out.push(mode);
        for i in 0..channel.len() {
            let pred = match mode {
                0 => if i >= 1 { channel[i-1] as i32 } else { 0 }, // Repeat
                1 => if i >= 2 { 2 * (channel[i-1] as i32) - (channel[i-2] as i32) } else { 0 }, // Linear
                2 => if i >= 3 { 3 * (channel[i-1] as i32) - 3 * (channel[i-2] as i32) + (channel[i-3] as i32) } else { 0 }, // Cubic
                3 => if i >= 2 { ((channel[i-1] as i32) + (channel[i-2] as i32)) / 2 } else { 0 }, // Median
                _ => 0,
            };
            out.push((channel[i] as i32 - pred).rem_euclid(256) as u8);
        }
        let ent = calculate_shannon_entropy(&out[1..]);
        if ent < best_ent { best_ent = ent; best_out = out; }
    }
    best_out
}

fn process_1d_inverse_multi(encoded: &[u8]) -> Vec<u8> {
    if encoded.is_empty() { return Vec::new(); }
    let mode = encoded[0];
    let data = &encoded[1..];
    let mut decoded = Vec::with_capacity(data.len());
    
    for i in 0..data.len() {
        let pred = match mode {
            0 => if i >= 1 { decoded[i-1] as i32 } else { 0 },
            1 => if i >= 2 { 2 * (decoded[i-1] as i32) - (decoded[i-2] as i32) } else { 0 },
            2 => if i >= 3 { 3 * (decoded[i-1] as i32) - 3 * (decoded[i-2] as i32) + (decoded[i-3] as i32) } else { 0 },
            3 => if i >= 2 { ((decoded[i-1] as i32) + (decoded[i-2] as i32)) / 2 } else { 0 },
            _ => 0,
        };
        decoded.push((data[i] as i32 + pred).rem_euclid(256) as u8);
    }
    decoded
}

fn process_2d_forward(data: &[u8], channels: usize, row_stride: usize) -> Vec<u8> {
    let mut out = vec![0u8; data.len()];
    for i in 0..data.len() {
        let left   = if i >= channels          { data[i - channels] as i32 } else { 0 };
        let up     = if i >= row_stride        { data[i - row_stride] as i32 } else { 0 };
        let upleft = if i >= row_stride + channels { data[i - row_stride - channels] as i32 } else { 0 };
        let pred = if i < row_stride { left } else if (i % row_stride) < channels { up } else { left + up - upleft };
        out[i] = ((data[i] as i32 - pred).rem_euclid(256)) as u8;
    }
    out
}

fn process_2d_inverse(encoded: &[u8], channels: usize, row_stride: usize, orig_len: usize) -> Vec<u8> {
    let mut decoded = vec![0u8; orig_len];
    for i in 0..orig_len {
        let left   = if i >= channels          { decoded[i - channels] as i32 } else { 0 };
        let up     = if i >= row_stride        { decoded[i - row_stride] as i32 } else { 0 };
        let upleft = if i >= row_stride + channels { decoded[i - row_stride - channels] as i32 } else { 0 };
        let pred = if i < row_stride { left } else if (i % row_stride) < channels { up } else { left + up - upleft };
        decoded[i] = ((encoded[i] as i32 + pred).rem_euclid(256)) as u8;
    }
    decoded
}

fn optimize_file(raw: &[u8]) -> (EngineType, usize, usize, Vec<u8>) {
    // Bypass small files or files that are already highly compressed
    if raw.len() < 4096 { return (EngineType::ZstdOnly, 1, 1, raw.to_vec()); }
    let raw_entropy = calculate_shannon_entropy(raw);
    if raw_entropy > 7.95 { return (EngineType::ZstdOnly, 1, 1, raw.to_vec()); }

    let sample_len = std::cmp::min(raw.len(), 60_000);
    let sample = &raw[..sample_len];
    let mut best_channels = 1;
    let mut best_1d_score = u64::MAX;

    // Brute-force 1D channels
    for channels in 1..=8 {
        let mut score: u64 = 0;
        for i in channels..sample_len { score += (sample[i] as i32 - sample[i - channels] as i32).abs() as u64; }
        if score < best_1d_score { best_1d_score = score; best_channels = channels; }
    }

    let min_stride = best_channels * 10;
    let max_stride = std::cmp::min(best_channels * 4000, sample_len / 4);
    let mut best_row_stride = best_channels;
    let mut best_2d_score = best_1d_score * 2;

    // Brute-force 2D strides
    if max_stride > min_stride {
        let coarse_step = (best_channels * 8).max(1);
        let mut coarse_best_stride = min_stride;
        let mut coarse_best_score = u64::MAX;

        for stride in (min_stride..=max_stride).step_by(coarse_step) {
            let mut score: u64 = 0;
            for i in stride..std::cmp::min(stride + 3000, sample_len) {
                score += (sample[i] as i32 - sample[i - stride] as i32).abs() as u64;
            }
            if score < coarse_best_score { coarse_best_score = score; coarse_best_stride = stride; }
        }

        let fine_min = coarse_best_stride.saturating_sub(coarse_step).max(min_stride);
        let fine_max = (coarse_best_stride + coarse_step).min(max_stride);
        for stride in (fine_min..=fine_max).step_by(best_channels.max(1)) {
            let mut score: u64 = 0;
            for i in stride..std::cmp::min(stride + 3000, sample_len) {
                score += (sample[i] as i32 - sample[i - stride] as i32).abs() as u64;
            }
            if score < best_2d_score { best_2d_score = score; best_row_stride = stride; }
        }
    }

    let mut channels_data: Vec<Vec<u8>> = vec![Vec::new(); best_channels];
    for (i, &byte) in raw.iter().enumerate() { channels_data[i % best_channels].push(byte); }
    let mut t_bytes_1d = Vec::with_capacity(raw.len() + best_channels);
    for channel in channels_data.iter() { 
        t_bytes_1d.extend_from_slice(&process_1d_forward_multi(channel)); 
    }
    
    let ent_1d = calculate_shannon_entropy(&t_bytes_1d);
    let mut t_bytes_2d = Vec::new();
    let mut ent_2d = f64::MAX;

    if best_row_stride >= 256 && best_2d_score < best_1d_score {
        t_bytes_2d = process_2d_forward(raw, best_channels, best_row_stride);
        ent_2d = calculate_shannon_entropy(&t_bytes_2d);
    }

    let threshold = raw_entropy * 0.99;
    if ent_2d < ent_1d && ent_2d < threshold {
        (EngineType::Agz2D, best_channels, best_row_stride, t_bytes_2d)
    } else if ent_1d < threshold {
        (EngineType::Agz1D, best_channels, best_channels, t_bytes_1d)
    } else {
        (EngineType::ZstdOnly, 1, 1, raw.to_vec())
    }
}

// ============================================================================
// MULTI-THREADED ARCHIVER (Optimized for O(1) Memory Usage per thread)
// ============================================================================

fn pack_archive(input_path: &str, output_path: &str) -> Result<()> {
    let mut output_file = BufWriter::new(File::create(output_path)?);
    output_file.write_all(&AGZ_MAGIC)?;

    let mut paths = Vec::new();
    let base_path = Path::new(input_path);
    for entry in WalkDir::new(input_path).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() { paths.push(entry.path().to_path_buf()); }
    }

    println!("[i] Found {} files. Initiating analysis...", paths.len());

    let total_orig = AtomicU64::new(0);
    let total_comp = AtomicU64::new(0);

    // MPSC channel to stream data from worker threads to the writer thread
    let (tx, rx) = mpsc::sync_channel::<FileRecord>(16);

    // Writer thread
    let write_thread = std::thread::spawn(move || -> Result<()> {
        for r in rx {
            let path_bytes = r.rel_path.as_bytes();
            output_file.write_all(&(path_bytes.len() as u16).to_le_bytes())?;
            output_file.write_all(path_bytes)?;
            output_file.write_all(&[r.engine as u8])?;
            output_file.write_all(&(r.channels as u32).to_le_bytes())?;
            output_file.write_all(&(r.row_stride as u32).to_le_bytes())?;
            output_file.write_all(&(r.orig_size).to_le_bytes())?;
            output_file.write_all(&(r.compressed.len() as u64).to_le_bytes())?;
            output_file.write_all(&r.compressed)?;
        }
        output_file.flush()?;
        Ok(())
    });

    // Worker threads (Rayon)
    paths.par_iter().for_each_with(tx, |s, path| {
        let rel_path = path.strip_prefix(if base_path.is_dir() { base_path } else { base_path.parent().unwrap() })
            .unwrap_or(path).to_string_lossy().into_owned();
            
        let mut raw = Vec::new();
        if File::open(path).and_then(|mut f| f.read_to_end(&mut raw)).is_ok() && !raw.is_empty() {
            let orig_len = raw.len() as u64;
            let (engine, channels, row_stride, transformed) = optimize_file(&raw);
            
            if let Ok(compressed) = encode_all(&transformed[..], 21) {
                total_orig.fetch_add(orig_len, Ordering::Relaxed);
                total_comp.fetch_add(compressed.len() as u64, Ordering::Relaxed);

                println!("[+] Processed: {} (Engine: {:?})", rel_path, engine);
                let _ = s.send(FileRecord { rel_path, engine, channels, row_stride, orig_size: orig_len, compressed });
            }
        }
    });

    write_thread.join().unwrap()?;

    let t_o = total_orig.load(Ordering::Relaxed);
    let t_c = total_comp.load(Ordering::Relaxed);
    println!("========================================");
    println!("AGZ STREAM COMPLETE");
    println!("Original:   {} bytes", t_o);
    println!("Compressed: {} bytes", t_c);
    println!("Ratio:      {:.2}x", t_o as f64 / t_c as f64);
    Ok(())
}

fn unpack_archive(input_path: &str, output_dir: &str) -> Result<()> {
    let file = File::open(input_path)?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if magic != AGZ_MAGIC {
        return Err(Error::new(ErrorKind::InvalidData, "Invalid or corrupt AGZ archive!"));
    }

    fs::create_dir_all(output_dir)?;

    loop {
        let mut path_len_bytes = [0u8; 2];
        match reader.read_exact(&mut path_len_bytes) {
            Ok(_) => (),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break, // Normal EOF
            Err(e) => return Err(e),
        }
        
        let path_len = u16::from_le_bytes(path_len_bytes) as usize;
        let mut path_buf = vec![0u8; path_len];
        reader.read_exact(&mut path_buf)?;
        let path_str = String::from_utf8_lossy(&path_buf).into_owned();

        let mut meta_buf = [0u8; 25]; // 1 + 4 + 4 + 8 + 8 bytes
        reader.read_exact(&mut meta_buf)?;
        
        let engine_byte = meta_buf[0];
        let channels = u32::from_le_bytes(meta_buf[1..5].try_into().unwrap()) as usize;
        let row_stride = u32::from_le_bytes(meta_buf[5..9].try_into().unwrap()) as usize;
        let orig_size = u64::from_le_bytes(meta_buf[9..17].try_into().unwrap()) as usize;
        let comp_size = u64::from_le_bytes(meta_buf[17..25].try_into().unwrap()) as usize;

        let mut compressed_data = vec![0u8; comp_size];
        reader.read_exact(&mut compressed_data)?;

        let transformed_bytes = decode_all(&compressed_data[..])?;

        let final_bytes = match engine_byte {
            0 => transformed_bytes,
            1 => {
                let mut f_bytes = vec![0u8; orig_size];
                let mut read_offset = 0;
                for c_idx in 0..channels {
                    let len = (orig_size / channels + if c_idx < orig_size % channels { 1 } else { 0 }) + 1;
                    let restored = process_1d_inverse_multi(&transformed_bytes[read_offset..read_offset + len]);
                    for (i, &byte) in restored.iter().enumerate() { f_bytes[i * channels + c_idx] = byte; }
                    read_offset += len;
                }
                f_bytes
            },
            2 => process_2d_inverse(&transformed_bytes, channels, row_stride, orig_size),
            _ => return Err(Error::new(ErrorKind::InvalidData, "Unknown compression engine in metadata")),
        };

        let out_path = Path::new(output_dir).join(&path_str);
        if let Some(parent) = out_path.parent() { fs::create_dir_all(parent)?; }
        
        let mut out_file = BufWriter::new(File::create(out_path)?);
        out_file.write_all(&final_bytes)?;
        println!("[+] Extracted: {}", path_str);
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        println!("Agnostic Z-stream (agz)");
        println!("Usage: cargo run --release pack <folder_or_file> <output.agz>");
        println!("       cargo run --release unpack <input.agz> <output_folder>");
        return;
    }
    let command = &args[1];
    let start = Instant::now();
    if command == "pack" {
        if let Err(e) = pack_archive(&args[2], &args[3]) { eprintln!("Critical error: {}", e); }
    } else if command == "unpack" {
        if let Err(e) = unpack_archive(&args[2], &args[3]) { eprintln!("Critical error: {}", e); }
    }
    println!("Time elapsed: {:?}", start.elapsed());
}