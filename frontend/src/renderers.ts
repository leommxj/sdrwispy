import { finiteBinRanges, frameTimeMs, validBinMask, WaterfallRows } from "./display-timing";

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
  wifi_overlay_rssi_floor_dbm: number;
  wifi_overlay_rssi_ceiling_dbm: number;
  wifi_overlay_floor_db: number;
  wifi_overlay_ceiling_db: number;
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

    this.drawGrid();
    if (options.show_current) this.drawTrace(frame.current_db, [0.0, 1.0, 0.35], frame.valid_bin_ranges);
    if (options.show_average) this.drawTrace(frame.average_db, [0.0, 0.78, 1.0]);
    if (options.show_max_hold) this.drawTrace(peakValues, [1.0, 0.55, 0.05], finiteBinRanges(peakValues));
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

  private xIndex(i: number, n: number): number {
    return n <= 1 ? -1 : (i / (n - 1)) * 2 - 1;
  }

  private drawGrid() {
    const verts: number[] = [];
    for (let i = 0; i <= 10; i++) {
      const x = -1 + i * 0.2;
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

  private drawTrace(values: number[], rgb: [number, number, number], validRanges?: [number, number][]) {
    if (!values.length) return;
    // Downsample to roughly one column per physical pixel. The old hard lower
    // bound of 256 columns caused heavy overdraw and visually broken traces in
    // narrow windows.
    const maxCols = Math.max(32, Math.min(Math.floor(this.canvas.width), 4096));
    const ranges = validRanges ?? [[0, values.length] as [number, number]];
    const totalValid = ranges.reduce((sum, [start, end]) => sum + Math.max(0, Math.min(values.length, end) - Math.max(0, start)), 0);
    if (totalValid <= 0) return;
    for (const [rawStart, rawEnd] of ranges) {
      const start = Math.max(0, Math.min(values.length, Math.floor(rawStart)));
      const end = Math.max(start, Math.min(values.length, Math.ceil(rawEnd)));
      const len = end - start;
      if (len === 1) {
        this.drawVerts([this.xIndex(start, values.length), this.y(values[start])], rgb, this.gl.POINTS);
        continue;
      }
      if (len <= 0) continue;
      const rangeCols = Math.max(2, Math.min(len, Math.floor(maxCols * len / totalValid)));
      this.drawTraceRange(values, start, end, rangeCols, rgb);
    }
  }

  private drawTraceRange(values: number[], start: number, end: number, maxCols: number, rgb: [number, number, number]) {
    const verts: number[] = [];
    const len = end - start;
    if (len <= maxCols) {
      for (let i = start; i < end; i++) verts.push(this.xIndex(i, values.length), this.y(values[i]));
      this.drawVerts(verts, rgb, this.gl.LINE_STRIP);
      return;
    }
    const cols = Math.floor(maxCols);
    for (let c = 0; c < cols; c++) {
      const a = start + Math.floor(c * len / cols);
      const b = Math.max(a + 1, start + Math.floor((c + 1) * len / cols));
      let max = -Infinity;
      for (let i = a; i < b && i < end; i++) max = Math.max(max, values[i]);
      verts.push(this.xIndex(a, values.length), this.y(max));
    }
    this.drawVerts(verts, rgb, this.gl.LINE_STRIP);
  }

  private drawVerts(verts: number[], rgb: [number, number, number], mode: number) {
    const gl = this.gl;
    gl.uniform3f(this.color, rgb[0], rgb[1], rgb[2]);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.DYNAMIC_DRAW);
    gl.drawArrays(mode, 0, verts.length / 2);
  }
}

export class WifiOverlayRenderer {
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

