import assert from "node:assert/strict";
import { WaveformRenderer, waveformSpikeMarkers, nearestSpikeMarker } from "../src/waveform.js";

const frame = { spikeEpoch: 7, sampleRateHz: 1000, channelCount: 64,
  firstSampleIndex: 100, sampleCount: 10, flags: 0 };
const event = { id: 1, channelIndex: 63, sampleIndex: 102, peakSampleIndex: 106,
  peakUv: -90, displayPeakUv: -10, sampleRateHz: 1000 };
const options = { epoch: 7, channels: [63], startSample: 100, endSample: 110,
  plot: { left: 20, top: 10, width: 200, height: 100 }, gain: 1,
  focusRange: { minimumUv: -20, maximumUv: 20 } };
const [marker] = waveformSpikeMarkers([frame], [event], options);
assert.equal(marker.x, 140, "markers use the exact peak sample, not threshold crossing time");
assert.ok(Math.abs(marker.y - 82.5) < 1e-9, "marker height uses displayed voltage, not the independent detector voltage");
assert.equal(nearestSpikeMarker([marker], 145, 85), marker);
assert.equal(nearestSpikeMarker([marker], 150, 95), null);
for (const changed of [
  { peakSampleIndex: 99 }, { peakSampleIndex: 110 }, { peakSampleIndex: NaN },
  { displayPeakUv: undefined }, { displayPeakUv: -40 }, { sampleRateHz: 2000 }, { channelIndex: 62 }
]) assert.equal(waveformSpikeMarkers([frame], [{ ...event, ...changed }], options).length, 0);
assert.equal(waveformSpikeMarkers([{ ...frame, spikeEpoch: 6 }], [event], options).length, 0);
assert.equal(waveformSpikeMarkers([{ ...frame, flags: 2 }], [event], options).length, 0);
assert.equal(waveformSpikeMarkers([{ ...frame, sampleCount: 3 }], [event], options).length, 0, "no markers in missing waveform intervals");
const overview = { ...options, channels: [48, 49, 50, 63], focusRange: null };
const [multi] = waveformSpikeMarkers([frame], [event], overview);
assert.ok(multi.y > 85 && multi.y < 110, "channel 63 maps to the correct overview lane");

let draws = 0;
const surface = { paused: false, spikeEpoch: null, spikeCursor: null, frames: [frame], draw() { draws++; } };
const history = { epoch: 7, afterId: 1, events: [{ ...event, waveformUv: [1, 2] }] };
WaveformRenderer.prototype.setSpikeEvents.call(surface, history);
assert.equal(surface.spikeEvents.length, 1);
assert.equal(surface.spikeEvents[0].waveformUv, undefined, "overlay copies only bounded marker metadata");
WaveformRenderer.prototype.setSpikeEvents.call(surface, history);
assert.equal(draws, 1, "unchanged polling must not redraw the plot");
surface.paused = true;
WaveformRenderer.prototype.setSpikeEvents.call(surface, { epoch: 8, afterId: 2, events: [] });
assert.equal(surface.spikeEpoch, 7, "display pause freezes existing markers with the waveform");
surface.paused = false;
WaveformRenderer.prototype.setSpikeEvents.call(surface, { epoch: 8, afterId: 0, events: [] });
assert.equal(surface.spikeEvents.length, 0, "new acquisition/seek/filter epochs clear old candidates");

const tooltip = { hidden: true, style: {} };
const inspector = { spikeTooltip: tooltip, markerPoints: [marker], channelNames: Array(64).fill("C-31"),
  canvas: { getBoundingClientRect: () => ({ left: 10, top: 20, width: 500, height: 200 }) },
  hideSpikeTooltip: WaveformRenderer.prototype.hideSpikeTooltip };
WaveformRenderer.prototype.inspectSpike.call(inspector, { clientX: 150, clientY: 102.5 });
assert.equal(tooltip.hidden, false);
assert.match(tooltip.textContent, /C-31 · 0.106000 s/);
assert.match(tooltip.textContent, /检测峰值 -90.00 µV/);
assert.match(tooltip.textContent, /当前波形 -10.00 µV/);
WaveformRenderer.prototype.inspectSpike.call(inspector, { clientX: 450, clientY: 180 });
assert.equal(tooltip.hidden, true);

const circles = [];
const context = { fillRect() {}, beginPath() {}, moveTo() {}, lineTo() {}, stroke() {},
  arc(x, y, radius) { circles.push({ x, y, radius, color: this.fillStyle }); }, fill() {} };
const renderFrame = { ...frame, pointCount: 10, binWidthSamples: 1,
  minimums: Array(640).fill(32768), maximums: Array(640).fill(32768) };
const renderer = { canvas: { width: 500, height: 300 }, context, pixelRatio: 1,
  frame: renderFrame, spikeEvents: [event], spikeEpoch: 7, showSpikeMarkers: true,
  focusChannel: 63, focusRange: null, timeWindowMs: 100, gain: 1,
  channelColors: [], visibleChannels: Array(64).fill(true),
  panes: WaveformRenderer.prototype.panes, columns: 1, labelWidth: 102,
  displayChannels: () => [63], displayFrames: () => [renderFrame], drawFocusGrid() {}, drawGrid() {} };
WaveformRenderer.prototype.draw.call(renderer);
assert.equal(circles.length, 1);
assert.equal(circles[0].color, "#ff4d5e", "canvas renders actual red dots, not only a legend");
renderer.focusChannel = null; renderer.visibleChannels[63] = false; circles.length = 0;
WaveformRenderer.prototype.draw.call(renderer);
assert.equal(circles.length, 0, "hidden overview channels never leak markers");
renderer.focusChannel = 63; renderer.showSpikeMarkers = false;
WaveformRenderer.prototype.draw.call(renderer);
assert.equal(circles.length, 0, "marker visibility never changes detector state or waveform data");
const dashes = [];
context.setLineDash = (values) => dashes.push(values);
renderer.showSpikeMarkers = true;
renderer.detection = { epoch: 7, config: { enabled: true, bandPass: true }, thresholds: Array.from({ length: 64 }, () => ({ thresholdUv: -50 })) };
renderer.voltageRange = () => ({ minimumUv: -500, maximumUv: 500 });
WaveformRenderer.prototype.draw.call(renderer);
assert.equal(dashes.length, 0, "a separately filtered detector threshold must not be drawn on a different signal");
renderer.detection.config.bandPass = false;
WaveformRenderer.prototype.draw.call(renderer);
assert.deepEqual(dashes, [[5, 4], []], "matching waveform and detector signals can show the actual threshold line");
const dualOptions = { ...options, channels: Array.from({ length: 32 }, (_, index) => index + 32),
  plot: { left: 702, top: 0, width: 498, height: 640 }, rows: 32, focusRange: null,
  channelRanges: Array.from({ length: 64 }, (_, index) => index === 63 ? { minimumUv: -20, maximumUv: 20 } : null) };
const [dualMarker] = waveformSpikeMarkers([frame], [event], dualOptions);
assert.equal(dualMarker.x, 1000.8);
assert.equal(dualMarker.y, 634.5, "right-column markers use the channel's own range and lane");
console.log("PASS: sample-accurate spike overlays, displayed voltages, 64-channel lanes, range/gap/epoch isolation, pause and tooltips");
