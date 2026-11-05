import { getMeaProfile } from "./mea.js";
import { PROBE_64_CHANNELS } from "./probe-map.js";

export function isHardwareOutputMode(mode) {
  return ["hardware-bench", "hardware-functional", "hardware-configurable"].includes(mode);
}

export function acquisitionFaultNotice(status) {
  if (status?.mode !== "fault") return null;
  const error = String(status.lastError || "采集发生故障");
  const compliance = error.includes("RHS2116 compliance warning");
  const channels = error.match(/channels=([A-D]-\d{3}(?:,[A-D]-\d{3})*)/)?.[1];
  return {
    title: compliance ? "电流源电压余量报警 · 采集已停止" : "采集故障 · 已停止更新",
    detail: compliance
      ? `${channels ? `${channels} · ` : ""}刺激输出已禁用。保留最后一帧，非实时数据；请核对电极、回流和负载连接，勿直接重试。`
      : "保留最后一帧，非实时数据。请检查下方故障详情。",
    error
  };
}

export function stimulationConfirmations(mode, enabled, values) {
  const result = { benchConfirmation: null, outputConfirmation: null };
  if (!enabled) return result;
  const operator = String(values.operator || "").trim();
  if (mode === "configurable") {
    result.outputConfirmation = {
      operator, routingConfirmed: Boolean(values.routingConfirmed),
      parametersConfirmed: Boolean(values.parametersConfirmed),
      unverifiedOutputAcknowledged: Boolean(values.unverifiedOutputAcknowledged)
    };
  } else if (["measured", "functional", "functional-repeat"].includes(mode)) {
    result.benchConfirmation = {
      operator, sampleDisconnected: Boolean(values.sampleDisconnected),
      dummyLoadConnected: Boolean(values.dummyLoadConnected), measurementReady: Boolean(values.measurementReady),
      functionalOnly: mode.startsWith("functional"), functionalRepeat: mode === "functional-repeat"
    };
  } else {
    throw new Error("Unknown stimulation output mode");
  }
  return result;
}

export function stimulationControls(stimulation, active, busy = false) {
  const faulted = stimulation.state === "fault";
  const armed = stimulation.armId !== null && stimulation.armId !== undefined;
  const pending = stimulation.pendingCommands > 0;
  return {
    armed, pending,
    armDisabled: faulted || !active || busy || armed || pending,
    triggerDisabled: faulted || !active || busy || !armed || pending ||
      (stimulation.outputMode === "hardware-functional" &&
        (stimulation.submittedCommands ?? stimulation.completedCommands ?? 0) >= (stimulation.functionalTriggerLimit || 1)) ||
      (stimulation.outputMode === "hardware-configurable" &&
        (stimulation.submittedCommands || 0) >= (stimulation.commandLimit || 16)),
    cancelDisabled: !active || busy || !pending,
    disarmDisabled: !active || busy || !armed || pending
  };
}

export function shouldPreloadStimulation(profileId, selectedChannels) {
  return ["rhs2116-logic-16", "mea-ab-64"].includes(profileId) &&
    Array.isArray(selectedChannels) && selectedChannels.length > 0;
}

export function buildProbe64ElectrodeMap(detectedChips, streamGroups) {
  const chips = validateProbe64Chips(detectedChips);
  // Intan assigns port-local channel numbers in ascending command-stream order.
  if (streamGroups === undefined) {
    streamGroups = Object.fromEntries(["B", "C"].map((port) => [
      port, [...chips.values()].filter((chip) => chip.port === port)
        .map((chip) => chip.stream).sort((a, b) => a - b)
    ]));
  }
  if (!streamGroups || !Array.isArray(streamGroups.B) || !Array.isArray(streamGroups.C) ||
      streamGroups.B.length !== 2 || streamGroups.C.length !== 2) {
    throw new Error("Explicit B/C stream order is required for both 16-channel groups");
  }
  for (const port of ["B", "C"]) {
    const streams = streamGroups[port];
    if (new Set(streams).size !== 2 || streams.some((stream) => chips.get(stream)?.port !== port)) {
      throw new Error(`Stream assignment does not match detected port ${port}`);
    }
  }
  return PROBE_64_CHANNELS.map((entry) => ({
    electrodeId: entry.id,
    stream: streamGroups[entry.port][Math.floor(entry.channel / 16)],
    channel: entry.channel % 16
  }));
}

