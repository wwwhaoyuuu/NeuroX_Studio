const DEFAULT_COLORS = [
  "#42d3b0", "#69b9ff", "#f3b64b", "#ff7d6e",
  "#9b8cff", "#55c7d9", "#a8d35f", "#f38ab7"
];

function safeText(value, fallback = "") {
  return typeof value === "string" && value.trim() ? value.trim() : fallback;
}

function validColor(value, fallback) {
  return /^#[0-9a-f]{6}$/i.test(value || "") ? value : fallback;
}

export function normalizeChannelSettings(profile, stored = {}) {
  const result = {};
  profile.channels.forEach((channel, index) => {
    const candidate = stored?.[channel.id] || {};
    // Older layouts describe different electrode locations; retain aliases/colors, not stale coordinates.
    const rebaseProbeMap = Boolean(profile.probeMapVersion && candidate.probeMapVersion !== profile.probeMapVersion);
    result[channel.id] = {
      label: safeText(rebaseProbeMap && candidate.label === channel.id ? channel.label : candidate.label, channel.label).slice(0, 32),
      color: validColor(candidate.color, DEFAULT_COLORS[index % DEFAULT_COLORS.length]),
      enabled: candidate.enabled !== false,
      row: Math.min(profile.rows - 1, Math.max(0, Math.floor(Number(rebaseProbeMap ? channel.row : candidate.row ?? channel.row)))),
      column: Math.min(profile.columns - 1, Math.max(0, Math.floor(Number(rebaseProbeMap ? channel.column : candidate.column ?? channel.column)))),
      ...(profile.probeMapVersion ? { probeMapVersion: profile.probeMapVersion } : {})
    };
  });
  return result;
}

export function configuredProfile(profile, settings = {}) {
  const normalized = normalizeChannelSettings(profile, settings);
  const channels = profile.channels.map((channel) => ({ ...channel, ...normalized[channel.id] }));
  const cells = Array.from({ length: profile.rows * profile.columns }, () => null);
  const overflow = [];
  for (const channel of channels) {
    const index = channel.row * profile.columns + channel.column;
    if (cells[index]) overflow.push(channel);
    else cells[index] = channel;
  }
  for (const channel of overflow) {
    const originalIndex = channel.cellIndex;
    const available = cells[originalIndex] ? cells.findIndex((cell) => !cell) : originalIndex;
    if (available >= 0) {
      channel.row = Math.floor(available / profile.columns);
      channel.column = available % profile.columns;
      cells[available] = channel;
    }
  }
  return { ...profile, channels, cells, channelCount: channels.length };
}

function csvCell(value) {
  const text = String(value ?? "");
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

export function buildCsv(rows, columns) {
  const definitions = columns || Object.keys(rows?.[0] || {}).map((key) => ({ key, label: key }));
  return [
    definitions.map((column) => csvCell(column.label)).join(","),
    ...(rows || []).map((row) => definitions.map((column) => csvCell(row[column.key])).join(","))
  ].join("\r\n");
}

function escapeXml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

export function buildExcelWorkbook(sheets) {
  const worksheets = (sheets || []).map((sheet) => {
    const rows = sheet.rows || [];
    const columns = sheet.columns || Object.keys(rows[0] || {}).map((key) => ({ key, label: key }));
    const header = `<Row>${columns.map((column) => `<Cell><Data ss:Type="String">${escapeXml(column.label)}</Data></Cell>`).join("")}</Row>`;
    const body = rows.map((row) => `<Row>${columns.map((column) => {
      const value = row[column.key];
      const numeric = typeof value === "number" && Number.isFinite(value);
      return `<Cell><Data ss:Type="${numeric ? "Number" : "String"}">${escapeXml(value)}</Data></Cell>`;
    }).join("")}</Row>`).join("");
    return `<Worksheet ss:Name="${escapeXml(safeText(sheet.name, "Sheet").slice(0, 31))}"><Table>${header}${body}</Table></Worksheet>`;
  }).join("");
  return `<?xml version="1.0"?><?mso-application progid="Excel.Sheet"?><Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">${worksheets}</Workbook>`;
}

export function buildHtmlReport(title, sections) {
  const body = (sections || []).map((section) => {
    const rows = section.rows || [];
    const columns = section.columns || Object.keys(rows[0] || {}).map((key) => ({ key, label: key }));
    return `<section><h2>${escapeXml(section.name)}</h2><table><thead><tr>${columns.map((column) => `<th>${escapeXml(column.label)}</th>`).join("")}</tr></thead><tbody>${rows.map((row) => `<tr>${columns.map((column) => `<td>${escapeXml(row[column.key])}</td>`).join("")}</tr>`).join("")}</tbody></table></section>`;
  }).join("");
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>${escapeXml(title)}</title><style>body{font:14px system-ui;margin:32px;color:#263238}h1{font-size:24px}h2{margin-top:28px;font-size:16px}table{border-collapse:collapse;width:100%}th,td{padding:7px 9px;border:1px solid #ccd4d6;text-align:left}th{background:#eef2f3}</style></head><body><h1>${escapeXml(title)}</h1>${body}</body></html>`;
}
