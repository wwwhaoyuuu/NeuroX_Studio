import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourcePath = path.join(root, "src", "waveform.js");
const { WaveformRenderer, channelPageState, waveformAmplitudeStats, visibleVoltageRangeUv, waveformChannelSelection, waveformColumns, waveformSampleCount, normalizeWaveformRange, waveformVoltageY, waveformPanes, waveformPointerChannel } = await import(pathToFileURL(sourcePath));

const smallSurface = {
  canvas: { parentElement: { getBoundingClientRect: () => ({ width: 180, height: 120 }) }, style: {} },
  pixelRatio: 2, context: { setTransform() {} }, draw() {}
};
WaveformRenderer.prototype.resize.call(smallSurface);
assert.equal(smallSurface.canvas.width, 360);
assert.equal(smallSurface.canvas.height, 240);
assert.equal(smallSurface.canvas.style.height, "120px");

assert.deepEqual(channelPageState(16, 0), {
  totalChannels: 16,
  pageIndex: 0,
  pageCount: 1,
  pageSize: 16,
  start: 0,
  end: 16,
  visibleCount: 16
});

assert.deepEqual(
  [0, 1, 2, 3].map((page) => channelPageState(64, page)).map(({ start, end }) => [start, end]),
  [[0, 16], [16, 32], [32, 48], [48, 64]]
);
assert.equal(channelPageState(64, 99).pageIndex, 3);
assert.equal(channelPageState(64, -4).pageIndex, 0);

const final120Page = channelPageState(120, 7);
assert.equal(final120Page.pageCount, 8);
assert.equal(final120Page.start, 112);
assert.equal(final120Page.end, 120);
assert.equal(final120Page.visibleCount, 8);

const frame = {
  channelCount: 2,
  pointCount: 2,
  minimums: [32758, 32768, 32748, 0],
  maximums: [32778, 32768, 32788, 65535]
};
const first = waveformAmplitudeStats([frame], 0);
assert.ok(Math.abs(first.minimumUv + 3.9) < 1e-9);
assert.ok(Math.abs(first.maximumUv - 3.9) < 1e-9);
assert.ok(Math.abs(first.peakToPeakUv - 7.8) < 1e-9);
assert.equal(first.touchesRail, false);
assert.equal(waveformAmplitudeStats([frame], 1).touchesRail, true);
assert.equal(waveformAmplitudeStats([frame], 2), null);
assert.ok(visibleVoltageRangeUv(0.05) > 6_390);
assert.ok(visibleVoltageRangeUv(4) < visibleVoltageRangeUv(1));

for (const total of [16, 64, 120]) {
  assert.deepEqual(waveformChannelSelection(total, 0, 16, total - 1), [total - 1]);
  assert.deepEqual(waveformChannelSelection(total, 0, 16, 999), [total - 1]);
}
assert.deepEqual(waveformChannelSelection(64, 3, 16, 9), [9]);
assert.deepEqual(waveformChannelSelection(64, 2, 16), Array.from({ length: 16 }, (_, index) => index + 32));
const visibility = Array.from({ length: 64 }, (_, index) => index !== 9);
const originalFrames = [{ source: "unchanged" }];
const focusSurface = {
  channelCount: 64, pageIndex: 2, pageSize: 16, focusChannel: null,
  gain: 0.5, visibleChannels: visibility, frames: originalFrames, buildLabels() {}, draw() {},
  displayChannels: WaveformRenderer.prototype.displayChannels,
  panes: WaveformRenderer.prototype.panes, columns: 1, labelWidth: 102
};
WaveformRenderer.prototype.setFocusChannel.call(focusSurface, 9);
assert.deepEqual(focusSurface.displayChannels(), [9]);
assert.equal(focusSurface.pageIndex, 2);
assert.equal(focusSurface.gain, 0.5);
assert.equal(focusSurface.frames, originalFrames);
assert.equal(focusSurface.visibleChannels, visibility);
assert.equal(visibility[9], false, "zoom must not change multi-channel visibility");
WaveformRenderer.prototype.setFocusChannel.call(focusSurface, null);
assert.equal(focusSurface.displayChannels()[0], 32, "return to the original page");

