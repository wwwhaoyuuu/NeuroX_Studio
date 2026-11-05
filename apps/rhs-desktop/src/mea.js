import { PROBE_64_CELLS, PROBE_64_MAP_VERSION } from "./probe-map.js";

const rhs2116Layout = Array.from({ length: 16 }, (_, index) => `CH-${String(index + 1).padStart(2, "0")}`);

const abcdLayoutRows = [
  [null, null, null, "A-014", "A-010", "A-005", "B-029", "B-026", "B-023", "B-020", "B-017"],
  [null, null, "A-015", "A-013", "A-009", "A-004", "B-030", "B-027", "B-024", "B-021", "B-018"],
  [null, "A-019", "A-018", "A-012", "A-008", "A-003", "B-031", "B-028", "B-025", "B-022", "B-019"],
  ["A-022", "A-021", "A-020", "A-011", "A-007", "A-002", "B-010", "B-011", "B-012", "B-013", "B-014"],
  ["A-026", "A-025", "A-024", "A-023", "A-006", "A-001", "B-005", "B-006", "B-007", "B-008", "B-009"],
  ["A-030", "A-031", "A-029", "A-028", "A-027", "A-000", "B-000", "B-001", "B-002", "B-003", "B-004"],
  ["C-001", "C-000", "C-002", "C-003", "C-004", "C-031", "D-027", "D-028", "D-029", "D-030", "D-031"],
  ["C-005", "C-006", "C-007", "C-008", "C-025", "C-030", "D-022", "D-023", "D-024", "D-025", "D-026"],
  ["C-009", "C-010", "C-011", "C-020", "C-024", "C-029", "D-017", "D-018", "D-019", "D-020", "D-021"],
  [null, "C-012", "C-013", "C-019", "C-023", "C-028", "D-000", "D-003", "D-006", "D-009", "D-012"],
  [null, null, "C-016", "C-018", "C-022", "C-027", "D-001", "D-004", "D-007", "D-010", "D-013"],
  [null, null, null, "C-017", "C-021", "C-026", "D-002", "D-005", "D-008", "D-011", "D-014"]
];

function channelPort(label) {
  return label.startsWith("CH-") ? "RHS" : label.split("-")[0];
}

function createProfile({ id, name, shortName, rows, columns, labels, description, probeMapVersion }) {
  const cells = labels.map((entry, cellIndex) => entry ? {
    id: typeof entry === "string" ? entry : entry.label,
    label: typeof entry === "string" ? entry : entry.displayLabel || entry.label,
    siteNumber: typeof entry === "string" ? null : entry.siteNumber,
    port: channelPort(typeof entry === "string" ? entry : entry.label),
    cellIndex,
    row: Math.floor(cellIndex / columns),
    column: cellIndex % columns
  } : null);
  const channels = cells.filter(Boolean).slice().sort((left, right) => left.id.localeCompare(right.id, "en", { numeric: true }));
  return Object.freeze({
    id,
    name,
    shortName,
    rows,
    columns,
    cells: Object.freeze(cells),
    channels: Object.freeze(channels),
    channelCount: channels.length,
    description,
    ...(probeMapVersion ? { probeMapVersion } : {})
  });
}

export const MEA_PROFILES = Object.freeze([
  createProfile({
    id: "rhs2116-logic-16",
    name: "16 通道 · 单芯片",
    shortName: "16 通道",
    rows: 4,
    columns: 4,
    labels: rhs2116Layout,
    description: "单芯片 · 16 通道 · 4×4 布局"
  }),
  createProfile({
    id: "mea-ab-64",
    name: "MEA 64 · BC 8×8",
    shortName: "MEA 64",
    rows: 8,
    columns: 8,
    labels: PROBE_64_CELLS,
    probeMapVersion: PROBE_64_MAP_VERSION,
    description: "4 颗芯片 · B/C 各 32 通道 · 8×8 阵列"
  }),
  createProfile({
    id: "mea-abcd-120",
    name: "MEA 120 · ABCD 12×11",
    shortName: "MEA 120",
    rows: 12,
    columns: 11,
    labels: abcdLayoutRows.flat(),
    description: "A/B/C/D 四端口 · 12×11 阵列"
  })
]);

export const DEFAULT_MEA_PROFILE_ID = MEA_PROFILES[0].id;

export function getMeaProfile(id) {
  return MEA_PROFILES.find((profile) => profile.id === id) || MEA_PROFILES[0];
}

export function deviceDisplayName(device, preview = false) {
  if (!device) return "采集设备";
  const count = device.capabilities?.detectedChannelCount ?? device.channels;
  const prefix = Number.isInteger(count) && count > 0 ? `${count} 通道 · ` : "";
  return `${prefix}${preview ? "模拟信号源" : "采集刺激设备"}`;
}

export function normalizeChannelSelection(profile, channelIds) {
  const available = new Set(profile.channels.filter((channel) => channel.enabled !== false).map((channel) => channel.id));
  return new Set((channelIds || []).filter((channelId) => available.has(channelId)));
}

export function channelSelectionPreset(profile, preset) {
  const channels = profile.channels.filter((channel) => channel.enabled !== false);
  if (preset === "none") return new Set();
  if (preset === "all") return new Set(channels.map((channel) => channel.id));

  const isCenter = (channel) => {
    const normalizedRow = (channel.row + 0.5) / profile.rows;
    const normalizedColumn = (channel.column + 0.5) / profile.columns;
    return normalizedRow >= 0.25 && normalizedRow <= 0.75 && normalizedColumn >= 0.25 && normalizedColumn <= 0.75;
  };
  if (preset === "center") return new Set(channels.filter(isCenter).map((channel) => channel.id));
  if (preset === "perimeter") return new Set(channels.filter((channel) => !isCenter(channel)).map((channel) => channel.id));
  return new Set();
}

export const STIM_PARAMETER_PRESETS = Object.freeze([
  Object.freeze({
    id: "bench-single-v1",
    name: "单次双相脉冲",
    waveform: "biphasic",
    amplitudeNa: 10,
    phaseWidthUs: 200,
    interphaseUs: 100,
    frequencyHz: 10,
    pulseCount: 1,
    chargeRecovery: true
  }),
  Object.freeze({
    id: "bench-train-v1",
    name: "短脉冲序列",
    waveform: "biphasic",
    amplitudeNa: 10,
    phaseWidthUs: 200,
    interphaseUs: 100,
    frequencyHz: 10,
    pulseCount: 5,
    chargeRecovery: true
  })
]);

export function getStimParameterPreset(id) {
  return STIM_PARAMETER_PRESETS.find((preset) => preset.id === id) || STIM_PARAMETER_PRESETS[0];
}
