import { invoke } from "@tauri-apps/api/core";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";
import "./style.css";
import { SpectrumRenderer, WaterfallRenderer, cssColorForBssid, type SpectrumDisplayOptions, type SpectrumFrame, type WifiBss } from "./renderers";

interface BandConfig { preset: string; start_hz: number; stop_hz: number; bin_width_hz: number; }
interface SweepConfig { sample_rate_hz: number; usable_bandwidth_ratio: number; overlap_hz: number; dwell_ms: number; settle_ms: number; fft_size: number; }
interface DisplaySettings {
  show_current: boolean;
  show_average: boolean;
  show_max_hold: boolean;
  show_wifi_overlay: boolean;
  show_wifi_labels: boolean;
  label_aps_by_bssid: boolean;
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
}
interface SdrDevice { id: string; label: string; driver: string; }
interface WifiBackendOption { id: string; label: string; requires_elevation: boolean; }
interface Snapshot {
  band: BandConfig;
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
          <div class="field"><label>RX gain dB</label><input id="gainDb" type="number" step="1" min="0" max="100"></div>
          <div class="field"><label>Band</label></div>
          <div class="segmented"><button data-band="2.4g">2.4G</button><button data-band="5g">5G</button><button data-band="6e">6E</button></div>
          <div class="grid2">
            <div class="field"><label>Start MHz</label><input id="startMhz" type="number" step="1" min="1"></div>
            <div class="field"><label>Stop MHz</label><input id="stopMhz" type="number" step="1" min="1"></div>
            <div class="field"><label>Bin kHz</label><input id="binKhz" type="number" step="10" min="1"></div>
          </div>
          <div class="grid2">
            <div class="field"><label>Sample MS/s</label><input id="sampleRate" type="number" step="1" min="1" max="250"></div>
            <div class="field"><label>Usable ratio</label><input id="usableRatio" type="number" step="0.01" min="0.1" max="0.95"></div>
            <div class="field"><label>Overlap MHz</label><input id="overlap" type="number" step="0.5" min="0"></div>
            <div class="field"><label>Dwell ms</label><input id="dwell" type="number" step="0.5" min="0.1"></div>
            <div class="field"><label>Settle ms</label><input id="settle" type="number" step="0.5" min="0"></div>
            <div class="field"><label>FFT size</label><input id="fft" type="number" step="256" min="256"></div>
          </div>
          <button id="applyRfSweep">Apply RF / sweep</button>
          <div id="rfPending" class="hint pending hidden">RF/sweep edits are pending. Apply or Run commits them; band changes will not overwrite them.</div>
        </section>

        <details class="card" open>
          <summary>Display</summary>
          <div class="grid2 checks">
            <label class="check"><input id="showCurrent" type="checkbox"> Current</label>
            <label class="check"><input id="showAverage" type="checkbox"> Average</label>
            <label class="check"><input id="showMaxHold" type="checkbox"> Max hold</label>
            <label class="check"><input id="showWifiOverlay" type="checkbox"> Wi-Fi overlay</label>
            <label class="check"><input id="showWifiLabels" type="checkbox"> AP labels</label>
          </div>
          <label class="check"><input id="labelBssid" type="checkbox"> Prefer BSSID labels</label>
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
          <div class="display-control">
            <label>Graph floor</label>
            <input id="overlayFloor" type="number" step="1">
            <input id="overlayFloorSlider" type="range" min="-130" max="-10" step="1">
          </div>
          <div class="display-control">
            <label>Graph ceiling</label>
            <input id="overlayCeil" type="number" step="1">
            <input id="overlayCeilSlider" type="range" min="-130" max="-10" step="1">
          </div>
          <div class="hint">Display settings apply immediately. Numeric fields commit on Enter or blur; sliders update live.</div>
        </details>
      </aside>

