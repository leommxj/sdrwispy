import { invoke } from "@tauri-apps/api/core";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";
import "./style.css";
import { DisplayClock, FrameFreshness, frameRangeKey, frameTimeMs, PeakEnvelope } from "./display-timing";
import { SpectrumRenderer, WaterfallRenderer, WifiOverlayRenderer, cssColorForBssid, frequencyTicks, powerColor, type FrequencyView, type SpectrumDisplayOptions, type SpectrumFrame, type WifiBss } from "./renderers";

interface BandConfig { preset: string; start_hz: number; stop_hz: number; bin_width_hz: number; }
interface BandDefaults { ghz2: [number, number, number]; ghz5: [number, number, number]; ghz6: [number, number, number]; }
interface SweepConfig { sample_rate_hz: number; usable_bandwidth_ratio: number; overlap_hz: number; dwell_ms: number; settle_ms: number; fft_size: number; stream_gating: boolean; }
interface DisplaySettings {
  show_current: boolean;
  show_average: boolean;
  show_max_hold: boolean;
  peak_decay_db_per_sec: number;
  waterfall_span_s: number;
  waterfall_floor_db: number;
  waterfall_ceiling_db: number;
  show_wifi_overlay: boolean;
  show_wifi_labels: boolean;
  label_aps_by_bssid: boolean;
  spectrum_floor_db: number;
  spectrum_ceiling_db: number;
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
}
interface SdrDevice { id: string; label: string; driver: string; }
interface WifiBackendOption { id: string; label: string; requires_elevation: boolean; }
interface Snapshot {
  band: BandConfig;
  band_defaults: BandDefaults;
  sweep: SweepConfig;
  display: DisplaySettings;
  sdr_gain_db: number;
  soapy_args: string;
  devices: SdrDevice[];
  selected_device_index: number;
  aps: WifiBss[];
  wifi_interfaces: string[];
  selected_wifi_interface: string;
  wifi_backends: WifiBackendOption[];
  selected_wifi_backend: string;
  wifi_auto_scan_enabled: boolean;
  wifi_auto_scan_interval_secs: number;
  wifi_scan_in_progress: boolean;
  wifi_helper_running: boolean;
  wifi_status: string;
  running: boolean;
  capture_epoch: number;
  status: string;
  profile_name: string;
  profile_path: string;
  latest_frame_id?: number;
  history_len: number;
  hidden_wifi_overlay_bssids: string[];
}

interface HistoryInfo { capture_epoch: number; frame_ids: number[]; oldest_time_ns: number | null; newest_time_ns: number | null; }
interface FrameBatch { capture_epoch: number; frames: SpectrumFrame[]; latest: SpectrumFrame | null; history: HistoryInfo; missed_frames: number | null; total_evicted: number; more: boolean; }
interface HistoryFrame { frame: SpectrumFrame | null; expired: boolean; history: HistoryInfo; }

interface WifiSnapshot {
  aps: WifiBss[];
  wifi_interfaces: string[];
  selected_wifi_interface: string;
  wifi_backends: WifiBackendOption[];
  selected_wifi_backend: string;
  wifi_auto_scan_enabled: boolean;
  wifi_auto_scan_interval_secs: number;
  wifi_scan_in_progress: boolean;
  wifi_helper_running: boolean;
  wifi_status: string;
  status: string;
  hidden_wifi_overlay_bssids: string[];
}

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = `
  <div class="shell">
    <header class="appbar">
      <div class="brand">
        <div class="title">SDRWispy</div>
        <div class="subtitle">Swept SDR Wi-Fi spectrum analyzer · Tauri/WebGL</div>
      </div>
      <div class="runbar">
        <button id="run" class="primary">Run</button>
        <button id="stop">Stop</button>
        <button id="refresh">Refresh</button>
        <button id="toggleSettings" aria-expanded="true" title="Show or hide settings">Settings</button>
      </div>
      <div class="status" id="status">Loading...</div>
    </header>

    <div class="layout">
      <aside class="sidebar">
        <section class="card">
          <h3>Profile</h3>
          <div class="row"><button id="loadConfig">Load config…</button><button id="saveConfig">Save config…</button></div>
          <div class="hint">Startup uses built-in defaults. Load or save a user TOML profile with a file dialog.</div>
        </section>

        <section class="card">
          <h3>RF / Sweep</h3>
          <div class="field"><label>SDR device</label><select id="deviceSelect"></select></div>
          <button id="refreshDevices">Refresh devices</button>
          <div class="field"><label>Soapy args appended / override</label><input id="soapyArgs" type="text" spellcheck="false" placeholder="serial=..., clock_source=external, antenna=RX2"></div>
          <div class="hint">Comma-separated SoapySDR key=value args. Duplicate keys override the selected device args.</div>
          <div class="field"><label>RX gain (driver units)</label><input id="gainDb" type="number" step="any"></div>
          <div class="field"><label>Band</label></div>
          <div class="segmented"><button data-band="2.4g">2.4G</button><button data-band="5g">5G</button><button data-band="6e">6E</button></div>
          <div class="grid2">
            <div class="field"><label>Start MHz</label><input id="startMhz" type="number" step="1" min="1"></div>
            <div class="field"><label>Stop MHz</label><input id="stopMhz" type="number" step="1" min="1"></div>
            <div class="field"><label>Bin kHz</label><input id="binKhz" type="number" step="10" min="1"></div>
          </div>
          <div class="grid2">
            <div class="field"><label>Sample MS/s</label><input id="sampleRate" type="number" step="any" min="0"></div>
            <div class="field"><label>Usable ratio</label><input id="usableRatio" type="number" step="0.01" min="0.1" max="0.95"></div>
            <div class="field"><label>Overlap MHz</label><input id="overlap" type="number" step="0.5" min="0"></div>
            <div class="field"><label>Dwell ms</label><input id="dwell" type="number" step="0.5" min="0.1"></div>
            <div class="field"><label>Settle ms</label><input id="settle" type="number" step="0.5" min="0"></div>
            <div class="field"><label>FFT size</label><input id="fft" type="number" step="256" min="256"></div>
          </div>
          <label class="check" title="Disable only for drivers that support tuning while streaming. SDRWISPY_STREAM_GATING overrides this setting."><input id="streamGating" type="checkbox"> Stop stream during tuning</label>
          <button id="applyRfSweep">Apply RF / sweep</button>
          <div id="rfPending" class="hint pending hidden">RF/sweep edits are pending. Apply or Run commits them.</div>
        </section>

        <details class="card" open>
          <summary>Display</summary>
          <div class="grid2 checks">
            <label class="check"><input id="showCurrent" type="checkbox"> Current</label>
            <label class="check"><input id="showAverage" type="checkbox"> Average</label>
            <label class="check" title="Live: display-only peak envelope. Replay: recorded historical maximum."><input id="showMaxHold" type="checkbox"> Max hold</label>
            <label class="check"><input id="showWifiOverlay" type="checkbox"> Wi-Fi overlay</label>
            <label class="check"><input id="showWifiLabels" type="checkbox"> AP labels</label>
          </div>
          <label class="check"><input id="labelBssid" type="checkbox"> Prefer BSSID labels</label>
          <div class="grid2">
            <div class="field"><label for="peakDecay">Peak fall (dB/s)</label><input id="peakDecay" type="number" min="0" max="120" step="0.5" title="Live Max hold fall rate. 0 holds observed peaks; replay never decays. Fall pauses after 2 seconds without fresh valid measurements."></div>
            <div class="field"><label for="waterfallSpan">Waterfall span (s)</label><input id="waterfallSpan" type="number" min="1" max="3600" step="1" title="Visible time span; smaller values scroll faster."></div>
          </div>
          <div class="row"><button id="resetPeaks" title="Reset the live display peak envelope; recorded historical maxima stay unchanged">Reset live peaks</button><button id="autoLevels" title="Fit levels once to the current valid measurements, not continuously">Fit levels once</button></div>
          <div class="hint">Display levels are relative dB, not calibrated dBm. Gray = no observation; purple = DC excluded. Waterfall rows mark sweep end times, not simultaneous whole-band samples.</div>
          <div class="grid2">
            <div class="field"><label for="waterfallFloor">Waterfall floor</label><input id="waterfallFloor" type="number" min="-200" max="50" step="1"></div>
            <div class="field"><label for="waterfallCeil">Waterfall ceiling</label><input id="waterfallCeil" type="number" min="-200" max="50" step="1"></div>
          </div>
          <div class="display-control">
            <label>Spectrum floor</label>
            <input id="spectrumFloor" type="number" step="1">
            <input id="spectrumFloorSlider" type="range" min="-150" max="20" step="1">
          </div>
          <div class="display-control">
            <label>Spectrum ceiling</label>
            <input id="spectrumCeil" type="number" step="1">
            <input id="spectrumCeilSlider" type="range" min="-150" max="20" step="1">
          </div>
          <div class="display-control">
            <label>RSSI floor</label>
            <input id="rssiFloor" type="number" step="1">
            <input id="rssiFloorSlider" type="range" min="-110" max="-20" step="1">
          </div>
          <div class="display-control">
            <label>RSSI ceiling</label>
            <input id="rssiCeil" type="number" step="1">
            <input id="rssiCeilSlider" type="range" min="-110" max="-20" step="1">
          </div>
          <details class="advanced-display">
            <summary>Advanced overlay</summary>
            <div class="display-control">
              <label>Overlay floor</label>
              <input id="overlayFloor" type="number" step="1">
              <input id="overlayFloorSlider" type="range" min="-130" max="-10" step="1">
            </div>
            <div class="display-control">
              <label>Overlay ceiling</label>
              <input id="overlayCeil" type="number" step="1">
              <input id="overlayCeilSlider" type="range" min="-130" max="-10" step="1">
            </div>
          </details>
          <div class="hint">Display settings apply immediately. Numeric fields commit on Enter or blur; sliders update live.</div>
        </details>
      </aside>

      <main class="workspace">
        <div class="topbar" id="topbar"></div>
        <div class="historybar">
          <div class="history-mode"><button id="historyLive">Live</button><button id="historyReplay" title="Freeze the waterfall and inspect a completed sweep; acquisition continues">Freeze / Replay</button></div>
          <button id="historyPrev">Prev</button>
          <input id="historySlider" type="range" min="0" max="0" step="1" value="0">
          <button id="historyNext">Next</button>
          <span id="historyFrameLabel">history 0/0</span>
          <span id="historyTimeLabel">--</span>
        </div>
        <div class="plot-toolbar"><div class="trace-legend"><span class="trace-current">Current</span><span class="trace-average">Average</span><span class="trace-peak" id="peakLegend">Peak</span></div><button id="resetZoom" title="Reset display zoom only; RF settings do not change">Full band</button><span class="hint">Drag to zoom · double-click to reset</span></div>
        <section class="panel spectrum-panel"><div class="panel-title">Spectrum · relative dB</div><div class="level-axis" id="levelAxis"></div><div class="plot-area spectrum-area"><canvas id="spectrum"></canvas><canvas id="wifiOverlay"></canvas><div id="wifiLabels" class="wifi-label-layer"></div><div class="plot-cursor hidden"></div><div class="zoom-selection hidden"></div></div></section>
        <div class="frequency-axis" id="freqAxis"></div>
        <div id="plotDivider" role="separator" aria-label="Adjust spectrum height" aria-orientation="horizontal" tabindex="0" title="Drag to resize; arrow keys adjust height"></div>
        <section class="panel waterfall-panel"><div class="panel-title" id="waterfallTitle">Waterfall</div><div class="waterfall-times"><span id="waterfallTopTime"></span><span id="waterfallBottomTime"></span></div><div class="plot-area waterfall-area"><canvas id="waterfall"></canvas><div id="waterfallUnavailable" class="unavailable-time hidden"></div><div class="plot-cursor hidden"></div><div class="zoom-selection hidden"></div></div></section>
        <div class="waterfall-legend"><span id="waterfallLow"></span><span id="waterfallColorbar"></span><span id="waterfallHigh"></span><span class="no-data-key">No observation</span><span class="dc-key">DC excluded</span></div>
        <div class="plot-status" id="plotStatus" aria-live="polite"></div><div id="plotTooltip" class="plot-tooltip hidden"></div>
      </main>

      <aside class="rightbar">
        <section class="card wifi-card">
          <div class="card-head"><h3>WLAN APs</h3><div class="row compact"><button id="scanWifi">Scan</button><button id="overlayAll">All</button><button id="overlayNone">None</button><button id="toggleWifiPanel" title="Collapse WLAN panel">Collapse</button></div></div>
          <div class="wifi-interface-row"><label for="wifiBackend">Backend</label><select id="wifiBackend"></select><span id="wifiBackendState" class="backend-state"></span></div>
          <div class="wifi-interface-row"><label for="wifiInterface">Interface</label><select id="wifiInterface"></select></div>
          <div class="wifi-interface-row"><label for="autoWifiScan">Refresh</label><label class="check auto-scan-check"><input id="autoWifiScan" type="checkbox"> Auto</label><div class="interval-control"><input id="wifiAutoInterval" type="number" min="5" max="600" step="5"><span>s</span></div></div>
          <div class="muted" id="apCount"></div>
          <div class="ap-table"><table><thead><tr><th>Overlay</th><th>Name</th><th>BSSID</th><th>Ch</th><th>Primary</th><th>Center</th><th>Width</th><th>RSSI</th><th>PHY</th></tr></thead><tbody id="apRows"></tbody></table></div>
        </section>
      </aside>
    </div>
  </div>
`;

