# SDRWispy

Swept SDR Wi-Fi spectrum analyzer with a Rust/Tauri backend and a WebGL frontend.

## Human Says
<img width="2555" height="1379" alt="Screenshot" src="https://github.com/user-attachments/assets/e3bbae80-01fc-42e2-a42e-c1c3412f3fb8" />


## Features

- SoapySDR/UHD swept spectrum capture, enabled by default and loaded dynamically at runtime.
- Rust-side tuning, IQ read, FFT/PSD, sweep stitching, average and max-hold.
- WebGL spectrum and waterfall rendering.
- Wi-Fi AP overlay with RSSI-scaled channel shapes.
- Wi-Fi scan via Windows Native Wi-Fi API, Linux `iw`, or macOS CoreWLAN.
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
  w1700k.toml             W1700K 2.4 GHz, 160 MS/s, continuous stream
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

## macOS

Build natively with Xcode Command Line Tools, Node.js and Rust 1.88 or newer:

```bash
xcode-select --install
brew install node soapysdr
cargo install tauri-cli --version "^2"
npm run setup
export SDRWISPY_SOAPY_ROOT="$(brew --prefix soapysdr)"
npm run build
```

The macOS configuration builds an `.app` bundle (minimum macOS 11). On Apple Silicon,
keep the app, Soapy runtime and driver modules on the same architecture. Soapy and its
device plugins are external runtime dependencies, not bundled by these commands.
`npm run build:no-soapy` builds a Wi-Fi/UI-only app without SDR enumeration or capture.

The Wi-Fi backend uses CoreWLAN directly, with a small Objective-C bridge compiled at
build time; no Swift interpreter, `airport`, `sudo` or runtime helper is needed. Auto
selects the first Wi-Fi interface; explicit interface selection is also supported.
Scanning only reads nearby AP information and does not join networks or change Wi-Fi
power, interface settings or SDR state.

Recent macOS releases require **Location Services authorization to disclose SSIDs and
BSSIDs**. Open the built `.app` locally and allow SDRWispy in **System Settings →
Privacy & Security → Location Services**, then press Scan again. The app includes the
usage descriptions and requests authorization on the main thread; it does not request
geographic location updates. A development executable or SSH session may receive
redacted identities even when channel, RSSI and PHY are available. The status explains
this case; anonymous BSSIDs are not fabricated or combined into misleading AP overlays.

Channel centers use HT/VHT/HE operation information where available and standard
5/6 GHz contiguous channel blocks otherwise. An 80+80 operation with segment information
is shown as its primary 80 MHz segment. Unknown geometry uses a conservative width;
320 MHz EHT geometry is not yet decoded.

### Wi-Fi-only native test

From the project root on a Mac, compile the exact production bridge with a small probe:

```bash
xcrun clang -fobjc-arc -mmacosx-version-min=11.0 -Wall -Wextra -Werror \
  src-tauri/native/macos_wifi.m tests/macos_wifi_probe.m \
  -framework Foundation -framework CoreWLAN -framework CoreLocation \
  -o /tmp/sdrwispy-wifi-probe
/tmp/sdrwispy-wifi-probe
/tmp/sdrwispy-wifi-probe en0
```

This probe does not ask for permission or touch SDRs. Its default output omits SSIDs,
BSSIDs and raw information elements; `--json` optionally emits the full native result.

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
You can also launch with `--profile /path/to/profile.toml --run` to load a profile
and start capture immediately. On macOS, pass these after `open -a /path/to/SDRWispy.app --args`.

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
stream_gating = true
```

The built-in default sample rate / RX bandwidth is 20 MS/s.
`stream_gating` defaults to `true` in older profiles: RX stops around each retune,
which is more reliable for B2xx/UHD. Set it to `false` only for drivers supporting
active retunes; the UI exposes this as **Stop stream during tuning**. An explicit
`SDRWISPY_STREAM_GATING` environment variable overrides the profile (`1`, `true`,
`yes`, `on` enable it, case-insensitively; any other value disables it).
Sample rate, bandwidth and gain are set once when opening the receiver; each sweep
hop changes only frequency, plus stream activation/deactivation when gating is enabled.

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

### W1700K example

Install the updated SoapyW1700K module and matching `w1700k` host controller from
our `MT7996-ap-sdr` fork of `w1700k-sdr` (persistent-control update `e222485` or later),
configure its device/SSH connection, and start `w1700k-stream` on the
same host as SDRWispy. Make `w1700k` available through `PATH` or set
`W1700K_CMD=/path/to/w1700k` before launching the app. Set `SOAPY_SDR_PLUGIN_PATH`
if the module is outside Soapy's plugin search path, then verify:

```bash
SoapySDRUtil --find="driver=w1700k"
SoapySDRUtil --probe="driver=w1700k,band=0"
```

Select W1700K 2.4G, load **profiles/w1700k.toml**, and press Run. It uses 160 MS/s
(128 MHz usable span, covering 2400–2485 MHz in one hop), gain 8 steps, and disables
flattening and stream gating. The updated driver's default `control=serve` reuses
a persistent control session and discards queued samples after tuning. Keep the
profile's settle/discard margin: UDP packets already in transit can still contain
old samples. This is a starting configuration, not a calibrated timing guarantee.

Supported rates are 40/80/160/320 MS/s; gain is 0–13 steps, not dB. For 5 or 6 GHz,
change **both** the scan-band preset and the profile's Soapy args to `band=1` or
`band=2`, respectively. The explicit `band=` overrides the selected device's band.
Use a fixed band rather than `band=all`: SDRWispy does not call `setAntenna`.
Older drivers should keep stream gating enabled; `control=cli` is the driver's
compatibility mode for older host controllers.

## Checks

```bash
npm run check
npm run check:all
```
