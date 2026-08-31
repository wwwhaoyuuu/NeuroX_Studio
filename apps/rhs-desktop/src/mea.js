const rhs2116Layout = Array.from({ length: 16 }, (_, index) => `CH-${String(index + 1).padStart(2, "0")}`);

const sisterAbNumbers = [
  9, 10, 11, 15, 18, 22, 23, 24,
  12, 13, 14, 16, 17, 19, 20, 21,
  7, 8, 6, 5, 28, 27, 25, 26,
  3, 4, 2, 1, 32, 31, 29, 30,
  34, 33, 35, 36, 61, 62, 64, 63,
  38, 37, 39, 40, 57, 58, 60, 59,
  44, 45, 46, 48, 49, 51, 52, 53,
  41, 42, 43, 47, 50, 54, 55, 56
];

const sisterAbcdRows = [
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

function sisterAbLabel(number) {
  if (number <= 32) return `A-${String(number - 1).padStart(3, "0")}`;
  return `B-${String(number - 33).padStart(3, "0")}`;
}

function channelPort(label) {
  return label.startsWith("CH-") ? "RHS" : label.split("-")[0];
}

function createProfile({ id, name, shortName, rows, columns, labels, provenance, prototype = false }) {
  const cells = labels.map((label, cellIndex) => label ? {
    id: label,
    label,
    port: channelPort(label),
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
    provenance,
    prototype
  });
}

export const MEA_PROFILES = Object.freeze([
  createProfile({
    id: "rhs2116-logic-16",
    name: "RHS2116 · 16 通道",
    shortName: "RHS2116",
    rows: 4,
    columns: 4,
    labels: rhs2116Layout,
    provenance: "当前采集后端 · 4×4 逻辑布局"
  }),
  createProfile({
    id: "sister-ab-64",
    name: "师姐版 AB · 64 通道",
    shortName: "AB 8×8",
    rows: 8,
    columns: 8,
    labels: sisterAbNumbers.map(sisterAbLabel),
    provenance: "Intan 修改版 ChannelGridLayout · 8×8",
    prototype: true
  }),
  createProfile({
    id: "sister-abcd-120",
    name: "师姐版 ABCD · 120 电极",
    shortName: "ABCD 12×11",
    rows: 12,
    columns: 11,
    labels: sisterAbcdRows.flat(),
    provenance: "Intan 修改版 WaveformPortMatrixWidget · 12×11",
    prototype: true
  })
]);

export const DEFAULT_MEA_PROFILE_ID = MEA_PROFILES[0].id;

export function getMeaProfile(id) {
  return MEA_PROFILES.find((profile) => profile.id === id) || MEA_PROFILES[0];
}

export function normalizeChannelSelection(profile, channelIds) {
  const available = new Set(profile.channels.map((channel) => channel.id));
  return new Set((channelIds || []).filter((channelId) => available.has(channelId)));
}

export function channelSelectionPreset(profile, preset) {
  const channels = profile.channels;
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
    name: "Bench Single v1",
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
    name: "Bench Train v1",
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
