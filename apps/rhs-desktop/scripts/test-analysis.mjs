import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = fs.readFileSync(path.join(root, "src", "analysis.js"), "utf8");
const moduleUrl = `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const {
  appendSignalHistory,
  buildIsiHistogram,
  buildPsth,
  buildSpectrogram,
  buildThresholdRaster,
  extractSpikeSnippets,
  frameChannelActivity,
  frameChannelSeries,
  normalizeActivity,
  spikeActivitySummary,
  powerSpectrum
} = await import(moduleUrl);

const frame = {
  pointCount: 2,
  channelCount: 2,
  minimums: [32760, 32000, 32750, 31000],
  maximums: [32780, 33500, 32790, 34500]
};
const activity = frameChannelActivity(frame);
assert.equal(activity.length, 2);
assert.ok(activity[1].score > activity[0].score);
assert.deepEqual(normalizeActivity(activity), [0, 1]);
assert.deepEqual(frameChannelActivity({}), []);
assert.deepEqual(spikeActivitySummary([], 1000, 10000, 64).flashes, Array(64).fill(0));
const heatEvent = { channelIndex: 9, peakSampleIndex: 900, sampleRateHz: 1000 };
const heat = spikeActivitySummary([heatEvent], 1000, 10000, 64, 0, 200);
assert.equal(heat.flashes[9], 0.5);
assert.equal(heat.ratesHz[9], 0.1);
assert.equal(heat.flashes[8], 0);
const faded = spikeActivitySummary([heatEvent], 1000, 10000, 64, 101, 200);
assert.equal(faded.flashes[9], 0, "a stopped sample clock must not leave the electrode lit forever");
assert.equal(faded.ratesHz[9], 0.1, "flashes and the longer event-rate window are independent");
assert.equal(spikeActivitySummary([heatEvent], 1200, 200, 64).ratesHz[9], 0, "old events leave the rate window");
assert.equal(spikeActivitySummary([{ ...heatEvent, peakSampleIndex: 1100 }], 1000, 10000, 64).counts[9], 0);

const raster = buildThresholdRaster([
  { id: 1, category: "threshold", channelIndex: 0, elapsedMs: 4_000, sampleIndex: 120_000 },
  { id: 2, category: "threshold", channelIndex: 1, elapsedMs: 9_000, sampleIndex: 270_000 },
  { id: 3, category: "device", channelIndex: 0, elapsedMs: 9_500 },
  { id: 4, category: "threshold", channelIndex: 3, elapsedMs: 9_800 }
], 10_000, 5_000, 2);
assert.equal(raster.points.length, 1);
assert.equal(raster.points[0].channelIndex, 1);
assert.equal(raster.points[0].position, 0.8);
assert.deepEqual(raster.counts, [0, 1]);
assert.deepEqual(raster.ratesHz, [0, 0.2]);

const historyFrame = {
  sequence: 1,
  firstSampleIndex: 100,
  sampleRateHz: 1000,
  binWidthSamples: 2,
  pointCount: 4,
  channelCount: 2,
  minimums: [32767, 32768, 32769, 32770, 32771, 32772, 32773, 32774],
  maximums: [32769, 32770, 32771, 32772, 32773, 32774, 32775, 32776]
};
const series = frameChannelSeries(historyFrame, 1);
assert.deepEqual(series.samples, [1, 3, 5, 7]);
assert.deepEqual(series.sampleIndexes, [100, 102, 104, 106]);
assert.equal(series.sampleRateHz, 500);
const history = appendSignalHistory(null, historyFrame, 256);
assert.equal(history.channels.length, 2);
assert.deepEqual(history.channels[0], [0, 2, 4, 6]);
appendSignalHistory(history, { ...historyFrame, sequence: 2, firstSampleIndex: 108 });
assert.equal(history.channels[0].length, 8, "contiguous blocks extend the analysis history");
appendSignalHistory(history, { ...historyFrame, sequence: 3, firstSampleIndex: 200 });
assert.equal(history.channels[0].length, 4, "spectra must not concatenate across missing time");
appendSignalHistory(history, { ...historyFrame, sequence: 4, firstSampleIndex: 208, flags: 1 });
assert.equal(history.channels[0].length, 4, "timestamp discontinuity resets spectral history");

const sine = Array.from({ length: 256 }, (_, index) => Math.sin(2 * Math.PI * 64 * index / 1024));
const spectrum = powerSpectrum(sine, 1024, 256);
const peakBin = spectrum.powerDb.slice(1).reduce((best, value, index, values) => value > values[best] ? index : best, 0) + 1;
assert.equal(spectrum.frequencies[peakBin], 64);
assert.ok(buildSpectrogram([...sine, ...sine], 1024, { windowSize: 128 }).frames.length > 1);

const spikeEvents = [100, 150, 260, 500].map((elapsedMs, index) => ({
  id: index + 1,
  category: "threshold",
  channelIndex: 0,
  elapsedMs,
  sampleIndex: 100 + index * 2
}));
const isi = buildIsiHistogram(spikeEvents, 0, 200, 4);
assert.deepEqual(isi.intervalsMs, [50, 110]);
const psth = buildPsth(spikeEvents, [{ elapsedMs: 200 }], 0, 100, 200, 50);
assert.equal(psth.triggerCount, 1);
assert.equal(psth.bins.reduce((sum, value) => sum + value, 0), 3);
const snippets = extractSpikeSnippets({
  sampleIndexes: Array.from({ length: 20 }, (_, index) => index * 2),
  channels: [Array.from({ length: 20 }, (_, index) => index)]
}, [{ category: "threshold", channelIndex: 0, sampleIndex: 20 }], 0, 3, 4);
assert.deepEqual(snippets, [[0, 1, 2, 3, 4, 5, 6]]);

console.log("PASS: activity, FFT, spectrogram, ISI, PSTH and spike snippets are deterministic");
