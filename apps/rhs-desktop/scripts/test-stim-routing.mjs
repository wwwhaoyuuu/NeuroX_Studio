import assert from "node:assert/strict";
import { getMeaProfile } from "../src/mea.js";
import { PROBE_64_CHANNELS } from "../src/probe-map.js";
import { acquisitionFaultNotice, buildProbe64ElectrodeMap, resolveStimRouting, shouldPreloadStimulation, isHardwareOutputMode, stimulationControls, stimulationConfirmations } from "../src/stim-routing.js";

const complianceError = "ProtocolError: RHS2116 compliance warning at sample 364262, mask=0x0001, channels=B-000; acquisition stopped and stimulation disabled";
const faultNotice = acquisitionFaultNotice({ mode: "fault", lastError: complianceError });
assert.match(faultNotice.title, /电压余量报警/);
assert.match(faultNotice.detail, /B-000/);
assert.match(faultNotice.detail, /非实时数据/);
assert.equal(faultNotice.error, complianceError);
assert.match(acquisitionFaultNotice({ mode: "fault", lastError: "USB read failed" }).title, /采集故障/);
assert.equal(acquisitionFaultNotice({ mode: "acquiring", lastError: complianceError }), null);
assert.equal(acquisitionFaultNotice({ mode: "ready" }), null);
assert.equal(acquisitionFaultNotice(null), null);

const reviewed = { operator: " operator ", routingConfirmed: true, parametersConfirmed: true, unverifiedOutputAcknowledged: true };
assert.deepEqual(stimulationConfirmations("configurable", true, reviewed), {
  benchConfirmation: null,
  outputConfirmation: { operator: "operator", routingConfirmed: true, parametersConfirmed: true, unverifiedOutputAcknowledged: true }
});
assert.deepEqual(stimulationConfirmations("configurable", false, reviewed), { benchConfirmation: null, outputConfirmation: null });
const unchecked = stimulationConfirmations("configurable", true, {}).outputConfirmation;
assert.equal(unchecked.routingConfirmed, false);
assert.equal(unchecked.parametersConfirmed, false);
assert.equal(unchecked.unverifiedOutputAcknowledged, false);
assert.equal(stimulationConfirmations("functional-repeat", true, {}).benchConfirmation.sampleDisconnected, false);
assert.equal(stimulationConfirmations("functional-repeat", true, {}).benchConfirmation.functionalRepeat, true);
assert.equal(stimulationConfirmations("measured", true, {}).outputConfirmation, null);
assert.throws(() => stimulationConfirmations("unknown", true, {}), /Unknown/);

const disarmed = { armId: null, pendingCommands: 0, completedCommands: 0, outputMode: "interlocked" };
assert.equal(stimulationControls(disarmed, true).armDisabled, false);
assert.equal(stimulationControls(disarmed, true).triggerDisabled, true);
const armedFunctional = { ...disarmed, armId: 1, outputMode: "hardware-functional" };
assert.equal(stimulationControls({ ...armedFunctional, state: "fault" }, true).triggerDisabled, true);
assert.equal(stimulationControls({ ...disarmed, state: "fault" }, true).armDisabled, true);
assert.equal(stimulationControls(armedFunctional, true).armDisabled, true);
assert.equal(stimulationControls(armedFunctional, true).triggerDisabled, false);
assert.equal(stimulationControls(armedFunctional, true).disarmDisabled, false);
assert.equal(stimulationControls({ ...armedFunctional, completedCommands: 1 }, true).triggerDisabled, true);
for (let submittedCommands = 0; submittedCommands < 3; submittedCommands += 1) {
  assert.equal(stimulationControls({ ...armedFunctional, functionalTriggerLimit: 3, submittedCommands }, true).triggerDisabled, false);
}
assert.equal(stimulationControls({ ...armedFunctional, functionalTriggerLimit: 3, submittedCommands: 3 }, true).triggerDisabled, true);
assert.equal(stimulationControls({ ...armedFunctional, submittedCommands: 1, completedCommands: 0 }, true).triggerDisabled, true);
assert.equal(stimulationControls({ ...armedFunctional, pendingCommands: 1 }, true).cancelDisabled, false);
const configurable = { ...armedFunctional, outputMode: "hardware-configurable", commandLimit: 16, submittedCommands: 1 };
assert.equal(stimulationControls(configurable, true).triggerDisabled, false);
assert.equal(stimulationControls({ ...configurable, submittedCommands: 16 }, true).triggerDisabled, true);
assert.equal(stimulationControls({ ...configurable, pendingCommands: 1 }, true).triggerDisabled, true);
for (const snapshot of [disarmed, armedFunctional, { ...armedFunctional, pendingCommands: 1 }]) {
  for (const controls of [stimulationControls(snapshot, false), stimulationControls(snapshot, true, true)]) {
    assert.equal(controls.armDisabled, true);
    assert.equal(controls.triggerDisabled, true);
    assert.equal(controls.cancelDisabled, true);
    assert.equal(controls.disarmDisabled, true);
  }
}

