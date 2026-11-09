# SDRWispy

Swept SDR Wi-Fi spectrum analyzer with a Rust/Tauri backend and a WebGL frontend.

## Human Says
<img width="2555" height="1379" alt="Screenshot" src="https://github.com/user-attachments/assets/e3bbae80-01fc-42e2-a42e-c1c3412f3fb8" />


## Features

- SoapySDR/UHD swept spectrum capture, enabled by default and loaded dynamically at runtime.
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
npm run setup
```

Run in development mode:

```bash
npm run dev
```

Build a packaged app:

```bash
npm run build
```

Default builds include SDR capture support. For a UI/debug build without the SoapySDR wrapper:

```bash
npm run dev:no-soapy
```

Use `npm run build:no-soapy` for the matching packaged build.

If `cargo tauri` is unavailable:

```bash
cargo install tauri-cli --version "^2"
```

Linux desktop builds require the normal Tauri/WebKitGTK development packages for the target distribution.

The WLAN panel lets you choose a Wi-Fi interface; the interface `Auto` option scans only the first detected interface. The refresh control can automatically update AP scan results every 5-600 seconds, while `Scan` forces an immediate refresh. The panel reports command, permission, interface, backend, and unsupported-platform errors directly.

Linux Wi-Fi scanning has two backends:

- `iw`: runs `iw dev <interface> scan` from the normal app process.
- `privileged_helper`: starts the same executable in `--wifi-helper` mode through `pkexec`, then sends scan requests over stdin/stdout. The helper only accepts fixed Wi-Fi scan commands and returns raw `iw` output for the normal app process to parse.

If `iw dev <interface> scan` fails, install `iw`, check that the wireless interface is enabled, and select `privileged_helper` when the target system requires elevated permission for active scans. Avoid running the whole GUI with `sudo`; that can break display, conda, and SoapySDR runtime environment discovery.

## SoapySDR runtime discovery

SDRWispy enables SoapySDR support by default and loads the SoapySDR C runtime dynamically at startup. The app can still open on machines without SoapySDR, but SDR device enumeration and capture require a usable runtime plus the matching Soapy driver modules. Use `--no-default-features` only for builds that intentionally remove SDR capture support.

Runtime roots are checked in this order:

1. `SDRWISPY_SOAPY_ROOT`
2. `SOAPY_SDR_ROOT`, `SOAPYSDR_ROOT`, or `POTHOS_ROOT`
3. `CONDA_PREFIX`
4. a SoapySDR install visible through `PATH` via `SoapySDRUtil` or the runtime library
5. platform loader defaults such as `libSoapySDR.so`, `SoapySDR.dll`, or `libSoapySDR.dylib`

For an explicit local runtime, point `SDRWISPY_SOAPY_ROOT` at the install prefix that contains `bin/` and `lib/`:

```bash
export SDRWISPY_SOAPY_ROOT=/path/to/soapy-prefix
cargo tauri dev
```

If a conda-style environment is activated before launching, `CONDA_PREFIX` is used automatically. Soapy driver module directories under `lib/SoapySDR/modules*` are added to `SOAPY_SDR_PLUGIN_PATH` for the app process.

## Configuration

Profiles are TOML files. The app starts with built-in defaults; use **Load config…** and **Save config…** in the UI for user profiles.

Example receiver/sweep fields:

```toml
[receiver]
# Units/range come from the driver. gain_db is the legacy key; gain is an alias.
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

### Receiver parameter checks

The app reads back the actual sample rate after configuration and the actual center
frequency after every tune. A rate mismatch greater than 1 ppm (at least 1 Hz), or a
center-frequency mismatch greater than 100 Hz, stops capture with the requested and
reported values instead of drawing a misleading frequency axis. Accepted small
rounding uses the actual values for hop planning and FFT coordinates. Readback checks
verify what the driver reports, not an independent measurement of the hardware.

Bandwidth and gain readbacks appear in the capture status, including requested values;
unsupported readbacks are shown as `unknown`. Bandwidth remains tied to the requested
sample rate, and the usable-bandwidth ratio is explicit, not automatically calibrated
from the analog filter. RX gain is in driver-defined units, which may be dB or discrete
steps. The UI does not impose a 250 MS/s cap or a 0–100 gain range; use values supported
by the selected driver. Non-finite/invalid sweep settings are rejected.

For SoapyW1700K, start with 40 MS/s and a gain in 0–13 (steps, not dB); supported rates
are 40/80/160/320 MS/s. Select the matching enumerated device (`band=0`, `1`, or `2`)
for 2.4/5/6 GHz. Changing the UI's scan-band preset does not switch the hardware band,
and a `band=` override in Soapy args takes precedence over the selected device.
Use a fixed band rather than `band=all`: SDRWispy does not currently call `setAntenna`.
The matching Soapy module, `w1700k` controller and `w1700k-stream` receiver must already
be available. Keep stream gating enabled until retune/backlog behavior has been tested.

## Checks

```bash
npm run check
npm run check:all
```
