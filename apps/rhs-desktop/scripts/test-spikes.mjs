import assert from "node:assert/strict";
import { SpikeHistory, spikeCsv, drawSpikeWaveforms } from "../src/spikes.js";

const event = (id, channelIndex = 0) => ({ id, channelIndex, stream: 2, chipChannel: channelIndex,
  sampleIndex: id * 30, timeMs: id, peakSampleIndex: id * 30 + 2,
  peakUv: -100.035, peakToPeakUv: 123.045, sampleRateHz: 30000, preSamples: 2,
  waveformUv: [0, -2, -50, -100.035, 23.01, 0] });
const history = new SpikeHistory(3);
history.append({ epoch: 1, missedEvents: 2, events: [event(3), event(4, 1)] });
assert.equal(history.missed, 2);
history.append({ epoch: 1, missedEvents: 0, events: [event(4, 1), event(5), event(6)] });
assert.deepEqual(history.events.map((value) => value.id), [4, 5, 6]);
assert.equal(history.evicted, 1);
assert.equal(history.afterId, 6);
assert.equal(history.analysisEvents()[0].category, "threshold");
assert.equal(history.analysisEvents()[0].elapsedMs, 4);
assert.equal(history.snippets(1)[0].peakUv, -100.035);
const csv = spikeCsv(history.events);
assert.equal(csv.split("\r\n").length, 4);
assert.ok(csv.includes("0;-2;-50;-100.035;23.01;0"));
assert.ok(csv.includes("peak_uv,peak_to_peak_uv,sample_rate_hz"));
history.append({ epoch: 2, missedEvents: 0, events: [event(1)] });
assert.equal(history.missed, 0);
assert.equal(history.evicted, 0);
assert.equal(history.afterId, 1);
assert.equal(history.events.length, 1);
const marks = [], dashes = [];
const context = { scale() {}, beginPath() {}, moveTo() {}, lineTo() {}, stroke() {}, fillText() {},
  setLineDash(pattern) { dashes.push(pattern); }, arc(x, y, radius) { marks.push({ x, y, radius, color: this.fillStyle }); }, fill() {} };
globalThis.window = { devicePixelRatio: 1 };
drawSpikeWaveforms({ getBoundingClientRect: () => ({ width: 400, height: 200 }), getContext: () => context },
  [{ ...event(1), peakSampleIndex: 31 }], 200, { thresholdUv: -50 });
assert.deepEqual(dashes[0], [5, 4]);
assert.equal(marks.length, 1);
assert.equal(marks[0].color, "#ff4d5e");

globalThis.window = {};
const { bridge } = await import("../src/bridge.js");
const config = { enabled: true, thresholdUv: -50, refractoryMs: 3, preMs: 1, postMs: 2, bandPass: true, recordingPath: null };
await assert.rejects(bridge.invoke("configure_spikes", { config }), /停止采集/);
await bridge.invoke("stop_acquisition");
await assert.rejects(bridge.invoke("configure_spikes", { config: { ...config, recordingPath: "fake.csv" } }), /预览模式/);
await assert.rejects(bridge.invoke("configure_spikes", { config: { ...config, thresholdUv: 0 } }), /无效/);
await bridge.invoke("configure_spikes", { config });
bridge.setPreviewChannelCount(64);
await bridge.invoke("start_acquisition");
const waveforms = [];
for (let index = 0; index < 45; index++) waveforms.push((await bridge.invoke("poll_waveforms")).frames[0]);
const batch = await bridge.invoke("poll_spikes", { epoch: 0, afterId: 0 });
assert.equal(new Set(batch.events.map(({ channelIndex }) => channelIndex)).size, 64);
assert.equal(batch.running, true);
assert.equal(batch.events.find(({ channelIndex }) => channelIndex === 63).waveformUv.length, 91);
assert.equal(batch.recordedEvents, 0);
assert.equal(waveforms[0].spikeEpoch, batch.epoch);
for (const event of batch.events) {
  const waveform = waveforms.find((frame) => event.peakSampleIndex >= frame.firstSampleIndex && event.peakSampleIndex < frame.firstSampleIndex + frame.sampleCount);
  const point = (event.peakSampleIndex - waveform.firstSampleIndex) / waveform.binWidthSamples;
  assert.equal(event.displayPeakUv, (waveform.firstValues[point * 64 + event.channelIndex] - 32768) * 0.195);
}
await bridge.invoke("stop_acquisition");
await bridge.invoke("configure_spikes", { config: { ...config, thresholdMode: "noise", noiseMultiplier: 5, calibrationMs: 1000 } });
await bridge.invoke("start_acquisition");
for (let index = 0; index < 20; index++) await bridge.invoke("poll_waveforms");
assert.equal((await bridge.invoke("poll_spikes", { epoch: 0, afterId: 0 })).events.length, 0);
assert.equal((await bridge.invoke("poll_spikes", { epoch: 0, afterId: 0 })).thresholds[0].state, "calibrating");
for (let index = 0; index < 30; index++) await bridge.invoke("poll_waveforms");
const calibrated = await bridge.invoke("poll_spikes", { epoch: 0, afterId: 0 });
assert.equal(calibrated.thresholds[0].thresholdUv, -30);
assert.ok(calibrated.events.length > 0);
await bridge.invoke("stop_acquisition");
await bridge.invoke("configure_spikes", { config: { ...config, thresholdMode: "fixed", thresholdUv: -500 } });
await bridge.invoke("start_acquisition");
for (let index = 0; index < 45; index++) await bridge.invoke("poll_waveforms");
assert.equal((await bridge.invoke("poll_spikes", { epoch: 0, afterId: 0 })).events.length, 0, "preview thresholds must not inflate the source signal to force detections");
assert.equal((await bridge.invoke("get_status")).device.serial, "PREVIEW");
console.log("PASS: spike cursor, bounded history, calibrated CSV, 64-channel synthetic preview and recording safeguards");