const fitFrame = { channelCount: 64, pointCount: 1, minimums: Array(64).fill(12768), maximums: Array(64).fill(52768) };
fitFrame.minimums[9] = 32748; fitFrame.maximums[9] = 32788;
focusSurface.pageIndex = 0; focusSurface.displayFrames = () => [fitFrame];
const multiGain = WaveformRenderer.prototype.fitGain.call(focusSurface);
focusSurface.focusChannel = 9;
assert.ok(WaveformRenderer.prototype.fitGain.call(focusSurface) > multiGain, "fit uses only the focused channel, even when hidden in overview");

const plotFrame = { channelCount: 64, pointCount: 2, sampleRateHz: 30000, firstSampleIndex: 10000,
  binWidthSamples: 1, minimums: Array(128).fill(32768), maximums: Array(128).fill(32768) };
plotFrame.minimums[9] = 31768; plotFrame.maximums[9] = 33768;
const samplesBefore = JSON.stringify(plotFrame);
const strokes = [];
const ctx = { fillRect() {}, beginPath() { this.points = []; }, moveTo(x, y) { this.points.push([x, y]); }, lineTo(x, y) { this.points.push([x, y]); }, stroke() { strokes.push(this.points); } };
const drawing = { ...focusSurface, canvas: { width: 800, height: 400 }, pixelRatio: 1, context: ctx,
  gain: 1, frame: plotFrame, channelColors: [], timeWindowMs: 100,
  visibleChannels: Array.from({ length: 64 }, (_, index) => index === 9), focusChannel: null,
  displayFrames: () => [plotFrame], drawGrid() {}, drawFocusGrid() {} };
WaveformRenderer.prototype.draw.call(drawing);
assert.equal(strokes.length, 1);
const smallHeight = Math.abs(strokes[0][0][1] - strokes[0][1][1]);
drawing.focusChannel = 9; strokes.length = 0;
WaveformRenderer.prototype.draw.call(drawing);
assert.equal(strokes.length, 1, "single-channel mode draws only one trace");
assert.ok(Math.abs(strokes[0][0][1] - strokes[0][1][1]) > smallHeight * 12, "single trace occupies the full plot height");
assert.ok(strokes[0].every(([x]) => x >= 167 && x <= 785), "focused time axis stays in plot bounds");
assert.equal(JSON.stringify(plotFrame), samplesBefore, "zoom never rewrites sample data");
const frameSurface = { paused: true, frame: plotFrame, frames: [plotFrame] };
WaveformRenderer.prototype.setFrame.call(frameSurface, { ...plotFrame, sequence: 100 });
assert.equal(frameSurface.frame, plotFrame, "display pause retains the last view");

const entered = [];
const clickSurface = { channelCount: 64, pageIndex: 2, pageSize: 16, focusChannel: null,
  canvas: { getBoundingClientRect: () => ({ left: 0, top: 100, width: 800, height: 320 }) },
  panes: WaveformRenderer.prototype.panes, columns: 1, labelWidth: 102,
  displayChannels: WaveformRenderer.prototype.displayChannels, onFocusChannel: (channel) => entered.push(channel) };
for (const clientY of [100, 119, 120, 419, 99, 420]) WaveformRenderer.prototype.focusAtPointer.call(clickSurface, { clientX: 300, clientY });
assert.deepEqual(entered, [32, 32, 33, 47], "single click enters the correct lane on the current page; outside clicks are ignored");
clickSurface.focusChannel = 40;
WaveformRenderer.prototype.focusAtPointer.call(clickSurface, { clientY: 200 });
assert.equal(entered.length, 4, "clicking the detailed trace must not leave the channel");

