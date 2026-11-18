import { binFrequency, frequencyTicks, powerColor, SpectrumRenderer, WaterfallRenderer, WifiOverlayRenderer,
  WATERFALL_DC_COLOR, WATERFALL_NO_DATA_COLOR, type SpectrumDisplayOptions, type SpectrumFrame } from "./renderers";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
function equal(actual: unknown, expected: unknown, message: string) {
  assert(JSON.stringify(actual) === JSON.stringify(expected), message);
}
function near(actual: number, expected: number) {
  assert(Math.abs(actual - expected) < 1e-6, `${actual} != ${expected}`);
}

const calls: { name: string; args: unknown[] }[] = [];
const named = (name: string) => calls.filter(call => call.name === name);
const constants = new Map<string, number>();
const gl = new Proxy({
  createShader: () => ({}), createProgram: () => ({}), createBuffer: () => ({}), createTexture: () => ({}),
  getShaderParameter: () => true, getProgramParameter: () => true,
  getUniformLocation: (_program: unknown, name: string) => name, getAttribLocation: () => 0,
  getParameter: () => 4096,
}, {
  get(target, property: string) {
    if (property in target) return target[property as keyof typeof target];
    if (/^[A-Z_0-9]+$/.test(property)) {
      if (!constants.has(property)) constants.set(property, constants.size + 1);
      return constants.get(property);
    }
    return (...args: unknown[]) => calls.push({
      name: property,
      args: args.map(arg => arg instanceof Uint8Array || arg instanceof Float32Array ? arg.slice() : arg),
    });
  },
});
const view = globalThis as unknown as { window: { devicePixelRatio: number } };
view.window = { devicePixelRatio: 1 };
const canvas = { width: 0, height: 0, clientWidth: 640, clientHeight: 300, getContext: () => gl };
const frame: SpectrumFrame = {
  capture_epoch: 1, frame_id: 1, frame_revision: 1, partial: false,
  sweep_start_time_ns: 999_000_000_000, sweep_end_time_ns: 1_000_000_000_000,
  band_start_hz: 2.4e9, band_stop_hz: 2.5e9, bin_width_hz: 1e6,
  current_db: [-115, -40, -50, -60], average_db: [-60, -70, -80, -90], max_hold_db: [-10, -20, -30, -40],
  valid_bin_ranges: [[1, 3]], history_valid_bin_ranges: [[0, 4]], dc_excluded_bin_ranges: [[2, 3]],
  invalid_bin_count: 2, estimated_hops: 1, completed_hops: 1,
};
const waterfall = new WaterfallRenderer(canvas as unknown as HTMLCanvasElement);
const input = { ...frame, current_db: [...frame.current_db] };
waterfall.pushFrame(input);
input.current_db[1] = -10;
const allocations = named("texImage2D");
assert(allocations.length === 1 && allocations[0].args[8] === null, "allocate texture once without a full CPU image");
let uploads = named("texSubImage2D");
assert(uploads.length === 1 && uploads[0].args[5] === 1, "one texture row per completed frame");
equal([...(uploads[0].args[8] as Uint8Array)],
  [...WATERFALL_NO_DATA_COLOR, ...powerColor(-40), ...WATERFALL_DC_COLOR, ...WATERFALL_NO_DATA_COLOR],
  "unobserved gray, actual power and excluded DC have distinct colors");
const row = named("bufferSubData")[0].args[2] as Float32Array;
assert(row[1] === 0 && row[4] > 0, "row positions carry timestamp relative to ring origin");
near(row[5], -0.92);
equal([0, 1, 2].map(i => waterfall.sampleAt(1_000_000, binFrequency(frame, i))), [
  { timeMs: 1_000_000, value: null, dc: false }, { timeMs: 1_000_000, value: -40, dc: false },
  { timeMs: 1_000_000, value: null, dc: true },
], "hover uses immutable raw samples, not the mutable input or color values");
for (let i = 0; i < 120; i++) waterfall.draw(1_000_000 + i * 16, 60);
assert(named("texSubImage2D").length === 1 && named("bufferSubData").length === 1,
  "animation uploads neither synthetic rows nor replacement geometry");
