import assert from "node:assert/strict";
import { buildImpedanceCsv, impedanceControls, impedanceLayout, impedanceChannelOrder } from "../src/impedance.js";
import { getMeaProfile } from "../src/mea.js";
import { configuredProfile } from "../src/workspace-data.js";

assert.equal(impedanceControls({ mode: "ready" }).measureDisabled, false);
for (const status of [{ mode: "acquiring" }, { mode: "recording" }, { mode: "connecting" }, { playback: {} },
  { impedance: { state: "running" } }, { impedance: { state: "cancelling" } }]) {
  assert.equal(impedanceControls(status).measureDisabled, true);
}
assert.equal(impedanceControls({}, true).measureDisabled, true);
assert.equal(impedanceControls({ impedance: { state: "running" } }).cancelDisabled, false);
assert.equal(impedanceControls({ impedance: { state: "cancelling" } }).cancelDisabled, true);
assert.equal(impedanceControls({}).exportDisabled, true);
const result = { frequencyHz: 1234, actualFrequencyHz: 1250, source: "hardware", channels: [
  { channelId: 'CH-01', stream: 3, chipChannel: 0, magnitudeOhms: 100000, phaseDegrees: -45, quality: "measured" },
  { channelId: 'C-031', stream: 5, chipChannel: 15, magnitudeOhms: null, phaseDegrees: null, quality: "pending" }
] };
assert.equal(impedanceControls({ impedance: result }).exportDisabled, false);
const csv = buildImpedanceCsv(result);
assert.match(csv, /"B-016","3","0","1234","1250","100000","-45","measured","hardware"/);
assert.match(csv, /"C-031","C-031","5","15","1234","1250","","","pending"/);
assert.ok(csv.startsWith("\uFEFF"));
const profile = getMeaProfile("mea-ab-64");
const sites = [9,8,5,1,62,58,55,54,10,11,6,2,61,57,52,53,12,13,7,3,60,56,50,51,
  14,15,4,0,63,59,48,49,17,16,27,31,32,36,47,46,19,18,24,28,35,39,45,44,
  21,20,25,29,34,38,43,42,22,23,26,30,33,37,40,41];
const mappedResult = { profileId: profile.id, frequencyHz: 1000, source: "synthetic-test-only", channels:
  Array.from({ length: 64 }, (_, site) => ({ channelId: `${site < 32 ? "B" : "C"}-${String(site % 32).padStart(3, "0")}`,
    stream: 2 + Math.floor(site / 16), chipChannel: site % 16,
    magnitudeOhms: 100000 + site, phaseDegrees: -site, quality: "measured" })).reverse() };
const before = JSON.stringify(mappedResult);
const mapped = impedanceLayout(profile, mappedResult);
assert.equal(mapped.rows, 8);
assert.equal(mapped.columns, 8);
assert.equal(mapped.entries.length, 64);
assert.deepEqual(mapped.entries.slice(0, 8).map(entry => entry.label), ["B-09", "B-08", "B-05", "B-01", "C-30", "C-26", "C-23", "C-22"]);
assert.deepEqual(impedanceChannelOrder(profile), mapped.entries.map(entry => entry.channelId));
for (const [index, site] of sites.entries()) {
  const entry = mapped.entries[index];
  assert.equal(entry.siteNumber, site);
  assert.equal(entry.row, Math.floor(index / 8));
  assert.equal(entry.column, index % 8);
  assert.equal(entry.result.magnitudeOhms, 100000 + site, "values must follow physical channels, not incoming list order");
  assert.equal(entry.result.phaseDegrees, -site);
  assert.equal(entry.routeMismatch, false);
}
const mappedCsv = buildImpedanceCsv(mappedResult, profile).split("\r\n");
assert.equal(mappedCsv.length, 65);
assert.ok(mappedCsv[1].startsWith('"B-009","B-009","2","9"'));
assert.ok(mappedCsv[1].includes('"B-09","9","1","1"'));
assert.ok(mappedCsv[5].startsWith('"C-030","C-030","5","14"'));
assert.ok(mappedCsv[5].includes('"C-30","62","1","5"'));
assert.equal(JSON.stringify(mappedResult), before, "sorting must not alter measurements or physical routes");
const pending = impedanceLayout(profile, { channels: [mappedResult.channels[0]] });
assert.equal(pending.entries.length, 64, "partial results keep all array positions");
assert.equal(pending.entries.filter(entry => entry.result).length, 1);
const mismatch = { channels: [{ channelId: "B-009", stream: 2, chipChannel: 8, magnitudeOhms: 123 }] };
assert.equal(impedanceLayout(profile, mismatch).entries[0].routeMismatch, true);
assert.match(buildImpedanceCsv(mismatch, profile), /"mismatch"/);
const duplicate = { channels: [mappedResult.channels[0], mappedResult.channels[0]] };
assert.equal(impedanceLayout(profile, duplicate).entries.filter(entry => entry.routeMismatch).length, 1);
assert.equal(buildImpedanceCsv(duplicate, profile).split("\r\n").length, 3, "duplicate results remain auditable in CSV");
const unknown = { channels: [{ channelId: "UNKNOWN", magnitudeOhms: 123 }] };
assert.equal(impedanceLayout(profile, unknown).unmappedResults.length, 1);
assert.match(buildImpedanceCsv(unknown, profile), /UNKNOWN/);
const configured = configuredProfile(profile, { "B-009": { label: "Left 9", enabled: false, row: 0, column: 0, probeMapVersion: profile.probeMapVersion } });
assert.equal(impedanceLayout(configured, mappedResult).entries[0].label, "Left 9");
assert.equal(impedanceChannelOrder(configured).includes("B-009"), false);
for (const id of ["rhs2116-logic-16", "mea-abcd-120"]) {
  const selected = getMeaProfile(id);
  const layout = impedanceLayout(selected);
  assert.equal(layout.entries.length, selected.channelCount);
  assert.equal(new Set(layout.entries.map(entry => `${entry.row}:${entry.column}`)).size, selected.channelCount);
}
console.log("PASS: impedance isolation, full 64-site order, unique physical values, partial/mismatched routes, aliases and mapped CSV");
