// Canonical 8x8 site layout and zero-based B/C port-local channel mapping.
export const PROBE_64_MAP_VERSION = 2;
export const PROBE_64_SITES = Object.freeze([
  9, 8, 5, 1, 62, 58, 55, 54,
  10, 11, 6, 2, 61, 57, 52, 53,
  12, 13, 7, 3, 60, 56, 50, 51,
  14, 15, 4, 0, 63, 59, 48, 49,
  17, 16, 27, 31, 32, 36, 47, 46,
  19, 18, 24, 28, 35, 39, 45, 44,
  21, 20, 25, 29, 34, 38, 43, 42,
  22, 23, 26, 30, 33, 37, 40, 41
]);

const PORT_SITES = Object.freeze({
  B: Object.freeze(Array.from({ length: 32 }, (_, channel) => channel)),
  C: Object.freeze(Array.from({ length: 32 }, (_, channel) => channel + 32))
});

export const PROBE_64_CHANNELS = Object.freeze(
  Object.entries(PORT_SITES).flatMap(([port, sites]) => sites.map((siteNumber, channel) =>
    Object.freeze({ port, channel, siteNumber, id: `${port}-${String(channel).padStart(3, "0")}` })
  ))
);

const bySite = new Map(PROBE_64_CHANNELS.map((entry) => [entry.siteNumber, entry]));
if (PROBE_64_SITES.length !== 64 || new Set(PROBE_64_SITES).size !== 64 ||
    PROBE_64_CHANNELS.length !== 64 || bySite.size !== 64 ||
    PROBE_64_SITES.some((site) => !bySite.has(site))) {
  throw new Error("Probe map must contain each of the 64 electrodes exactly once");
}

export const PROBE_64_CELLS = Object.freeze(PROBE_64_SITES.map((siteNumber) =>
  Object.freeze({ label: bySite.get(siteNumber).id,
    displayLabel: `${bySite.get(siteNumber).port}-${String(bySite.get(siteNumber).channel).padStart(2, "0")}`,
    siteNumber })
));

export function probeChannelForSite(siteNumber) {
  return bySite.get(siteNumber) || null;
}

export function probeSiteForChannel(port, channel) {
  return PORT_SITES[port]?.[channel] ?? null;
}
