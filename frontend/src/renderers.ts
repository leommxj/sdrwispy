import { dcBinMask, frameRangeKey, frameTimeMs, validBinMask, WATERFALL_ROW_MS, WaterfallRows } from "./display-timing";

export interface SpectrumFrame {
  capture_epoch: number;
  frame_id: number;
  frame_revision: number;
  partial: boolean;
  sweep_start_time_ns: number;
  sweep_end_time_ns: number;
  band_start_hz: number;
  band_stop_hz: number;
  bin_width_hz: number;
  current_db: number[];
  average_db: number[];
  max_hold_db: number[];
  valid_bin_ranges: [number, number][];
  history_valid_bin_ranges?: [number, number][];
  dc_excluded_bin_ranges?: [number, number][];
  invalid_bin_count: number;
  estimated_hops: number;
  completed_hops: number;
}

export interface WifiBss {
  ssid: string;
  bssid: string;
  primary_freq_hz: number;
  center_freq_hz: number;
  channel: number;
  width_mhz: number;
  rssi_dbm: number;
  phy: string;
}

export interface SpectrumDisplayOptions {
  show_current: boolean;
  show_average: boolean;
  show_max_hold: boolean;
  show_wifi_overlay: boolean;
  show_wifi_labels: boolean;
  label_aps_by_bssid: boolean;
  spectrum_floor_db: number;
  spectrum_ceiling_db: number;
  waterfall_floor_db?: number;
  waterfall_ceiling_db?: number;
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
}

export interface FrequencyView {
  startHz: number;
  stopHz: number;
}

export type RGBA = [number, number, number, number];
export const WATERFALL_NO_DATA_COLOR: RGBA = [9, 9, 9, 255];
export const WATERFALL_DC_COLOR: RGBA = [43, 34, 52, 255];

function checkedView(view: FrequencyView | null): FrequencyView | null {
  return view && Number.isFinite(view.startHz) && Number.isFinite(view.stopHz) && view.stopHz > view.startHz
    ? { ...view } : null;
}

function frameView(frame: SpectrumFrame): FrequencyView {
  return { startHz: frame.band_start_hz, stopHz: frame.band_stop_hz };
}

function frequencyX(hz: number, view: FrequencyView): number {
  return 2 * (hz - view.startHz) / (view.stopHz - view.startHz) - 1;
}

export function binFrequency(frame: SpectrumFrame, i: number): number {
  const start = frame.band_start_hz + i * frame.bin_width_hz;
  return (start + Math.min(start + frame.bin_width_hz, frame.band_stop_hz)) / 2;
}

export function frequencyTicks(view: FrequencyView, width: number): { hz: number; label: string }[] {
  if (!checkedView(view) || !Number.isFinite(width) || width <= 0) return [];
  const target = (view.stopHz - view.startHz) / Math.max(1, Math.floor(width / 90));
  const scale = 10 ** Math.floor(Math.log10(target));
  const step = [1, 2, 5, 10].find(multiple => multiple * scale >= target)! * scale;
  const magnitude = Math.max(Math.abs(view.startHz), Math.abs(view.stopHz));
  const [unit, suffix] = magnitude >= 1e9 ? [1e9, "GHz"] : magnitude >= 1e6 ? [1e6, "MHz"]
    : magnitude >= 1e3 ? [1e3, "kHz"] : [1, "Hz"];
  const decimals = Math.max(0, Math.min(12, -Math.floor(Math.log10(step / unit))));
  const first = Math.ceil(view.startHz / step);
  const last = Math.floor(view.stopHz / step);
  const ticks: { hz: number; label: string }[] = [];
  for (let i = first; i <= last; i++) {
    const hz = i * step;
    ticks.push({ hz, label: `${(hz / unit).toFixed(decimals)} ${suffix}` });
  }
  return ticks;
}

function compile(gl: WebGLRenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error("createShader failed");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) || "shader compile failed");
  }
  return shader;
}

