const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const required = [
  "dist/index.html",
  "dist/styles.css",
  "dist/app.js",
  "dist/bridge.js",
  "dist/waveform.js",
  "dist/vendor/lucide.js"
];

for (const file of required) {
  const absolute = path.join(root, file);
  if (!fs.existsSync(absolute) || fs.statSync(absolute).size === 0) {
    throw new Error(`missing built UI asset: ${file}`);
  }
}

const html = fs.readFileSync(path.join(root, "dist/index.html"), "utf8");
for (const id of [
  "waveform",
  "time-window",
  "start-acquisition",
  "record-toggle",
  "emergency-stop",
  "event-log",
  "pipeline-node-list",
  "pipeline-node-kind",
  "pipeline-preset",
  "pipeline-preset-dialog",
  "pipeline-validation",
  "mea-profile",
  "game-mea-grid",
  "stim-config-dialog",
  "stim-mea-grid",
  "stim-parameter-preset"
]) {
  if (!html.includes(`id="${id}"`)) {
    throw new Error(`missing required UI control: ${id}`);
  }
}
if (!html.includes("真实 Arm 与 Trigger 已锁定") || !html.includes("此窗口只生成通道与参数配置，不会 Arm 或 Trigger")) {
  throw new Error("stimulation controls must remain visibly interlocked");
}
if (html.includes("storage-meter")) {
  throw new Error("storage utilization must not be shown without a real capacity metric");
}
for (const removedControl of ["notch-enabled", "bandpass-enabled", "blanking-enabled", "threshold-enabled"]) {
  if (html.includes(`id="${removedControl}"`)) {
    throw new Error(`fixed pipeline control must not remain: ${removedControl}`);
  }
}
console.log("PASS: frontend assets, critical controls and stimulation interlock are present");