let spectrum: SpectrumRenderer;
let wifiOverlay: WifiOverlayRenderer;
let waterfall: WaterfallRenderer;
let snap: Snapshot | null = null;
let liveFrameEpoch: number | undefined = undefined;
let liveFrameRevision: number | undefined = undefined;
let liveFrame: SpectrumFrame | null = null;
let latestFrame: SpectrumFrame | null = null;
let historyMode: "live" | "replay" = "live";
let replayFrameId: number | null = null;
let historyInfo: HistoryInfo = { capture_epoch: 0, frame_ids: [], oldest_time_ns: null, newest_time_ns: null };
let completeEpoch: number | undefined;
let completeFrameId: number | undefined;
let missedFrames = 0;
let deliveryCountUnknown = false;
let replayExpired = false;
let view: FrequencyView | null = null;
let settingsCollapsed = false;
let waterfallEndMs: number | null = null;
let suppressPlotClick = false;
let historyRequestSeq = 0;
let hidden = new Set<string>();
let wifiCollapsed = false;
let activeEpoch = 0;
let rfDirtyFields = new Set<string>();
let selectedBssid: string | null = null;
let displayPatchSeq = 0;
let fatalShown = false;
let snapshotTimer: number | undefined = undefined;
let wifiAutoTimer: number | undefined = undefined;
let displayPatchTimer: number | undefined = undefined;
let pendingDisplayPatch: DisplayPatch = {};
let displayPatchInFlight = false;
let snapshotRefreshInFlight = false;
let wifiScanInFlight = false;
let wifiSnapshotRefreshInFlight = false;
let wifiScanPollTimer: number | undefined = undefined;
let lastAutoWifiScanAtMs = 0;
let apRowsSignature = "";
let wifiLabelsSignature = "";
const peakEnvelope = new PeakEnvelope();
const frameFreshness = new FrameFreshness();
const waterfallClock = new DisplayClock();
let displayAnimationId: number | undefined;
let displayGeneration = 0;
let visibilitySyncPending = false;
let stopRequested = false;
let waterfallTimeSignature = "";

const rfInputIds = ["gainDb", "soapyArgs", "startMhz", "stopMhz", "binKhz", "sampleRate", "usableRatio", "overlap", "dwell", "settle", "fft", "streamGating"];

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`UI element #${id} is missing`);
  return node as T;
}

function mhz(hz: number): string { return (hz / 1e6).toFixed(1); }
function html(value: string): string {
  return value.replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[ch]!));
}
function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
function setStatusText(message: string) {
  const node = document.getElementById("status");
  if (node) node.textContent = message;
}
function showCommandError(context: string, err: unknown) {
  const detail = errorMessage(err);
  const message = detail ? `${context}: ${detail}` : context;
  console.error(message, err);
  if (snap) {
    snap = { ...snap, status: message };
    renderSnapshot();
  } else {
    setStatusText(message);
  }
}
function commandHandler(context: string, fn: (ev: Event) => Promise<void> | void): (ev: Event) => void {
  return ev => {
    void Promise.resolve(fn(ev)).catch(err => showCommandError(context, err));
  };
}
function hasRealSelectedDevice(snapshot: Snapshot): boolean {
  const device = snapshot.devices[snapshot.selected_device_index];
  return !!device && device.driver !== "soapy-info";
}
function setNumberInput(id: string, value: number | string) {
  const input = el<HTMLInputElement>(id);
  if (document.activeElement !== input) input.value = String(value);
}
function setNumberInputForced(id: string, value: number | string) {
  el<HTMLInputElement>(id).value = String(value);
}
function setRangeInput(id: string, value: number | string) {
  const input = el<HTMLInputElement>(id);
  input.value = String(value);
}
function setTextInput(id: string, value: string) {
  const input = el<HTMLInputElement>(id);
  if (document.activeElement !== input) input.value = value;
}
function setCheckbox(id: string, checked: boolean) {
  el<HTMLInputElement>(id).checked = checked;
}
function isRfDirty(): boolean {
  return rfDirtyFields.size > 0;
}
function markRfDirty(ev: Event) {
  const id = (ev.currentTarget as HTMLInputElement).id;
  if (id) rfDirtyFields.add(id);
  updateRfDirtyUi();
}
function clearRfDirty() {
  rfDirtyFields.clear();
  updateRfDirtyUi();
}
function bandDefaultsForButton(value: string | undefined): [number, number, number] | null {
  if (!snap || !value) return null;
  if (value === "2.4g") return snap.band_defaults.ghz2;
  if (value === "5g") return snap.band_defaults.ghz5;
  if (value === "6e") return snap.band_defaults.ghz6;
  return null;
}
function applyBandPresetToRfForm(value: string | undefined) {
  const defaults = bandDefaultsForButton(value);
  if (!defaults) return;
  const [startMhz, stopMhz, binKhz] = defaults;
  setNumberInputForced("startMhz", startMhz.toFixed(3));
  setNumberInputForced("stopMhz", stopMhz.toFixed(3));
  setNumberInputForced("binKhz", binKhz.toFixed(3));
  rfDirtyFields.add("startMhz");
  rfDirtyFields.add("stopMhz");
  rfDirtyFields.add("binKhz");
  updateRfDirtyUi();
}
function updateRfDirtyUi() {
  const btn = el<HTMLButtonElement>("applyRfSweep");
  const dirty = isRfDirty();
  btn.textContent = dirty ? "Apply RF / sweep *" : "Apply RF / sweep";
  btn.classList.toggle("pending-button", dirty);
  el<HTMLDivElement>("rfPending").classList.toggle("hidden", !dirty);
}
function resetDisplayHistory() {
  displayGeneration++;
  historyRequestSeq++;
  frameFreshness.reset();
  peakEnvelope.reset();
  waterfallClock.reset();
  if (typeof spectrum !== "undefined") spectrum.clear();
  liveFrameEpoch = undefined;
  liveFrameRevision = undefined;
  liveFrame = null;
  latestFrame = null;
  historyMode = "live";
  replayFrameId = null;
  historyInfo = { capture_epoch: 0, frame_ids: [], oldest_time_ns: null, newest_time_ns: null };
  completeEpoch = undefined;
  completeFrameId = undefined;
  missedFrames = 0;
  deliveryCountUnknown = false;
  replayExpired = false;
  view = null;
  if (typeof spectrum !== "undefined") spectrum.setView(null);
  if (typeof wifiOverlay !== "undefined") wifiOverlay.setView(null);
  if (typeof waterfall !== "undefined") waterfall.setView(null);
  wifiLabelsSignature = "";
  const layer = document.getElementById("wifiLabels");
  if (layer) layer.innerHTML = "";
  if (typeof wifiOverlay !== "undefined") wifiOverlay.clear();
  waterfall.clear();
  updateHistoryControls();
  redrawWaterfallView();
}

function updateWifiPanel() {
  document.querySelector<HTMLDivElement>(".layout")?.classList.toggle("wifi-collapsed", wifiCollapsed);
  document.querySelector<HTMLElement>(".rightbar")?.classList.toggle("collapsed", wifiCollapsed);
  const btn = el<HTMLButtonElement>("toggleWifiPanel");
  btn.textContent = wifiCollapsed ? "Expand" : "Collapse";
  btn.title = wifiCollapsed ? "Expand WLAN panel" : "Collapse WLAN panel";
}

function clampHistoryIndex(index: number): number {
  const len = historyInfo.frame_ids.length;
  return Math.max(0, Math.min(Math.max(0, len - 1), Math.round(index)));
}

