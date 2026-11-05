import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  MEA_PROFILES, channelSelectionPreset, deviceDisplayName, getMeaProfile, normalizeChannelSelection
} from "../src/mea.js";
import { PROBE_64_CHANNELS, PROBE_64_SITES, probeChannelForSite, probeSiteForChannel } from "../src/probe-map.js";
import { configuredProfile, normalizeChannelSettings } from "../src/workspace-data.js";
import { frameChannelActivity } from "../src/analysis.js";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourcePath = path.join(root, "src", "mea.js");
const source = fs.readFileSync(sourcePath, "utf8");
assert.deepEqual(MEA_PROFILES.map((profile) => profile.channelCount), [16, 64, 120]);
assert.equal(new Set(MEA_PROFILES.map((profile) => profile.id)).size, MEA_PROFILES.length);
assert.equal(getMeaProfile("rhs2116-logic-16").name, "16 通道 · 单芯片");
assert.equal(deviceDisplayName(null), "采集设备");
assert.equal(deviceDisplayName({ channels: 16, chipName: "RHS2116" }), "16 通道 · 采集刺激设备");
assert.equal(deviceDisplayName({ channels: 16, capabilities: { detectedChannelCount: 64 } }), "64 通道 · 采集刺激设备");
assert.equal(deviceDisplayName({ channels: 64 }, true), "64 通道 · 模拟信号源");
assert.equal(deviceDisplayName({ channels: NaN }), "采集刺激设备");
assert.equal(deviceDisplayName({ channels: 0 }), "采集刺激设备");
assert.ok(MEA_PROFILES.every(({ name, shortName, description }) => !/RHS/.test(`${name} ${shortName} ${description}`)));

for (const profile of MEA_PROFILES) {
  assert.equal(profile.cells.length, profile.rows * profile.columns, `${profile.id} grid dimensions`);
  assert.equal(new Set(profile.channels.map((channel) => channel.id)).size, profile.channelCount, `${profile.id} unique channels`);
  assert.equal(channelSelectionPreset(profile, "all").size, profile.channelCount, `${profile.id} select all`);
  assert.equal(channelSelectionPreset(profile, "none").size, 0, `${profile.id} select none`);
  assert.equal(
    channelSelectionPreset(profile, "center").size + channelSelectionPreset(profile, "perimeter").size,
    profile.channelCount,
    `${profile.id} spatial presets partition channels`
  );
}

const ab = getMeaProfile("mea-ab-64");
assert.deepEqual(ab.cells.slice(0, 8).map((channel) => channel.siteNumber), [9, 8, 5, 1, 62, 58, 55, 54]);
assert.equal(ab.cells[14].siteNumber, 52);
assert.equal(ab.cells[15].siteNumber, 53);
assert.equal(new Set(PROBE_64_SITES).size, 64);
assert.equal(PROBE_64_CHANNELS.length, 64);
for (const entry of PROBE_64_CHANNELS) {
  assert.equal(probeSiteForChannel(entry.port, entry.channel), entry.siteNumber);
  assert.deepEqual(probeChannelForSite(entry.siteNumber), entry);
}
assert.equal(probeSiteForChannel("C", 0), 32);
assert.equal(probeSiteForChannel("C", 16), 48);
assert.equal(probeSiteForChannel("B", 0), 0);
assert.equal(probeSiteForChannel("B", 16), 16);
assert.deepEqual(ab.cells.slice(0, 8).map(({ label }) => label), ["B-09", "B-08", "B-05", "B-01", "C-30", "C-26", "C-23", "C-22"]);
assert.equal(ab.cells[36].label, "C-00");
assert.equal(ab.channels[9].label, "B-09");
assert.equal(ab.channels[62].label, "C-30");

const activity = frameChannelActivity({ channelCount: 64, pointCount: 1,
  minimums: Array(64).fill(32768), maximums: Array.from({ length: 64 }, (_, index) => 32768 + index + 1) });
for (const [cellIndex, cell] of ab.cells.entries()) {
  const site = cell.siteNumber;
  const port = cellIndex % 8 < 4 ? "B" : "C";
  const localChannel = port === "B" ? site : site - 32;
  assert.equal(cell.id, `${port}-${String(localChannel).padStart(3, "0")}`);
  assert.equal(cell.label, `${port}-${String(localChannel).padStart(2, "0")}`);
  const dataIndex = ab.channels.findIndex(({ id }) => id === cell.id);
  assert.equal(dataIndex, site, `site ${site} reads its matching data lane`);
  assert.equal(activity[dataIndex].peakDeviation, site + 1);
}

const legacySettings = Object.fromEntries(ab.channels.map((channel, index) => [channel.id, {
  label: channel.id, row: 7 - Math.floor(index / 8), column: index % 8,
  color: "#123456", enabled: index !== 7
}]));
legacySettings["B-009"].label = "Custom alias";
const migrated = normalizeChannelSettings(ab, legacySettings);
const restored = configuredProfile(ab, legacySettings);
for (const [cellIndex, cell] of restored.cells.entries()) {
  assert.equal(cell.id, ab.cells[cellIndex].id, "legacy/recovered workspace must not restore old electrode positions");
  assert.equal(cell.siteNumber, ab.cells[cellIndex].siteNumber);
  assert.equal(cell.color, "#123456");
  assert.equal(migrated[cell.id].probeMapVersion, ab.probeMapVersion);
}
assert.equal(restored.channels[9].label, "Custom alias");
assert.equal(restored.channels[7].enabled, false);
assert.equal(restored.channels[0].label, "B-00");
assert.deepEqual(normalizeChannelSettings(ab, migrated), migrated, "migration is idempotent");
const edited = { ...migrated, "B-009": { ...migrated["B-009"], row: 2, column: 3 } };
assert.equal(normalizeChannelSettings(ab, edited)["B-009"].row, 2, "current-version edits remain editable");
assert.equal(probeChannelForSite(-1), null);
assert.equal(probeChannelForSite(64), null);
assert.equal(probeSiteForChannel("A", 0), null);

const abcd = getMeaProfile("mea-abcd-120");
assert.equal(abcd.cells.filter((channel) => channel === null).length, 12);
assert.deepEqual([...normalizeChannelSelection(abcd, ["A-014", "D-014", "X-999"])], ["A-014", "D-014"]);

assert.ok(source.includes("function createProfile"));
console.log("PASS: MEA profiles, physical layouts and selection presets are valid");