function program(gl: WebGLRenderingContext, vs: string, fs: string): WebGLProgram {
  const p = gl.createProgram();
  if (!p) throw new Error("createProgram failed");
  gl.attachShader(p, compile(gl, gl.VERTEX_SHADER, vs));
  gl.attachShader(p, compile(gl, gl.FRAGMENT_SHADER, fs));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) || "link failed");
  return p;
}

function resizeCanvas(canvas: HTMLCanvasElement): boolean {
  const dpr = window.devicePixelRatio || 1;
  // Keep a small non-zero backing store even while CSS grid is being resized.
  // A 1px WebGL viewport plus dense line traces produces ugly aliasing during
  // narrow/dragged layouts; below this size it is better to render into a sane
  // minimum backing buffer and let CSS scale it down.
  const w = Math.max(64, Math.floor(canvas.clientWidth * dpr));
  const h = Math.max(48, Math.floor(canvas.clientHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
    return true;
  }
  return false;
}

function colorForBssid(bssid: string): [number, number, number] {
  const palette: [number, number, number][] = [
    [0, 220, 255], [255, 176, 0], [80, 240, 120], [255, 96, 210], [155, 145, 255], [255, 92, 92],
    [210, 255, 70], [64, 170, 255], [255, 135, 72], [95, 255, 220], [220, 130, 255], [255, 230, 80],
    [0, 190, 140], [255, 120, 155], [120, 205, 255], [175, 255, 145], [190, 120, 80], [145, 180, 255],
  ];
  let hash = 0x811c9dc5;
  for (const ch of bssid.toLowerCase()) {
    if (!/[0-9a-f]/.test(ch)) continue;
    hash ^= ch.charCodeAt(0);
    hash = Math.imul(hash, 0x01000193);
  }
  return palette[Math.abs(hash) % palette.length];
}

export function cssColorForBssid(bssid: string): string {
  const [r, g, b] = colorForBssid(bssid);
  return `rgb(${r},${g},${b})`;
}

const OVERLAY_Y_MIN_DB = -115;
const OVERLAY_Y_MAX_DB = -20;

export class SpectrumRenderer {
  private gl: WebGLRenderingContext;
  private prog: WebGLProgram;
  private pos: number;
  private color: WebGLUniformLocation;
  private buffer: WebGLBuffer;
  private yMin = -115;
  private yMax = -20;
  private view: FrequencyView | null = null;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl", { antialias: false, alpha: false });
    if (!gl) throw new Error("WebGL unavailable");
    this.gl = gl;
    this.prog = program(gl,
      `attribute vec2 a_pos; void main(){ gl_Position = vec4(a_pos,0.0,1.0); gl_PointSize=1.0; }`,
      `precision mediump float; uniform vec3 u_color; void main(){ gl_FragColor = vec4(u_color,1.0); }`,
    );
    this.pos = gl.getAttribLocation(this.prog, "a_pos");
    const loc = gl.getUniformLocation(this.prog, "u_color");
    if (!loc) throw new Error("u_color missing");
    this.color = loc;
    const buffer = gl.createBuffer();
    if (!buffer) throw new Error("createBuffer failed");
    this.buffer = buffer;
  }

  setView(view: FrequencyView | null) {
    this.view = checkedView(view);
  }

  draw(frame: SpectrumFrame, options: SpectrumDisplayOptions, peakValues: number[] = frame.max_hold_db) {
    const gl = this.gl;
    this.setYRange(options);
    resizeCanvas(this.canvas);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0.005, 0.008, 0.012, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(this.pos);
    gl.vertexAttribPointer(this.pos, 2, gl.FLOAT, false, 0, 0);

    const view = this.view ?? frameView(frame);
    this.drawGrid(view);
    const currentMask = validBinMask(frame);
    const historyMask = validBinMask(frame, true);
    if (options.show_current) this.drawTrace(frame, frame.current_db, [0.0, 1.0, 0.35], currentMask, view);
    if (options.show_average) this.drawTrace(frame, frame.average_db, [0.0, 0.78, 1.0], historyMask, view);
    if (options.show_max_hold) this.drawTrace(frame, peakValues, [1.0, 0.55, 0.05], historyMask, view);
  }

  clear() {
    const gl = this.gl;
    gl.clearColor(0.005, 0.008, 0.012, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  private setYRange(options: SpectrumDisplayOptions) {
    const floor = Number.isFinite(options.spectrum_floor_db) ? options.spectrum_floor_db : -115;
    const ceiling = Number.isFinite(options.spectrum_ceiling_db) ? options.spectrum_ceiling_db : -20;
    this.yMin = Math.min(floor, ceiling - 1);
    this.yMax = Math.max(ceiling, this.yMin + 1);
  }

  private y(db: number): number {
    const t = (db - this.yMin) / (this.yMax - this.yMin);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private drawGrid(view: FrequencyView) {
    const verts: number[] = [];
    for (const tick of frequencyTicks(view, this.canvas.clientWidth)) {
      const x = frequencyX(tick.hz, view);
      verts.push(x, -1, x, 1);
    }
    const firstDb = Math.ceil(this.yMin / 10) * 10;
    const lastDb = Math.floor(this.yMax / 10) * 10;
    for (let db = firstDb; db <= lastDb; db += 10) {
      const y = this.y(db);
      verts.push(-1, y, 1, y);
    }
    this.drawVerts(verts, [0.10, 0.18, 0.26], this.gl.LINES);
  }

  private drawTrace(frame: SpectrumFrame, values: number[], rgb: [number, number, number], mask: Uint8Array, view: FrequencyView) {
    const first = Math.max(0, Math.floor((view.startHz - frame.band_start_hz) / frame.bin_width_hz - 0.5));
    const end = Math.min(values.length, frame.current_db.length,
      Math.ceil((view.stopHz - frame.band_start_hz) / frame.bin_width_hz - 0.5) + 1);
    let start = -1;
    for (let i = first; i <= end; i++) {
      if (i < end && mask[i] && Number.isFinite(values[i])) {
        if (start < 0) start = i;
      } else if (start >= 0) {
        this.drawTraceRange(frame, values, start, i, rgb, view);
        start = -1;
      }
    }
  }

  private drawTraceRange(frame: SpectrumFrame, values: number[], start: number, end: number, rgb: [number, number, number], view: FrequencyView) {
    const verts: number[] = [];
    const width = Math.max(1, this.canvas.width);
    const x = (i: number) => frequencyX(binFrequency(frame, i), view);
    let a = start;
    while (a < end) {
      const column = Math.floor((x(a) + 1) * width / 2);
      let b = a + 1;
      let min = a;
      let max = a;
      while (b < end && Math.floor((x(b) + 1) * width / 2) === column) {
        if (values[b] < values[min]) min = b;
        if (values[b] > values[max]) max = b;
        b++;
      }
      // Keep extrema at their own frequencies, in acquisition-bin order.
      const indices = [...new Set([a, min, max, b - 1])].sort((i, j) => i - j);
      for (const i of indices) verts.push(x(i), this.y(values[i]));
      a = b;
    }
    if (verts.length) this.drawVerts(verts, rgb, verts.length === 2 ? this.gl.POINTS : this.gl.LINE_STRIP);
  }

  private drawVerts(verts: number[], rgb: [number, number, number], mode: number) {
    const gl = this.gl;
    gl.uniform3f(this.color, rgb[0], rgb[1], rgb[2]);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.DYNAMIC_DRAW);
    gl.drawArrays(mode, 0, verts.length / 2);
  }
}

export class WifiOverlayRenderer {
  private view: FrequencyView | null = null;
  private gl: WebGLRenderingContext;
  private prog: WebGLProgram;
  private pos: number;
  private color: WebGLUniformLocation;
  private buffer: WebGLBuffer;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl", {
      antialias: false,
      alpha: true,
      premultipliedAlpha: false,
    });
    if (!gl) throw new Error("WebGL unavailable");
    this.gl = gl;
    this.prog = program(gl,
      `attribute vec2 a_pos; void main(){ gl_Position = vec4(a_pos,0.0,1.0); }`,
      `precision mediump float; uniform vec3 u_color; void main(){ gl_FragColor = vec4(u_color,1.0); }`,
    );
    this.pos = gl.getAttribLocation(this.prog, "a_pos");
    const loc = gl.getUniformLocation(this.prog, "u_color");
    if (!loc) throw new Error("u_color missing");
    this.color = loc;
    const buffer = gl.createBuffer();
    if (!buffer) throw new Error("createBuffer failed");
    this.buffer = buffer;
  }

  setView(view: FrequencyView | null) {
    this.view = checkedView(view);
  }

  clear() {
    const gl = this.gl;
    resizeCanvas(this.canvas);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  draw(frame: SpectrumFrame | null, aps: WifiBss[], hidden: Set<string>, options: SpectrumDisplayOptions, selectedBssid: string | null = null) {
    this.clear();
    if (!frame || !options.show_wifi_overlay) return;

    const gl = this.gl;
    gl.useProgram(this.prog);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(this.pos);
    gl.vertexAttribPointer(this.pos, 2, gl.FLOAT, false, 0, 0);

    const selected = selectedBssid?.toLowerCase() ?? null;
    const visible = aps.filter(ap => !hidden.has(ap.bssid.toLowerCase()));
    for (const ap of visible) {
      if (ap.bssid.toLowerCase() !== selected) this.drawOne(frame, ap, options, false);
    }
    if (selected) {
      const ap = visible.find(ap => ap.bssid.toLowerCase() === selected);
      if (ap) this.drawOne(frame, ap, options, true);
    }
  }

  private y(db: number): number {
    const t = (db - OVERLAY_Y_MIN_DB) / (OVERLAY_Y_MAX_DB - OVERLAY_Y_MIN_DB);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private drawOne(frame: SpectrumFrame, ap: WifiBss, options: SpectrumDisplayOptions, selected: boolean) {
    const half = ap.width_mhz * 1e6 / 2;
    const left = ap.center_freq_hz - half;
    const right = ap.center_freq_hz + half;
    const view = this.view ?? frameView(frame);
    if (right < view.startHz || left > view.stopHz) return;

    const x1 = frequencyX(left, view);
    const x2 = frequencyX(right, view);
    const shoulder = (x2 - x1) * 0.23;
    const rssiSpan = Math.max(1, options.wifi_overlay_rssi_ceiling_dbm - options.wifi_overlay_rssi_floor_dbm);
    const t = Math.max(0, Math.min(1, (ap.rssi_dbm - options.wifi_overlay_rssi_floor_dbm) / rssiSpan));
    const topDb = options.wifi_overlay_floor_db + t * (options.wifi_overlay_ceiling_db - options.wifi_overlay_floor_db);
    const base = this.y(options.wifi_overlay_floor_db);
    const top = this.y(topDb);
    const verts = [x1, base, x1 + shoulder, top, x2 - shoulder, top, x2, base];
    const [r, g, b] = colorForBssid(ap.bssid).map(v => v / 255) as [number, number, number];
    if (selected) {
      this.drawVerts(verts, [1.0, 1.0, 1.0]);
    }
    this.drawVerts(
      verts,
      selected ? [Math.min(1, r * 1.25), Math.min(1, g * 1.25), Math.min(1, b * 1.25)] : [r, g, b],
    );
  }

  private drawVerts(verts: number[], rgb: [number, number, number]) {
    const gl = this.gl;
    gl.uniform3f(this.color, rgb[0], rgb[1], rgb[2]);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.DYNAMIC_DRAW);
    gl.drawArrays(gl.LINE_STRIP, 0, verts.length / 2);
  }
}