waterfall.pushFrame(frame);
waterfall.pushFrame({ ...frame, partial: true, frame_id: 2 });
assert(named("texSubImage2D").length === 1, "duplicates and partials do not add rows");
waterfall.pushFrame({ ...frame, frame_id: 2, sweep_start_time_ns: 1_009_000_000_000, sweep_end_time_ns: 1_010_000_000_000 });
uploads = named("texSubImage2D");
assert(uploads.length === 2 && uploads[1].args[3] === 1 && uploads[1].args[5] === 1, "only next ring slot uploads despite ten-second gap");
assert((named("bufferSubData")[1].args[2] as Float32Array)[1] === 10, "gaps preserve real elapsed timestamp seconds");
assert(waterfall.sampleAt(1_005_000, binFrequency(frame, 1)) === null &&
  waterfall.sampleAt(1_000_000, frame.band_start_hz + 20e6) === null, "hover cannot invent temporal or frequency coverage");
canvas.clientWidth = 1100;
canvas.clientHeight = 450;
view.window.devicePixelRatio = 2;
waterfall.draw(1_010_000, 15);
assert(canvas.width === 2200 && canvas.height === 900, "HiDPI resize changes backing canvas");
assert(named("texImage2D").length === 1, "resize does not discard or copy history texture");
waterfall.setReplayFrame(frame);
waterfall.draw(1_010_000, 15, true);
assert(named("drawArrays").slice(-1)[0].args[2] === 12 && named("texImage2D").length === 2,
  "replay draws a separate frozen history window, not a single selected row");
assert(named("uniform1f").filter(call => call.args[0] === "u_end").slice(-1)[0].args[1] === 10,
  "replay honors the requested window end rather than pinning selection at the top");
waterfall.setLevels(-90, -30);
uploads = named("texSubImage2D");
assert(uploads.length === 8 && named("bufferSubData").length === 4, "levels recolor both cached views without rewriting timestamps");
equal([...(uploads.slice(-1)[0].args[8] as Uint8Array).slice(4, 12)], [...powerColor(-40, -90, -30), ...WATERFALL_DC_COLOR],
  "recoloring uses original dB and preserves DC");
waterfall.setLevels(-90, -30);
assert(named("texSubImage2D").length === 8, "unchanged levels do not re-upload rows");
const frozenRetention = waterfall.retention(true);
for (let id = 3; id <= 2050; id++) {
  waterfall.pushFrame({ ...frame, frame_id: id, sweep_start_time_ns: (1007 + id) * 1e9, sweep_end_time_ns: (1008 + id) * 1e9 });
}
waterfall.setReplayFrame({ ...frame, sweep_end_time_ns: 9_000_000_000_000 });
const beforeReplayDraw = named("texSubImage2D").length;
waterfall.draw(1_010_000, 15, true);
equal(waterfall.retention(true), frozenRetention, "live eviction and repeat selection cannot change frozen history timestamps");
assert(waterfall.retention().count === 2047 && waterfall.sampleAt(1_000_000, binFrequency(frame, 1)) === null &&
  waterfall.sampleAt(1_000_000, binFrequency(frame, 1), true)?.value === -40 &&
  named("texSubImage2D").length === beforeReplayDraw, "bounded live ring evicts independently of replay CPU and GPU snapshots");
waterfall.setView({ startHz: 2.401e9, stopHz: 2.403e9 });
waterfall.draw(1_010_000, 15, true);
equal(named("uniform2f").slice(-1)[0].args.slice(1), [50, 48], "waterfall zoom changes only the shared frequency transform");
waterfall.setView(null);
waterfall.setReplayFrame({ ...frame, capture_epoch: 2 });
assert(waterfall.retention(true).count === 1 && waterfall.retention().count === 2047, "selecting an uncached epoch does not clear live history");
waterfall.setReplayFrame(null);
const before = named("drawArrays").length;
waterfall.draw(9_000_000, 15, true);
assert(named("drawArrays").length === before && waterfall.retention(true).count === 0, "exiting replay drops its snapshot only");
waterfall.clear();
waterfall.draw(1_010_000, 15);
assert(named("drawArrays").length === before && waterfall.retention().oldestMs === null, "reset clears old row metadata immediately");
assert(named("texImage2D").length === 2, "reset needs no whole-texture upload");

