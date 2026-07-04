export interface SpectrumFrame {
  capture_epoch: number;
  frame_id: number;
  frame_revision: number;
  partial: boolean;
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

  draw(frame: SpectrumFrame, aps: WifiBss[], hidden: Set<string>, options: SpectrumDisplayOptions, selectedBssid: string | null = null) {
    const gl = this.gl;
    resizeCanvas(this.canvas);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0.005, 0.008, 0.012, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(this.pos);
    gl.vertexAttribPointer(this.pos, 2, gl.FLOAT, false, 0, 0);

    this.drawGrid();
    if (options.show_wifi_overlay) this.drawWifi(frame, aps, hidden, options, selectedBssid);
    if (options.show_current) this.drawTrace(frame.current_db, [0.0, 1.0, 0.35], frame.valid_bin_ranges);
    if (options.show_average) this.drawTrace(frame.average_db, [0.0, 0.78, 1.0]);
    if (options.show_max_hold) this.drawTrace(frame.max_hold_db, [1.0, 0.55, 0.05]);
  }

  private y(db: number): number {
    const t = (db - this.yMin) / (this.yMax - this.yMin);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private xIndex(i: number, n: number): number {
    return n <= 1 ? -1 : (i / (n - 1)) * 2 - 1;
  }

  private xFreq(hz: number, frame: SpectrumFrame): number {
    const t = (hz - frame.band_start_hz) / Math.max(1, frame.band_stop_hz - frame.band_start_hz);
    return Math.max(-1, Math.min(1, t * 2 - 1));
  }

  private drawGrid() {
    const verts: number[] = [];
    for (let i = 0; i <= 10; i++) {
      const x = -1 + i * 0.2;
      verts.push(x, -1, x, 1);
    }
    for (let db = -110; db <= -20; db += 10) {
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
    const ranges = validRanges?.length ? validRanges : [[0, values.length] as [number, number]];
    const totalValid = ranges.reduce((sum, [start, end]) => sum + Math.max(0, Math.min(values.length, end) - Math.max(0, start)), 0);
    if (totalValid <= 0) return;
    for (const [rawStart, rawEnd] of ranges) {
      const start = Math.max(0, Math.min(values.length, Math.floor(rawStart)));
      const end = Math.max(start, Math.min(values.length, Math.ceil(rawEnd)));
      const len = end - start;
      if (len <= 1) continue;
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

  private drawWifi(frame: SpectrumFrame, aps: WifiBss[], hidden: Set<string>, options: SpectrumDisplayOptions, selectedBssid: string | null) {
    const selected = selectedBssid?.toLowerCase() ?? null;
    const visible = aps.filter(ap => !hidden.has(ap.bssid.toLowerCase()));
    for (const ap of visible) {
      if (ap.bssid.toLowerCase() !== selected) this.drawWifiOne(frame, ap, options, false);
    }
    if (selected) {
      const ap = visible.find(ap => ap.bssid.toLowerCase() === selected);
      if (ap) this.drawWifiOne(frame, ap, options, true);
    }
  }

  private drawWifiOne(frame: SpectrumFrame, ap: WifiBss, options: SpectrumDisplayOptions, selected: boolean) {
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
    const top = this.y(topDb);
    const base = this.y(options.wifi_overlay_floor_db);
    const verts = [x1, base, x1 + shoulder, top, x2 - shoulder, top, x2, base];
    const [r,g,b] = colorForBssid(ap.bssid).map(v => v / 255) as [number, number, number];
    if (selected) {
      this.drawVerts(verts, [1.0, 1.0, 1.0], this.gl.LINE_STRIP);
    }
    this.drawVerts(verts, selected ? [Math.min(1, r * 1.25), Math.min(1, g * 1.25), Math.min(1, b * 1.25)] : [r,g,b], this.gl.LINE_STRIP);
  }

  private drawVerts(verts: number[], rgb: [number, number, number], mode: number) {
    const gl = this.gl;
    gl.uniform3f(this.color, rgb[0], rgb[1], rgb[2]);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.DYNAMIC_DRAW);
    gl.drawArrays(mode, 0, verts.length / 2);
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
  private pos: number;
  private buffer: WebGLBuffer;
  private width = 0;
  private height = 0;
  private textureData = new Uint8Array(0);
  private rowData = new Uint8Array(0);
  private quad = new Float32Array([-1,-1, 1,-1, -1,1, 1,1]);

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl", { antialias: false, alpha: false, preserveDrawingBuffer: false });
    if (!gl) throw new Error("WebGL unavailable");
    this.gl = gl;
    this.prog = program(gl,
      `attribute vec2 a_pos; varying vec2 v_uv; void main(){ v_uv=(a_pos+1.0)*0.5; gl_Position=vec4(a_pos,0.0,1.0); }`,
      `precision mediump float;
       varying vec2 v_uv;
       uniform sampler2D u_tex;
       void main(){
         // Texture row 0 is our newest/top display row. WebGL texture t=0 is
         // sampled at the bottom by default, so invert y for screen-space top.
         gl_FragColor = texture2D(u_tex, vec2(v_uv.x, 1.0 - v_uv.y));
       }`,
    );
    this.pos = gl.getAttribLocation(this.prog, "a_pos");
    const buffer = gl.createBuffer();
    const tex = gl.createTexture();
    if (!buffer || !tex) throw new Error("WebGL allocation failed");
    this.buffer = buffer;
    this.tex = tex;

    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.ensureTextureSize();
  }

  clear() {
    const gl = this.gl;
    this.ensureTextureSize();
    this.textureData.fill(0);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, this.width, this.height, gl.RGBA, gl.UNSIGNED_BYTE, this.textureData);
  }

  pushFrame(frame: SpectrumFrame) {
    if (!frame.current_db.length) return;
    this.ensureTextureSize();

    for (let x = 0; x < this.width; x++) {
      const start = Math.floor(x * frame.current_db.length / this.width);
      const end = Math.max(start + 1, Math.floor((x + 1) * frame.current_db.length / this.width));
      let max = -Infinity;
      for (let i = start; i < end && i < frame.current_db.length; i++) max = Math.max(max, frame.current_db[i]);
      const [r,g,b,a] = powerColor(max);
      const o = x * 4;
      this.rowData[o] = r;
      this.rowData[o + 1] = g;
      this.rowData[o + 2] = b;
      this.rowData[o + 3] = a;
    }

    // Row 0 is newest/top. Existing rows are copied down by exactly one
    // texture/screen pixel. Because the texture height follows the canvas
    // backing height, already-rendered rows move down without resampling
    // against a changing circular origin, so their colors do not mutate.
    const stride = this.width * 4;
    this.textureData.copyWithin(stride, 0, stride * (this.height - 1));
    this.textureData.set(this.rowData, 0);

    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, this.width, this.height, gl.RGBA, gl.UNSIGNED_BYTE, this.textureData);
  }

  draw() {
    const gl = this.gl;
    this.ensureTextureSize();
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(0.005,0.008,0.012,1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, this.quad, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(this.pos);
    gl.vertexAttribPointer(this.pos, 2, gl.FLOAT, false, 0, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  private ensureTextureSize() {
    resizeCanvas(this.canvas);
    const nextWidth = Math.max(64, Math.min(4096, this.canvas.width));
    const nextHeight = Math.max(64, Math.min(2048, this.canvas.height));
    if (nextWidth === this.width && nextHeight === this.height) return;

    const oldWidth = this.width;
    const oldHeight = this.height;
    const oldData = this.textureData;
    const nextData = new Uint8Array(nextWidth * nextHeight * 4);
    const copyRows = Math.min(oldHeight, nextHeight);
    const copyColsBytes = Math.min(oldWidth, nextWidth) * 4;
    for (let row = 0; row < copyRows; row++) {
      nextData.set(
        oldData.subarray(row * oldWidth * 4, row * oldWidth * 4 + copyColsBytes),
        row * nextWidth * 4,
      );
    }

    this.width = nextWidth;
    this.height = nextHeight;
    this.textureData = nextData;
    this.rowData = new Uint8Array(this.width * 4);

    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.width, this.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, this.textureData);
  }
}
