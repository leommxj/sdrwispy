import type { SpectrumFrame } from "./renderers";

export function frameRangeKey(frame: SpectrumFrame): string {
  return `${frame.capture_epoch}:${frame.band_start_hz}:${frame.band_stop_hz}:${frame.bin_width_hz}:${frame.current_db.length}`;
}

export function frameTimeMs(frame: SpectrumFrame): number | null {
  const ms = frame.sweep_end_time_ns / 1e6;
  return Number.isFinite(ms) && ms > 0 ? ms : null;
}

function rangeMask(length: number, ranges: [number, number][]): Uint8Array {
  const mask = new Uint8Array(length);
  for (const [start, end] of ranges) {
    if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
      mask.fill(1, Math.max(0, Math.ceil(start)), Math.max(0, Math.min(length, Math.floor(end))));
    }
  }
  return mask;
}

export function dcBinMask(frame: SpectrumFrame): Uint8Array {
  return rangeMask(frame.current_db.length, frame.dc_excluded_bin_ranges ?? []);
}

export function validBinMask(frame: SpectrumFrame, history = false): Uint8Array {
  if (history && frame.history_valid_bin_ranges !== undefined) {
    return rangeMask(frame.current_db.length, frame.history_valid_bin_ranges);
  }
  const mask = rangeMask(frame.current_db.length, frame.valid_bin_ranges);
  for (const [start, end] of frame.dc_excluded_bin_ranges ?? []) {
    if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
      mask.fill(0, Math.max(0, Math.ceil(start)), Math.max(0, Math.min(mask.length, Math.floor(end))));
    }
  }
  return mask;
}

export function finiteBinRanges(values: ArrayLike<number>): [number, number][] {
  const ranges: [number, number][] = [];
  let start = -1;
  for (let i = 0; i <= values.length; i++) {
    if (i < values.length && Number.isFinite(values[i])) {
      if (start < 0) start = i;
    } else if (start >= 0) {
      ranges.push([start, i]);
      start = -1;
    }
  }
  return ranges;
}

// Running snapshots are not acquisition heartbeats. Long gaps conservatively
// freeze peak fall until another timestamped, valid measurement arrives.
export class FrameFreshness {
  private lastMs: number | null = null;
  private revision = "";
  private measuredMs = 0;

  constructor(readonly timeoutMs = 2000) {}

  reset() {
    this.lastMs = null;
    this.revision = "";
    this.measuredMs = 0;
  }

  observe(frame: SpectrumFrame, nowMs: number, wallMs: number) {
    const revision = `${frameRangeKey(frame)}:${frame.frame_id}:${frame.frame_revision}`;
    const measuredMs = frameTimeMs(frame);
    if (revision === this.revision || measuredMs === null || measuredMs <= this.measuredMs) return;
    this.revision = revision;
    this.measuredMs = measuredMs;
    const mask = validBinMask(frame);
    if (!frame.current_db.some((value, i) => !!mask[i] && Number.isFinite(value))) return;
    this.lastMs = nowMs - Math.max(0, wallMs - measuredMs);
  }

  isFresh(nowMs: number): boolean {
    return this.lastMs !== null && nowMs - this.lastMs >= 0 && nowMs - this.lastMs < this.timeoutMs;
  }
}

// Only the display envelope changes. Latest valid measurements remain its floor.
export class PeakEnvelope {
  values: number[] = [];
  private latest: number[] = [];
  private rangeKey = "";
  private observed = new Map<number, { revision: number; partial: boolean }>();
  private completedFrameId = -1;
  private latestFrame = new Float64Array(0);
  private latestRevision = new Float64Array(0);
  private lastMs: number | null = null;

  reset() {
    this.values = [];
    this.latest = [];
    this.rangeKey = "";
    this.observed.clear();
    this.completedFrameId = -1;
    this.latestFrame = new Float64Array(0);
    this.latestRevision = new Float64Array(0);
    this.lastMs = null;
  }

  pause() {
    this.lastMs = null;
  }

  private prepare(frame: SpectrumFrame) {
    const rangeKey = frameRangeKey(frame);
    if (rangeKey !== this.rangeKey) {
      this.reset();
      this.rangeKey = rangeKey;
      this.values = new Array(frame.current_db.length).fill(NaN);
      this.latest = new Array(frame.current_db.length).fill(NaN);
      this.latestFrame = new Float64Array(frame.current_db.length).fill(-1);
      this.latestRevision = new Float64Array(frame.current_db.length).fill(-1);
    }
  }

  // Ingest every delivered measurement, even when several arrive between paints.
  // Observation attacks the envelope; only update() advances its live clock.
  observe(frame: SpectrumFrame): boolean {
    this.prepare(frame);
    if (frame.frame_id <= this.completedFrameId) return false;
    if (!frame.partial) this.completedFrameId = frame.frame_id;
    return this.ingest(frame);
  }