function formatFrameTime(frame: SpectrumFrame | null): string {
  if (!frame) return "--";
  const ns = frame.sweep_end_time_ns || frame.sweep_start_time_ns;
  const ms = ns / 1e6;
  if (!Number.isFinite(ms) || ms <= 0) return "--";
  const date = new Date(ms);
  const pad = (value: number, size = 2) => String(value).padStart(size, "0");
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
}

function formatScanProgress(frame: SpectrumFrame): string {
  const total = Math.max(0, frame.estimated_hops);
  const width = Math.max(1, String(total).length);
  const done = Math.min(frame.completed_hops, total);
  return `scanning ${String(done).padStart(width, "0")}/${String(total).padStart(width, "0")}`;
}

function overlayDbRange(): [number, number] {
  return [-115, -20];
}

function updateHistoryControls() {
  const ids = historyInfo.frame_ids;
  const len = ids.length;
  const canReplay = len > 0;
  const replayIndex = replayFrameId === null ? -1 : ids.indexOf(replayFrameId);
  const sliderIndex = historyMode === "replay" ? Math.max(0, replayIndex) : Math.max(0, len - 1);

  const live = el<HTMLButtonElement>("historyLive");
  const replay = el<HTMLButtonElement>("historyReplay");
  const prev = el<HTMLButtonElement>("historyPrev");
  const next = el<HTMLButtonElement>("historyNext");
  const slider = el<HTMLInputElement>("historySlider");

  live.classList.toggle("active", historyMode === "live");
  replay.classList.toggle("active", historyMode === "replay");
  replay.disabled = !canReplay;
  prev.disabled = historyMode !== "replay" || !canReplay || replayIndex <= 0;
  next.disabled = historyMode !== "replay" || !canReplay || replayIndex < 0 || replayIndex >= len - 1;
  slider.disabled = !canReplay;
  slider.max = String(Math.max(0, len - 1));
  if (document.activeElement !== slider) slider.value = String(sliderIndex);

  el<HTMLSpanElement>("historyFrameLabel").textContent = historyMode === "replay"
    ? (replayExpired || replayIndex < 0 ? `#${replayFrameId ?? "--"} · evicted from history` : `#${replayFrameId} · ${replayIndex + 1}/${len}`)
    : `live · ${len} sweeps`;
  el<HTMLButtonElement>("resetPeaks").disabled = historyMode === "replay";
  const frame = historyMode === "replay" ? latestFrame : liveFrame;
  el<HTMLSpanElement>("historyTimeLabel").textContent = frame?.partial
    ? formatScanProgress(frame)
    : formatFrameTime(frame);
}

function showSpectrumFrame(frame: SpectrumFrame | null) {
  const rangeChanged = !latestFrame || !frame || frameRangeKey(latestFrame) !== frameRangeKey(frame);
  latestFrame = frame;
  redrawSpectrumView();
  if (rangeChanged) redrawWifiOverlayView();
  if (historyMode === "replay" && frame) waterfall.setReplayFrame(frame);
  redrawWaterfallView();
  updateHistoryControls();
}

async function showHistoryFrame(frameId: number, direction = 0) {
  if (!historyInfo.frame_ids.length) return;
  const epoch = historyInfo.capture_epoch;
  const generation = displayGeneration;
  const seq = ++historyRequestSeq;
  historyMode = "replay";
  stopDisplayAnimation();
  const result = await invoke<HistoryFrame>("get_history_frame_by_id", { captureEpoch: epoch, frameId, direction });
  if (seq !== historyRequestSeq || generation !== displayGeneration || historyMode !== "replay") return;
  historyInfo = result.history;
  replayExpired = result.expired;
  if (result.frame) {
    replayFrameId = result.frame.frame_id;
    showSpectrumFrame(result.frame);
  } else {
    updateHistoryControls();
    redrawWaterfallView();
  }
}

function setHistoryLiveMode() {
  historyMode = "live";
  historyRequestSeq++;
  replayExpired = false;
  peakEnvelope.pause();
  waterfallClock.pause();
  waterfall.setReplayFrame(null);
  showSpectrumFrame(liveFrame);
  syncDisplayAnimation();
}

async function loadSnapshot(): Promise<boolean> {
  if (fatalShown) return false;
  const generation = displayGeneration;
  const next = await invoke<Snapshot>("get_snapshot");
  if (generation !== displayGeneration || next.capture_epoch < Math.max(liveFrameEpoch ?? 0, activeEpoch, snap?.capture_epoch ?? 0)) return false;
  snap = next;
  visibilitySyncPending = false;
  renderSnapshot();
  return true;
}

function applyWifiSnapshot(next: WifiSnapshot) {
  if (!snap) return;
  snap = {
    ...snap,
    aps: next.aps,
    wifi_interfaces: next.wifi_interfaces,
    selected_wifi_interface: next.selected_wifi_interface,
    wifi_backends: next.wifi_backends,
    selected_wifi_backend: next.selected_wifi_backend,
    wifi_auto_scan_enabled: next.wifi_auto_scan_enabled,
    wifi_auto_scan_interval_secs: next.wifi_auto_scan_interval_secs,
    wifi_scan_in_progress: next.wifi_scan_in_progress,
    wifi_helper_running: next.wifi_helper_running,
    wifi_status: next.wifi_status,
    status: next.status,
    hidden_wifi_overlay_bssids: next.hidden_wifi_overlay_bssids,
  };
}

async function refreshWifiSnapshot() {
  if (fatalShown || wifiSnapshotRefreshInFlight) return;
  wifiSnapshotRefreshInFlight = true;
  try {
    const next = await invoke<WifiSnapshot>("get_wifi_snapshot");
    applyWifiSnapshot(next);
    renderWifiSnapshot();
  } catch (err) {
    showCommandError("Wi-Fi refresh failed", err);
  } finally {
    wifiSnapshotRefreshInFlight = false;
  }
}

function scheduleWifiScanPoll(delayMs = 350) {
  if (fatalShown || wifiScanPollTimer !== undefined) return;
  wifiScanPollTimer = window.setTimeout(async () => {
    wifiScanPollTimer = undefined;
    await refreshWifiSnapshot();
    if (snap?.wifi_scan_in_progress) scheduleWifiScanPoll(350);
  }, delayMs);
}

function syncWifiSnapshotState() {
  if (!snap) return;
  hidden = new Set(snap.hidden_wifi_overlay_bssids.map(v => v.toLowerCase()));
  if (selectedBssid && (!snap.aps.some(ap => ap.bssid.toLowerCase() === selectedBssid) || hidden.has(selectedBssid))) {
    selectedBssid = null;
  }
}

function renderWifiScanButton() {
  if (!snap) return;
  const scanButton = el<HTMLButtonElement>("scanWifi");
  scanButton.disabled = snap.wifi_scan_in_progress || wifiScanInFlight;
  scanButton.textContent = (snap.wifi_scan_in_progress || wifiScanInFlight) ? "Scanning" : "Scan";
}

function renderWifiControls() {
  if (!snap) return;
  renderWifiScanButton();
  setCheckbox("autoWifiScan", snap.wifi_auto_scan_enabled);
  setNumberInput("wifiAutoInterval", snap.wifi_auto_scan_interval_secs);
  renderWifiBackendSelect();
  renderWifiInterfaceSelect();
  el<HTMLDivElement>("apCount").textContent = snap.wifi_status || `${snap.aps.length} APs in selected band`;
  updateWifiPanel();
}

function renderWifiSnapshot() {
  if (!snap || fatalShown) return;
  syncWifiSnapshotState();
  setStatusText(snap.status);
  renderWifiControls();
  renderAps();
  redrawWifiOverlayView();
}

function renderSnapshot() {
  if (!snap || fatalShown) return;
  syncWifiSnapshotState();
  if (snap.capture_epoch !== activeEpoch) {
    activeEpoch = snap.capture_epoch;
    if (liveFrameEpoch !== activeEpoch) resetDisplayHistory();
  }
  const canRun = hasRealSelectedDevice(snap);
  setStatusText(snap.status);
  const runButton = el<HTMLButtonElement>("run");
  runButton.disabled = snap.running || !canRun;
  runButton.title = canRun ? "Start capture" : "No usable SDR device is available";
  el<HTMLButtonElement>("stop").disabled = !snap.running;
  el<HTMLDivElement>("topbar").innerHTML = `
    <span>${snap.profile_name}</span>
    <span>Band ${mhz(snap.band.start_hz)}-${mhz(snap.band.stop_hz)} MHz</span>
    <span>bins ${Math.ceil((snap.band.stop_hz - snap.band.start_hz) / snap.band.bin_width_hz)}</span>
    <span>bin ${(snap.band.bin_width_hz/1e3).toFixed(1)} kHz</span>
    <span>requested rate ${(snap.sweep.sample_rate_hz/1e6).toFixed(2)} MS/s</span>
    <span>requested gain ${snap.sdr_gain_db} (driver units)</span>
    <span>run #${snap.capture_epoch}</span>
    <span>history ${snap.history_len}</span>
  `;

  const devices = el<HTMLSelectElement>("deviceSelect");
  const deviceOptions = snap.devices.map((d, i) => `<option value="${i}" ${i === snap!.selected_device_index ? "selected" : ""}>${html(d.label)} [${html(d.driver)}]</option>`).join("");
  if (devices.innerHTML !== deviceOptions) devices.innerHTML = deviceOptions;
  devices.value = String(snap.selected_device_index);

  if (!rfDirtyFields.has("gainDb")) setNumberInput("gainDb", snap.sdr_gain_db);
  if (!rfDirtyFields.has("soapyArgs")) setTextInput("soapyArgs", snap.soapy_args);
  if (!rfDirtyFields.has("startMhz")) setNumberInput("startMhz", (snap.band.start_hz / 1e6).toFixed(3));
  if (!rfDirtyFields.has("stopMhz")) setNumberInput("stopMhz", (snap.band.stop_hz / 1e6).toFixed(3));
  if (!rfDirtyFields.has("binKhz")) setNumberInput("binKhz", (snap.band.bin_width_hz / 1e3).toFixed(3));
  if (!rfDirtyFields.has("sampleRate")) setNumberInput("sampleRate", snap.sweep.sample_rate_hz / 1e6);
  if (!rfDirtyFields.has("usableRatio")) setNumberInput("usableRatio", snap.sweep.usable_bandwidth_ratio);
  if (!rfDirtyFields.has("overlap")) setNumberInput("overlap", snap.sweep.overlap_hz / 1e6);
  if (!rfDirtyFields.has("dwell")) setNumberInput("dwell", snap.sweep.dwell_ms);
  if (!rfDirtyFields.has("settle")) setNumberInput("settle", snap.sweep.settle_ms);
  if (!rfDirtyFields.has("fft")) setNumberInput("fft", snap.sweep.fft_size);
  if (!rfDirtyFields.has("streamGating")) setCheckbox("streamGating", snap.sweep.stream_gating);
  setCheckbox("showCurrent", snap.display.show_current);
  setCheckbox("showAverage", snap.display.show_average);
  setCheckbox("showMaxHold", snap.display.show_max_hold);
  setNumberInput("peakDecay", snap.display.peak_decay_db_per_sec);
  setNumberInput("waterfallSpan", snap.display.waterfall_span_s);
  setNumberInput("waterfallFloor", snap.display.waterfall_floor_db);
  setNumberInput("waterfallCeil", snap.display.waterfall_ceiling_db);
  setCheckbox("showWifiOverlay", snap.display.show_wifi_overlay);
  setCheckbox("showWifiLabels", snap.display.show_wifi_labels);
  setCheckbox("labelBssid", snap.display.label_aps_by_bssid);
  syncDisplayValue("spectrumFloor", "spectrumFloorSlider", snap.display.spectrum_floor_db);
  syncDisplayValue("spectrumCeil", "spectrumCeilSlider", snap.display.spectrum_ceiling_db);
  syncDisplayValue("rssiFloor", "rssiFloorSlider", snap.display.wifi_overlay_rssi_floor_dbm);
  syncDisplayValue("rssiCeil", "rssiCeilSlider", snap.display.wifi_overlay_rssi_ceiling_dbm);
  syncDisplayValue("overlayFloor", "overlayFloorSlider", snap.display.wifi_overlay_floor_db);
  syncDisplayValue("overlayCeil", "overlayCeilSlider", snap.display.wifi_overlay_ceiling_db);
  renderWifiSnapshot();
  updateRfDirtyUi();
  renderFrequencyAxis();
  if (historyMode === "replay" && snap.history_len <= 0) setHistoryLiveMode();
  updateHistoryControls();
  syncDisplayAnimation();
  redrawWaterfallView();
}