const previousDocument = globalThis.document;
function element() {
  return { style: { setProperty() {} }, dataset: {}, children: [], events: {}, classList: { toggle() {} },
    setAttribute() {}, append(...children) { this.children.push(...children); },
    appendChild(child) { this.children.push(child); }, replaceChildren() { this.children = []; },
    addEventListener(event, handler) { this.events[event] = handler; } };
}
try {
  globalThis.document = { createElement: element };
  const labels = element();
  const surface = { ...clickSurface, focusChannel: null, labels, channelNames: [], channelColors: [],
    visibleChannels: Array(64).fill(true), pageState: WaveformRenderer.prototype.pageState, draw() {} };
  WaveformRenderer.prototype.buildLabels.call(surface);
  const [channelButton, showCheckbox] = labels.children[9].children;
  channelButton.events.click();
  assert.equal(entered.at(-1), 41, "label single click opens channel details");
  assert.equal(surface.visibleChannels[41], true, "entering details never hides a channel");
  showCheckbox.checked = false; showCheckbox.events.change();
  assert.equal(surface.visibleChannels[41], false, "visibility remains a separate explicit control");
  assert.equal(entered.length, 5, "visibility control does not enter details");
} finally { globalThis.document = previousDocument; }

const timedFrame = (firstSampleIndex, values, options = {}) => ({ firstSampleIndex, sampleCount: values.length,
  sampleRateHz: 1000, binWidthSamples: 1, channelCount: 1, pointCount: values.length, flags: 0,
  minimums: values, maximums: values, firstValues: values, lastValues: values, ...options });
const contiguous = [timedFrame(0, [32760, 32770]), timedFrame(2, [32780, 32790])];
const connected = waveformColumns(contiguous, 0, 0, 4, 40);
assert.equal(connected.length, 1, "adjacent blocks are one continuous segment");
assert.equal(connected[0].length, 4);
assert.equal(connected[0][2].first, 32780, "connectors use real endpoints, not the envelope midpoint");
const gap = waveformColumns([contiguous[0], timedFrame(4, [32780, 32790])], 0, 0, 6, 60);
assert.equal(gap.length, 2, "missing samples stay unconnected");
assert.equal(gap[1][0].column, 40, "x spacing follows source samples, not concatenated point count");
assert.equal(waveformColumns([contiguous[0], { ...contiguous[1], flags: 1 }], 0, 0, 4, 40).length, 2);
const compressed = waveformColumns(contiguous, 0, 0, 4, 1);
assert.equal(compressed[0].length, 1, "dense points merge to one envelope per pixel");
assert.equal(compressed[0][0].minimum, 32760);
assert.equal(compressed[0][0].maximum, 32790, "pixel merging must retain narrow extrema");
const partial = timedFrame(0, [32000, 33000], { sampleCount: 3, binWidthSamples: 2 });
assert.equal(waveformSampleCount(partial), 3);
assert.equal(waveformColumns([partial, timedFrame(3, [32500])], 0, 0, 4, 40).length, 1, "partial final bin must not create a false gap");
const timedSurface = { frame: timedFrame(1000, [32768]), frames: [timedFrame(0, [32768]), timedFrame(1000, [32768])], timeWindowMs: 100 };
assert.equal(WaveformRenderer.prototype.displayFrames.call(timedSurface).length, 1, "a time window must not stretch over missing history");

const manualRange = { minimumUv: -20, maximumUv: 20 };
assert.deepEqual(normalizeWaveformRange(manualRange), manualRange);
assert.deepEqual(normalizeWaveformRange({ minimumUv: 0.2, maximumUv: 0.3 }), { minimumUv: 0.2, maximumUv: 0.3 });
for (const range of [null, {}, { minimumUv: 20, maximumUv: -20 }, { minimumUv: 0, maximumUv: 0 },
  { minimumUv: NaN, maximumUv: 20 }, { minimumUv: -20, maximumUv: Infinity },
  { minimumUv: -1_000_001, maximumUv: 20 }, { minimumUv: -20, maximumUv: 1_000_001 }]) assert.equal(normalizeWaveformRange(range), null);
