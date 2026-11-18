import { DisplayClock, FrameFreshness, dcBinMask, finiteBinRanges, frameTimeMs, PeakEnvelope, validBinMask, WaterfallRows } from "./display-timing";
import type { SpectrumFrame } from "./renderers";

function assert(condition: boolean, message = "assertion failed"): asserts condition {
  if (!condition) throw new Error(message);
}

function equal(actual: unknown, expected: unknown) {
  assert(JSON.stringify(actual) === JSON.stringify(expected), `${JSON.stringify(actual)} !== ${JSON.stringify(expected)}`);
}

function near(actual: number, expected: number) {
  assert(Math.abs(actual - expected) < 1e-8, `${actual} != ${expected}`);
}

let passed = 0;
function test(name: string, body: () => void) {
  body();
  passed++;
  console.log(`ok ${passed} - ${name}`);
}

function frame(overrides: Partial<SpectrumFrame> = {}): SpectrumFrame {
  return {
    capture_epoch: 1, frame_id: 1, frame_revision: 1, partial: false,
    sweep_start_time_ns: 999_000_000_000, sweep_end_time_ns: 1_000_000_000_000,
    band_start_hz: 2.4e9, band_stop_hz: 2.5e9, bin_width_hz: 1e6,
    current_db: [-30, -40, -50], average_db: [-60, -70, -80], max_hold_db: [-10, -20, -30],
    valid_bin_ranges: [[0, 3]], invalid_bin_count: 0, estimated_hops: 1, completed_hops: 1,
    ...overrides,
  };
}

function fallingEnvelope(): [PeakEnvelope, SpectrumFrame] {
  const peaks = new PeakEnvelope();
  peaks.update(frame(), 0, true, 4);
  const low = frame({ frame_id: 2, current_db: [-80, -80, -80] });
  peaks.update(low, 0, true, 4);
  return [peaks, low];
}

test("stale running snapshots and duplicate frames cannot keep peaks animating", () => {
  const fresh = new FrameFreshness();
  const measured = frame();
  fresh.observe(measured, 0, 1_000_000);
  assert(fresh.isFresh(1000));
  fresh.observe(measured, 1900, 1_001_900);
  assert(!fresh.isFresh(2000));
  fresh.observe(frame({ frame_revision: 2 }), 3000, 1_003_000);
  assert(!fresh.isFresh(3000));
  fresh.observe(frame({ frame_revision: 3, sweep_end_time_ns: 1_003_000_000_000 }), 3000, 1_003_000);
  assert(fresh.isFresh(3000));
  fresh.reset();
  assert(!fresh.isFresh(3000));
});

test("delayed frames and invalid-bin updates are not fresh acquisition", () => {
  const fresh = new FrameFreshness();
  fresh.observe(frame(), 0, 1_010_000);
  assert(!fresh.isFresh(0));
  fresh.observe(frame({ frame_revision: 2, sweep_end_time_ns: 1_010_000_000_000, valid_bin_ranges: [] }), 10, 1_010_000);
  assert(!fresh.isFresh(10));
});

test("fall follows elapsed seconds, not animation or sweep count", () => {
  const [many, low] = fallingEnvelope();
  const [once] = fallingEnvelope();
  for (let ms = 10; ms <= 2000; ms += 10) many.update(low, ms, true, 4);
  once.update(low, 2000, true, 4);
  many.values.forEach((value, i) => near(value, once.values[i]));
  near(once.values[0], -38);
  once.update(low, 100000, true, 4);
  equal(once.values, low.current_db);
});

test("attack is immediate and duplicate revisions do not re-attack", () => {
  const [peaks, low] = fallingEnvelope();
  peaks.update(low, 1000, true, 4);
  near(peaks.values[0], -34);
  const higher = frame({ frame_id: 3, current_db: [-15, -90, -90] });
  peaks.update(higher, 1500, true, 4);
  near(peaks.values[0], -15);
  near(peaks.values[1], -46);
  peaks.update(higher, 2000, true, 4);
  near(peaks.values[0], -15);
  const burst = frame({ frame_id: 4, current_db: [-5, -10, -20] });
  const tail = frame({ frame_id: 5, current_db: [-90, -90, -90] });
  assert(peaks.observe(burst) && peaks.observe(tail));
  equal(peaks.values, burst.current_db);
  peaks.update(tail, 3000, true, 4);
  equal(peaks.values, [-9, -14, -24]);
  assert(!peaks.observe(tail) && !peaks.observe(burst));
  equal(peaks.values, [-9, -14, -24]);
  for (const partial of [false, true]) {
    const paged = new PeakEnvelope();
    for (let id = 1; id <= 16; id++) paged.observe(frame({ frame_id: id, current_db: [-90, -90, -90] }));
    const ahead = frame({ frame_id: 64, frame_revision: 64, partial, current_db: [-90, -90, -90] });
    paged.update(ahead, 0, true, 4);
    const transient = frame({ frame_id: 17, frame_revision: 17, current_db: [-4, -8, -12] });
    assert(paged.observe(transient));
    for (let id = 18; id <= 32; id++) paged.observe(frame({ frame_id: id, current_db: [-90, -90, -90] }));
    paged.update(ahead, 1000, true, 4);
    equal(paged.values, [-8, -12, -16]);
    assert(!paged.observe(transient) && !paged.observe({ ...transient, frame_revision: 65 }));
    assert(!paged.observe({ ...ahead, partial: true, frame_revision: 63 }));
    paged.update(ahead, 100000, true, 4);
    equal(paged.values, [-90, -90, -90]);
  }
});