function renderWifiBackendSelect() {
  if (!snap) return;
  const s = snap;
  const select = el<HTMLSelectElement>("wifiBackend");
  const options = s.wifi_backends.map(backend => {
    const suffix = backend.requires_elevation ? " *" : "";
    return `<option value="${html(backend.id)}">${html(backend.label)}${suffix}</option>`;
  });
  const markup = options.join("");
  if (select.innerHTML !== markup) select.innerHTML = markup;
  select.value = s.selected_wifi_backend;

  const selected = s.wifi_backends.find(backend => backend.id === s.selected_wifi_backend);
  const state = el<HTMLSpanElement>("wifiBackendState");
  state.classList.toggle("running", s.selected_wifi_backend === "privileged_helper" && s.wifi_helper_running);
  state.textContent = s.selected_wifi_backend === "privileged_helper"
    ? (s.wifi_helper_running ? "running" : "stopped")
    : (selected?.label ?? s.selected_wifi_backend);
}

function renderWifiInterfaceSelect() {
  if (!snap) return;
  const select = el<HTMLSelectElement>("wifiInterface");
  const selected = snap.selected_wifi_interface.trim();
  const autoLabel = snap.wifi_interfaces.length ? `Auto (${snap.wifi_interfaces[0]})` : "Auto (none)";
  const options = [
    `<option value="">${html(autoLabel)}</option>`,
    ...snap.wifi_interfaces.map(iface => `<option value="${html(iface)}">${html(iface)}</option>`),
  ];
  if (selected && !snap.wifi_interfaces.includes(selected)) {
    options.push(`<option value="${html(selected)}">${html(selected)} (missing)</option>`);
  }
  const markup = options.join("");
  if (select.innerHTML !== markup) select.innerHTML = markup;
  select.value = selected || "";
}

async function confirmWifiHelperStart(): Promise<boolean> {
  return confirm(
    "Active Wi-Fi scan will start SDRWispy's privileged helper through the system authorization dialog.",
    { title: "Start Wi-Fi helper", kind: "warning" },
  );
}

async function ensureWifiHelperStarted(): Promise<boolean> {
  if (!snap || snap.selected_wifi_backend !== "privileged_helper" || snap.wifi_helper_running) {
    return true;
  }
  const allowed = await confirmWifiHelperStart();
  if (!allowed) {
    renderWifiSnapshot();
    return false;
  }
  applyWifiSnapshot(await invoke<WifiSnapshot>("start_wifi_helper"));
  renderWifiSnapshot();
  return snap.wifi_helper_running;
}

function wifiAutoIntervalSecs(): number {
  const input = el<HTMLInputElement>("wifiAutoInterval");
  const value = Math.round(Number(input.value));
  if (!Number.isFinite(value)) return snap?.wifi_auto_scan_interval_secs ?? 30;
  return Math.max(5, Math.min(600, value));
}

async function updateWifiAutoScanSettings() {
  applyWifiSnapshot(await invoke<WifiSnapshot>("set_wifi_auto_scan", {
    enabled: el<HTMLInputElement>("autoWifiScan").checked,
    intervalSecs: wifiAutoIntervalSecs(),
  }));
  if (!snap?.wifi_auto_scan_enabled) lastAutoWifiScanAtMs = 0;
  renderWifiSnapshot();
}

async function refreshSnapshotFromTimer() {
  if (
    fatalShown ||
    document.hidden ||
    snapshotRefreshInFlight ||
    wifiScanInFlight ||
    snap?.wifi_scan_in_progress ||
    wifiSnapshotRefreshInFlight ||
    displayPatchInFlight ||
    displayPatchTimer !== undefined ||
    Object.keys(pendingDisplayPatch).length > 0
  ) return;
  snapshotRefreshInFlight = true;
  try {
    await loadSnapshot();
  } catch (err) {
    showCommandError("Refresh failed", err);
  } finally {
    snapshotRefreshInFlight = false;
  }
}

async function runAutoWifiScan() {
  if (
    fatalShown ||
    wifiScanInFlight ||
    !snap?.wifi_auto_scan_enabled ||
    snap.wifi_scan_in_progress ||
    (snap.selected_wifi_backend === "privileged_helper" && !snap.wifi_helper_running)
  ) {
    return;
  }
  const now = Date.now();
  const intervalMs = Math.max(5000, snap.wifi_auto_scan_interval_secs * 1000);
  if (lastAutoWifiScanAtMs > 0 && now - lastAutoWifiScanAtMs < intervalMs) return;
  lastAutoWifiScanAtMs = now;

  wifiScanInFlight = true;
  renderWifiScanButton();
  try {
    applyWifiSnapshot(await invoke<WifiSnapshot>("scan_wifi_auto"));
    if (snap?.wifi_scan_in_progress) scheduleWifiScanPoll();
  } catch (err) {
    showCommandError("Wi-Fi auto scan failed", err);
  } finally {
    wifiScanInFlight = false;
    renderWifiSnapshot();
  }
}

function renderAps() {
  if (!snap) return;
  const s = snap;
  const selected = selectedBssid?.toLowerCase() ?? null;
  const updateRssiCells = () => {
    const byBssid = new Map(s.aps.map(ap => [ap.bssid.toLowerCase(), ap]));
    document.querySelectorAll<HTMLTableCellElement>("#apRows td[data-rssi-bssid]").forEach(cell => {
      const ap = byBssid.get(cell.dataset.rssiBssid ?? "");
      if (ap) cell.textContent = `${ap.rssi_dbm} dBm`;
    });
  };
  const signature = JSON.stringify({
    label: s.display.label_aps_by_bssid,
    selected,
    hidden: [...hidden].sort(),
    aps: s.aps.map(ap => [
      ap.bssid,
      ap.ssid,
      ap.channel,
      ap.primary_freq_hz,
      ap.center_freq_hz,
      ap.width_mhz,
      ap.phy,
    ]),
  });
  if (signature === apRowsSignature) {
    updateRssiCells();
    return;
  }
  apRowsSignature = signature;

  el<HTMLTableSectionElement>("apRows").innerHTML = s.aps.map(ap => {
    const key = ap.bssid.toLowerCase();
    const checked = hidden.has(key) ? "" : "checked";
    const name = (s.display.label_aps_by_bssid ? ap.bssid : ap.ssid) || `<hidden:${ap.bssid}>`;
    const color = cssColorForBssid(ap.bssid);
    const cls = selected === key ? ' class="selected-ap"' : "";
    return `<tr${cls} data-bssid="${html(ap.bssid)}">
      <td><input type="checkbox" data-bssid="${html(ap.bssid)}" ${checked}></td>
      <td><span class="color-dot" style="background:${color}"></span>${html(name)}</td>
      <td>${html(ap.bssid)}</td><td>${ap.channel}</td><td>${mhz(ap.primary_freq_hz)} MHz</td><td>${mhz(ap.center_freq_hz)} MHz</td>
      <td>${ap.width_mhz} MHz</td><td data-rssi-bssid="${html(key)}">${ap.rssi_dbm} dBm</td><td>${html(ap.phy)}</td>
    </tr>`;
  }).join("");
  document.querySelectorAll<HTMLTableRowElement>("#apRows tr[data-bssid]").forEach(row => {
    row.onclick = () => {
      const bssid = row.dataset.bssid?.toLowerCase() ?? null;
      selectAp(selectedBssid === bssid ? null : bssid);
    };
  });
  document.querySelectorAll<HTMLInputElement>("#apRows input[type=checkbox]").forEach(input => {
    input.onclick = ev => ev.stopPropagation();
    input.onchange = commandHandler("Overlay update failed", async () => {
      const bssid = input.dataset.bssid!;
      if (!input.checked && selectedBssid === bssid.toLowerCase()) selectedBssid = null;
      applyWifiSnapshot(await invoke<WifiSnapshot>("set_wifi_overlay_hidden", { bssid, hidden: !input.checked }));
      renderWifiSnapshot();
    });
  });
}

function currentView(): FrequencyView {
  return view ?? { startHz: latestFrame?.band_start_hz ?? snap?.band.start_hz ?? 2400e6, stopHz: latestFrame?.band_stop_hz ?? snap?.band.stop_hz ?? 2485e6 };
}

function setPlotView(next: FrequencyView | null) {
  view = next;
  spectrum.setView(view);
  wifiOverlay.setView(view);
  waterfall.setView(view);
  wifiLabelsSignature = "";
  renderFrequencyAxis();
  redrawDisplayViews();
  redrawWaterfallView();
}