  private xFreq(hz: number, frame: SpectrumFrame): number {
    const t = (hz - frame.band_start_hz) / Math.max(1, frame.band_stop_hz - frame.band_start_hz);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private y(db: number): number {
    const t = (db - OVERLAY_Y_MIN_DB) / (OVERLAY_Y_MAX_DB - OVERLAY_Y_MIN_DB);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private drawOne(frame: SpectrumFrame, ap: WifiBss, options: SpectrumDisplayOptions, selected: boolean) {
    const half = ap.width_mhz * 1e6 / 2;
    const left = ap.center_freq_hz - half;
    const right = ap.center_freq_hz + half;
    if (right < frame.band_start_hz || left > frame.band_stop_hz) return;

    const x1 = this.xFreq(Math.max(left, frame.band_start_hz), frame);
    const x2 = this.xFreq(Math.min(right, frame.band_stop_hz), frame);
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

function powerColor(db: number): [number, number, number, number] {
  const t = Math.max(0, Math.min(1, (db + 105) / 70));
  const stops: [number, number, number][] = t < 0.25 ? [[5,8,30],[25,50,160]] : t < 0.55 ? [[25,50,160],[30,210,220]] : t < 0.80 ? [[30,210,220],[250,220,40]] : [[250,220,40],[255,50,35]];
  const local = t < 0.25 ? t/0.25 : t < 0.55 ? (t-0.25)/0.30 : t < 0.80 ? (t-0.55)/0.25 : (t-0.80)/0.20;
  const r = stops[0][0] + (stops[1][0] - stops[0][0]) * local;
  const g = stops[0][1] + (stops[1][1] - stops[0][1]) * local;
  const b = stops[0][2] + (stops[1][2] - stops[0][2]) * local;
  return [r, g, b, 255];
}

export class WaterfallRenderer {
  private gl: WebGLRenderingContext;
  private prog: WebGLProgram;
  private tex: WebGLTexture;
  private rowPosition: number;
  private uv: number;
  private end: WebGLUniformLocation;
  private span: WebGLUniformLocation;
  private pixel: WebGLUniformLocation;
  private halfHeight: WebGLUniformLocation;
  private buffer: WebGLBuffer;
  private width = 0;
  private maxWidth: number;
  private rowData = new Uint8Array(0);
  private rows = new WaterfallRows(2047);
  private replayTimeMs: number | null = null;
  private replayKey = "";

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl", { antialias: false, alpha: false });
    if (!gl) throw new Error("WebGL unavailable");
    this.gl = gl;
    this.prog = program(gl,
      `attribute vec3 a_row;
       attribute vec2 a_uv;
       uniform float u_end, u_span, u_pixel;
       varying vec2 v_uv;
       varying float v_edge, v_age;
       void main(){
         v_uv = a_uv;
         v_edge = a_row.z;
         v_age = (u_end - a_row.y) / u_span;
         gl_Position = vec4(a_row.x, 1.0 - 2.0*v_age + a_row.z*u_pixel, 0.0, 1.0);
       }`,
      `precision mediump float;
       varying vec2 v_uv;
       varying float v_edge, v_age;
       uniform sampler2D u_tex;
       uniform float u_half_height;
       void main(){
         if (v_age < 0.0 || v_age > 1.0) discard;
         vec4 color = texture2D(u_tex, v_uv);
         color.a *= clamp((1.0 - abs(v_edge)) * u_half_height, 0.0, 1.0);
         gl_FragColor = color;
       }`,
    );
    this.maxWidth = Math.min(4096, gl.getParameter(gl.MAX_TEXTURE_SIZE) as number);
    this.rowPosition = gl.getAttribLocation(this.prog, "a_row");
    this.uv = gl.getAttribLocation(this.prog, "a_uv");
    this.end = gl.getUniformLocation(this.prog, "u_end")!;
    this.span = gl.getUniformLocation(this.prog, "u_span")!;
    this.pixel = gl.getUniformLocation(this.prog, "u_pixel")!;
    this.halfHeight = gl.getUniformLocation(this.prog, "u_half_height")!;
    const buffer = gl.createBuffer();
    const tex = gl.createTexture();
    if (!buffer || !tex) throw new Error("WebGL allocation failed");
    this.buffer = buffer;
    this.tex = tex;
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, (this.rows.capacity + 1) * 6 * 5 * 4, gl.DYNAMIC_DRAW);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  clear() {
    this.rows.clear();
    this.replayKey = "";
    this.replayTimeMs = null;
    const gl = this.gl;
    gl.clearColor(0.035, 0.035, 0.035, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  pushFrame(frame: SpectrumFrame) {
    const row = this.rows.add(frame);
    if (row) this.uploadRow(frame, row.slot, (row.timeMs - this.rows.originMs) / 1000);
  }

  setReplayFrame(frame: SpectrumFrame | null) {
    if (!frame) {
      this.replayKey = "";
      this.replayTimeMs = null;
      return;
    }
    const key = `${frame.capture_epoch}:${frame.frame_id}:${frame.frame_revision}`;
    if (key === this.replayKey) return;
    this.replayKey = key;
    this.replayTimeMs = frameTimeMs(frame);
    if (this.replayTimeMs !== null) this.uploadRow(frame, this.rows.capacity, 0);
  }

  draw(endMs: number, spanS: number, replay = false) {
    const gl = this.gl;
    resizeCanvas(this.canvas);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    // Neutral gray is no data; even the lowest measured power is blue.
    gl.clearColor(0.035, 0.035, 0.035, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    const count = replay ? (this.replayTimeMs === null ? 0 : 1) : this.rows.count;
    if (!count || !this.width) return;
    gl.useProgram(this.prog);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(this.rowPosition);
    gl.enableVertexAttribArray(this.uv);
    gl.vertexAttribPointer(this.rowPosition, 3, gl.FLOAT, false, 20, 0);
    gl.vertexAttribPointer(this.uv, 2, gl.FLOAT, false, 20, 12);
    gl.uniform1f(this.end, replay ? 0 : (endMs - this.rows.originMs) / 1000);
    gl.uniform1f(this.span, spanS);
    const halfHeight = (window.devicePixelRatio || 1) + 0.5;
    gl.uniform1f(this.pixel, halfHeight * 2 / this.canvas.height);
    gl.uniform1f(this.halfHeight, halfHeight);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    if (replay) {
      gl.drawArrays(gl.TRIANGLES, this.rows.capacity * 6, 6);
    } else {
      const first = this.rows.oldestSlot;
      gl.drawArrays(gl.TRIANGLES, first * 6, (count - first) * 6);
      if (first > 0) gl.drawArrays(gl.TRIANGLES, 0, first * 6);
    }
    gl.disable(gl.BLEND);
  }

  private uploadRow(frame: SpectrumFrame, slot: number, timeS: number) {
    const gl = this.gl;
    const width = Math.max(1, Math.min(this.maxWidth, frame.current_db.length));
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    if (width !== this.width) {
      this.width = width;
      this.rowData = new Uint8Array(width * 4);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, this.rows.capacity + 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    }
    this.rowData.fill(0);
    const mask = validBinMask(frame);
    for (let x = 0; x < width; x++) {
      const start = Math.floor(x * frame.current_db.length / width);
      const end = Math.max(start + 1, Math.floor((x + 1) * frame.current_db.length / width));
      let max = -Infinity;
      for (let i = start; i < end; i++) {
        if (mask[i] && Number.isFinite(frame.current_db[i])) max = Math.max(max, frame.current_db[i]);
      }
      if (Number.isFinite(max)) this.rowData.set(powerColor(max), x * 4);
    }
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, slot, width, 1, gl.RGBA, gl.UNSIGNED_BYTE, this.rowData);
    const v = (slot + 0.5) / (this.rows.capacity + 1);
    const vertices = new Float32Array([
      -1,timeS,-1,0,v, 1,timeS,-1,1,v, -1,timeS,1,0,v,
      -1,timeS,1,0,v, 1,timeS,-1,1,v, 1,timeS,1,1,v,
    ]);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferSubData(gl.ARRAY_BUFFER, slot * vertices.byteLength, vertices);
  }
}