test("uses current power and only latest valid bins, never historical maxima", () => {
  const peaks = new PeakEnvelope();
  peaks.update(frame({ valid_bin_ranges: [[1, 2]] }), 0, true, 3);
  assert(Number.isNaN(peaks.values[0]));
  equal(peaks.values[1], -40);
  assert(Number.isNaN(peaks.values[2]));
  const partial = frame({ frame_id: 2, partial: true, valid_bin_ranges: [[0, 1]], current_db: [-25, -5, -5] });
  peaks.update(partial, 1000, true, 3);
  near(peaks.values[0], -25);
  near(peaks.values[1], -40);
  assert(Number.isNaN(peaks.values[2]));
});

test("stopped and hidden intervals do not decay, resuming does not catch up peaks", () => {
  const [peaks, low] = fallingEnvelope();
  peaks.update(low, 1000, true, 4);
  const frozen = [...peaks.values];
  peaks.update(low, 30000, false, 4);
  peaks.update(low, 60000, false, 4);
  equal(peaks.values, frozen);
  peaks.update(low, 90000, true, 4);
  equal(peaks.values, frozen);
  peaks.update(low, 91000, true, 4);
  near(peaks.values[0], frozen[0] - 4);
  peaks.pause();
  peaks.update(low, 200000, true, 4);
  near(peaks.values[0], frozen[0] - 4);
});

test("zero rate holds peaks; rate changes use the new dB/s", () => {
  const [peaks, low] = fallingEnvelope();
  peaks.update(low, 10000, true, 0);
  near(peaks.values[0], -30);
  peaks.update(low, 11000, true, 12);
  near(peaks.values[0], -42);
  peaks.update(low, 10500, true, 12);
  near(peaks.values[0], -42);
});

test("epoch, frequency range, bin width and bin count reset envelopes", () => {
  for (const change of [{ capture_epoch: 2 }, { band_stop_hz: 2.6e9 }, { bin_width_hz: 2e6 }, { current_db: [-90, -95] }]) {
    const [peaks] = fallingEnvelope();
    const next = frame({ current_db: [-90, -95, -100], ...change });
    peaks.update(next, 500, true, 3);
    equal(peaks.values, next.current_db);
  }
});

test("replay reset is independent of the recorded frame arrays", () => {
  const measurement = frame();
  const before = JSON.stringify(measurement);
  Object.freeze(measurement.current_db);
  Object.freeze(measurement.average_db);
  Object.freeze(measurement.max_hold_db);
  const peaks = new PeakEnvelope();
  peaks.update(measurement, 0, true, 4);
  peaks.reset();
  equal(peaks.values, []);
  peaks.update(frame({ current_db: [-90, -95, -100] }), 100000, false, 4);
  equal(peaks.values, [-90, -95, -100]);
  equal(JSON.stringify(measurement), before);
});

test("empty valid ranges and nonfinite bins remain gaps", () => {
  equal([...validBinMask(frame({ valid_bin_ranges: [] }))], [0, 0, 0]);
  equal([...validBinMask(frame({ valid_bin_ranges: [[-2, 1], [2, 9]] }))], [1, 0, 1]);
  equal(finiteBinRanges([NaN, -50, -60, NaN, -70, Infinity]), [[1, 3], [4, 5]]);
  const peaks = new PeakEnvelope();
  peaks.update(frame({ current_db: [NaN, -40, Infinity] }), 0, true, 3);
  equal(finiteBinRanges(peaks.values), [[1, 2]]);
  const masked = frame({ valid_bin_ranges: [[1, 3]], history_valid_bin_ranges: [[0, 3]], dc_excluded_bin_ranges: [[1, 2]] });
  equal([...validBinMask(masked)], [0, 0, 1]);
  equal([...validBinMask(masked, true)], [1, 1, 1]);
  equal([...dcBinMask(masked)], [0, 1, 0]);
  equal([...validBinMask({ ...masked, history_valid_bin_ranges: undefined }, true)], [0, 0, 1]);
  equal([...validBinMask({ ...masked, history_valid_bin_ranges: [] }, true)], [0, 0, 0]);
  peaks.reset();
  peaks.observe(masked);
  equal(finiteBinRanges(peaks.values), [[2, 3]]);
});