function renderFrequencyAxis() {
  if (!snap) return;
  const axis = el<HTMLDivElement>("freqAxis");
  const range = currentView();
  const width = Math.max(1, axis.clientWidth);
  const ticks = frequencyTicks(range, width);
  axis.innerHTML = ticks.map(({ hz, label }, i) => {
    const percent = (hz - range.startHz) / (range.stopHz - range.startHz) * 100;
    const edge = i === 0 && percent < 8 ? " edge-left" : i === ticks.length - 1 && percent > 88 ? " edge-right" : "";
    return `<div class="freq-tick${edge}" style="left:${percent}%"><span>${html(label)}</span></div>`;
  }).join("");
  el<HTMLButtonElement>("resetZoom").disabled = view === null;
}

function renderLevelAxis() {
  if (!snap) return;
  const low = snap.display.spectrum_floor_db, high = snap.display.spectrum_ceiling_db;
  const labels: string[] = [];
  for (let db = Math.ceil(low / 10) * 10; db <= high; db += 10) {
    labels.push(`<span class="${db === high ? "top-edge" : db === low ? "bottom-edge" : ""}" style="top:${(high - db) / (high - low) * 100}%">${db}</span>`);
  }
  el<HTMLDivElement>("levelAxis").innerHTML = labels.join("");
  document.querySelector<HTMLElement>(".trace-current")!.classList.toggle("disabled", !snap.display.show_current);
  document.querySelector<HTMLElement>(".trace-average")!.classList.toggle("disabled", !snap.display.show_average);
  const peak = el<HTMLSpanElement>("peakLegend");
  peak.classList.toggle("disabled", !snap.display.show_max_hold);
  peak.textContent = historyMode === "replay" ? "Historical max" : snap.display.peak_decay_db_per_sec === 0 ? "Peak hold" : `Peak · ${snap.display.peak_decay_db_per_sec} dB/s`;
}

function renderWifiOverlayLabels(frame: SpectrumFrame | null) {
  const layer = el<HTMLDivElement>("wifiLabels");
  if (!snap || !frame || !snap.display.show_wifi_overlay || !snap.display.show_wifi_labels) {
    if (wifiLabelsSignature !== "") {
      layer.innerHTML = "";
      wifiLabelsSignature = "";
    }
    return;
  }

  const rssiSpan = Math.max(1, snap.display.wifi_overlay_rssi_ceiling_dbm - snap.display.wifi_overlay_rssi_floor_dbm);
  const [dbMin, dbMax] = overlayDbRange();
  const labels: string[] = [];
  const range = currentView();
  const occupied: { x: number; top: number }[] = [];
  const plotWidth = Math.max(1, layer.clientWidth);
  const ordered = [...snap.aps].sort((a, b) => Number(b.bssid.toLowerCase() === selectedBssid) - Number(a.bssid.toLowerCase() === selectedBssid) || b.rssi_dbm - a.rssi_dbm);
  for (const ap of ordered) {
    if (hidden.has(ap.bssid.toLowerCase())) continue;
    const half = ap.width_mhz * 1e6 / 2;
    const left = ap.center_freq_hz - half;
    const right = ap.center_freq_hz + half;
    if (right < range.startHz || left > range.stopHz) continue;

    const center = Math.max(range.startHz, Math.min(range.stopHz, ap.center_freq_hz));
    const x = (center - range.startHz) / Math.max(1, range.stopHz - range.startHz) * 100;
    const rssiT = Math.max(0, Math.min(1, (ap.rssi_dbm - snap.display.wifi_overlay_rssi_floor_dbm) / rssiSpan));
    const topDb = snap.display.wifi_overlay_floor_db + rssiT * (snap.display.wifi_overlay_ceiling_db - snap.display.wifi_overlay_floor_db);
    const yNorm = Math.max(0, Math.min(1, (topDb - dbMin) / (dbMax - dbMin)));
    const top = Math.max(6, Math.min(88, (1 - yNorm) * 100 - 4));
    const text = (snap.display.label_aps_by_bssid || !ap.ssid.trim()) ? ap.bssid : ap.ssid;
    const color = cssColorForBssid(ap.bssid);
    const selected = selectedBssid?.toLowerCase() === ap.bssid.toLowerCase();
    if (!selected && occupied.some(p => Math.abs(p.x - x) * plotWidth / 100 < 155 && Math.abs(p.top - top) < 9)) continue;
    occupied.push({ x, top });
    labels.push(`<div class="wifi-label${selected ? " selected" : ""}" style="left:${x.toFixed(3)}%; top:${top.toFixed(2)}%; border-color:${color}; color:${color}">${html(text)}</div>`);
  }
  const markup = labels.join("");
  if (markup !== wifiLabelsSignature) {
    layer.innerHTML = markup;
    wifiLabelsSignature = markup;
  }
}

function selectAp(bssid: string | null) {
  selectedBssid = bssid ? bssid.toLowerCase() : null;
  renderAps();
  redrawWifiOverlayView();
}

type DisplayPatch = Partial<{
  show_current: boolean;
  show_average: boolean;
  show_max_hold: boolean;
  peak_decay_db_per_sec: number;
  waterfall_span_s: number;
  waterfall_floor_db: number;
  waterfall_ceiling_db: number;
  show_wifi_overlay: boolean;
  show_wifi_labels: boolean;
  label_aps_by_bssid: boolean;
  spectrum_floor_db: number;
  spectrum_ceiling_db: number;
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
}>;

const spectrumDisplayKeys: (keyof DisplayPatch)[] = [
  "show_current",
  "show_average",
  "show_max_hold",
  "peak_decay_db_per_sec",
  "spectrum_floor_db",
  "spectrum_ceiling_db",
];
const wifiOverlayCanvasDisplayKeys: (keyof DisplayPatch)[] = [
  "show_wifi_overlay",
  "wifi_overlay_rssi_floor_dbm",
  "wifi_overlay_rssi_ceiling_dbm",
  "wifi_overlay_floor_db",
  "wifi_overlay_ceiling_db",
];
const wifiLabelDisplayKeys: (keyof DisplayPatch)[] = [
  "show_wifi_overlay",
  "show_wifi_labels",
  "label_aps_by_bssid",
  "wifi_overlay_rssi_floor_dbm",
  "wifi_overlay_rssi_ceiling_dbm",
  "wifi_overlay_floor_db",
  "wifi_overlay_ceiling_db",
];

function patchHasAny(patch: DisplayPatch, keys: (keyof DisplayPatch)[]): boolean {
  return keys.some(key => Object.prototype.hasOwnProperty.call(patch, key));
}

function syncDisplayValue(numberId: string, sliderId: string, value: number) {
  const number = el<HTMLInputElement>(numberId);
  const slider = el<HTMLInputElement>(sliderId);
  if (document.activeElement === number || document.activeElement === slider) return;
  setNumberInput(numberId, value);
  setRangeInput(sliderId, value);
}

function displayAcquiring(): boolean {
  return !!snap?.running && historyMode === "live" && !document.hidden && !visibilitySyncPending && !stopRequested;
}

function redrawSpectrumView() {
  if (!snap || document.hidden) return;
  renderLevelAxis();
  if (!latestFrame) {
    spectrum.clear();
    return;
  }
  if (historyMode === "live") {
    const now = performance.now();
    peakEnvelope.update(latestFrame, now, displayAcquiring() && frameFreshness.isFresh(now), snap.display.peak_decay_db_per_sec);
  }
  spectrum.draw(latestFrame, snap.display, historyMode === "live" ? peakEnvelope.values : latestFrame.max_hold_db);
}

function redrawWaterfallView(now = performance.now()) {
  if (!snap || document.hidden || fatalShown) return;
  const replay = historyMode === "replay";
  const end = replay
    ? (latestFrame ? frameTimeMs(latestFrame) : null)
    : waterfallClock.tick(now, Date.now(), displayAcquiring());
  waterfallEndMs = end;
  const span = snap.display.waterfall_span_s;
  waterfall.setLevels(snap.display.waterfall_floor_db, snap.display.waterfall_ceiling_db);
  waterfall.draw(end ?? 0, span, replay);
  const retained = waterfall.retention(replay);
  const unavailable = el<HTMLDivElement>("waterfallUnavailable");
  const age = end === null || retained.oldestMs === null ? 1 : Math.min(1, Math.max(0, (end - retained.oldestMs) / (span * 1000)));
  unavailable.classList.toggle("hidden", age >= 1 || retained.oldestMs === null);
  unavailable.style.top = `${age * 100}%`;
  unavailable.textContent = "Outside retained observations";
  const mode = replay ? "Frozen history" : snap.running && !stopRequested
    ? (frameFreshness.isFresh(now) ? "Live" : "Waiting for data") : "Paused";
  const signature = `${mode}:${span}:${Math.floor((end ?? 0) / 1000)}:${snap.display.waterfall_floor_db}:${snap.display.waterfall_ceiling_db}:${retained.oldestMs}:${missedFrames}:${replayExpired}`;
  if (signature !== waterfallTimeSignature) {
    waterfallTimeSignature = signature;
    el<HTMLDivElement>("waterfallTitle").textContent = `Waterfall · ${span}s · ${mode}`;
    el<HTMLSpanElement>("waterfallTopTime").textContent = end === null ? "--" : "0s";
    el<HTMLSpanElement>("waterfallBottomTime").textContent = end === null ? "--" : `−${span}s`;
    const duration = retained.oldestMs === null || retained.newestMs === null ? 0 : Math.max(0, (retained.newestMs - retained.oldestMs) / 1000);
    const historyDuration = historyInfo.oldest_time_ns === null || historyInfo.newest_time_ns === null ? 0 : Math.max(0, (historyInfo.newest_time_ns - historyInfo.oldest_time_ns) / 1e9);
    const sweepMs = latestFrame ? Math.max(0, (latestFrame.sweep_end_time_ns - latestFrame.sweep_start_time_ns) / 1e6) : 0;
    el<HTMLDivElement>("plotStatus").textContent = `Waterfall ${duration.toFixed(1)}s retained · replay ${historyDuration.toFixed(1)}s · sweep ${sweepMs.toFixed(1)} ms${deliveryCountUnknown ? " · delivery gap count unavailable (cursor expired)" : missedFrames ? ` · ${missedFrames} completed sweeps unavailable before delivery` : " · no delivery gaps"}${replayExpired ? " · selected frame evicted from replay history" : ""}`;
    const low = snap.display.waterfall_floor_db, high = snap.display.waterfall_ceiling_db;
    el<HTMLSpanElement>("waterfallLow").textContent = `${low} dB`;
    el<HTMLSpanElement>("waterfallHigh").textContent = `${high} dB`;
    el<HTMLSpanElement>("waterfallColorbar").style.background = `linear-gradient(to right, ${Array.from({length: 9}, (_, i) => { const [r,g,b] = powerColor(low + (high-low)*i/8, low, high); return `rgb(${r},${g},${b})`; }).join(",")})`;
  }
}

