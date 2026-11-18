import { SpectrumRenderer, WaterfallRenderer, type SpectrumDisplayOptions, type SpectrumFrame } from "./renderers";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const calls: { name: string; args: unknown[] }[] = [];
const constants = new Map<string, number>();
const gl = new Proxy({
  createShader: () => ({}), createProgram: () => ({}), createBuffer: () => ({}), createTexture: () => ({}),
  getShaderParameter: () => true, getProgramParameter: () => true,
  getUniformLocation: () => ({}), getAttribLocation: () => 0,
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
  valid_bin_ranges: [[1, 3]], invalid_bin_count: 2, estimated_hops: 1, completed_hops: 1,
};
const waterfall = new WaterfallRenderer(canvas as unknown as HTMLCanvasElement);
waterfall.pushFrame(frame);
const allocations = calls.filter(call => call.name === "texImage2D");
assert(allocations.length === 1 && allocations[0].args[8] === null, "allocate texture once without a full CPU image");
let uploads = calls.filter(call => call.name === "texSubImage2D");
assert(uploads.length === 1 && uploads[0].args[5] === 1, "one texture row per completed frame");
const pixels = uploads[0].args[8] as Uint8Array;
assert(pixels[3] === 0 && pixels[7] === 255 && pixels[11] === 255 && pixels[15] === 0, "invalid bins are transparent, not measured low power");
const row = calls.find(call => call.name === "bufferSubData")!.args[2] as Float32Array;
assert(row[1] === 0 && row[4] > 0, "row positions carry timestamp relative to ring origin");
for (let i = 0; i < 120; i++) waterfall.draw(1_000_000 + i * 16, 60);
assert(calls.filter(call => call.name === "texSubImage2D").length === 1, "animation uploads no synthetic rows");
assert(calls.filter(call => call.name === "bufferSubData").length === 1, "scrolling updates uniforms, not row geometry");
waterfall.pushFrame(frame);
waterfall.pushFrame({ ...frame, partial: true, frame_id: 2 });
assert(calls.filter(call => call.name === "texSubImage2D").length === 1, "duplicates and partials do not add rows");
waterfall.pushFrame({ ...frame, frame_id: 2, sweep_end_time_ns: 1_010_000_000_000 });
uploads = calls.filter(call => call.name === "texSubImage2D");
assert(uploads.length === 2 && uploads[1].args[3] === 1 && uploads[1].args[5] === 1, "next ring slot uploads only itself despite ten-second gap");
const secondRow = calls.filter(call => call.name === "bufferSubData")[1].args[2] as Float32Array;
assert(secondRow[1] === 10, "gaps preserve real elapsed timestamp seconds");
canvas.clientWidth = 1100;
canvas.clientHeight = 450;
view.window.devicePixelRatio = 2;
waterfall.draw(1_010_000, 15);
assert(canvas.width === 2200 && canvas.height === 900, "HiDPI resize changes backing canvas");
assert(calls.filter(call => call.name === "texImage2D").length === 1, "resize does not discard or copy history texture");
waterfall.setReplayFrame(frame);
const replayUpload = calls.filter(call => call.name === "texSubImage2D")[2];
assert(replayUpload.args[3] === 2047, "replay uses reserved texture row without overwriting live history");
waterfall.draw(9_000_000, 15, true);
const draws = calls.filter(call => call.name === "drawArrays");
assert(draws[draws.length - 1].args[1] === 2047 * 6 && draws[draws.length - 1].args[2] === 6, "replay draws only selected observation");
waterfall.setReplayFrame(null);
const before = calls.filter(call => call.name === "drawArrays").length;
waterfall.draw(9_000_000, 15, true);
assert(calls.filter(call => call.name === "drawArrays").length === before, "pending replay never shows stale live history");
waterfall.clear();
waterfall.draw(1_010_000, 15);
assert(calls.filter(call => call.name === "drawArrays").length === before, "reset clears old row metadata immediately");
assert(calls.filter(call => call.name === "texImage2D").length === 1, "reset needs no whole-texture upload");

const spectrum = new SpectrumRenderer(canvas as unknown as HTMLCanvasElement);
const options: SpectrumDisplayOptions = {
  show_current: true, show_average: false, show_max_hold: false,
  show_wifi_overlay: false, show_wifi_labels: false, label_aps_by_bssid: false,
  spectrum_floor_db: -115, spectrum_ceiling_db: -20,
  wifi_overlay_rssi_floor_dbm: -95, wifi_overlay_rssi_ceiling_dbm: -30,
  wifi_overlay_floor_db: -113, wifi_overlay_ceiling_db: -30,
};
calls.length = 0;
spectrum.draw({ ...frame, valid_bin_ranges: [] }, options);
assert(calls.filter(call => call.name === "drawArrays").length === 1, "all-invalid Current draws only grid, not fake measured floor");
console.log("Renderer tests passed: row-only uploads, real-time gaps, validity, resize/HiDPI, replay and reset");
