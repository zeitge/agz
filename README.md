# agz (Agnostic Z-stream)

**agz** is a high-performance, memory-efficient, and domain-agnostic archival tool built in Rust.
It utilizes a hybrid approach to data compression by combining Heuristic Adaptive Pre-conditioning with the Zstandard (Zstd) entropy coder. **agz** dynamically analyzes data to identify spatial and linear correlations, reducing Shannon entropy before final compression.

## Technical Implementation Details

* **Engine 0 (ZstdOnly):** Used for high-entropy data (already compressed files, encrypted data).
* **Engine 1 (Agz1D):** Optimized for multi-channel interleaved data like raw PCM audio.
* **Engine 2 (Agz2D):** Designed for 2D spatial data like uncompressed bitmaps or structured sensor arrays.

## Core Concept

The primary strength of **agz** is its ability to flatten data variance in uncompressed binary formats (like raw audio, telemetry, or bitmaps) where standard compression often fails.

The process follows a 3-stage pipeline:

* **Heuristic Analysis:** The engine samples the data to identify optimal 1D or 2D "strides" (interleaved channels or row widths).
* **Adaptive Pre-conditioning:** Based on the analysis, it applies one of several prediction models (Repeat, Linear, Cubic, or Median) to transform absolute values into minimal residuals.
* **Entropy Coding:** The resulting low-entropy residual stream is compressed using Zstandard at high-compression levels (Level 21).

## Key Features

* **Domain-Agnostic:** Automatically detects data structures without relying on file extensions.
* **Memory-Efficient:** Implements a streaming I/O architecture with $O(1)$ memory complexity per thread for archiving and $O(M)$ for extraction (where $M$ is the largest single file).
* **Highly Parallelized:** Leverages the Rayon framework for multi-core analysis and a dedicated MPSC channel-based writer thread to prevent I/O bottlenecks.
* **Lossless Integrity:** Mathematically verified reconstruction through inverse differential pulse-code modulation (DPCM).

## Performance Comparison

In benchmark tests involving hybrid datasets (PDFs, raw WAV audio, and BMP images), **agz** has demonstrated better compression ratios compared to standard ZIP and 7z (LZMA2) for uncompressed media assets.

| Tool | Compressed Size |
| :--- | :--- |
| ZIP (Deflate) | 173.7 MB |
| 7z (LZMA2) | 162.5 MB |
| **agz** (Hybrid) | **161.6 MB** |

## Installation & Usage

### Prerequisites

* Rust (Stable)
* Cargo

### Build

```bash
cargo build --release
```

### Usage

**Pack a directory or file:**
```bash
./target/release/agz pack <source_path> <output.agz>
```

**Unpack an archive:**
```bash
./target/release/agz unpack <input.agz> <output_directory>
```

## Technical Implementation Details

* **Engine 0 (ZstdOnly):** Used for high-entropy data (already compressed files, encrypted data).
* **Engine 1 (Agz1D):** Optimized for multi-channel interleaved data like raw PCM audio.
* **Engine 2 (Agz2D):** Designed for 2D spatial data like uncompressed bitmaps or structured sensor arrays.

## License

This project is licensed under the **MIT License**.
See the [LICENSE](LICENSE) file for details.

The software is provided "as is", without warranty of any kind, express or implied.