function stopDisplayAnimation() {
  if (displayAnimationId !== undefined) cancelAnimationFrame(displayAnimationId);
  displayAnimationId = undefined;
  peakEnvelope.pause();
  waterfallClock.pause();
}

function animateDisplay(now: number) {
  displayAnimationId = undefined;
  if (!displayAcquiring() || fatalShown) return;
  if (snap && latestFrame && peakEnvelope.update(latestFrame, now, frameFreshness.isFresh(now), snap.display.peak_decay_db_per_sec) && snap.display.show_max_hold) {
    spectrum.draw(latestFrame, snap.display, peakEnvelope.values);
  }
  redrawWaterfallView(now);
  displayAnimationId = requestAnimationFrame(animateDisplay);
}

function syncDisplayAnimation() {
  if (displayAcquiring() && !fatalShown) {
    if (displayAnimationId === undefined) displayAnimationId = requestAnimationFrame(animateDisplay);
  } else {
    stopDisplayAnimation();
  }
}

function redrawWifiOverlayCanvas() {
  if (snap) {
    wifiOverlay.draw(latestFrame, snap.aps, hidden, snap.display as SpectrumDisplayOptions, selectedBssid);
  } else {
    wifiOverlay.clear();
  }
}

function redrawWifiLabels() {
  renderWifiOverlayLabels(latestFrame);
}

function redrawWifiOverlayView() {
  redrawWifiOverlayCanvas();
  redrawWifiLabels();
}

function redrawDisplayViews() {
  redrawSpectrumView();
  redrawWifiOverlayView();
}

function applyLocalDisplayPatch(patch: DisplayPatch) {
  if (!snap) return;
  Object.assign(snap.display, patch);
  if (patch.label_aps_by_bssid !== undefined) renderAps();
  if (patchHasAny(patch, spectrumDisplayKeys)) redrawSpectrumView();
  if (patchHasAny(patch, ["waterfall_span_s", "waterfall_floor_db", "waterfall_ceiling_db"])) redrawWaterfallView();
  if (patchHasAny(patch, wifiOverlayCanvasDisplayKeys)) redrawWifiOverlayCanvas();
  if (patchHasAny(patch, wifiLabelDisplayKeys)) redrawWifiLabels();
}

function patchDisplayLive(patch: DisplayPatch) {
  displayPatchSeq++;
  applyLocalDisplayPatch(patch);
  Object.assign(pendingDisplayPatch, patch);
  if (displayPatchTimer !== undefined) window.clearTimeout(displayPatchTimer);
  displayPatchTimer = window.setTimeout(() => {
    void flushDisplayPatch();
  }, 180);
}

async function flushDisplayPatch() {
  if (displayPatchTimer !== undefined) {
    window.clearTimeout(displayPatchTimer);
    displayPatchTimer = undefined;
  }
  if (wifiScanPollTimer !== undefined) {
    window.clearTimeout(wifiScanPollTimer);
    wifiScanPollTimer = undefined;
  }
  const patch = pendingDisplayPatch;
  pendingDisplayPatch = {};
  if (!Object.keys(patch).length) return;

  const seq = displayPatchSeq;
  const generation = displayGeneration;
  displayPatchInFlight = true;
  try {
    const next = await invoke<Snapshot>("patch_display", { patch });
    if (seq !== displayPatchSeq || generation !== displayGeneration || next.capture_epoch < (snap?.capture_epoch ?? 0)) return;
    snap = next;
    setStatusText(next.status);
  } catch (err) {
    showCommandError("Display update failed", err);
  } finally {
    displayPatchInFlight = false;
  }
}

function bindDisplayCheckbox(id: string, key: keyof DisplayPatch) {
  el<HTMLInputElement>(id).onchange = () => {
    void patchDisplayLive({ [key]: el<HTMLInputElement>(id).checked } as DisplayPatch);
  };
}

function bindDisplayNumberAndSlider(numberId: string, sliderId: string, key: keyof DisplayPatch) {
  const number = el<HTMLInputElement>(numberId);
  const slider = el<HTMLInputElement>(sliderId);
  const commitNumber = () => {
    const value = Number(number.value);
    if (!Number.isFinite(value)) return;
    slider.value = String(value);
    void patchDisplayLive({ [key]: value } as DisplayPatch);
  };
  number.oninput = () => {
    if (number.value.trim() !== "") slider.value = number.value;
  };
  number.onblur = commitNumber;
  number.onkeydown = ev => {
    if (ev.key === "Enter") {
      commitNumber();
      number.blur();
    }
  };
  slider.oninput = () => {
    number.value = slider.value;
    void patchDisplayLive({ [key]: Number(slider.value) } as DisplayPatch);
  };
}

function bindDisplayTiming(id: string, key: "peak_decay_db_per_sec" | "waterfall_span_s") {
  const input = el<HTMLInputElement>(id);
  const commit = () => {
    if (input.value.trim() === "" || !input.checkValidity() || !Number.isFinite(Number(input.value))) {
      input.value = String(snap?.display[key] ?? (key === "waterfall_span_s" ? 60 : 3));
      return;
    }
    patchDisplayLive({ [key]: Number(input.value) });
  };
  input.onblur = commit;
  input.onkeydown = event => { if (event.key === "Enter") input.blur(); };
}

function bindDisplayControls() {
  for (const [id, key] of [["waterfallFloor", "waterfall_floor_db"], ["waterfallCeil", "waterfall_ceiling_db"]] as const) {
    const input = el<HTMLInputElement>(id);
    input.onblur = () => {
      if (!snap) return;
      const value = Number(input.value);
      const low = key === "waterfall_floor_db" ? value : snap.display.waterfall_floor_db;
      const high = key === "waterfall_ceiling_db" ? value : snap.display.waterfall_ceiling_db;
      if (input.value.trim() === "" || !Number.isFinite(value) || value < -200 || value > 50 || high - low < 1) {
        input.value = String(snap.display[key]); return;
      }
      patchDisplayLive({ [key]: value });
    };
    input.onkeydown = event => { if (event.key === "Enter") input.blur(); };
  }
  el<HTMLButtonElement>("resetPeaks").onclick = () => { peakEnvelope.reset(); redrawSpectrumView(); };
  el<HTMLButtonElement>("autoLevels").onclick = () => {
    if (!latestFrame) return;
    const values = latestFrame.valid_bin_ranges.flatMap(([a,b]) => latestFrame!.current_db.slice(a,b)).filter(Number.isFinite).sort((a,b) => a-b);
    if (!values.length) return;
    const low = Math.max(-200, Math.min(40, Math.floor(values[Math.floor((values.length-1)*0.05)]-5)));
    const high = Math.max(low+10, Math.min(50, Math.ceil(values[values.length-1]+5)));
    patchDisplayLive({waterfall_floor_db:low,waterfall_ceiling_db:high,spectrum_floor_db:low,spectrum_ceiling_db:high});
    setNumberInput("waterfallFloor", low); setNumberInput("waterfallCeil", high);
    syncDisplayValue("spectrumFloor", "spectrumFloorSlider", low); syncDisplayValue("spectrumCeil", "spectrumCeilSlider", high);
  };
  bindDisplayTiming("peakDecay", "peak_decay_db_per_sec");
  bindDisplayTiming("waterfallSpan", "waterfall_span_s");
  bindDisplayCheckbox("showCurrent", "show_current");
  bindDisplayCheckbox("showAverage", "show_average");
  bindDisplayCheckbox("showMaxHold", "show_max_hold");
  bindDisplayCheckbox("showWifiOverlay", "show_wifi_overlay");
  bindDisplayCheckbox("showWifiLabels", "show_wifi_labels");
  bindDisplayCheckbox("labelBssid", "label_aps_by_bssid");
  bindDisplayNumberAndSlider("spectrumFloor", "spectrumFloorSlider", "spectrum_floor_db");
  bindDisplayNumberAndSlider("spectrumCeil", "spectrumCeilSlider", "spectrum_ceiling_db");
  bindDisplayNumberAndSlider("rssiFloor", "rssiFloorSlider", "wifi_overlay_rssi_floor_dbm");
  bindDisplayNumberAndSlider("rssiCeil", "rssiCeilSlider", "wifi_overlay_rssi_ceiling_dbm");
  bindDisplayNumberAndSlider("overlayFloor", "overlayFloorSlider", "wifi_overlay_floor_db");
  bindDisplayNumberAndSlider("overlayCeil", "overlayCeilSlider", "wifi_overlay_ceiling_db");
}

function overlayLineDbAt(ap: WifiBss, freqHz: number, display: DisplaySettings): number | null {
  const half = ap.width_mhz * 1e6 / 2;
  const left = ap.center_freq_hz - half;
  const right = ap.center_freq_hz + half;
  if (freqHz < left || freqHz > right) return null;
  const rssiSpan = Math.max(1, display.wifi_overlay_rssi_ceiling_dbm - display.wifi_overlay_rssi_floor_dbm);
  const rssiT = Math.max(0, Math.min(1, (ap.rssi_dbm - display.wifi_overlay_rssi_floor_dbm) / rssiSpan));
  const topDb = display.wifi_overlay_floor_db + rssiT * (display.wifi_overlay_ceiling_db - display.wifi_overlay_floor_db);
  const width = Math.max(1, right - left);
  const shoulder = width * 0.23;
  if (freqHz < left + shoulder) {
    const t = (freqHz - left) / shoulder;
    return display.wifi_overlay_floor_db + (topDb - display.wifi_overlay_floor_db) * t;
  }
  if (freqHz > right - shoulder) {
    const t = (right - freqHz) / shoulder;
    return display.wifi_overlay_floor_db + (topDb - display.wifi_overlay_floor_db) * t;
  }
  return topDb;
}

