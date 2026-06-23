# SDRWispy

Swept SDR Wi-Fi spectrum analyzer using a **Tauri + WebGL** GUI and a Rust backend.

The project now keeps only the Tauri/WebGL implementation. The old egui desktop prototype and simulation/session code have been removed.

## What it does

- SDR swept spectrum capture through SoapySDR/UHD when built with `--features soapy`.
- Rust backend performs tuning, short IQ reads, FFT/PSD, sweep stitching, average, max-hold, and bounded history.
- WebGL frontend renders:
  - current spectrum;
  - average spectrum;
  - max-hold;
  - waterfall;
  - Wi-Fi AP channel overlays.
- Wi-Fi scanner:
  - Windows Native Wi-Fi API;
  - Linux `iw` scan backend.
- TOML config:
  - startup uses built-in defaults;
  - `Load config…` opens a TOML profile;
  - `Save config…` writes the current settings to TOML.
  - profiles cover RF band ranges, sweep/sample bandwidth, receiver/device hints,
    display toggles/ranges, and per-BSSID hidden Wi-Fi overlays.
- RF/Sweep panel includes SDR device, Soapy args, RX gain, band, sample rate, dwell, settle, FFT size.
- WLAN AP panel can be collapsed to free screen space.

Raw IQ is not sent to the frontend. Tauri IPC only sends display-ready `SpectrumFrame` data.

## Project layout

```text
frontend/                 TypeScript + Vite frontend
  src/main.ts             UI and Tauri IPC
  src/renderers.ts        WebGL spectrum/waterfall renderers
  src/style.css           dark UI theme

src-tauri/                Rust/Tauri application
  src/main.rs             Tauri commands, state, worker management
  src/model.rs            shared data models
  src/real_sweep.rs       SoapySDR sweep + FFT/PSD stitching
  src/soapy.rs            dynamic SoapySDR C API wrapper
  src/sdr.rs              SDR catalog abstraction
  src/wifi.rs             Windows/Linux Wi-Fi scanning
  src/profile.rs          TOML profile parsing

profiles/                 example TOML profiles
  default.toml            default 2.4 GHz startup-style profile
  default_5g.toml         same defaults with 5 GHz selected
  chanalyzer.toml         Chanalyzer-compatible output-bin profile
```

## Windows real-device run

```powershell
cd N:\tmpfiles\202606_sdrwispy
$env:POTHOS_ROOT="N:\Program Files\PothosSDR"

cd frontend
# Do not reuse WSL node_modules on Windows.
Remove-Item -Recurse -Force node_modules -ErrorAction SilentlyContinue
npm install --include=optional

cd ..\src-tauri
cargo tauri dev --features soapy
```

If `cargo tauri` is missing:

```powershell
cargo install tauri-cli --version "^2"
```

## Soapy/UHD args

The Tauri version does not hide extra UHD parameters behind environment variables. If a device needs explicit args, put them in the GUI **Soapy args** field, for example:

```text
serial=..., clock_source=external, antenna=RX2
```

Duplicate keys override the selected device's enumerated args.

The same can be stored in TOML:

```toml
[receiver]
device_id = "driver=uhd,type=b200" # optional; omit for portable profiles
gain_db = 40.0
soapy_args = "serial=..., clock_source=external"

[sweep]
sample_rate_msps = 20.0
```

The built-in default sample rate / RX bandwidth is 20 MS/s. Increase it in the
RF/Sweep panel or TOML only when the device and host can sustain the rate.

## Development checks

From WSL/Linux for code checks:

```bash
cd frontend
npm install --include=optional
npm run check
npm run build

cd ../src-tauri
cargo check --target x86_64-pc-windows-gnu --features soapy
cargo clippy --target x86_64-pc-windows-gnu --features soapy
```

Native Linux Tauri GUI builds require GTK/WebKit development packages. WSL is mainly used here for frontend checks and Windows-target Rust checks; run the actual B210 test on Windows.
