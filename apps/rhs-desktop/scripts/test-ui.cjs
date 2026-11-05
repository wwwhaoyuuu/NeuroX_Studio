const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const required = [
  "dist/index.html",
  "dist/styles.css",
  "dist/app.js",
  "dist/bridge.js",
  "dist/analysis.js",
  "dist/workspace-data.js",
  "dist/waveform.js",
  "dist/recording.js",
  "dist/vendor/lucide.js"
];

for (const file of required) {
  const absolute = path.join(root, file);
  if (!fs.existsSync(absolute) || fs.statSync(absolute).size === 0) {
    throw new Error(`missing built UI asset: ${file}`);
  }
}

const html = fs.readFileSync(path.join(root, "dist/index.html"), "utf8");
const appSource = fs.readFileSync(path.join(root, "dist/app.js"), "utf8");
const desktopConfig = JSON.parse(fs.readFileSync(path.join(root, "src-tauri/tauri.conf.json"), "utf8"));
if (!html.includes("<title>NeuroX Studio</title>") || !html.includes("<strong>NeuroX Studio</strong>")
    || html.includes("RHS Studio") || html.includes("RHS Source") || appSource.includes('"RHS Studio"')) {
  throw new Error("product branding must consistently use NeuroX Studio");
}
if (desktopConfig.productName !== "NeuroX Studio" || desktopConfig.app.windows[0].title !== "NeuroX Studio") {
  throw new Error("desktop title and product metadata must use NeuroX Studio");
}
if (desktopConfig.identifier !== "cn.rhs-lab.studio" || !appSource.includes('"rhs-studio.workspace-recovery.v1"')) {
  throw new Error("branding must preserve existing settings");
}
if (html.includes("RHS") || !appSource.includes("deviceDisplayName(status.device, bridge.isPreview)")) {
  throw new Error("visible device names must describe channel capabilities instead of driver models");
}
for (const id of [
  "waveform",
  "waveform-spike-markers",
  "waveform-spike-count",
  "waveform-spike-state",
  "waveform-spike-settings",
  "waveform-spike-tooltip",
  "acquisition-fault",
  "acquisition-fault-title",
  "acquisition-fault-detail",
  "acquisition-fault-error",
  "amplitude-channel",
  "waveform-visible-range",
  "waveform-peak-to-peak",
  "waveform-rail-warning",
  "time-window",
  "channel-pager",
  "previous-channel-page",
  "channel-page-label",
  "next-channel-page",
  "start-acquisition",
  "record-toggle",
  "playback-path",
  "open-playback",
  "open-analysis-export",
  "summary-open-analysis-export",
  "analysis-export-dialog",
  "analysis-export-form",
  "analysis-export-source",
  "analysis-export-directory",
  "analysis-export-format",
  "submit-analysis-export",
  "analysis-export-result",
  "playback-toggle",
  "close-playback",
  "playback-speed",
  "playback-position",
  "trigger-recording-path",
  "trigger-recording-source",
  "arm-trigger-recording",
  "fire-recording-trigger",
  "cancel-trigger-recording",
  "emergency-stop",
  "toggle-left-panel",
  "toggle-right-panel",
  "left-panel",
  "right-panel",
  "focus-range-form",
  "focus-range-min",
  "focus-range-max",
  "focus-range-reset",
  "range-channel-name",
  "waveform-layout",
  "all-channel-range-apply",
  "all-channel-range-reset",
  "waveform-range-clipped",
  "pipeline-node-list",
  "pipeline-node-kind",
  "pipeline-preset",
  "pipeline-preset-dialog",
  "pipeline-validation",
  "mea-profile",
  "game-mea-grid",
  "activity-mea-grid",
  "spike-raster",
  "spike-window",
  "activity-metric",
  "spectrum-view",
  "spectrum-channel",
  "spectrum-canvas",
  "spectrogram-canvas",
  "statistics-view",
  "statistics-channel",
  "isi-canvas",
  "psth-canvas",
  "snippet-canvas",
  "experiment-preset",
  "experiment-preset-dialog",
  "session-name",
  "session-operator",
  "session-subject",
  "session-notes",
  "recovery-status",
  "stim-config-dialog",
  "stim-mea-grid",
  "stim-parameter-preset",
  "arm-stimulation",
  "trigger-stimulation",
  "stim-validation-mode",
  "stim-measured-confirmations",
  "stim-configurable-confirmations",
  "stim-routing-confirmed",
  "stim-parameters-confirmed",
  "stim-unverified-acknowledged",
  "stim-programmed-summary",
  "stim-sample-disconnected",
  "stim-real-output",
  "cancel-stimulation",
  "disarm-stimulation",
  "device-tools-view",
  "impedance-frequency",
  "prepare-impedance",
  "cancel-impedance",
  "impedance-confirmed",
  "impedance-progress",
  "export-impedance",
  "expand-impedance",
  "impedance-profile-name",
  "impedance-results",
  "audio-channel",
  "audio-gain",
  "toggle-audio",
  "export-diagnostics",
  "diagnostic-summary",
  "open-channel-editor",
  "channel-editor-dialog",
  "open-export",
  "export-dialog"
]) {
  if (!html.includes(`id="${id}"`)) {
    throw new Error(`missing required UI control: ${id}`);
  }
}
if (!html.includes("真实输出已锁定") || !html.includes("此窗口只生成通道与参数配置，不会 Arm 或 Trigger")) {
  throw new Error("stimulation controls must remain visibly interlocked");
}
if (html.includes("storage-meter")) {
  throw new Error("storage utilization must not be shown without a real capacity metric");
}
if (appSource.includes("SampleBlock · 30 kHz · 16 ch")) {
  throw new Error("pipeline validation summary must use the active source channel count");
}
for (const removedControl of ["notch-enabled", "bandpass-enabled", "blanking-enabled", "threshold-enabled", "event-log", "toggle-console", "clear-events",
  "digital-input-bits", "digital-output-bits", "apply-io-output", "analog-output-1", "analog-output-2",
  "remote-api-port", "remote-api-token", "start-remote-api", "stop-remote-api"]) {
  if (html.includes(`id="${removedControl}"`)) {
    throw new Error(`fixed pipeline control must not remain: ${removedControl}`);
  }
}
if (appSource.includes("renderIoState") || appSource.includes("renderRemoteApiState")) {
  throw new Error("removed tools must not leave dangling render callbacks");
}
if (html.includes("event-console") || appSource.includes("renderEvents()")) {
  throw new Error("event console must be removed, not hidden");
}
console.log("PASS: frontend assets, critical controls and stimulation interlock are present");