const spectrum = new SpectrumRenderer(canvas as unknown as HTMLCanvasElement);
near(binFrequency({ ...frame, band_start_hz: 100, band_stop_hz: 100.2, bin_width_hz: 1 }, 0), 100.1);
near(binFrequency({ ...frame, band_start_hz: 100, band_stop_hz: 101.2, bin_width_hz: 1 }, 1), 101.1);
const options: SpectrumDisplayOptions = {
  show_current: true, show_average: false, show_max_hold: false,
  show_wifi_overlay: false, show_wifi_labels: false, label_aps_by_bssid: false,
  spectrum_floor_db: -115, spectrum_ceiling_db: -20,
  wifi_overlay_rssi_floor_dbm: -95, wifi_overlay_rssi_ceiling_dbm: -30,
  wifi_overlay_floor_db: -113, wifi_overlay_ceiling_db: -30,
};
calls.length = 0;
spectrum.draw({ ...frame, valid_bin_ranges: [] }, options);
assert(named("drawArrays").length === 1, "all-invalid Current draws only grid, not fake measured floor");
calls.length = 0;
spectrum.draw(frame, { ...options, show_average: true, show_max_hold: true });
const traces = named("bufferData").slice(1).map(call => call.args[1] as Float32Array);
equal(traces.map(trace => trace.length), [2, 8, 8], "history masks preserve prior measurements across a current DC gap");
near(traces[0][0], -0.97);
near(traces[1][0], -0.99);
near(traces[1][6], -0.93);
calls.length = 0;
spectrum.draw({ ...frame, history_valid_bin_ranges: undefined }, { ...options, show_average: true, show_max_hold: true });
equal(named("bufferData").slice(1).map(call => (call.args[1] as Float32Array).length), [2, 2, 2], "legacy history falls back to the current mask");
const zoom = { startHz: 2.401e9, stopHz: 2.403e9 };
spectrum.setView(zoom);
calls.length = 0;
spectrum.draw(frame, options);
near((named("bufferData")[1].args[1] as Float32Array)[0], -0.5);
const grid = named("bufferData")[0].args[1] as Float32Array;
frequencyTicks(zoom, canvas.clientWidth).forEach((tick, i) => near(grid[i * 4], 2 * (tick.hz - zoom.startHz) / (zoom.stopHz - zoom.startHz) - 1));
equal(frequencyTicks({ startHz: 2.4e9, stopHz: 2.5e9 }, 640).map(tick => tick.hz),
  [2.4e9, 2.42e9, 2.44e9, 2.46e9, 2.48e9, 2.5e9], "ticks use shared 1/2/5 spacing, not guessed channels");
spectrum.setView(null);
canvas.clientWidth = 64;
view.window.devicePixelRatio = 1;
const dense = new Array(1024).fill(-80);
dense[6] = -25;
dense[11] = -105;
calls.length = 0;
spectrum.draw({ ...frame, band_start_hz: 0, band_stop_hz: 1024, bin_width_hz: 1,
  current_db: dense, valid_bin_ranges: [[0, 1024]], dc_excluded_bin_ranges: [] }, options);
const compressed = named("bufferData")[1].args[1] as Float32Array;
near(compressed[2], 2 * 6.5 / 1024 - 1);
near(compressed[3], 2 * 90 / 95 - 1);
near(compressed[4], 2 * 11.5 / 1024 - 1);
near(compressed[5], 2 * 10 / 95 - 1);
assert(compressed.length < dense.length * 2, "pixel compression retains min/max at true bin centers in frequency order");
const wifi = new WifiOverlayRenderer(canvas as unknown as HTMLCanvasElement);
wifi.setView({ startHz: 2.4015e9, stopHz: 2.4025e9 });
calls.length = 0;
wifi.draw(frame, [{ ssid: "test", bssid: "00:11:22:33:44:55", primary_freq_hz: 2.402e9, center_freq_hz: 2.402e9,
  channel: 0, width_mhz: 2, rssi_dbm: -60, phy: "test" }], new Set(), { ...options, show_wifi_overlay: true });
const wifiTrace = named("bufferData")[0].args[1] as Float32Array;
assert(wifiTrace[0] === -2 && wifiTrace[6] === 2, "Wi-Fi shares frequency coordinates without reshaping clipped shoulders");
console.log("Renderer tests passed: real frequencies, masks, min/max, row-only uploads, recoloring, hover and frozen history");