function validateProbe64Chips(detectedChips) {
  if (!Array.isArray(detectedChips) || detectedChips.length > 8) {
    throw new Error("Device scan must contain at most eight RHS2116 streams");
  }
  const chips = new Map();
  const seen = new Set();
  for (const chip of detectedChips) {
    if (chip?.chipName !== "RHS2116" || !["A", "B", "C", "D"].includes(chip.port) ||
        !Number.isInteger(chip.stream) || chip.stream < 0 || chip.stream > 7 ||
        Math.floor(chip.stream / 2) !== chip.port.charCodeAt(0) - 65 ||
        !Number.isInteger(chip.channels) ||
        chip.channels !== 16 || seen.has(chip.stream)) {
      throw new Error("Device scan must contain distinct, valid RHS2116 port/stream addresses");
    }
    seen.add(chip.stream);
    if (["B", "C"].includes(chip.port)) chips.set(chip.stream, chip);
  }
  if (["B", "C"].some((port) => [...chips.values()].filter((chip) => chip.port === port).length !== 2)) {
    throw new Error("MEA 64 requires four B/C RHS2116 streams, two on each port");
  }
  return chips;
}

export function resolveStimRouting(profileId, selectedChannelIds, detectedChips, electrodeMap) {
  const profile = getMeaProfile(profileId);
  if (profile.id !== profileId) throw new Error("Unknown MEA profile");
  if (profile.channelCount !== 64) throw new Error("This routing check requires the 64-channel MEA profile");
  if (!Array.isArray(selectedChannelIds) || selectedChannelIds.length === 0) {
    throw new Error("Select at least one electrode");
  }
  if (!Array.isArray(electrodeMap)) {
    throw new Error("A device scan and an explicit electrode map are required");
  }
  const chipByStream = validateProbe64Chips(detectedChips);

  const knownElectrodes = new Set(profile.channels.map((channel) => channel.id));
  const addressByElectrode = new Map();
  const usedAddresses = new Set();
  for (const entry of electrodeMap) {
    if (!entry || !knownElectrodes.has(entry.electrodeId) ||
        addressByElectrode.has(entry.electrodeId) ||
        !Number.isInteger(entry.stream) || entry.stream < 0 || entry.stream > 7 ||
        !Number.isInteger(entry.channel) ||
        entry.channel < 0 || entry.channel > 15) {
      throw new Error("Electrode map contains an unknown, duplicate, or invalid channel");
    }
    const chip = chipByStream.get(entry.stream);
    if (!chip) throw new Error(`No detected RHS2116 on stream ${entry.stream}`);
    if (!entry.electrodeId.startsWith(`${chip.port}-`)) {
      throw new Error(`Electrode ${entry.electrodeId} is routed to the wrong port`);
    }
    const key = `${entry.stream}:${entry.channel}`;
    if (usedAddresses.has(key)) throw new Error(`Duplicate physical address ${key}`);
    usedAddresses.add(key);
    addressByElectrode.set(entry.electrodeId, {
      electrodeId: entry.electrodeId,
      port: chip.port,
      stream: entry.stream,
      channel: entry.channel
    });
  }
  if (addressByElectrode.size !== 64) throw new Error("All 64 electrodes must have a physical address");

  const selected = new Set(selectedChannelIds);
  if (selected.size !== selectedChannelIds.length ||
      [...selected].some((id) => !addressByElectrode.has(id))) {
    throw new Error("Selection contains an unknown or duplicate electrode");
  }
  return profile.channels.filter((channel) => selected.has(channel.id))
    .map((channel) => addressByElectrode.get(channel.id));
}