      <main class="workspace">
        <div class="topbar" id="topbar"></div>
        <section class="panel spectrum-panel"><div class="panel-title">Spectrum</div><canvas id="spectrum"></canvas><div id="wifiLabels" class="wifi-label-layer"></div></section>
        <div class="frequency-axis" id="freqAxis"></div>
        <section class="panel waterfall-panel"><div class="panel-title">Waterfall</div><canvas id="waterfall"></canvas></section>
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
let waterfall: WaterfallRenderer;
let snap: Snapshot | null = null;
let latestFrameEpoch: number | undefined = undefined;
let latestFrameId: number | undefined = undefined;
let latestFrameRevision: number | undefined = undefined;
let latestFrame: SpectrumFrame | null = null;
let hidden = new Set<string>();
let wifiCollapsed = false;
let activeEpoch = 0;
let rfDirtyFields = new Set<string>();
let selectedBssid: string | null = null;
let displayPatchSeq = 0;
let fatalShown = false;
let snapshotTimer: number | undefined = undefined;
let wifiAutoTimer: number | undefined = undefined;
let snapshotRefreshInFlight = false;
let wifiScanInFlight = false;
let apRowsSignature = "";
let wifiLabelsSignature = "";

const rfInputIds = ["gainDb", "soapyArgs", "startMhz", "stopMhz", "binKhz", "sampleRate", "usableRatio", "overlap", "dwell", "settle", "fft"];

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
function updateRfDirtyUi() {
  const btn = el<HTMLButtonElement>("applyRfSweep");
  const dirty = isRfDirty();
  btn.textContent = dirty ? "Apply RF / sweep *" : "Apply RF / sweep";
  btn.classList.toggle("pending-button", dirty);
  el<HTMLDivElement>("rfPending").classList.toggle("hidden", !dirty);
}
function resetDisplayHistory() {
  latestFrameEpoch = undefined;
  latestFrameId = undefined;
  latestFrameRevision = undefined;
  latestFrame = null;
  wifiLabelsSignature = "";
  const layer = document.getElementById("wifiLabels");
  if (layer) layer.innerHTML = "";
  waterfall.clear();
}

function updateWifiPanel() {
  document.querySelector<HTMLDivElement>(".layout")?.classList.toggle("wifi-collapsed", wifiCollapsed);
  document.querySelector<HTMLElement>(".rightbar")?.classList.toggle("collapsed", wifiCollapsed);
  const btn = el<HTMLButtonElement>("toggleWifiPanel");
  btn.textContent = wifiCollapsed ? "Expand" : "Collapse";
  btn.title = wifiCollapsed ? "Expand WLAN panel" : "Collapse WLAN panel";
}

async function loadSnapshot() {
  if (fatalShown) return;
  snap = await invoke<Snapshot>("get_snapshot");
  hidden = new Set(snap.hidden_wifi_overlay_bssids.map(v => v.toLowerCase()));
  renderSnapshot();
}

function renderSnapshot() {
  if (!snap || fatalShown) return;
  hidden = new Set(snap.hidden_wifi_overlay_bssids.map(v => v.toLowerCase()));
  if (selectedBssid && (!snap.aps.some(ap => ap.bssid.toLowerCase() === selectedBssid) || hidden.has(selectedBssid))) {
    selectedBssid = null;
  }
  if (snap.capture_epoch !== activeEpoch) {
    activeEpoch = snap.capture_epoch;
    resetDisplayHistory();
  }
  const canRun = hasRealSelectedDevice(snap);
  setStatusText(snap.status);
  const runButton = el<HTMLButtonElement>("run");
  runButton.disabled = snap.running || !canRun;
  runButton.title = canRun ? "Start capture" : "No usable SDR device is available";
  el<HTMLButtonElement>("stop").disabled = !snap.running;
  const scanButton = el<HTMLButtonElement>("scanWifi");
  scanButton.disabled = snap.wifi_scan_in_progress || wifiScanInFlight;
  scanButton.textContent = (snap.wifi_scan_in_progress || wifiScanInFlight) ? "Scanning" : "Scan";
  el<HTMLDivElement>("topbar").innerHTML = `
    <span>${snap.profile_name}</span>
    <span>Band ${mhz(snap.band.start_hz)}-${mhz(snap.band.stop_hz)} MHz</span>
    <span>bins ${Math.ceil((snap.band.stop_hz - snap.band.start_hz) / snap.band.bin_width_hz)}</span>
    <span>bin ${(snap.band.bin_width_hz/1e3).toFixed(1)} kHz</span>
    <span>rate ${(snap.sweep.sample_rate_hz/1e6).toFixed(2)} MS/s</span>
    <span>gain ${snap.sdr_gain_db.toFixed(1)} dB</span>
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
  setCheckbox("showCurrent", snap.display.show_current);
  setCheckbox("showAverage", snap.display.show_average);
  setCheckbox("showMaxHold", snap.display.show_max_hold);
  setCheckbox("showWifiOverlay", snap.display.show_wifi_overlay);
  setCheckbox("showWifiLabels", snap.display.show_wifi_labels);
  setCheckbox("labelBssid", snap.display.label_aps_by_bssid);
  setCheckbox("autoWifiScan", snap.wifi_auto_scan_enabled);
  setNumberInput("wifiAutoInterval", snap.wifi_auto_scan_interval_secs);
  syncDisplayValue("rssiFloor", "rssiFloorSlider", snap.display.wifi_overlay_rssi_floor_dbm);
  syncDisplayValue("rssiCeil", "rssiCeilSlider", snap.display.wifi_overlay_rssi_ceiling_dbm);
  syncDisplayValue("overlayFloor", "overlayFloorSlider", snap.display.wifi_overlay_floor_db);
  syncDisplayValue("overlayCeil", "overlayCeilSlider", snap.display.wifi_overlay_ceiling_db);
  renderWifiBackendSelect();
  renderWifiInterfaceSelect();
  el<HTMLDivElement>("apCount").textContent = snap.wifi_status || `${snap.aps.length} APs in selected band`;
  renderAps();
  updateWifiPanel();
  updateRfDirtyUi();
  renderFrequencyAxis();
  renderWifiOverlayLabels(latestFrame);
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
    renderSnapshot();
    return false;
  }
  snap = await invoke<Snapshot>("start_wifi_helper");
  renderSnapshot();
  return snap.wifi_helper_running;
}

function wifiAutoIntervalSecs(): number {
  const input = el<HTMLInputElement>("wifiAutoInterval");
  const value = Math.round(Number(input.value));
  if (!Number.isFinite(value)) return snap?.wifi_auto_scan_interval_secs ?? 30;
  return Math.max(5, Math.min(600, value));
}

async function updateWifiAutoScanSettings() {
  snap = await invoke<Snapshot>("set_wifi_auto_scan", {
    enabled: el<HTMLInputElement>("autoWifiScan").checked,
    intervalSecs: wifiAutoIntervalSecs(),
  });
  renderSnapshot();
}

async function refreshSnapshotFromTimer() {
  if (fatalShown || snapshotRefreshInFlight) return;
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

  wifiScanInFlight = true;
  renderSnapshot();
  try {
    snap = await invoke<Snapshot>("scan_wifi_auto");
    renderSnapshot();
  } catch (err) {
    showCommandError("Wi-Fi auto scan failed", err);
  } finally {
    wifiScanInFlight = false;
    renderSnapshot();
  }
}

function renderAps() {
  if (!snap) return;
  const s = snap;
  const selected = selectedBssid?.toLowerCase() ?? null;
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
      ap.rssi_dbm,
      ap.phy,
    ]),
  });
  if (signature === apRowsSignature) return;
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
      <td>${ap.width_mhz} MHz</td><td>${ap.rssi_dbm} dBm</td><td>${html(ap.phy)}</td>
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
      await invoke("set_wifi_overlay_hidden", { bssid, hidden: !input.checked });
      await loadSnapshot();
    });
  });
}

function freqToWifiChannel(mhz: number): string | null {
  if (Math.abs(mhz - 2484) <= 2.5) return "14";
  if (mhz >= 2412 && mhz <= 2472) return String(Math.round((mhz - 2407) / 5));
  if (mhz >= 5005 && mhz <= 5895) return String(Math.round((mhz - 5000) / 5));
  if (mhz >= 5955 && mhz <= 7115) return String(Math.round((mhz - 5950) / 5));
  return null;
}

function renderFrequencyAxis() {
  if (!snap) return;
  const axis = el<HTMLDivElement>("freqAxis");
  const width = Math.max(240, axis.clientWidth || axis.parentElement?.clientWidth || 800);
  const tickCount = Math.max(2, Math.min(9, Math.floor(width / 150) + 1));
  const start = snap.band.start_hz / 1e6;
  const stop = snap.band.stop_hz / 1e6;
  const ticks: string[] = [];
  for (let i = 0; i < tickCount; i++) {
    const t = tickCount === 1 ? 0 : i / (tickCount - 1);
    const mhzValue = start + (stop - start) * t;
    const ch = freqToWifiChannel(mhzValue);
    const edgeClass = i === 0 ? " edge-left" : i === tickCount - 1 ? " edge-right" : "";
    ticks.push(`<div class="freq-tick${edgeClass}" style="left:${(t * 100).toFixed(3)}%">
      <span>${mhzValue.toFixed(0)} MHz${ch ? ` · ch ${ch}` : ""}</span>
    </div>`);
  }
  axis.innerHTML = ticks.join("");
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
  const dbMin = -115;
  const dbMax = -20;
  const labels: string[] = [];
  for (const ap of snap.aps) {
    if (hidden.has(ap.bssid.toLowerCase())) continue;
    const half = ap.width_mhz * 1e6 / 2;
    const left = ap.center_freq_hz - half;
    const right = ap.center_freq_hz + half;
    if (right < frame.band_start_hz || left > frame.band_stop_hz) continue;

    const center = Math.max(frame.band_start_hz, Math.min(frame.band_stop_hz, ap.center_freq_hz));
    const x = (center - frame.band_start_hz) / Math.max(1, frame.band_stop_hz - frame.band_start_hz) * 100;
    const rssiT = Math.max(0, Math.min(1, (ap.rssi_dbm - snap.display.wifi_overlay_rssi_floor_dbm) / rssiSpan));
    const topDb = snap.display.wifi_overlay_floor_db + rssiT * (snap.display.wifi_overlay_ceiling_db - snap.display.wifi_overlay_floor_db);
    const yNorm = Math.max(0, Math.min(1, (topDb - dbMin) / (dbMax - dbMin)));
    const top = Math.max(6, Math.min(88, (1 - yNorm) * 100 - 4));
    const text = (snap.display.label_aps_by_bssid || !ap.ssid.trim()) ? ap.bssid : ap.ssid;
    const color = cssColorForBssid(ap.bssid);
    const selected = selectedBssid?.toLowerCase() === ap.bssid.toLowerCase();
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
  redrawDisplayViews();
}

type DisplayPatch = Partial<{
  show_current: boolean;
  show_average: boolean;
  show_max_hold: boolean;
  show_wifi_overlay: boolean;
  show_wifi_labels: boolean;
  label_aps_by_bssid: boolean;
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
}>;

function syncDisplayValue(numberId: string, sliderId: string, value: number) {
  const number = el<HTMLInputElement>(numberId);
  const slider = el<HTMLInputElement>(sliderId);
  if (document.activeElement === number || document.activeElement === slider) return;
  setNumberInput(numberId, value);
  setRangeInput(sliderId, value);
}

function redrawDisplayViews() {
  renderWifiOverlayLabels(latestFrame);
  if (snap && latestFrame) spectrum.draw(latestFrame, snap.aps, hidden, snap.display as SpectrumDisplayOptions, selectedBssid);
}

function applyLocalDisplayPatch(patch: DisplayPatch) {
  if (!snap) return;
  Object.assign(snap.display, patch);
  if (patch.label_aps_by_bssid !== undefined) renderAps();
  redrawDisplayViews();
}

async function patchDisplayLive(patch: DisplayPatch) {
  const seq = ++displayPatchSeq;
  applyLocalDisplayPatch(patch);
  try {
    const next = await invoke<Snapshot>("patch_display", { patch });
    if (seq === displayPatchSeq) {
      snap = next;
      renderSnapshot();
      redrawDisplayViews();
    }
  } catch (err) {
    console.error(err);
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

function bindDisplayControls() {
  bindDisplayCheckbox("showCurrent", "show_current");
  bindDisplayCheckbox("showAverage", "show_average");
  bindDisplayCheckbox("showMaxHold", "show_max_hold");
  bindDisplayCheckbox("showWifiOverlay", "show_wifi_overlay");
  bindDisplayCheckbox("showWifiLabels", "show_wifi_labels");
  bindDisplayCheckbox("labelBssid", "label_aps_by_bssid");
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
  const freqHz = latestFrame.band_start_hz + x / Math.max(1, rect.width) * (latestFrame.band_stop_hz - latestFrame.band_start_hz);
  const dbMin = -115;
  const dbMax = -20;
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
    const picked = pickWifiOverlay(ev.clientX, ev.clientY);
    if (picked) selectAp(selectedBssid === picked ? null : picked);
  });
}

async function pollFrame() {
  if (fatalShown) return;
  try {
    let drewWaterfall = false;
    const requestedEpoch = latestFrameEpoch;
    const requestedFrameId = latestFrameId;
    const requestedFrameRevision = latestFrameRevision;
    const frame = await invoke<SpectrumFrame | null>("get_latest_frame", {
      afterCaptureEpoch: requestedEpoch ?? null,
      afterFrameId: requestedFrameId ?? null,
      afterFrameRevision: requestedFrameRevision ?? null,
    });
    if (frame) {
      if (snap && frame.capture_epoch < snap.capture_epoch) {
        waterfall.draw();
        window.setTimeout(pollFrame, 33);
        return;
      }
      if (latestFrameEpoch !== undefined && frame.capture_epoch !== latestFrameEpoch) {
        latestFrame = null;
        latestFrameId = undefined;
        latestFrameRevision = undefined;
        waterfall.clear();
      }
      latestFrameEpoch = frame.capture_epoch;
      latestFrame = frame;
      latestFrameId = frame.frame_id;
      latestFrameRevision = frame.frame_revision;
      if (!frame.partial) {
        waterfall.pushFrame(frame);
        drewWaterfall = true;
      }
      if (snap) {
        spectrum.draw(frame, snap.aps, hidden, snap.display as SpectrumDisplayOptions, selectedBssid);
        renderWifiOverlayLabels(frame);
      }
    }
    if (drewWaterfall) waterfall.draw();
  } catch (err) {
    console.error(err);
  }
  if (!fatalShown) window.setTimeout(pollFrame, 33);
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
el<HTMLButtonElement>("stop").onclick = commandHandler("Stop failed", async () => { snap = await invoke<Snapshot>("stop_capture"); renderSnapshot(); });
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
  renderSnapshot();
  try {
    snap = await invoke<Snapshot>("scan_wifi");
    renderSnapshot();
  } finally {
    wifiScanInFlight = false;
    renderSnapshot();
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
      renderSnapshot();
      return;
    }
  }
  snap = await invoke<Snapshot>("select_wifi_backend", { backend });
  renderSnapshot();
  if (needsHelper) {
    snap = await invoke<Snapshot>("start_wifi_helper");
    renderSnapshot();
  }
});
el<HTMLSelectElement>("wifiInterface").onchange = commandHandler("Wi-Fi interface selection failed", async e => {
  const value = (e.target as HTMLSelectElement).value;
  snap = await invoke<Snapshot>("select_wifi_interface", { "interface": value });
  renderSnapshot();
});
el<HTMLButtonElement>("overlayAll").onclick = commandHandler("Overlay update failed", async () => { snap = await invoke<Snapshot>("set_wifi_overlay_all", { visible: true }); renderSnapshot(); });
el<HTMLButtonElement>("overlayNone").onclick = commandHandler("Overlay update failed", async () => { selectedBssid = null; snap = await invoke<Snapshot>("set_wifi_overlay_all", { visible: false }); renderSnapshot(); });
document.querySelectorAll<HTMLButtonElement>("button[data-band]").forEach(btn => btn.onclick = commandHandler("Band change failed", async () => { resetDisplayHistory(); snap = await invoke<Snapshot>("set_band_preset", { preset: btn.dataset.band }); renderSnapshot(); }));
el<HTMLButtonElement>("applyRfSweep").onclick = commandHandler("Apply RF / sweep failed", async () => { await applyRfSweepChanges(); });
el<HTMLButtonElement>("toggleWifiPanel").onclick = () => { wifiCollapsed = !wifiCollapsed; updateWifiPanel(); };

function showFatalError(err: unknown) {
  if (fatalShown) return;
  fatalShown = true;
  if (snapshotTimer !== undefined) {
    window.clearInterval(snapshotTimer);
    snapshotTimer = undefined;
  }
  if (wifiAutoTimer !== undefined) {
    window.clearInterval(wifiAutoTimer);
    wifiAutoTimer = undefined;
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
  waterfall = new WaterfallRenderer(document.querySelector<HTMLCanvasElement>("#waterfall")!);

  bindDisplayControls();
  bindSpectrumSelection();

  await loadSnapshot();
  pollFrame();
  snapshotTimer = window.setInterval(() => {
    void refreshSnapshotFromTimer();
  }, 1000);
  wifiAutoTimer = window.setInterval(() => {
    void runAutoWifiScan();
  }, 1000);
  window.addEventListener("resize", () => {
    if (fatalShown) return;
    renderFrequencyAxis();
    redrawDisplayViews();
    waterfall.draw();
  });
}

void boot().catch(showFatalError);
