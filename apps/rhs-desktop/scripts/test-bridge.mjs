import assert from "node:assert/strict";

globalThis.window = {};

const { bridge } = await import("../src/bridge.js");
assert.match(bridge.defaultRecordingPath(), /^work\/neurox-studio-.*\.rhsraw$/);
assert.match(bridge.defaultTriggeredRecordingPath(), /^work\/neurox-trigger-.*\.rhsraw$/);

bridge.setPreviewChannelCount(120);
const status = await bridge.invoke("get_status");

if (status.device?.chipName !== "Synthetic MEA" || status.device?.serial !== "PREVIEW") {
  throw new Error("preview status must identify itself as a synthetic source");
}
if (status.device.channels !== 120 || status.device.chips.length !== 8) {
  throw new Error("preview status must describe all selected MEA channels");
}
if (status.device.chips.some((chip) => chip.port !== "SIM" || chip.channels < 1 || chip.channels > 16)) {
  throw new Error("preview chip groups must remain bounded synthetic streams");
}
await bridge.invoke("start_acquisition");
await assert.rejects(bridge.invoke("arm_stimulation", { config: { channelIds: ["B-000"], outputConfirmation: { routingConfirmed: true } } }), /不能启用真实硬件输出/);
await assert.rejects(bridge.invoke("arm_stimulation", { config: { channelIds: ["B-000"], benchConfirmation: { sampleDisconnected: true } } }), /不能启用真实硬件输出/);
assert.equal((await bridge.invoke("get_status")).stimulation.outputMode, "interlocked");
const frame = await bridge.invoke("poll_waveform");
const nextFrame = await bridge.invoke("poll_waveform");
const batch = await bridge.invoke("poll_waveforms");
assert.equal(batch.frames.length, 1);
assert.equal(batch.frames[0].sampleCount, 1080);
assert.equal(batch.frames[0].firstValues.length, 360 * 120);
assert.equal(batch.frames[0].lastValues.length, 360 * 120);
assert.equal(batch.droppedFrames, 0);
if (nextFrame.firstSampleIndex !== frame.firstSampleIndex + frame.pointCount * frame.binWidthSamples) {
  throw new Error("preview waveform frames must have a continuous sample timeline");
}
if (frame.baseline.length !== 120 || frame.baseline.some((entry) =>
  !Number.isFinite(entry.offsetAdc) || !Number.isFinite(entry.noiseAdc) || entry.quality !== "preview"
)) {
  throw new Error("preview baseline must be finite and explicitly synthetic for every channel");
}
await bridge.invoke("stop_acquisition");
await assert.rejects(bridge.invoke("prepare_impedance_scan", { request: { channelIds: ["B-000"], confirmedTestSignal: true } }), /不能执行真实阻抗测量/);

console.log("PASS: preview bridge reports 16/64/120-channel synthetic sources without claiming hardware");
