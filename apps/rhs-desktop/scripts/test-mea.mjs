import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourcePath = path.join(root, "src", "mea.js");
const source = fs.readFileSync(sourcePath, "utf8");
const moduleUrl = `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const {
  MEA_PROFILES,
  channelSelectionPreset,
  getMeaProfile,
  normalizeChannelSelection
} = await import(moduleUrl);

assert.deepEqual(MEA_PROFILES.map((profile) => profile.channelCount), [16, 64, 120]);
assert.equal(new Set(MEA_PROFILES.map((profile) => profile.id)).size, MEA_PROFILES.length);

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

const ab = getMeaProfile("sister-ab-64");
assert.deepEqual(ab.cells.slice(0, 8).map((channel) => channel.id), [
  "A-008", "A-009", "A-010", "A-014", "A-017", "A-021", "A-022", "A-023"
]);

const abcd = getMeaProfile("sister-abcd-120");
assert.equal(abcd.cells.filter((channel) => channel === null).length, 12);
assert.deepEqual([...normalizeChannelSelection(abcd, ["A-014", "D-014", "X-999"])], ["A-014", "D-014"]);

assert.ok(source.includes("ChannelGridLayout"));
assert.ok(pathToFileURL(sourcePath).href.startsWith("file:"));
console.log("PASS: MEA profiles, physical layouts and selection presets are valid");