function pickWifiOverlay(clientX: number, clientY: number): string | null {
  if (!snap || !latestFrame || !snap.display.show_wifi_overlay) return null;
  const rect = document.querySelector<HTMLCanvasElement>("#spectrum")!.getBoundingClientRect();
  const x = Math.max(0, Math.min(rect.width, clientX - rect.left));
  const y = Math.max(0, Math.min(rect.height, clientY - rect.top));
  const range = currentView();
  const freqHz = range.startHz + x / Math.max(1, rect.width) * (range.stopHz - range.startHz);
  const [dbMin, dbMax] = overlayDbRange();
  const clickedDb = dbMax - y / Math.max(1, rect.height) * (dbMax - dbMin);
  let best: { bssid: string; score: number; rssi: number } | null = null;
  for (const ap of snap.aps) {
    const key = ap.bssid.toLowerCase();
    if (hidden.has(key)) continue;
    const lineDb = overlayLineDbAt(ap, freqHz, snap.display);
    if (lineDb === null) continue;
    const distPx = Math.abs(clickedDb - lineDb) / (dbMax - dbMin) * rect.height;
    const inBandBonus = Math.abs(freqHz - ap.center_freq_hz) / Math.max(1, ap.width_mhz * 1e6);
    const score = distPx + inBandBonus * 4;
    if (distPx <= 18 && (!best || score < best.score || (score === best.score && ap.rssi_dbm > best.rssi))) {
      best = { bssid: key, score, rssi: ap.rssi_dbm };
    }
  }
  return best?.bssid ?? null;
}

function bindSpectrumSelection() {
  const panel = document.querySelector<HTMLElement>(".spectrum-panel")!;
  panel.addEventListener("click", ev => {
    if (suppressPlotClick) { suppressPlotClick = false; return; }
    const picked = pickWifiOverlay(ev.clientX, ev.clientY);
    if (picked) selectAp(selectedBssid === picked ? null : picked);
  });
}

function bindPlotInteraction() {
  const tooltip = el<HTMLDivElement>("plotTooltip");
  const areas = [...document.querySelectorAll<HTMLElement>(".plot-area")];
  let drag: { area: HTMLElement; x: number; view: FrequencyView } | null = null;
  const inRanges = (i: number, ranges: [number, number][]) => ranges.some(([a,b]) => i >= a && i < b);
  const hide = () => { tooltip.classList.add("hidden"); for (const area of areas) area.querySelector(".plot-cursor")!.classList.add("hidden"); };
  for (const area of areas) {
    area.addEventListener("pointermove", event => {
      if (!latestFrame || !snap) return;
      const rect = area.getBoundingClientRect(), range = currentView();
      const x = Math.max(0, Math.min(rect.width, event.clientX - rect.left));
      const frequency = range.startHz + x / Math.max(1, rect.width) * (range.stopHz-range.startHz);
      for (const target of areas) {
        const cursor = target.querySelector<HTMLElement>(".plot-cursor")!;
        cursor.classList.remove("hidden"); cursor.style.left = `${x / Math.max(1, rect.width) * 100}%`;
      }
      if (drag?.area === area) {
        const selection = area.querySelector<HTMLElement>(".zoom-selection")!;
        selection.classList.remove("hidden"); selection.style.left = `${Math.min(x,drag.x)}px`; selection.style.width = `${Math.abs(x-drag.x)}px`;
        hide(); return;
      }
      let text = `${(frequency/1e6).toFixed(4)} MHz`;
      if (area.classList.contains("waterfall-area") && waterfallEndMs !== null) {
        const time = waterfallEndMs - Math.max(0,Math.min(1,(event.clientY-rect.top)/rect.height))*snap.display.waterfall_span_s*1000;
        const sample = waterfall.sampleAt(time, frequency, historyMode === "replay");
        text += sample ? ` · ${new Date(sample.timeMs).toLocaleTimeString()} · ${sample.dc ? "DC excluded" : sample.value === null ? "No observation" : sample.value.toFixed(1)+" dB (relative)"}` : " · No retained observation at this time";
      } else {
        const i = Math.floor((frequency-latestFrame.band_start_hz)/latestFrame.bin_width_hz);
        const current = inRanges(i,latestFrame.valid_bin_ranges) ? latestFrame.current_db[i] : undefined;
        const historyValid = inRanges(i,latestFrame.history_valid_bin_ranges ?? latestFrame.valid_bin_ranges);
        const dc = inRanges(i,latestFrame.dc_excluded_bin_ranges ?? []);
        text += ` · ${dc ? "DC excluded" : Number.isFinite(current) ? `Current ${current!.toFixed(1)} dB` : "No observation"}`;
        if (historyValid && Number.isFinite(latestFrame.average_db[i])) text += ` · Avg ${latestFrame.average_db[i].toFixed(1)} dB`;
        const peak = historyMode === "live" ? peakEnvelope.values[i] : historyValid ? latestFrame.max_hold_db[i] : undefined;
        if (Number.isFinite(peak)) text += ` · Peak ${peak!.toFixed(1)} dB`;
      }
      tooltip.textContent = text; tooltip.classList.remove("hidden");
      tooltip.style.left = `${Math.max(8,Math.min(innerWidth-tooltip.offsetWidth-8,event.clientX+12))}px`;
      tooltip.style.top = `${Math.max(8,Math.min(innerHeight-tooltip.offsetHeight-8,event.clientY+16))}px`;
    });
    area.addEventListener("pointerleave", () => { if (!drag) hide(); });
    area.addEventListener("pointerdown", event => {
      if (event.button !== 0 || !latestFrame) return;
      const rect=area.getBoundingClientRect(); drag={area,x:event.clientX-rect.left,view:currentView()};
      suppressPlotClick=false; area.setPointerCapture(event.pointerId);
    });
    area.addEventListener("pointerup", event => {
      if (!drag || drag.area !== area) return;
      const rect=area.getBoundingClientRect(), x=Math.max(0,Math.min(rect.width,event.clientX-rect.left)), start=drag;
      drag=null; area.querySelector(".zoom-selection")!.classList.add("hidden");
      if (area.hasPointerCapture(event.pointerId)) area.releasePointerCapture(event.pointerId);
      if (Math.abs(x-start.x)>=8) {
        suppressPlotClick=true;
        const span=start.view.stopHz-start.view.startHz;
        const lo=start.view.startHz+Math.min(x,start.x)/rect.width*span, hi=start.view.startHz+Math.max(x,start.x)/rect.width*span;
        if (hi-lo >= (latestFrame?.bin_width_hz ?? 1)*2) setPlotView({startHz:lo,stopHz:hi});
      }
      hide();
    });
    area.addEventListener("pointercancel", () => { drag=null; area.querySelector(".zoom-selection")!.classList.add("hidden"); hide(); });
    area.addEventListener("dblclick", () => { setPlotView(null); hide(); });
  }
  const divider=el<HTMLDivElement>("plotDivider"), workspace=document.querySelector<HTMLElement>(".workspace")!;
  let sizing=false;
  const adjust=(ratio:number)=>{ const r=Math.max(.25,Math.min(.8,ratio)); workspace.style.setProperty("--spectrum-share",`${r}fr`); workspace.style.setProperty("--waterfall-share",`${1-r}fr`); };
  divider.onpointerdown=event=>{sizing=true;divider.setPointerCapture(event.pointerId);};
  divider.onpointermove=event=>{if(!sizing)return;const s=document.querySelector(".spectrum-panel")!.getBoundingClientRect(),w=document.querySelector(".waterfall-panel")!.getBoundingClientRect();adjust((event.clientY-s.top)/(w.bottom-s.top));};
  divider.onpointerup=()=>{sizing=false;}; divider.onpointercancel=()=>{sizing=false;};
  divider.onkeydown=event=>{if(event.key!=="ArrowUp"&&event.key!=="ArrowDown")return;event.preventDefault();const s=document.querySelector(".spectrum-panel")!.getBoundingClientRect(),w=document.querySelector(".waterfall-panel")!.getBoundingClientRect();adjust(s.height/(s.height+w.height)+(event.key==="ArrowDown"?.05:-.05));};
}

async function pollFrame() {
  if (fatalShown) return;
  if (document.hidden || visibilitySyncPending) {
    window.setTimeout(pollFrame, 250);
    return;
  }
  const generation = displayGeneration;
  let more = false;
  try {
    const batch = await invoke<FrameBatch>("get_frame_batch", {
      afterCaptureEpoch: completeEpoch ?? null, afterFrameId: completeFrameId ?? null, limit: 16,
    });
    if (generation === displayGeneration && !document.hidden && !visibilitySyncPending && batch.capture_epoch >= Math.max(snap?.capture_epoch ?? 0, activeEpoch)) {
      const first = batch.frames[0] ?? batch.latest;
      if (completeEpoch !== undefined && completeEpoch !== batch.capture_epoch || first && liveFrame && frameRangeKey(first) !== frameRangeKey(liveFrame)) resetDisplayHistory();
      completeEpoch = batch.capture_epoch;
      historyInfo = batch.history;
      more = batch.more;
      // Advance the completed-frame cursor independently of the latest progress frame.
      if (batch.frames.length) {
        if (batch.missed_frames === null) deliveryCountUnknown = true;
        else missedFrames += batch.missed_frames;
        for (const frame of batch.frames) {
          waterfall.pushFrame(frame);
          peakEnvelope.observe(frame);
          completeFrameId = frame.frame_id;
        }
      }
      const frame = batch.latest ?? batch.frames[batch.frames.length - 1];
      if (frame && (frame.capture_epoch !== liveFrameEpoch || frame.frame_revision !== liveFrameRevision)) {
        const now = performance.now();
        if (!frameFreshness.isFresh(now)) peakEnvelope.pause();
        frameFreshness.observe(frame, now, Date.now());
        liveFrameEpoch = frame.capture_epoch;
        liveFrame = frame;
        liveFrameRevision = frame.frame_revision;
        const measuredAt = frameTimeMs(frame);
        if (measuredAt !== null) waterfallClock.observe(measuredAt);
        if (historyMode === "live") showSpectrumFrame(frame);
      }
      updateHistoryControls();
      if (batch.frames.length) {
        if (historyMode === "live") redrawSpectrumView();
        redrawWaterfallView();
      }
    }
  } catch (err) {
    console.error(err);
  }
  if (!fatalShown) window.setTimeout(pollFrame, more ? 0 : 33);
}

function numberField(id: string): number {
  const input = el<HTMLInputElement>(id);
  const value = Number(input.value);
  if (!Number.isFinite(value)) throw new Error(`${id} must be a valid number`);
  return value;
}

