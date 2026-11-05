export function impedanceControls(status, preview = false) {
  const busy = ["running", "cancelling"].includes(status?.impedance?.state);
  return {
    busy,
    measureDisabled: busy || preview || ["acquiring", "recording", "connecting"].includes(status?.mode) || Boolean(status?.playback),
    cancelDisabled: status?.impedance?.state !== "running",
    exportDisabled: !(status?.impedance?.channels || []).some(channel => Number.isFinite(channel.magnitudeOhms))
  };
}

export function impedancePhysicalId(channel) {
  return Number.isInteger(channel.stream) && channel.stream >= 0 && channel.stream < 8
    && Number.isInteger(channel.chipChannel) && channel.chipChannel >= 0 && channel.chipChannel < 16
    ? `${String.fromCharCode(65 + Math.floor(channel.stream / 2))}-${String(channel.stream % 2 * 16 + channel.chipChannel).padStart(3, "0")}` : "";
}

export function impedanceLayout(profile, view = {}) {
  const results = new Map();
  const duplicateIds = new Set();
  for (const result of view.channels || []) {
    if (results.has(result.channelId)) duplicateIds.add(result.channelId);
    results.set(result.channelId, result);
  }
  const entries = profile.cells.filter(Boolean).map(channel => {
    const result = results.get(channel.id) || null;
    const hasAddress = result && (result.stream != null || result.chipChannel != null);
    const physical = result ? impedancePhysicalId(result) : "";
    const expected = /^CH-\d+$/.test(channel.id)
      ? physical && Number(result.chipChannel) === Number(channel.id.slice(3)) - 1
      : physical === channel.id;
    return { channelId: channel.id, label: channel.label, row: channel.row, column: channel.column,
      siteNumber: channel.siteNumber, enabled: channel.enabled !== false, result,
      routeMismatch: duplicateIds.has(channel.id) || Boolean(hasAddress && !expected) };
  });
  const ids = new Set(entries.map(entry => entry.channelId));
  return { rows: profile.rows, columns: profile.columns, entries,
    unmappedResults: (view.channels || []).filter(result => !ids.has(result.channelId)) };
}

export function impedanceChannelOrder(profile) {
  return impedanceLayout(profile).entries.filter(entry => entry.enabled).map(entry => entry.channelId);
}

export function buildImpedanceCsv(view, profile = null) {
  const cell = value => `"${String(value ?? "").replaceAll('"', '""')}"`;
  const rows = [["Channel", "Physical channel", "Stream", "Chip channel", "Requested Hz", "Actual Hz", "Magnitude Ohm", "Phase deg", "Quality", "Source", "Display channel", "Electrode site", "Map row", "Map column", "Route check"]];
  const layout = profile ? impedanceLayout(profile, view) : null;
  const entries = layout ? [...layout.entries.flatMap(entry => (view.channels || [])
    .filter(result => result.channelId === entry.channelId).map(result => ({ ...entry, result }))),
    ...layout.unmappedResults.map(result => ({ result }))] : (view.channels || []).map(result => ({ result }));
  for (const entry of entries) {
    const channel = entry.result;
    rows.push([channel.channelId, impedancePhysicalId(channel), channel.stream, channel.chipChannel,
      view.frequencyHz, view.actualFrequencyHz, channel.magnitudeOhms, channel.phaseDegrees, channel.quality, view.source,
      entry.label || channel.channelId, entry.siteNumber, entry.row == null ? "" : entry.row + 1,
      entry.column == null ? "" : entry.column + 1, entry.routeMismatch ? "mismatch" : ""]);
  }
  return "\uFEFF" + rows.map(row => row.map(cell).join(",")).join("\r\n");
}