test("display clock scrolls monotonically and freezes without acquisition", () => {
  const clock = new DisplayClock();
  equal(clock.tick(0, 1_000_000, true), 1_000_000);
  equal(clock.tick(250, 900_000, true), 1_000_250);
  equal(clock.tick(1000, 9_000_000, true), 1_001_000);
  equal(clock.tick(10000, 9_010_000, false), 1_001_000);
  equal(clock.tick(20000, 9_020_000, false), 1_001_000);
});

test("visibility resume leaves elapsed-time gaps; stop can show late real frames", () => {
  const clock = new DisplayClock();
  clock.tick(0, 1_000_000, true);
  clock.pause();
  equal(clock.tick(30000, 1_030_000, true), 1_030_000);
  clock.pause();
  clock.observe(1_031_000);
  equal(clock.tick(32000, 1_032_000, false), 1_031_000);
  clock.reset();
  equal(clock.tick(33000, 1_033_000, false), 1_033_000);
});

test("waterfall groups occupied 100 ms intervals without filling time gaps", () => {
  const rows = new WaterfallRows(8);
  const first = rows.add(frame())!;
  for (let id = 2; id <= 6; id++) {
    const merged = rows.add(frame({ frame_id: id, sweep_end_time_ns: (1_000_000 + (id - 1) * 17) * 1e6 }))!;
    equal(merged.slot, first.slot);
    equal(merged.frames, id);
  }
  equal(rows.count, 1);
  equal(first.frames, 1);
  const last = rows.add(frame({ frame_id: 10, sweep_end_time_ns: 1_010_000_000_000 }))!;
  equal(first.timeMs, 1_000_000);
  equal(last.timeMs, 1_010_000);
  equal(rows.count, 2);
  equal(rows.visible(1_005_000, 1), []);
  equal(rows.visible(1_010_000, 60).length, 2);
  equal(rows.visible(1_010_000, 5).length, 1);
  equal(rows.visible(1_070_100, 60).length, 0);
});

test("partial, duplicate, older and untimestamped frames never invent rows", () => {
  const rows = new WaterfallRows(8);
  assert(rows.add(frame({ partial: true })) === null);
  assert(rows.add(frame({ sweep_end_time_ns: 0 })) === null);
  assert(rows.add(frame({ sweep_end_time_ns: NaN })) === null);
  assert(rows.add(frame()) !== null);
  assert(rows.add(frame({ frame_revision: 2 })) === null);
  assert(rows.add(frame({ frame_id: 0 })) === null);
  equal(rows.count, 1);
  equal(frameTimeMs(frame({ sweep_end_time_ns: 1_000_123_456_789 })), 1_000_123.456789);
});

test("ring wraps without moving existing rows and resets on range changes", () => {
  const rows = new WaterfallRows(3);
  const slots = [];
  for (let id = 1; id <= 5; id++) {
    slots.push(rows.add(frame({ frame_id: id, sweep_end_time_ns: (1000 + id) * 1e9 }))!.slot);
  }
  equal(slots, [0, 1, 2, 0, 1]);
  equal(rows.count, 3);
  equal(rows.oldestSlot, 2);
  equal(rows.visible(1_005_000, 60).map(row => row.frameId).sort(), [3, 4, 5]);
  equal(rows.add(frame({ capture_epoch: 2 }))!.slot, 0);
  equal(rows.count, 1);
  equal(rows.originMs, 1_000_000);
  rows.clear();
  equal(rows.count, 0);
  equal(rows.visible(1_005_000, 60), []);
  const fast = new WaterfallRows(2047);
  for (let id = 1; id <= 3600; id++) {
    fast.add(frame({ frame_id: id, sweep_end_time_ns: (1_000_000 + (id - 1) * 1000 / 60) * 1e6 }));
  }
  equal(fast.count, 600);
  equal(fast.visible(1_060_000, 60).length, 600);
});

console.log(`${passed} display timing tests passed`);