async function applyRfSweepChanges() {
  resetDisplayHistory();
  await invoke<Snapshot>("patch_band", { patch: {
    start_mhz: numberField("startMhz"),
    stop_mhz: numberField("stopMhz"),
    bin_width_khz: numberField("binKhz"),
  }});
  await invoke<Snapshot>("patch_receiver", { patch: {
    gain_db: numberField("gainDb"),
    soapy_args: el<HTMLInputElement>("soapyArgs").value,
  }});
  snap = await invoke<Snapshot>("patch_sweep", { patch: {
    sample_rate_hz: numberField("sampleRate") * 1e6,
    usable_bandwidth_ratio: numberField("usableRatio"),
    overlap_hz: numberField("overlap") * 1e6,
    dwell_ms: numberField("dwell"),
    settle_ms: numberField("settle"),
    fft_size: Math.max(256, Math.round(numberField("fft"))),
    stream_gating: el<HTMLInputElement>("streamGating").checked,
  }});
  clearRfDirty();
  renderSnapshot();
}

rfInputIds.forEach(id => el<HTMLInputElement>(id).addEventListener("input", markRfDirty));

el<HTMLButtonElement>("run").onclick = commandHandler("Run failed", async () => {
  if (isRfDirty()) await applyRfSweepChanges();
  resetDisplayHistory();
  snap = await invoke<Snapshot>("start_capture");
  renderSnapshot();
});
el<HTMLButtonElement>("stop").onclick = commandHandler("Stop failed", async () => {
  stopRequested = true;
  displayGeneration++;
  stopDisplayAnimation();
  redrawWaterfallView();
  try {
    snap = await invoke<Snapshot>("stop_capture");
    displayGeneration++;
  } finally {
    stopRequested = false;
    renderSnapshot();
  }
});
el<HTMLButtonElement>("refresh").onclick = commandHandler("Refresh failed", async () => { await loadSnapshot(); });
el<HTMLButtonElement>("refreshDevices").onclick = commandHandler("Device refresh failed", async () => { await invoke("list_sdr_devices"); await loadSnapshot(); });
el<HTMLSelectElement>("deviceSelect").onchange = commandHandler("Device selection failed", async e => { resetDisplayHistory(); snap = await invoke<Snapshot>("select_sdr_device", { index: Number((e.target as HTMLSelectElement).value) }); renderSnapshot(); });
el<HTMLButtonElement>("loadConfig").onclick = commandHandler("Load config failed", async () => {
  const selected = await open({
    multiple: false,
    filters: [{ name: "TOML profile", extensions: ["toml"] }],
  });
  const path = Array.isArray(selected) ? selected[0] : selected;
  if (!path) return;
  resetDisplayHistory();
  snap = await invoke<Snapshot>("load_profile", { path });
  clearRfDirty();
  renderSnapshot();
});
el<HTMLButtonElement>("saveConfig").onclick = commandHandler("Save config failed", async () => {
  await flushDisplayPatch();
  const path = await save({
    defaultPath: "sdrwispy_profile.toml",
    filters: [{ name: "TOML profile", extensions: ["toml"] }],
  });
  if (!path) return;
  snap = await invoke<Snapshot>("save_profile", { path });
  renderSnapshot();
});
el<HTMLButtonElement>("scanWifi").onclick = commandHandler("Wi-Fi scan failed", async () => {
  if (wifiScanInFlight) return;
  if (!(await ensureWifiHelperStarted())) return;
  wifiScanInFlight = true;
  renderWifiScanButton();
  try {
    applyWifiSnapshot(await invoke<WifiSnapshot>("scan_wifi"));
    lastAutoWifiScanAtMs = Date.now();
    if (snap?.wifi_scan_in_progress) scheduleWifiScanPoll();
  } finally {
    wifiScanInFlight = false;
    renderWifiSnapshot();
  }
});
el<HTMLInputElement>("autoWifiScan").onchange = commandHandler("Wi-Fi auto scan update failed", updateWifiAutoScanSettings);
el<HTMLInputElement>("wifiAutoInterval").onchange = commandHandler("Wi-Fi auto scan interval update failed", updateWifiAutoScanSettings);
el<HTMLInputElement>("wifiAutoInterval").onkeydown = commandHandler("Wi-Fi auto scan interval update failed", async e => {
  if ((e as KeyboardEvent).key !== "Enter") return;
  await updateWifiAutoScanSettings();
  el<HTMLInputElement>("wifiAutoInterval").blur();
});
el<HTMLSelectElement>("wifiBackend").onchange = commandHandler("Wi-Fi backend selection failed", async e => {
  const backend = (e.target as HTMLSelectElement).value;
  const needsHelper = backend === "privileged_helper";
  if (needsHelper) {
    const allowed = await confirmWifiHelperStart();
    if (!allowed) {
      renderWifiSnapshot();
      return;
    }
  }
  applyWifiSnapshot(await invoke<WifiSnapshot>("select_wifi_backend", { backend }));
  renderWifiSnapshot();
  if (needsHelper) {
    applyWifiSnapshot(await invoke<WifiSnapshot>("start_wifi_helper"));
    renderWifiSnapshot();
  }
});
el<HTMLSelectElement>("wifiInterface").onchange = commandHandler("Wi-Fi interface selection failed", async e => {
  const value = (e.target as HTMLSelectElement).value;
  applyWifiSnapshot(await invoke<WifiSnapshot>("select_wifi_interface", { "interface": value }));
  renderWifiSnapshot();
});
el<HTMLButtonElement>("overlayAll").onclick = commandHandler("Overlay update failed", async () => { applyWifiSnapshot(await invoke<WifiSnapshot>("set_wifi_overlay_all", { visible: true })); renderWifiSnapshot(); });
el<HTMLButtonElement>("overlayNone").onclick = commandHandler("Overlay update failed", async () => { selectedBssid = null; applyWifiSnapshot(await invoke<WifiSnapshot>("set_wifi_overlay_all", { visible: false })); renderWifiSnapshot(); });
document.querySelectorAll<HTMLButtonElement>("button[data-band]").forEach(btn => {
  btn.onclick = () => applyBandPresetToRfForm(btn.dataset.band);
});
el<HTMLButtonElement>("applyRfSweep").onclick = commandHandler("Apply RF / sweep failed", async () => { await applyRfSweepChanges(); });
el<HTMLButtonElement>("toggleWifiPanel").onclick = () => { wifiCollapsed = !wifiCollapsed; updateWifiPanel(); };
el<HTMLButtonElement>("toggleSettings").onclick = () => {
  settingsCollapsed = !settingsCollapsed;
  document.querySelector(".layout")!.classList.toggle("settings-collapsed", settingsCollapsed);
  el<HTMLButtonElement>("toggleSettings").setAttribute("aria-expanded", String(!settingsCollapsed));
};
el<HTMLButtonElement>("resetZoom").onclick = () => setPlotView(null);
el<HTMLButtonElement>("historyLive").onclick = () => setHistoryLiveMode();
el<HTMLButtonElement>("historyReplay").onclick = commandHandler("History replay failed", async () => { const ids=historyInfo.frame_ids, id=ids[ids.length-1]; if (id !== undefined) await showHistoryFrame(id); });
el<HTMLButtonElement>("historyPrev").onclick = commandHandler("History replay failed", async () => { if (replayFrameId !== null) await showHistoryFrame(replayFrameId, -1); });
el<HTMLButtonElement>("historyNext").onclick = commandHandler("History replay failed", async () => { if (replayFrameId !== null) await showHistoryFrame(replayFrameId, 1); });
el<HTMLInputElement>("historySlider").oninput = commandHandler("History replay failed", async e => {
  const id = historyInfo.frame_ids[clampHistoryIndex(Number((e.target as HTMLInputElement).value))];
  if (id !== undefined) await showHistoryFrame(id);
});

function showFatalError(err: unknown) {
  if (fatalShown) return;
  fatalShown = true;
  stopDisplayAnimation();
  if (snapshotTimer !== undefined) {
    window.clearInterval(snapshotTimer);
    snapshotTimer = undefined;
  }
  if (wifiAutoTimer !== undefined) {
    window.clearInterval(wifiAutoTimer);
    wifiAutoTimer = undefined;
  }
  if (displayPatchTimer !== undefined) {
    window.clearTimeout(displayPatchTimer);
    displayPatchTimer = undefined;
  }
  const message = err instanceof Error ? `${err.name}: ${err.message}\n${err.stack ?? ""}` : String(err);
  console.error(err);
  app.innerHTML = `
    <div class="fatal">
      <h1>SDRWispy failed to start</h1>
      <p>The frontend JavaScript initialized but hit a fatal error. On Linux, also check the terminal running Tauri for backend logs.</p>
      <pre>${html(message)}</pre>
    </div>
  `;
}

window.addEventListener("error", ev => showFatalError(ev.error ?? ev.message));
window.addEventListener("unhandledrejection", ev => showFatalError(ev.reason));

async function boot() {
  spectrum = new SpectrumRenderer(document.querySelector<HTMLCanvasElement>("#spectrum")!);
  wifiOverlay = new WifiOverlayRenderer(document.querySelector<HTMLCanvasElement>("#wifiOverlay")!);
  waterfall = new WaterfallRenderer(document.querySelector<HTMLCanvasElement>("#waterfall")!);

  bindDisplayControls();
  bindSpectrumSelection();
  bindPlotInteraction();
  wifiCollapsed = innerWidth < 1250;
  updateWifiPanel();

  await loadSnapshot();
  pollFrame();
  snapshotTimer = window.setInterval(() => {
    void refreshSnapshotFromTimer();
  }, 1000);
  wifiAutoTimer = window.setInterval(() => {
    void runAutoWifiScan();
  }, 1000);
  const resizePlots = () => {
    if (fatalShown) return;
    renderFrequencyAxis();
    redrawDisplayViews();
    redrawWaterfallView();
  };
  let resizeQueued = false;
  new ResizeObserver(() => {
    if (resizeQueued) return;
    resizeQueued = true;
    requestAnimationFrame(() => { resizeQueued = false; resizePlots(); });
  }).observe(document.querySelector(".workspace")!);
  new ResizeObserver(resizePlots).observe(document.querySelector(".spectrum-area")!);
  let narrowLayout = innerWidth <= 1100;
  window.addEventListener("resize", () => {
    const narrow = innerWidth <= 1100;
    if (narrow && !narrowLayout) { wifiCollapsed = true; updateWifiPanel(); }
    narrowLayout = narrow;
    resizePlots();
  });
  document.addEventListener("visibilitychange", () => {
    stopDisplayAnimation();
    if (document.hidden) {
      frameFreshness.reset();
      return;
    }
    visibilitySyncPending = true;
    void loadSnapshot().then(applied => {
      if (!applied) return;
      redrawDisplayViews();
      redrawWaterfallView();
      syncDisplayAnimation();
    }).catch(err => {
      showCommandError("Resume refresh failed", err);
      // Stay paused until a successful refresh confirms capture state.
    });
  });
}

void boot().catch(showFatalError);
