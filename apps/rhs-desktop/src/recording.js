import { MEA_PROFILES } from "./mea.js";

let lastStamp = "", sameStampCount = 0;

export function analysisExportDirectory(path) {
  const value = String(path || "").trim();
  if (!value || /[\\/]$/.test(value)) throw new Error("请选择原始记录文件");
  return value.replace(/\.rhsraw$/i, "") + "-analysis";
}

export function timestampRecordingPath(previous = "work/", kind = "continuous", date = new Date()) {
  if (!Number.isFinite(date.getTime())) throw new Error("录制时间无效");
  const pad = (number, length = 2) => String(number).padStart(length, "0");
  const stamp = `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}-${pad(date.getMilliseconds(), 3)}`;
  sameStampCount = stamp === lastStamp ? sameStampCount + 1 : 0;
  lastStamp = stamp;
  const separator = Math.max(previous.lastIndexOf("/"), previous.lastIndexOf("\\"));
  const directory = separator >= 0 ? previous.slice(0, separator + 1) : "";
  return `${directory}neurox-${kind === "triggered" ? "trigger" : "studio"}-${stamp}${sameStampCount ? `-${sameStampCount}` : ""}.rhsraw`;
}

export function recordingWorkspace(workspace) {
  const copy = JSON.parse(JSON.stringify(workspace));
  delete copy.recording?.path;
  delete copy.recording?.playbackPath;
  delete copy.recording?.triggeredPath;
  delete copy.acquisition?.bitfile;
  delete copy.acquisition?.serial;
  const fields = ["profileId", "selectedChannels", "waveform", "amplitudeNa", "phaseWidthUs", "interphaseUs", "frequencyHz", "pulseCount", "chargeRecovery", "presetId"];
  copy.stimulation = Object.fromEntries(MEA_PROFILES.filter(({ id }) => copy.stimulation?.[id]).map(({ id }) => [id,
    Object.fromEntries(fields.filter((key) => Object.hasOwn(copy.stimulation[id], key)).map((key) => [key, copy.stimulation[id][key]]))
  ]));
  return copy;
}

export function playbackWorkspace(configuration, channelCount) {
  if (!configuration) return null;
  if (configuration.schemaVersion !== 1 || !configuration.workspace || typeof configuration.workspace !== "object") throw new Error("不支持的记录参数版本");
  if (!configuration.workspace.meaProfileId) return null;
  const workspace = recordingWorkspace(configuration.workspace);
  const profile = MEA_PROFILES.find(({ id, channelCount: count }) => id === workspace.meaProfileId && count === channelCount);
  if (!profile) throw new Error("记录中的阵列配置与数据通道数不一致");
  // Restore analysis/display settings, never file destinations or output permissions.
  return {
    meaProfileId: profile.id, session: workspace.session, channelSettings: workspace.channelSettings,
    pipeline: workspace.pipeline, stimulation: workspace.stimulation,
    display: workspace.display, analysis: workspace.analysis, tools: workspace.tools,
    recording: workspace.recording, stimulationSchedule: workspace.stimulationSchedule
  };
}