  private ingest(frame: SpectrumFrame): boolean {
    const seen = this.observed.get(frame.frame_id);
    if (seen && (!seen.partial || frame.frame_revision < seen.revision ||
        (frame.frame_revision === seen.revision && frame.partial === seen.partial))) return false;
    this.observed.set(frame.frame_id, { revision: frame.frame_revision, partial: frame.partial });
    if (this.observed.size > 2048) this.observed.delete(this.observed.keys().next().value!);
    const mask = validBinMask(frame);
    for (let i = 0; i < this.values.length; i++) {
      const value = frame.current_db[i];
      if (!mask[i] || !Number.isFinite(value)) continue;
      this.values[i] = Number.isFinite(this.values[i]) ? Math.max(this.values[i], value) : value;
      // A paginated completed frame may arrive after a newer progress snapshot.
      if (frame.frame_id > this.latestFrame[i] ||
          (frame.frame_id === this.latestFrame[i] && frame.frame_revision >= this.latestRevision[i])) {
        this.latest[i] = value;
        this.latestFrame[i] = frame.frame_id;
        this.latestRevision[i] = frame.frame_revision;
      }
    }
    return true;
  }

  update(frame: SpectrumFrame, nowMs: number, acquiring: boolean, decayDbPerSec: number): boolean {
    this.prepare(frame);
    const elapsed = acquiring && this.lastMs !== null ? Math.max(0, nowMs - this.lastMs) / 1000 : 0;
    this.lastMs = acquiring ? nowMs : null;
    const fall = elapsed * Math.max(0, decayDbPerSec);
    let changed = false;
    if (fall > 0) {
      for (let i = 0; i < this.values.length; i++) {
        if (!Number.isFinite(this.values[i])) continue;
        const next = Math.max(this.latest[i], this.values[i] - fall);
        if (next !== this.values[i]) changed = true;
        this.values[i] = next;
      }
    }
    // Latest may be ahead of unread completed pages; it must not advance their cursor.
    const observed = frame.frame_id > this.completedFrameId && this.ingest(frame);
    return observed || changed;
  }
}

// Re-anchor on resume, not on every frame, so system-clock adjustments cannot
// make an active display run backwards. Hidden/stopped periods never decay peaks.
export class DisplayClock {
  private endMs: number | null = null;
  private lastMonoMs: number | null = null;

  reset() {
    this.endMs = null;
    this.lastMonoMs = null;
  }

  pause() {
    this.lastMonoMs = null;
  }

  observe(timeMs: number) {
    if (Number.isFinite(timeMs)) this.endMs = Math.max(this.endMs ?? timeMs, timeMs);
  }

  tick(monotonicMs: number, wallMs: number, acquiring: boolean): number {
    if (this.endMs === null) this.endMs = wallMs;
    if (acquiring) {
      this.endMs = this.lastMonoMs === null ? wallMs : this.endMs + Math.max(0, monotonicMs - this.lastMonoMs);
      this.lastMonoMs = monotonicMs;
    } else {
      this.lastMonoMs = null;
    }
    return this.endMs;
  }
}

export const WATERFALL_ROW_MS = 100;

export interface WaterfallRow {
  slot: number;
  bucketStartMs: number;
  firstTimeMs: number;
  timeMs: number;
  firstFrameId: number;
  frameId: number;
  frames: number;
}

// Only occupied sweep-end time buckets consume a row; empty intervals stay empty.
export class WaterfallRows {
  private rows: (WaterfallRow | undefined)[];
  private next = 0;
  private latestFrameId = -1;
  private rangeKey = "";
  count = 0;
  originMs = 0;

  constructor(readonly capacity: number) {
    this.rows = new Array(capacity);
  }

  clear() {
    this.rows.fill(undefined);
    this.next = 0;
    this.latestFrameId = -1;
    this.rangeKey = "";
    this.count = 0;
    this.originMs = 0;
  }

  add(frame: SpectrumFrame): WaterfallRow | null {
    const timeMs = frameTimeMs(frame);
    if (frame.partial || !frame.current_db.length || timeMs === null) return null;
    const key = frameRangeKey(frame);
    if (key !== this.rangeKey) {
      this.clear();
      this.rangeKey = key;
    }
    if (frame.frame_id <= this.latestFrameId) return null;
    const bucketStartMs = Math.floor(timeMs / WATERFALL_ROW_MS) * WATERFALL_ROW_MS;
    const previous = this.rows[(this.next + this.capacity - 1) % this.capacity];
    const merging = previous?.bucketStartMs === bucketStartMs;
    if (!this.count) this.originMs = bucketStartMs;
    const row: WaterfallRow = merging
      ? { ...previous, timeMs, frameId: frame.frame_id, frames: previous.frames + 1 }
      : { slot: this.next, bucketStartMs, firstTimeMs: timeMs, timeMs,
          firstFrameId: frame.frame_id, frameId: frame.frame_id, frames: 1 };
    this.rows[row.slot] = row;
    if (!merging) {
      this.next = (this.next + 1) % this.capacity;
      this.count = Math.min(this.capacity, this.count + 1);
    }
    this.latestFrameId = frame.frame_id;
    return row;
  }

  get oldestSlot(): number {
    return this.count === this.capacity ? this.next : 0;
  }

  visible(endMs: number, spanS: number): WaterfallRow[] {
    return this.rows.filter((row): row is WaterfallRow => !!row && row.bucketStartMs <= endMs &&
      row.bucketStartMs + WATERFALL_ROW_MS > endMs - spanS * 1000);
  }
}