export function powerColor(db: number, floor = -105, ceil = -35): RGBA {
  if (!Number.isFinite(db)) return [...WATERFALL_NO_DATA_COLOR];
  const [low, high] = powerLevels(floor, ceil);
  const t = Math.max(0, Math.min(1, (db - low) / (high - low)));
  const stops: [number, number, number][] = t < 0.25 ? [[5,8,30],[25,50,160]] : t < 0.55 ? [[25,50,160],[30,210,220]] : t < 0.80 ? [[30,210,220],[250,220,40]] : [[250,220,40],[255,50,35]];
  const local = t < 0.25 ? t/0.25 : t < 0.55 ? (t-0.25)/0.30 : t < 0.80 ? (t-0.55)/0.25 : (t-0.80)/0.20;
  const r = stops[0][0] + (stops[1][0] - stops[0][0]) * local;
  const g = stops[0][1] + (stops[1][1] - stops[0][1]) * local;
  const b = stops[0][2] + (stops[1][2] - stops[0][2]) * local;
  return [Math.round(r), Math.round(g), Math.round(b), 255];
}

function powerLevels(floor: number, ceil: number): [number, number] {
  const high = Number.isFinite(ceil) ? ceil : -35;
  const low = Number.isFinite(floor) ? floor : -105;
  return [Math.min(low, high - 1), high];
}