const plot = { top: 30, height: 400 };
assert.equal(waveformVoltageY(20, plot, manualRange), 50);
assert.equal(waveformVoltageY(0, plot, manualRange), 230);
assert.equal(waveformVoltageY(-20, plot, manualRange), 410);
assert.equal(waveformVoltageY(-100, plot, manualRange), 410, "manual range clips only the rendering coordinate");
assert.equal(waveformVoltageY(0, plot, { minimumUv: 0, maximumUv: 80 }), 410, "asymmetric ranges preserve absolute voltage");
const manualSurface = { focusChannel: 9, gain: 0.5, focusRange: manualRange };
assert.deepEqual(WaveformRenderer.prototype.voltageRange.call(manualSurface), manualRange);
manualSurface.focusChannel = null;
assert.ok(WaveformRenderer.prototype.voltageRange.call(manualSurface).maximumUv > 20, "overview gain remains independent");
drawing.focusRange = manualRange;
const clipping = [];
ctx.save = () => clipping.push("save");
ctx.rect = (...bounds) => clipping.push(bounds);
ctx.clip = () => clipping.push("clip");
ctx.restore = () => clipping.push("restore");
strokes.length = 0;
WaveformRenderer.prototype.draw.call(drawing);
assert.deepEqual(clipping, ["save", [167, 47, 618, 306], "clip", "restore"], "manual range clips the path at the voltage limits");
assert.ok(strokes[0].some(([, y]) => y < 47 || y > 353), "out-of-range vertices are not flattened into artificial plateaus");
assert.equal(JSON.stringify(plotFrame), samplesBefore, "manual ranges never rewrite samples");

const all64 = waveformChannelSelection(64, 0, 64);
const panes = waveformPanes(all64, 1200, 640, 2);
assert.deepEqual(panes.map(({ channels }) => [channels[0], channels.at(-1)]), [[0, 31], [32, 63]]);
assert.equal(waveformPointerChannel(panes, 300, 0), 0);
assert.equal(waveformPointerChannel(panes, 900, 0), 32);
assert.equal(waveformPointerChannel(panes, 900, 639), 63);
assert.equal(waveformPointerChannel(panes, 600, 100), null, "label gutter cannot select a trace");
assert.equal(waveformPointerChannel(panes, 1200, 100), null);
assert.deepEqual(channelPageState(120, 1, 64).visibleCount, 56);
const lastPanes = waveformPanes(waveformChannelSelection(120, 1, 64), 1200, 640, 2);
assert.equal(lastPanes.flatMap((pane) => pane.channels).length, 56);
assert.equal(waveformPanes(waveformChannelSelection(16, 0, 64), 1200, 640, 2).length, 1);
const ranged = { focusChannel: null, gain: 1, channelRanges: [manualRange, { minimumUv: 10, maximumUv: 50 }] };
assert.deepEqual(WaveformRenderer.prototype.voltageRange.call(ranged, 0), manualRange);
assert.deepEqual(WaveformRenderer.prototype.voltageRange.call(ranged, 1), { minimumUv: 10, maximumUv: 50 });
assert.ok(WaveformRenderer.prototype.voltageRange.call(ranged, 2).maximumUv > 20);
drawing.focusChannel = null; drawing.columns = 2; drawing.pageSize = 64; drawing.channelRanges = Array(64).fill(null);
drawing.channelRanges[9] = manualRange;
strokes.length = 0; clipping.length = 0;
WaveformRenderer.prototype.draw.call(drawing);
assert.equal(strokes.length, 1);
assert.deepEqual(clipping, ["save", [102, 112.5 + 0.625, 298, 11.25], "clip", "restore"], "overview uses a per-channel range within its own column and lane");
assert.equal(JSON.stringify(plotFrame), samplesBefore);
console.log("PASS: dual 32+32 lanes, physical selection, independent overview/focus ranges, clipping and continuous pixel envelopes");
