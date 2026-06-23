# SDRWispy

Swept SDR Wi-Fi spectrum analyzer with a Rust/Tauri backend and a WebGL frontend.

## Features

- SoapySDR/UHD swept spectrum capture (`--features soapy`).
- Rust-side tuning, IQ read, FFT/PSD, sweep stitching, average and max-hold.
- WebGL spectrum and waterfall rendering.
- Wi-Fi AP overlay with RSSI-scaled channel shapes.
- Wi-Fi scan via Windows Native Wi-Fi API or Linux `iw`.
- TOML profiles for RF band ranges, sweep settings, receiver args, display options, and hidden AP overlays.

Raw IQ is processed in Rust; the frontend only receives display-ready `SpectrumFrame` data.

## Layout

```text
frontend/                 TypeScript + Vite UI
  src/main.ts             UI state and Tauri IPC
  src/renderers.ts        WebGL spectrum/waterfall renderers
  src/style.css           Dark theme

src-tauri/                Tauri/Rust backend
  src/main.rs             App state, commands, worker management
  src/model.rs            Shared data models
  src/real_sweep.rs       SoapySDR sweep + FFT/PSD stitching
  src/soapy.rs            Dynamic SoapySDR C API wrapper
  src/sdr.rs              SDR device catalog
  src/wifi.rs             Windows/Linux Wi-Fi scan backends
  src/profile.rs          TOML profile parser

profiles/
  default.toml            Default 2.4 GHz profile
  default_5g.toml         Default 5 GHz profile
  chanalyzer.toml         Chanalyzer-like output-bin profile
```

## Build and run

Install frontend dependencies once per platform:

```bash
cd frontend
npm install --include=optional
```

Run in development mode:

```bash
cd src-tauri
cargo tauri dev --features soapy
```

Build a packaged app:

```bash
cd src-tauri
cargo tauri build --features soapy
```

If `cargo tauri` is unavailable:

```bash
cargo install tauri-cli --version "^2"
```

Linux desktop builds require the normal Tauri/WebKitGTK development packages for the target distribution.

## Configuration

Profiles are TOML files. The app starts with built-in defaults; use **Load config…** and **Save config…** in the UI for user profiles.

Example receiver/sweep fields:

```toml
[receiver]
gain_db = 40.0
# Optional exact SoapySDR device id, useful only for machine-specific profiles.
# device_id = "driver=uhd,type=b200"
# Optional extra args appended to/overriding enumerated Soapy args.
# soapy_args = "serial=..., clock_source=external"

[sweep]
sample_rate_msps = 20.0
usable_bandwidth_ratio = 0.80
overlap_mhz = 5.0
dwell_ms = 3.0
settle_ms = 2.0
fft_size = 4096
```

The built-in default sample rate / RX bandwidth is 20 MS/s.

## Checks

```bash
npm --prefix frontend run check
npm --prefix frontend run build

cargo check --manifest-path src-tauri/Cargo.toml --features soapy
cargo clippy --manifest-path src-tauri/Cargo.toml --features soapy -- -D warnings
```