interface CachedWaterfallRow {
  key: string;
  frameId: number;
  timeMs: number;
  firstTimeMs: number;
  bucketStartMs: number;
  frames: number;
  band: FrequencyView;
  binWidthHz: number;
  values: Float32Array;
  valid: Uint8Array;
  dc: Uint8Array;
}

interface WaterfallSurface {
  texture: WebGLTexture;
  buffer: WebGLBuffer;
  width: number;
  originMs: number;
}

export class WaterfallRenderer {
  private gl: WebGLRenderingContext;
  private prog: WebGLProgram;
  private rowPosition: number;
  private uv: number;
  private end: WebGLUniformLocation;
  private span: WebGLUniformLocation;
  private xTransform: WebGLUniformLocation;
  private maxWidth: number;
  private rowData = new Uint8Array(0);
  private rows = new WaterfallRows(2047);
  private cached = new Array<CachedWaterfallRow | undefined>(this.rows.capacity);
  private replayRows: CachedWaterfallRow[] = [];
  private frozenRows: CachedWaterfallRow[] = [];
  private replaySelection = "";
  private dirtySlots = new Set<number>();
  private uploadBucket = -Infinity;
  private liveSurface: WaterfallSurface;
  private replaySurface: WaterfallSurface | null = null;
  private replayReady = false;
  private view: FrequencyView | null = null;
  private floor = -105;
  private ceiling = -35;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl", { antialias: false, alpha: false });
    if (!gl) throw new Error("WebGL unavailable");
    this.gl = gl;
    this.prog = program(gl,
      `attribute vec3 a_row;
       attribute vec2 a_uv;
       uniform float u_end, u_span;
       uniform vec2 u_x;
       varying vec2 v_uv;
       varying float v_age;
       void main(){
         v_uv = a_uv;
         v_age = (u_end - a_row.y) / u_span;
         gl_Position = vec4(a_row.x*u_x.x + u_x.y, 1.0 - 2.0*v_age, 0.0, 1.0);
       }`,
      `precision mediump float;
       varying vec2 v_uv;
       varying float v_age;
       uniform sampler2D u_tex;
       void main(){
         if (v_age < 0.0 || v_age > 1.0) discard;
         gl_FragColor = texture2D(u_tex, v_uv);
       }`,
    );
    this.maxWidth = Math.min(4096, gl.getParameter(gl.MAX_TEXTURE_SIZE) as number);
    this.rowPosition = gl.getAttribLocation(this.prog, "a_row");
    this.uv = gl.getAttribLocation(this.prog, "a_uv");
    this.end = gl.getUniformLocation(this.prog, "u_end")!;
    this.span = gl.getUniformLocation(this.prog, "u_span")!;
    this.xTransform = gl.getUniformLocation(this.prog, "u_x")!;
    this.liveSurface = this.createSurface();
  }

  private createSurface(): WaterfallSurface {
    const gl = this.gl;
    const buffer = gl.createBuffer();
    const texture = gl.createTexture();
    if (!buffer || !texture) throw new Error("WebGL allocation failed");
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, this.rows.capacity * 6 * 5 * 4, gl.DYNAMIC_DRAW);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return { texture, buffer, width: 0, originMs: 0 };
  }

  setView(view: FrequencyView | null) {
    this.view = checkedView(view);
  }

  setLevels(floor: number, ceil: number) {
    const [low, high] = powerLevels(floor, ceil);
    if (this.floor === low && this.ceiling === high) return;
    this.floor = low;
    this.ceiling = high;
    this.cached.forEach((row, slot) => {
      if (row) this.uploadColors(this.liveSurface, row, slot);
    });
    if (this.replaySurface && this.replayReady) {
      this.replayRows.forEach((row, slot) => this.uploadColors(this.replaySurface!, row, slot));
    }
  }

  clear() {
    this.rows.clear();
    this.cached.fill(undefined);
    this.dirtySlots.clear();
    this.uploadBucket = -Infinity;
    this.setReplayFrame(null);
    this.clearCanvas();
  }

  private clearCanvas() {
    const gl = this.gl;
    const [r, g, b] = WATERFALL_NO_DATA_COLOR;
    gl.clearColor(r / 255, g / 255, b / 255, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  private capture(frame: SpectrumFrame): CachedWaterfallRow | null {
    const timeMs = frameTimeMs(frame);
    if (frame.partial || !frame.current_db.length || timeMs === null ||
        !checkedView(frameView(frame)) || !Number.isFinite(frame.bin_width_hz) || frame.bin_width_hz <= 0) return null;
    return {
      key: frameRangeKey(frame), frameId: frame.frame_id, timeMs, firstTimeMs: timeMs,
      bucketStartMs: Math.floor(timeMs / WATERFALL_ROW_MS) * WATERFALL_ROW_MS, frames: 1,
      band: frameView(frame), binWidthHz: frame.bin_width_hz,
      values: Float32Array.from(frame.current_db), valid: validBinMask(frame), dc: dcBinMask(frame),
    };
  }

  pushFrame(frame: SpectrumFrame) {
    if (!checkedView(frameView(frame)) || !Number.isFinite(frame.bin_width_hz) || frame.bin_width_hz <= 0) return;
    const added = this.rows.add(frame);
    if (!added) return;
    const row = this.capture(frame)!;
    if (this.rows.count === 1 && added.frames === 1) {
      this.cached.fill(undefined);
      this.dirtySlots.clear();
      this.uploadBucket = -Infinity;
      this.liveSurface.originMs = this.rows.originMs;
    }
    const previous = this.cached[added.slot];
    row.frames = added.frames;
    row.firstTimeMs = added.firstTimeMs;
    for (let i = 0; i < row.values.length; i++) {
      row.valid[i] &&= Number.isFinite(row.values[i]) ? 1 : 0;
      if (added.frames > 1 && previous) {
        if (previous.valid[i]) {
          row.values[i] = row.valid[i] ? Math.max(previous.values[i], row.values[i]) : previous.values[i];
          row.valid[i] = 1;
        }
        row.dc[i] ||= previous.dc[i];
      }
      if (row.valid[i]) row.dc[i] = 0;
    }
    // Replace, never mutate: frozen replay retains independent aggregate versions.
    this.cached[added.slot] = row;
    this.dirtySlots.add(added.slot);
  }

  private observations(replay: boolean): CachedWaterfallRow[] {
    return replay ? this.replayRows : this.cached.filter((row): row is CachedWaterfallRow => !!row);
  }

  setReplayFrame(frame: SpectrumFrame | null) {
    if (!frame) {
      this.replayRows = [];
      this.frozenRows = [];
      this.replaySelection = "";
      this.replayReady = false;
      return;
    }
    const selected = this.capture(frame);
    if (!selected) return;
    const selection = `${selected.key}:${frame.frame_id}`;
    if (selection === this.replaySelection) return;
    if (!this.frozenRows.some(row => row.key === selected.key && row.bucketStartMs === selected.bucketStartMs)) {
      this.frozenRows = this.observations(false).filter(row => row.key === selected.key)
        .sort((a, b) => a.bucketStartMs - b.bucketStartMs);
    }
    const rows = this.frozenRows.filter(row => row.bucketStartMs < selected.bucketStartMs);
    // Never show a peak from after the selected raw frame in its final bucket.
    rows.push(this.frozenRows.find(row => row.bucketStartMs === selected.bucketStartMs &&
      row.frameId === frame.frame_id) ?? selected);
    this.replayRows = rows.slice(-this.rows.capacity);
    this.replaySelection = selection;
    this.replayReady = false;
  }

  retention(replay = false): { oldestMs: number | null; newestMs: number | null; count: number } {
    const rows = this.observations(replay);
    let oldestMs: number | null = null;
    let newestMs: number | null = null;
    for (const row of rows) {
      oldestMs = Math.min(oldestMs ?? row.firstTimeMs, row.firstTimeMs);
      newestMs = Math.max(newestMs ?? row.timeMs, row.timeMs);
    }
    return { oldestMs, newestMs, count: rows.length };
  }

  sampleAt(timeMs: number, freqHz: number, replay = false): { timeMs: number; startMs: number; endMs: number; frames: number; value: number | null; dc: boolean } | null {
    if (!Number.isFinite(timeMs) || !Number.isFinite(freqHz)) return null;
    const row = this.observations(replay).find(row => timeMs >= row.bucketStartMs &&
      timeMs < row.bucketStartMs + WATERFALL_ROW_MS);
    if (!row || freqHz < row.band.startHz || freqHz >= row.band.stopHz) return null;
    const i = Math.floor((freqHz - row.band.startHz) / row.binWidthHz);
    if (i < 0 || i >= row.values.length) return null;
    return {
      timeMs: row.timeMs, startMs: row.bucketStartMs, endMs: row.bucketStartMs + WATERFALL_ROW_MS,
      frames: row.frames,
      value: row.valid[i] && Number.isFinite(row.values[i]) ? row.values[i] : null,
      dc: !!row.dc[i],
    };
  }

  private flushRows(endMs: number, force: boolean) {
    const bucket = Math.floor(endMs / WATERFALL_ROW_MS);
    if (!force && bucket === this.uploadBucket) return;
    for (const slot of this.dirtySlots) {
      const row = this.cached[slot]!;
      this.uploadColors(this.liveSurface, row, slot);
      this.uploadGeometry(this.liveSurface, row, slot);
    }
    this.dirtySlots.clear();
    this.uploadBucket = bucket;
  }

  draw(endMs: number, spanS: number, replay = false, acquiring = true) {
    const gl = this.gl;
    resizeCanvas(this.canvas);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    this.clearCanvas();
    const count = replay ? this.replayRows.length : this.rows.count;
    if (!count || !Number.isFinite(endMs) || !Number.isFinite(spanS) || spanS <= 0) return;
    if (!replay) this.flushRows(endMs, !acquiring);
    let surface = this.liveSurface;
    if (replay) {
      surface = this.replaySurface ??= this.createSurface();
      if (!this.replayReady) {
        surface.originMs = this.replayRows[0].timeMs;
        this.replayRows.forEach((row, slot) => {
          this.uploadColors(surface, row, slot);
          this.uploadGeometry(surface, row, slot);
        });
        this.replayReady = true;
      }
    }
    const first = replay ? 0 : this.rows.oldestSlot;
    const row = replay ? this.replayRows[0] : this.cached[first]!;
    const view = this.view ?? row.band;
    gl.useProgram(this.prog);
    gl.bindTexture(gl.TEXTURE_2D, surface.texture);
    gl.bindBuffer(gl.ARRAY_BUFFER, surface.buffer);
    gl.enableVertexAttribArray(this.rowPosition);
    gl.enableVertexAttribArray(this.uv);
    gl.vertexAttribPointer(this.rowPosition, 3, gl.FLOAT, false, 20, 0);
    gl.vertexAttribPointer(this.uv, 2, gl.FLOAT, false, 20, 12);
    gl.uniform2f(this.xTransform, (row.band.stopHz - row.band.startHz) / (view.stopHz - view.startHz),
      frequencyX((row.band.startHz + row.band.stopHz) / 2, view));
    gl.uniform1f(this.end, (endMs - surface.originMs) / 1000);
    gl.uniform1f(this.span, spanS);
    gl.disable(gl.BLEND);
    gl.drawArrays(gl.TRIANGLES, first * 6, (count - first) * 6);
    if (first > 0) gl.drawArrays(gl.TRIANGLES, 0, first * 6);
  }

  private uploadColors(surface: WaterfallSurface, row: CachedWaterfallRow, slot: number) {
    const gl = this.gl;
    const width = Math.min(this.maxWidth, row.values.length);
    gl.bindTexture(gl.TEXTURE_2D, surface.texture);
    if (surface.width !== width) {
      surface.width = width;
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, this.rows.capacity, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    }
    if (this.rowData.length !== width * 4) this.rowData = new Uint8Array(width * 4);
    for (let x = 0; x < width; x++) {
      const start = Math.floor(x * row.values.length / width);
      const end = Math.floor((x + 1) * row.values.length / width);
      let max = -Infinity;
      let dc = false;
      for (let i = start; i < end; i++) {
        dc ||= !!row.dc[i];
        if (row.valid[i] && Number.isFinite(row.values[i])) max = Math.max(max, row.values[i]);
      }
      // Never paint measured power over a known DC exclusion, even when compressed.
      this.rowData.set(dc ? WATERFALL_DC_COLOR : Number.isFinite(max)
        ? powerColor(max, this.floor, this.ceiling) : WATERFALL_NO_DATA_COLOR, x * 4);
    }
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, slot, width, 1, gl.RGBA, gl.UNSIGNED_BYTE, this.rowData);
  }

  private uploadGeometry(surface: WaterfallSurface, row: CachedWaterfallRow, slot: number) {
    const gl = this.gl;
    const startS = (row.bucketStartMs - surface.originMs) / 1000;
    const endS = startS + WATERFALL_ROW_MS / 1000;
    const v = (slot + 0.5) / this.rows.capacity;
    const dataSpan = row.values.length * row.binWidthHz;
    const rightHz = Math.min(row.band.stopHz, row.band.startHz + dataSpan);
    const right = frequencyX(rightHz, row.band);
    const u = (rightHz - row.band.startHz) / dataSpan;
    const vertices = new Float32Array([
      -1,startS,0,0,v, right,startS,0,u,v, -1,endS,0,0,v,
      -1,endS,0,0,v, right,startS,0,u,v, right,endS,0,u,v,
    ]);
    gl.bindBuffer(gl.ARRAY_BUFFER, surface.buffer);
    gl.bufferSubData(gl.ARRAY_BUFFER, slot * vertices.byteLength, vertices);
  }
}