assert.equal(isHardwareOutputMode("hardware-functional"), true);
assert.equal(isHardwareOutputMode("hardware-bench"), true);
assert.equal(isHardwareOutputMode("hardware-configurable"), true);
assert.equal(isHardwareOutputMode("interlocked"), false);
assert.equal(isHardwareOutputMode(undefined), false);

assert.equal(shouldPreloadStimulation("mea-ab-64", []), false);
assert.equal(shouldPreloadStimulation("rhs2116-logic-16", []), false);
assert.equal(shouldPreloadStimulation("mea-ab-64", ["B-000"]), true);
assert.equal(shouldPreloadStimulation("rhs2116-logic-16", ["CH-01"]), true);
assert.equal(shouldPreloadStimulation("mea-abcd-120", ["A-000"]), false);

const profile = getMeaProfile("mea-ab-64");
const chips = [2, 3, 4, 5].map((stream) => ({
  chipName: "RHS2116", port: stream < 4 ? "B" : "C", stream, channels: 16
}));
// The stream assignment is a synthetic fixture, not a claim about the connected board.
const map = buildProbe64ElectrodeMap(chips, { B: [2, 3], C: [4, 5] });
assert.deepEqual(buildProbe64ElectrodeMap(chips), map);
assert.deepEqual(buildProbe64ElectrodeMap([...chips].reverse()), map);
assert.equal(map.length, 64);
assert.deepEqual(new Set(map.map(({ stream, channel }) => `${stream}:${channel}`)).size, 64);
for (const entry of PROBE_64_CHANNELS) {
  const route = map.find(({ electrodeId }) => electrodeId === entry.id);
  assert.equal(route.stream, (entry.port === "B" ? 2 : 4) + Math.floor(entry.channel / 16));
  assert.equal(route.channel, entry.channel % 16);
}
const selected = ["B-000", "B-031", "C-000", "C-031"];
assert.deepEqual(resolveStimRouting(profile.id, selected, chips, map), [
  { electrodeId: "B-000", port: "B", stream: 2, channel: 0 },
  { electrodeId: "B-031", port: "B", stream: 3, channel: 15 },
  { electrodeId: "C-000", port: "C", stream: 4, channel: 0 },
  { electrodeId: "C-031", port: "C", stream: 5, channel: 15 }
]);
assert.throws(() => resolveStimRouting(profile.id, selected, chips.slice(0, 3), map), /four B\/C/);
assert.throws(() => resolveStimRouting(profile.id, selected, chips, map.slice(0, 63)), /All 64/);
assert.throws(() => resolveStimRouting(profile.id, selected, chips, [
  ...map.slice(0, 63), { ...map[63], stream: 4, channel: 0 }
]), /Duplicate physical address/);
assert.throws(() => resolveStimRouting(profile.id, ["B-000", "B-000"], chips, map), /duplicate electrode/);
assert.throws(() => resolveStimRouting(profile.id, ["X-000"], chips, map), /unknown or duplicate electrode/);
assert.throws(() => buildProbe64ElectrodeMap(chips, { B: [2, 4], C: [3, 5] }), /does not match detected port/);
assert.throws(() => buildProbe64ElectrodeMap(chips, { B: [2, 2], C: [4, 5] }), /does not match detected port/);
assert.throws(() => buildProbe64ElectrodeMap(chips, { B: [2], C: [4, 5] }), /Explicit B\/C stream order/);
assert.throws(() => resolveStimRouting(profile.id, selected, chips, [
  { ...map[0], stream: 4 }, ...map.slice(1)
]), /wrong port/);
assert.throws(() => resolveStimRouting(profile.id, selected, [...chips, chips[0]], map), /distinct/);
assert.throws(() => buildProbe64ElectrodeMap([
  { ...chips[0], stream: 4 }, chips[1], { ...chips[2], stream: 2 }, chips[3]
]), /distinct/);
const eightChips = Array.from({ length: 8 }, (_, stream) => ({
  chipName: "RHS2116", port: "ABCD"[Math.floor(stream / 2)], stream, channels: 16
}));
assert.deepEqual(buildProbe64ElectrodeMap(eightChips), map);
assert.deepEqual(buildProbe64ElectrodeMap([...eightChips].reverse()), map);
assert.deepEqual(resolveStimRouting(profile.id, selected, eightChips, map),
  resolveStimRouting(profile.id, selected, chips, map));
assert.throws(() => buildProbe64ElectrodeMap(eightChips.filter(chip => chip.stream !== 3)), /four B\/C/);
assert.throws(() => buildProbe64ElectrodeMap([...eightChips, eightChips[0]]), /at most eight/);
assert.throws(() => buildProbe64ElectrodeMap(eightChips.map(chip => chip.stream === 7 ? { ...chip, port: "A" } : chip)), /distinct/);
console.log("PASS: synthetic 64-electrode stimulation routes and invalid maps");
