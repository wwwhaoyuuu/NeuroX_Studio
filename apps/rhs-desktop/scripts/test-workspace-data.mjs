import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = fs.readFileSync(path.join(root, "src", "workspace-data.js"), "utf8");
const moduleUrl = `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const { buildCsv, buildExcelWorkbook, buildHtmlReport, configuredProfile, normalizeChannelSettings } = await import(moduleUrl);

const profile = {
  id: "test",
  name: "Test",
  rows: 2,
  columns: 2,
  channels: [
    { id: "A-0", label: "A-0", row: 0, column: 0, cellIndex: 0, port: "A" },
    { id: "A-1", label: "A-1", row: 0, column: 1, cellIndex: 1, port: "A" }
  ]
};
const settings = normalizeChannelSettings(profile, {
  "A-0": { label: "Motor", color: "#112233", enabled: false, row: 1, column: 1 }
});
assert.equal(settings["A-0"].label, "Motor");
assert.equal(settings["A-0"].enabled, false);
assert.equal(settings["A-1"].enabled, true);
const configured = configuredProfile(profile, settings);
assert.equal(configured.cells[3].id, "A-0");
assert.equal(configured.channels[0].color, "#112233");

const columns = [{ key: "name", label: "Name" }, { key: "value", label: "Value" }];
assert.equal(buildCsv([{ name: "a,b", value: 2 }], columns), 'Name,Value\r\n"a,b",2');
const excel = buildExcelWorkbook([{ name: "Data", columns, rows: [{ name: "alpha", value: 2 }] }]);
assert.ok(excel.includes('ss:Name="Data"'));
assert.ok(excel.includes('ss:Type="Number">2'));
const html = buildHtmlReport("<Report>", [{ name: "Data", columns, rows: [{ name: "<x>", value: 2 }] }]);
assert.ok(html.includes("&lt;Report&gt;"));
assert.ok(!html.includes("<x>"));

console.log("PASS: channel settings and CSV, HTML and Excel exports are deterministic");
