import assert from "node:assert/strict";
import { timestampRecordingPath, recordingWorkspace, playbackWorkspace, analysisExportDirectory } from "../src/recording.js";

assert.equal(analysisExportDirectory(" E:\\实验\\trial.RHSRAW "), "E:\\实验\\trial-analysis");
assert.equal(analysisExportDirectory("work/trial.rhsraw"), "work/trial-analysis");
assert.equal(analysisExportDirectory("trial"), "trial-analysis");
assert.throws(() => analysisExportDirectory(""), /记录文件/);
assert.throws(() => analysisExportDirectory("work/"), /记录文件/);

const date = new Date(2026, 9, 29, 23, 4, 5, 6);
assert.equal(timestampRecordingPath("work/old.rhsraw", "continuous", date), "work/neurox-studio-20261029-230405-006.rhsraw");
assert.equal(timestampRecordingPath("work/old.rhsraw", "continuous", date), "work/neurox-studio-20261029-230405-006-1.rhsraw");
assert.equal(timestampRecordingPath("E:\\实验\\old.rhsraw", "triggered", date), "E:\\实验\\neurox-trigger-20261029-230405-006-2.rhsraw");
assert.match(timestampRecordingPath("file.rhsraw", "continuous", new Date(2027, 0, 1)), /^neurox-studio-20270101-000000-000\.rhsraw$/);
assert.throws(() => timestampRecordingPath("work/", "continuous", new Date(NaN)), /时间无效/);

const workspace = {
  meaProfileId: "mea-ab-64", session: { sessionName: "MEA 实验" },
  pipeline: { schemaVersion: 1, nodes: [{ kind: "notch", frequencyHz: 50 }] },
  channelSettings: { "mea-ab-64": { "B-009": { label: "B-09", row: 0, column: 0 } } },
  stimulation: { "mea-ab-64": { selectedChannels: ["B-009"], amplitudeNa: 10, phaseWidthUs: 200,
    benchConfirmation: { sampleDisconnected: true }, outputConfirmation: { routingConfirmed: true }, armed: true } },
  acquisition: { bitfile: "old-machine.bit", serial: "OLD", blocksPerRead: 8 },
  recording: { path: "overwrite.rhsraw", triggeredPath: "overwrite-trigger.rhsraw", playbackPath: "old.rhsraw", autoName: false, preTriggerMs: 1000 },
  display: { gain: 1, timeWindowMs: 500, channelRanges: { "mea-ab-64": { "B-009": { minUv: -20, maxUv: 20 } } } },
  tools: { audioGain: 0.5, impedanceFrequency: 1000 }
};
const saved = recordingWorkspace(workspace);
assert.equal(saved.recording.path, undefined);
assert.equal(saved.acquisition.bitfile, undefined);
assert.equal(saved.acquisition.blocksPerRead, 8);
assert.equal(saved.stimulation["mea-ab-64"].outputConfirmation, undefined);
assert.equal(saved.stimulation["mea-ab-64"].armed, undefined);
assert.equal(workspace.stimulation["mea-ab-64"].armed, true, "snapshot must not mutate live settings");
const restored = playbackWorkspace({ schemaVersion: 1, workspace }, 64);
assert.deepEqual(restored.pipeline, workspace.pipeline);
assert.deepEqual(restored.channelSettings, workspace.channelSettings);
assert.deepEqual(restored.display, workspace.display);
assert.equal(restored.acquisition, undefined);
assert.equal(restored.recording.path, undefined);
assert.equal(restored.recording.autoName, false);
assert.deepEqual(restored.stimulation["mea-ab-64"].selectedChannels, ["B-009"]);
assert.equal(restored.stimulation["mea-ab-64"].amplitudeNa, 10);
assert.equal(restored.stimulation["mea-ab-64"].benchConfirmation, undefined);
assert.equal(playbackWorkspace(null, 16), null);
assert.equal(playbackWorkspace({ schemaVersion: 1, workspace: {} }, 16), null);
assert.throws(() => playbackWorkspace({ schemaVersion: 2, workspace }, 64), /版本/);
assert.throws(() => playbackWorkspace({ schemaVersion: 1, workspace }, 16), /通道数/);
console.log("PASS: recording timestamps, parameter round trips, analysis export paths and replay output isolation");
