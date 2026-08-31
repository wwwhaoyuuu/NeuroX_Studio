import { bridge } from "./bridge.js";
import {
  DEFAULT_MEA_PROFILE_ID,
  MEA_PROFILES,
  STIM_PARAMETER_PRESETS,
  channelSelectionPreset,
  getMeaProfile,
  getStimParameterPreset,
  normalizeChannelSelection
} from "./mea.js";
import { WaveformRenderer } from "./waveform.js";

const byId = (id) => document.getElementById(id);
const renderer = new WaveformRenderer(byId("waveform"), byId("channel-labels"));
const state = {
  status: null,
  pipelineRegistry: null,
  pipelineDraft: null,
  pipelinePresets: [],
  expandedNodes: new Set(),
  latestEventId: 0,
  events: [],
  eventFilter: "all",
  displayPaused: false,
  meaProfileId: DEFAULT_MEA_PROFILE_ID,
  stimConfigs: {},
  stimDraft: null,
  gameFocusChannel: null,
  pending: new Set()
};

async function init() {
  byId("recording-path").value = bridge.defaultRecordingPath();
  initializeMeaProfiles();
  bindControls();
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
  await initializePipeline();
  refreshEvents();
  refreshWaveform();
  window.setInterval(refreshStatus, 500);
  window.setInterval(refreshEvents, 650);
  window.setInterval(refreshWaveform, 50);
}

function bindControls() {
  byId("scan-device").addEventListener("click", () => runAction("scan", async () => {
    await bridge.invoke("scan_device", { request: deviceRequest() });
    toast("设备扫描完成", "success");
  }));

  byId("start-acquisition").addEventListener("click", () => runAction("start", async () => {
    await bridge.invoke("start_acquisition", {
      request: {
        ...deviceRequest(),
        blocksPerRead: Number(byId("blocks-per-read").value),
        queueCapacity: Number(byId("queue-capacity").value)
      }
    });
    toast("采集已启动", "success");
  }));

  byId("stop-acquisition").addEventListener("click", () => runAction("stop", async () => {
    await bridge.invoke("stop_acquisition");
    toast("采集已停止", "info");
  }));

  byId("record-toggle").addEventListener("click", () => runAction("record", async () => {
    if (state.status?.recording) {
      const result = await bridge.invoke("stop_recording");
      byId("record-blocks").textContent = `${formatNumber(result.blocks)} blocks`;
      toast("记录文件已完成", "success");
    } else {
      const path = byId("recording-path").value.trim();
      await bridge.invoke("start_recording", { path });
      toast("开始写入原始数据", "record");
    }
  }));

  byId("emergency-stop").addEventListener("click", () => runAction("emergency", async () => {
    await bridge.invoke("emergency_stop");
    toast("急停命令已发送，刺激保持禁用", "critical");
  }));

  byId("add-pipeline-node").addEventListener("click", addPipelineNode);
  byId("pipeline-node-list").addEventListener("click", handlePipelineNodeAction);
  byId("pipeline-node-list").addEventListener("input", handlePipelineNodeInput);
  byId("pipeline-node-list").addEventListener("change", handlePipelineNodeChange);
  byId("pipeline-preset").addEventListener("change", renderPresetActions);
  byId("load-pipeline-preset").addEventListener("click", loadSelectedPreset);
  byId("save-pipeline-preset").addEventListener("click", openPipelinePresetDialog);
  byId("delete-pipeline-preset").addEventListener("click", deletePipelinePreset);
  byId("pipeline-preset-form").addEventListener("submit", savePipelinePreset);
  byId("cancel-pipeline-preset").addEventListener("click", () => byId("pipeline-preset-dialog").close());

  byId("mea-profile").addEventListener("change", (event) => {
    state.meaProfileId = getMeaProfile(event.target.value).id;
    state.gameFocusChannel = null;
    persistMeaProfile();
    applyMeaProfile();
    toast(`MEA 已切换：${currentMeaProfile().name}`, "info");
  });
  byId("open-stim-config").addEventListener("click", openStimConfigDialog);
  byId("close-stim-config").addEventListener("click", closeStimConfigDialog);
  byId("cancel-stim-config").addEventListener("click", closeStimConfigDialog);
  byId("stim-config-form").addEventListener("submit", saveStimConfig);
  byId("stim-mea-grid").addEventListener("click", handleStimChannelClick);
  document.querySelector(".selection-presets").addEventListener("click", handleStimSelectionPreset);
  byId("stim-parameter-preset").addEventListener("change", applyStimParameterPreset);
  document.querySelector(".stim-form-grid").addEventListener("input", markStimPresetCustom);
  byId("stim-charge-recovery").addEventListener("change", markStimPresetCustom);
  byId("game-mea-grid").addEventListener("click", handleGameChannelClick);

  byId("display-gain").addEventListener("input", (event) => {
    renderer.setGain(event.target.value);
    byId("gain-label").textContent = `${Number(event.target.value).toFixed(2)}×`;
  });
  byId("time-window").addEventListener("change", (event) => {
    renderer.setTimeWindow(event.target.value);
  });
  byId("pause-display").addEventListener("click", () => {
    state.displayPaused = !state.displayPaused;
    renderer.setPaused(state.displayPaused);
    byId("pause-display").classList.toggle("active", state.displayPaused);
    byId("pause-display").title = state.displayPaused ? "继续波形显示" : "暂停波形显示";
    byId("pause-display").innerHTML = `<i data-lucide="${state.displayPaused ? "play" : "pause"}"></i>`;
    window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
  });
  byId("fit-waveform").addEventListener("click", () => {
    byId("display-gain").value = "1";
    byId("gain-label").textContent = "1.00×";
    renderer.setGain(1);
  });

  document.querySelectorAll(".tab").forEach((tab) => {
    tab.addEventListener("click", () => switchView(tab.dataset.view));
  });
  document.querySelectorAll(".filter-chip").forEach((chip) => {
    chip.addEventListener("click", () => {
      state.eventFilter = chip.dataset.level;
      document.querySelectorAll(".filter-chip").forEach((item) => item.classList.toggle("active", item === chip));
      renderEvents();
    });
  });
  byId("clear-events").addEventListener("click", () => {
    state.events = [];
    renderEvents();
  });
  byId("toggle-console").addEventListener("click", () => {
    document.querySelector(".event-console").classList.toggle("collapsed");
  });
}

const meaProfileKey = "rhs-studio.mea-profile.v1";
const stimConfigKey = "rhs-studio.stim-configs.v1";

function initializeMeaProfiles() {
  const select = byId("mea-profile");
  select.replaceChildren();
  for (const profile of MEA_PROFILES) {
    const option = document.createElement("option");
    option.value = profile.id;
    option.textContent = `${profile.name}${profile.prototype ? " · 原型" : ""}`;
    select.appendChild(option);
  }
  state.meaProfileId = getMeaProfile(localStorage.getItem(meaProfileKey)).id;
  state.stimConfigs = readStimConfigs();
  populateStimParameterPresets();
  applyMeaProfile();
}

function currentMeaProfile() {
  return getMeaProfile(state.meaProfileId);
}

function persistMeaProfile() {
  try {
    localStorage.setItem(meaProfileKey, state.meaProfileId);
  } catch (_) {
    // The profile still applies for the current session when storage is unavailable.
  }
}

function readStimConfigs() {
  try {
    const configs = JSON.parse(localStorage.getItem(stimConfigKey) || "{}");
    return configs && typeof configs === "object" && !Array.isArray(configs) ? configs : {};
  } catch (_) {
    return {};
  }
}

function persistStimConfigs() {
  localStorage.setItem(stimConfigKey, JSON.stringify(state.stimConfigs));
}

function defaultStimConfig(profile) {
  const preset = getStimParameterPreset("bench-single-v1");
  return {
    profileId: profile.id,
    selectedChannels: profile.channels.length ? [profile.channels[0].id] : [],
    presetId: preset.id,
    waveform: preset.waveform,
    amplitudeNa: preset.amplitudeNa,
    phaseWidthUs: preset.phaseWidthUs,
    interphaseUs: preset.interphaseUs,
    frequencyHz: preset.frequencyHz,
    pulseCount: preset.pulseCount,
    chargeRecovery: preset.chargeRecovery
  };
}

function activeStimConfig(profile = currentMeaProfile()) {
  const stored = state.stimConfigs[profile.id];
  if (!stored) {
    state.stimConfigs[profile.id] = defaultStimConfig(profile);
  } else {
    stored.profileId = profile.id;
    stored.selectedChannels = [...normalizeChannelSelection(profile, stored.selectedChannels)];
  }
  return state.stimConfigs[profile.id];
}

function populateStimParameterPresets() {
  const select = byId("stim-parameter-preset");
  select.replaceChildren();
  for (const preset of STIM_PARAMETER_PRESETS) {
    const option = document.createElement("option");
    option.value = preset.id;
    option.textContent = preset.name;
    select.appendChild(option);
  }
  const custom = document.createElement("option");
  custom.value = "custom";
  custom.textContent = "自定义";
  select.appendChild(custom);
}

function applyMeaProfile() {
  const profile = currentMeaProfile();
  const config = activeStimConfig(profile);
  byId("mea-profile").value = profile.id;
  byId("source-node-summary").textContent = `${profile.channelCount} 电极 · 30 kHz${profile.prototype ? " · 原型" : ""}`;
  byId("plot-empty").querySelector("span").textContent = `${profile.name} · 当前数据后端 16 lanes`;
  byId("waveform").setAttribute("aria-label", `${profile.name} 实时波形`);
  renderer.setChannelNames(profile.channels.map((channel) => channel.label));
  if (!state.gameFocusChannel || !profile.channels.some((channel) => channel.id === state.gameFocusChannel)) {
    state.gameFocusChannel = config.selectedChannels[0] || profile.channels[0]?.id || null;
  }
  renderStimSummary();
  renderGameMap();
}

function renderStimSummary() {
  const config = activeStimConfig();
  const selected = config.selectedChannels.length;
  byId("stim-channel-summary").textContent = selected === 0
    ? "未选择"
    : selected === 1 ? config.selectedChannels[0] : `${selected} 通道`;
  byId("stim-amplitude-summary").textContent = `${formatNumber(config.amplitudeNa)} nA`;
  byId("stim-pulse-summary").textContent = formatNumber(config.pulseCount);
  byId("stim-width-summary").textContent = `${formatNumber(config.phaseWidthUs)} µs`;
}

function openStimConfigDialog() {
  state.stimDraft = deepClone(activeStimConfig());
  renderStimDialog();
  byId("stim-config-dialog").showModal();
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
}

function closeStimConfigDialog() {
  state.stimDraft = null;
  byId("stim-config-dialog").close();
}

function renderStimDialog() {
  const profile = currentMeaProfile();
  const draft = state.stimDraft || deepClone(activeStimConfig(profile));
  state.stimDraft = draft;
  const selected = normalizeChannelSelection(profile, draft.selectedChannels);
  draft.selectedChannels = [...selected];
  byId("stim-dialog-profile").textContent = `${profile.name} · ${profile.rows}×${profile.columns}`;
  byId("stim-selected-count").textContent = `${selected.size} / ${profile.channelCount}`;
  byId("stim-array-source").textContent = profile.provenance;
  byId("stim-last-channel").textContent = draft.selectedChannels.at(-1) || "—";
  renderMeaGrid(byId("stim-mea-grid"), profile, {
    selected,
    interactive: true,
    mode: "stim"
  });
  renderStimParameterFields(draft);
}

function renderStimParameterFields(config) {
  byId("stim-parameter-preset").value = STIM_PARAMETER_PRESETS.some((preset) => preset.id === config.presetId) ? config.presetId : "custom";
  byId("stim-waveform").value = config.waveform;
  byId("stim-amplitude").value = String(config.amplitudeNa);
  byId("stim-phase-width").value = String(config.phaseWidthUs);
  byId("stim-interphase").value = String(config.interphaseUs);
  byId("stim-frequency").value = String(config.frequencyHz);
  byId("stim-pulse-count").value = String(config.pulseCount);
  byId("stim-charge-recovery").checked = Boolean(config.chargeRecovery);
}

function captureStimParameterFields(config) {
  return {
    ...config,
    presetId: byId("stim-parameter-preset").value,
    waveform: byId("stim-waveform").value,
    amplitudeNa: clampNumber(byId("stim-amplitude"), 0, 1_000_000, 10),
    phaseWidthUs: clampNumber(byId("stim-phase-width"), 1, 1_000_000, 200),
    interphaseUs: clampNumber(byId("stim-interphase"), 0, 1_000_000, 100),
    frequencyHz: clampNumber(byId("stim-frequency"), 0.1, 1_000, 10),
    pulseCount: Math.round(clampNumber(byId("stim-pulse-count"), 1, 10_000, 1)),
    chargeRecovery: byId("stim-charge-recovery").checked
  };
}

function clampNumber(input, minimum, maximum, fallback) {
  const entered = Number(input.value);
  const value = Number.isFinite(entered) ? entered : fallback;
  return Math.min(maximum, Math.max(minimum, value));
}

function handleStimChannelClick(event) {
  const button = event.target.closest("button[data-channel-id]");
  if (!button || !state.stimDraft) return;
  state.stimDraft = captureStimParameterFields(state.stimDraft);
  const selected = new Set(state.stimDraft.selectedChannels);
  if (selected.has(button.dataset.channelId)) selected.delete(button.dataset.channelId);
  else selected.add(button.dataset.channelId);
  state.stimDraft.selectedChannels = [...selected];
  renderStimDialog();
}

function handleStimSelectionPreset(event) {
  const button = event.target.closest("button[data-selection-preset]");
  if (!button || !state.stimDraft) return;
  state.stimDraft = captureStimParameterFields(state.stimDraft);
  state.stimDraft.selectedChannels = [...channelSelectionPreset(currentMeaProfile(), button.dataset.selectionPreset)];
  renderStimDialog();
}

function applyStimParameterPreset(event) {
  if (!state.stimDraft || event.target.value === "custom") return;
  const preset = getStimParameterPreset(event.target.value);
  state.stimDraft = {
    ...state.stimDraft,
    presetId: preset.id,
    waveform: preset.waveform,
    amplitudeNa: preset.amplitudeNa,
    phaseWidthUs: preset.phaseWidthUs,
    interphaseUs: preset.interphaseUs,
    frequencyHz: preset.frequencyHz,
    pulseCount: preset.pulseCount,
    chargeRecovery: preset.chargeRecovery
  };
  renderStimParameterFields(state.stimDraft);
}

function markStimPresetCustom() {
  if (!state.stimDraft) return;
  byId("stim-parameter-preset").value = "custom";
  state.stimDraft.presetId = "custom";
}

function saveStimConfig(event) {
  event.preventDefault();
  if (!state.stimDraft) return;
  const profile = currentMeaProfile();
  const saved = captureStimParameterFields(state.stimDraft);
  saved.profileId = profile.id;
  saved.selectedChannels = [...normalizeChannelSelection(profile, saved.selectedChannels)];
  state.stimConfigs[profile.id] = saved;
  try {
    persistStimConfigs();
    state.stimDraft = null;
    byId("stim-config-dialog").close();
    state.gameFocusChannel = saved.selectedChannels[0] || state.gameFocusChannel;
    renderStimSummary();
    renderGameMap();
    toast(`刺激配置已保存 · ${saved.selectedChannels.length} 通道 · 输出仍锁定`, "success");
  } catch (error) {
    toast(`刺激配置保存失败：${normalizeError(error)}`, "error");
  }
}

function renderGameMap() {
  const profile = currentMeaProfile();
  const selected = new Set(activeStimConfig(profile).selectedChannels);
  byId("game-profile-name").textContent = profile.name;
  byId("game-array-size").textContent = `${profile.rows}×${profile.columns}`;
  byId("game-channel-count").textContent = formatNumber(profile.channelCount);
  byId("game-stim-count").textContent = formatNumber(selected.size);
  byId("game-focus-channel").textContent = state.gameFocusChannel || "—";
  renderMeaGrid(byId("game-mea-grid"), profile, {
    selected,
    focused: state.gameFocusChannel,
    interactive: true,
    mode: "game"
  });
}

function handleGameChannelClick(event) {
  const button = event.target.closest("button[data-channel-id]");
  if (!button) return;
  state.gameFocusChannel = button.dataset.channelId;
  renderGameMap();
}

function renderMeaGrid(container, profile, options = {}) {
  container.style.setProperty("--mea-cols", String(profile.columns));
  container.style.setProperty("--mea-rows", String(profile.rows));
  container.replaceChildren();
  for (const channel of profile.cells) {
    if (!channel) {
      const empty = document.createElement("span");
      empty.className = "mea-cell empty";
      empty.setAttribute("aria-hidden", "true");
      container.appendChild(empty);
      continue;
    }
    const cell = document.createElement(options.interactive ? "button" : "span");
    if (cell instanceof HTMLButtonElement) cell.type = "button";
    cell.className = `mea-cell port-${channel.port.toLowerCase()}`;
    cell.dataset.channelId = channel.id;
    cell.textContent = channel.label;
    cell.title = `${channel.label} · row ${channel.row + 1}, column ${channel.column + 1}`;
    if (options.selected?.has(channel.id)) cell.classList.add("is-selected");
    if (options.focused === channel.id) cell.classList.add("is-focused");
    if (options.mode === "stim") cell.setAttribute("aria-pressed", String(options.selected?.has(channel.id) || false));
    container.appendChild(cell);
  }
}

function deviceRequest() {
  return {
    bitfile: byId("bitfile-path").value.trim() || null,
    serial: byId("device-serial").value.trim() || null
  };
}

async function initializePipeline() {
  try {
    const [registry, status] = await Promise.all([
      bridge.invoke("get_pipeline_registry"),
      bridge.invoke("get_status")
    ]);
    state.pipelineRegistry = registry;
    state.pipelineDraft = migratePipelineConfig(status.pipeline);
    state.pipelinePresets = readPipelinePresets();
    state.status = status;
    populatePipelineKinds();
    renderPipelinePresets();
    renderPipelineEditor();
    renderStatus(status);
    setPipelineValidation("valid", "配置有效 · SampleBlock · 30 kHz · 16 ch");
  } catch (error) {
    setPipelineValidation("invalid", normalizeError(error));
    renderConnectionError(normalizeError(error));
  }
}

function populatePipelineKinds() {
  const select = byId("pipeline-node-kind");
  select.replaceChildren();
  for (const definition of state.pipelineRegistry?.nodes || []) {
    const option = document.createElement("option");
    option.value = definition.kind;
    option.textContent = definition.label;
    select.appendChild(option);
  }
  byId("add-pipeline-node").disabled = select.options.length === 0;
}

function addPipelineNode() {
  const definition = pipelineDefinition(byId("pipeline-node-kind").value);
  if (!definition || !state.pipelineDraft) return;
  const currentCount = state.pipelineDraft.nodes.filter((node) => node.kind === definition.kind).length;
  if (currentCount >= definition.maxInstances) {
    toast(`${definition.label}最多允许 ${definition.maxInstances} 个实例`, "error");
    return;
  }
  const node = deepClone(definition.defaultNode);
  node.id = nextNodeId(definition.kind);
  state.pipelineDraft.nodes.push(node);
  state.expandedNodes.add(node.id);
  renderPipelineEditor();
  updatePipeline();
}

function nextNodeId(kind) {
  const ids = new Set(state.pipelineDraft.nodes.map((node) => node.id));
  let suffix = 1;
  while (ids.has(`${kind}-${suffix}`)) suffix += 1;
  return `${kind}-${suffix}`;
}

function handlePipelineNodeAction(event) {
  const button = event.target.closest("button[data-action]");
  if (!button || !state.pipelineDraft) return;
  const index = state.pipelineDraft.nodes.findIndex((node) => node.id === button.dataset.nodeId);
  if (index < 0) return;
  switch (button.dataset.action) {
    case "expand":
      if (state.expandedNodes.has(button.dataset.nodeId)) state.expandedNodes.delete(button.dataset.nodeId);
      else state.expandedNodes.add(button.dataset.nodeId);
      renderPipelineEditor();
      return;
    case "move-up":
      if (index === 0) return;
      [state.pipelineDraft.nodes[index - 1], state.pipelineDraft.nodes[index]] = [
        state.pipelineDraft.nodes[index],
        state.pipelineDraft.nodes[index - 1]
      ];
      break;
    case "move-down":
      if (index >= state.pipelineDraft.nodes.length - 1) return;
      [state.pipelineDraft.nodes[index + 1], state.pipelineDraft.nodes[index]] = [
        state.pipelineDraft.nodes[index],
        state.pipelineDraft.nodes[index + 1]
      ];
      break;
    case "delete":
      state.expandedNodes.delete(button.dataset.nodeId);
      state.pipelineDraft.nodes.splice(index, 1);
      break;
    default:
      return;
  }
  renderPipelineEditor();
  updatePipeline();
}

function handlePipelineNodeChange(event) {
  const control = event.target.closest("[data-node-id][data-field]");
  if (!control || !state.pipelineDraft) return;
  const node = applyPipelineControl(control);
  if (!node) return;
  renderPipelineEditor();
  updatePipeline();
}

function handlePipelineNodeInput(event) {
  const control = event.target.closest('input[type="number"][data-node-id][data-field]');
  if (!control || !state.pipelineDraft) return;
  const node = applyPipelineControl(control);
  if (!node) return;
  const summary = control.closest(".pipeline-config-node")?.querySelector(".node-copy span");
  if (summary) summary.textContent = pipelineNodeSummary(node);
  updatePipeline();
}

function applyPipelineControl(control) {
  const node = state.pipelineDraft.nodes.find((candidate) => candidate.id === control.dataset.nodeId);
  if (!node) return null;
  if (control.dataset.field === "enabled") {
    node.enabled = control.checked;
  } else {
    const definition = pipelineDefinition(node.kind);
    const parameter = definition?.parameters.find((candidate) => candidate.key === control.dataset.field);
    if (!parameter) return null;
    if (parameter.control === "number") {
      const entered = Number(control.value);
      if (!Number.isFinite(entered)) return null;
      const clamped = Math.min(parameter.maximum, Math.max(parameter.minimum, entered));
      const value = clamped - (parameter.displayOffset || 0);
      node[parameter.key] = integerPipelineFields.has(parameter.key) ? Math.round(value) : value;
    } else {
      node[parameter.key] = control.value;
    }
  }
  return node;
}

const integerPipelineFields = new Set([
  "ttlMask",
  "blankSamples",
  "channelIndex",
  "threshold",
  "refractorySamples"
]);

function renderPipelineEditor() {
  if (!state.pipelineDraft || !state.pipelineRegistry) return;
  const list = byId("pipeline-node-list");
  list.replaceChildren();
  state.pipelineDraft.nodes.forEach((node, index) => {
    const definition = pipelineDefinition(node.kind);
    if (!definition) return;
    const expanded = state.expandedNodes.has(node.id);
    const article = document.createElement("article");
    article.className = `pipeline-node pipeline-config-node ${node.enabled ? "" : "disabled"}`;
    article.dataset.nodeId = node.id;
    article.innerHTML = `
      <div class="pipeline-node-main">
        <span class="node-order">${index + 1}</span>
        <div class="node-symbol ${escapeHtml(definition.tone)}"><i data-lucide="${escapeHtml(definition.icon)}"></i></div>
        <div class="node-copy"><strong>${escapeHtml(definition.label)}</strong><span>${escapeHtml(pipelineNodeSummary(node))}</span></div>
        <label class="node-switch" title="${node.enabled ? "禁用节点" : "启用节点"}">
          <input class="switch-input" type="checkbox" data-node-id="${escapeHtml(node.id)}" data-field="enabled" ${node.enabled ? "checked" : ""} />
          <span class="switch" aria-hidden="true"></span>
        </label>
        <button class="node-action" data-action="expand" data-node-id="${escapeHtml(node.id)}" title="${expanded ? "收起参数" : "编辑参数"}" aria-expanded="${expanded}"><i data-lucide="${expanded ? "chevron-up" : "settings-2"}"></i></button>
      </div>
      <div class="node-parameter-panel" ${expanded ? "" : "hidden"}>
        <div class="node-parameter-grid">${definition.parameters.map((parameter) => pipelineParameterMarkup(node, parameter)).join("")}</div>
        <div class="node-footer">
          <span>${escapeHtml(definition.inputType)} → ${escapeHtml(definition.outputType)}</span>
          <div class="node-order-actions">
            <button class="node-action" data-action="move-up" data-node-id="${escapeHtml(node.id)}" title="上移" ${index === 0 ? "disabled" : ""}><i data-lucide="arrow-up"></i></button>
            <button class="node-action" data-action="move-down" data-node-id="${escapeHtml(node.id)}" title="下移" ${index === state.pipelineDraft.nodes.length - 1 ? "disabled" : ""}><i data-lucide="arrow-down"></i></button>
            <button class="node-action danger" data-action="delete" data-node-id="${escapeHtml(node.id)}" title="删除节点"><i data-lucide="trash-2"></i></button>
          </div>
        </div>
      </div>`;
    list.appendChild(article);
  });
  byId("pipeline-node-count").textContent = `${state.pipelineDraft.nodes.length + 1} 节点`;
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
}

function pipelineParameterMarkup(node, parameter) {
  const value = node[parameter.key];
  const label = `${parameter.label}${parameter.unit ? ` · ${parameter.unit}` : ""}`;
  if (parameter.control === "select") {
    const options = parameter.choices.map((choice) =>
      `<option value="${escapeHtml(choice.value)}" ${choice.value === value ? "selected" : ""}>${escapeHtml(choice.label)}</option>`
    ).join("");
    return `<label class="node-field"><span>${escapeHtml(label)}</span><select data-node-id="${escapeHtml(node.id)}" data-field="${escapeHtml(parameter.key)}">${options}</select></label>`;
  }
  const displayed = Number(value) + (parameter.displayOffset || 0);
  return `<label class="node-field"><span>${escapeHtml(label)}</span><input type="number" min="${parameter.minimum}" max="${parameter.maximum}" step="${parameter.step}" value="${displayed}" data-node-id="${escapeHtml(node.id)}" data-field="${escapeHtml(parameter.key)}" /></label>`;
}

function pipelineNodeSummary(node) {
  switch (node.kind) {
    case "artifact-blanking": return `TTL ${node.ttlMask} · ${(node.blankSamples / 30).toFixed(1)} ms`;
    case "notch": return `${node.frequencyHz} Hz · Q ${node.qualityFactor}`;
    case "band-pass": return `${node.lowCutHz}–${node.highCutHz} Hz`;
    case "threshold": return `CH ${node.channelIndex + 1} · ${node.threshold}`;
    default: return node.kind;
  }
}

function pipelineDefinition(kind) {
  return state.pipelineRegistry?.nodes.find((definition) => definition.kind === kind);
}

function migratePipelineConfig(config) {
  const targetVersion = state.pipelineRegistry?.schemaVersion;
  if (config?.version === targetVersion && Array.isArray(config.nodes)) {
    const unsupported = config.nodes.find((node) => !pipelineDefinition(node.kind));
    if (unsupported) throw new Error(`方案包含当前版本不支持的节点：${unsupported.kind}`);
    return deepClone(config);
  }
  if (config && "notchEnabled" in config) {
    const nodes = state.pipelineRegistry.nodes.map((definition) => deepClone(definition.defaultNode));
    const kindCounts = new Map();
    for (const node of nodes) {
      const count = (kindCounts.get(node.kind) || 0) + 1;
      kindCounts.set(node.kind, count);
      node.id = `${node.kind}-${count}`;
      if (node.kind === "notch") node.enabled = Boolean(config.notchEnabled);
      if (node.kind === "band-pass") node.enabled = Boolean(config.bandPassEnabled);
      if (node.kind === "artifact-blanking") node.enabled = Boolean(config.artifactBlankingEnabled);
      if (node.kind === "threshold") {
        node.enabled = Boolean(config.thresholdEnabled);
        node.channelIndex = Number(config.thresholdChannel || 0);
        node.threshold = Number(config.thresholdValue ?? 40_000);
      }
    }
    return { version: targetVersion, nodes };
  }
  throw new Error(`无法载入管线版本 ${config?.version ?? "未知"}`);
}

const pipelinePresetKey = "rhs-studio.pipeline-presets.v1";

function readPipelinePresets() {
  try {
    const value = JSON.parse(localStorage.getItem(pipelinePresetKey) || "[]");
    return Array.isArray(value) ? value.filter((preset) => preset?.name && preset?.config) : [];
  } catch (_) {
    return [];
  }
}

function persistPipelinePresets() {
  localStorage.setItem(pipelinePresetKey, JSON.stringify(state.pipelinePresets));
}

function renderPipelinePresets(selectedName = "") {
  const select = byId("pipeline-preset");
  select.innerHTML = '<option value="">未保存方案</option>';
  for (const preset of state.pipelinePresets) {
    const option = document.createElement("option");
    option.value = preset.name;
    option.textContent = preset.name;
    select.appendChild(option);
  }
  select.value = selectedName;
  renderPresetActions();
}

function renderPresetActions() {
  const selected = Boolean(byId("pipeline-preset").value);
  byId("load-pipeline-preset").disabled = !selected;
  byId("delete-pipeline-preset").disabled = !selected;
}

function openPipelinePresetDialog() {
  if (!state.pipelineDraft) return;
  const selected = byId("pipeline-preset").value;
  const suggested = selected || `实验方案 ${state.pipelinePresets.length + 1}`;
  byId("pipeline-preset-name").value = suggested;
  byId("pipeline-preset-dialog").showModal();
  byId("pipeline-preset-name").focus();
  byId("pipeline-preset-name").select();
}

function savePipelinePreset(event) {
  event.preventDefault();
  if (!state.pipelineDraft) return;
  const name = byId("pipeline-preset-name").value.trim().slice(0, 40);
  if (!name) return;
  const existing = state.pipelinePresets.find((preset) => preset.name === name);
  if (existing) existing.config = deepClone(state.pipelineDraft);
  else state.pipelinePresets.push({ name, config: deepClone(state.pipelineDraft) });
  try {
    persistPipelinePresets();
    renderPipelinePresets(name);
    byId("pipeline-preset-dialog").close();
    toast(`已保存方案：${name}`, "success");
  } catch (error) {
    toast(`方案保存失败：${normalizeError(error)}`, "error");
  }
}

function loadSelectedPreset() {
  const name = byId("pipeline-preset").value;
  const preset = state.pipelinePresets.find((candidate) => candidate.name === name);
  if (!preset) return;
  try {
    state.pipelineDraft = migratePipelineConfig(preset.config);
    state.expandedNodes.clear();
    renderPipelineEditor();
    updatePipeline();
    toast(`已载入方案：${name}`, "success");
  } catch (error) {
    setPipelineValidation("invalid", normalizeError(error));
    toast(normalizeError(error), "error");
  }
}

function deletePipelinePreset() {
  const name = byId("pipeline-preset").value;
  if (!name || !window.confirm(`删除方案“${name}”？`)) return;
  state.pipelinePresets = state.pipelinePresets.filter((preset) => preset.name !== name);
  try {
    persistPipelinePresets();
    renderPipelinePresets();
    toast(`已删除方案：${name}`, "info");
  } catch (error) {
    toast(`方案删除失败：${normalizeError(error)}`, "error");
  }
}

function setPipelineValidation(kind, message) {
  const target = byId("pipeline-validation");
  const icon = kind === "valid" ? "circle-check" : kind === "invalid" ? "circle-alert" : "loader-circle";
  target.className = `pipeline-validation ${kind}`;
  target.innerHTML = `<i data-lucide="${icon}"></i><span>${escapeHtml(message)}</span>`;
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
}

function deepClone(value) {
  return JSON.parse(JSON.stringify(value));
}

let pipelineTimer;
let pipelineUpdateRunning = false;
let pipelineUpdateQueued = false;
function updatePipeline() {
  window.clearTimeout(pipelineTimer);
  setPipelineValidation("checking", "正在检查参数与连接兼容性");
  pipelineTimer = window.setTimeout(flushPipelineUpdate, 160);
}

async function flushPipelineUpdate() {
  if (!state.pipelineDraft) return;
  if (pipelineUpdateRunning) {
    pipelineUpdateQueued = true;
    return;
  }
  pipelineUpdateRunning = true;
  document.body.classList.add("busy");
  do {
    pipelineUpdateQueued = false;
    const snapshot = deepClone(state.pipelineDraft);
    try {
      const status = await bridge.invoke("update_pipeline", { config: snapshot });
      state.status = status;
      renderStatus(status);
      setPipelineValidation("valid", "配置有效 · 已按当前顺序重建");
    } catch (error) {
      const message = normalizeError(error);
      setPipelineValidation("invalid", message);
      toast(message, "error");
    }
  } while (pipelineUpdateQueued);
  pipelineUpdateRunning = false;
  if (state.pending.size === 0) document.body.classList.remove("busy");
}

async function runAction(key, action, notifyError = true) {
  if (state.pending.has(key)) return;
  state.pending.add(key);
  document.body.classList.add("busy");
  try {
    await action();
    await refreshStatus();
    await refreshEvents();
  } catch (error) {
    if (notifyError) toast(normalizeError(error), "error");
  } finally {
    state.pending.delete(key);
    if (state.pending.size === 0) document.body.classList.remove("busy");
  }
}

async function refreshStatus() {
  try {
    const status = await bridge.invoke("get_status");
    state.status = status;
    renderStatus(status);
  } catch (error) {
    renderConnectionError(normalizeError(error));
  }
}

async function refreshWaveform() {
  if (state.displayPaused) return;
  try {
    const frame = await bridge.invoke("poll_waveform");
    if (!frame) return;
    renderer.setFrame(frame);
    byId("plot-empty").hidden = true;
    byId("plot-rate").textContent = `${(frame.sampleRateHz / 1000).toFixed(1)} kHz`;
  } catch (_) {
    // The low-rate status channel owns user-visible connection errors.
  }
}

async function refreshEvents() {
  try {
    const events = await bridge.invoke("poll_events", { afterId: state.latestEventId });
    if (!events.length) return;
    state.events.push(...events);
    state.events = state.events.slice(-200);
    state.latestEventId = Math.max(state.latestEventId, ...events.map((event) => event.id));
    renderEvents();
  } catch (_) {
    // Status polling reports backend failures without flooding the console.
  }
}

function renderStatus(status) {
  const active = status.mode === "acquiring" || status.mode === "recording";
  const connected = Boolean(status.device);
  document.body.classList.toggle("is-recording", status.recording);
  document.body.classList.toggle("has-fault", status.health === "fault");

  byId("connection-state").className = `connection-state ${connected ? "connected" : ""} ${bridge.isPreview ? "preview" : ""}`;
  byId("connection-label").textContent = bridge.isPreview ? "演示模式" : connected ? `${status.device.serial} · Port ${status.device.port}` : "未连接";
  byId("device-name").textContent = status.device?.chipName || "RHS2116";
  byId("device-route").textContent = status.device ? `Port ${status.device.port} · Stream ${status.device.stream}` : "等待扫描";
  byId("device-led").classList.toggle("online", connected);
  byId("mode-chip").textContent = status.mode.toUpperCase();
  byId("elapsed-time").textContent = formatDuration(status.elapsedMs);

  byId("start-acquisition").disabled = active || status.mode === "connecting";
  byId("stop-acquisition").disabled = !active && status.mode !== "fault";
  byId("record-toggle").disabled = !active;
  byId("record-toggle").classList.toggle("active", status.recording);
  byId("record-toggle").querySelector("span").textContent = status.recording ? "停止记录" : "开始记录";
  byId("record-status").textContent = status.recording ? "REC" : "OFF";
  byId("record-status").classList.toggle("active", status.recording);
  byId("health-storage").textContent = status.recording ? "写入中" : "未启用";
  byId("health-usb").textContent = active ? "流式传输" : connected ? "就绪" : "待机";
  byId("health-timeline").textContent = status.metrics.timestampGaps ? "发现断裂" : "连续";

  const metrics = status.metrics;
  byId("metric-sample-rate").textContent = metrics.observedSampleRateHz ? `${(metrics.observedSampleRateHz / 1000).toFixed(2)} kS/s` : "—";
  byId("metric-throughput").textContent = metrics.bytesRead && status.elapsedMs ? `${formatBytes(metrics.bytesRead / (status.elapsedMs / 1000))}/s` : "—";
  byId("metric-gaps").textContent = formatNumber(metrics.timestampGaps);
  byId("metric-overflows").textContent = formatNumber(metrics.queueOverflows);
  byId("metric-sequence").textContent = metrics.latestSequence ? `#${formatNumber(metrics.latestSequence)}` : "—";
  byId("perf-samples").textContent = formatNumber(metrics.samplesPerChannel);
  byId("perf-bytes").textContent = formatBytes(metrics.bytesRead);
  byId("perf-empty").textContent = formatNumber(metrics.emptyReads);
  byId("perf-threshold").textContent = formatNumber(metrics.thresholdEvents);
  byId("plot-mode").textContent = pipelineDisplayMode(status.pipeline);

  if (status.lastError) renderConnectionError(status.lastError);
}

function pipelineDisplayMode(pipeline) {
  const nodes = pipeline?.nodes || [];
  const filters = nodes.filter((node) => node.enabled && (node.kind === "band-pass" || node.kind === "notch"));
  const last = filters.at(-1);
  if (last?.kind === "band-pass") return `BP ${last.lowCutHz}–${last.highCutHz}`;
  if (last?.kind === "notch") return `NOTCH ${last.frequencyHz}`;
  return "RAW";
}

function renderConnectionError(message) {
  byId("connection-state").className = "connection-state fault";
  byId("connection-label").textContent = "需要检查";
  byId("device-route").textContent = message;
}

function renderEvents() {
  const log = byId("event-log");
  const events = state.eventFilter === "error"
    ? state.events.filter((event) => event.level === "error" || event.level === "critical")
    : state.events;
  log.replaceChildren();
  for (const event of events.slice().reverse()) {
    const row = document.createElement("div");
    row.className = `event-row level-${event.level}`;
    row.innerHTML = `
      <time>${formatEventTime(event.elapsedMs)}</time>
      <span class="event-level">${event.level.toUpperCase()}</span>
      <span class="event-category">${escapeHtml(event.category)}</span>
      <span class="event-message">${escapeHtml(event.message)}</span>`;
    log.appendChild(row);
  }
  byId("event-count").textContent = String(state.events.length);
}

function switchView(id) {
  document.querySelectorAll(".tab").forEach((tab) => tab.classList.toggle("active", tab.dataset.view === id));
  document.querySelectorAll(".view-panel").forEach((panel) => panel.classList.toggle("active", panel.id === id));
}

function toast(message, type) {
  const region = byId("toast-region");
  const item = document.createElement("div");
  item.className = `toast ${type}`;
  item.innerHTML = `<span></span><strong>${escapeHtml(message)}</strong>`;
  region.appendChild(item);
  window.setTimeout(() => item.classList.add("leaving"), 2600);
  window.setTimeout(() => item.remove(), 3000);
}

function normalizeError(error) {
  if (typeof error === "string") return error;
  if (error?.message) return error.message;
  return "操作失败";
}

function formatDuration(milliseconds) {
  const seconds = Math.floor(milliseconds / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  return [hours, minutes, remainder].map((value) => String(value).padStart(2, "0")).join(":");
}

function formatEventTime(milliseconds) {
  const seconds = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(seconds / 60);
  return `${String(minutes).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}.${String(Math.floor(milliseconds % 1000)).padStart(3, "0")}`;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const exponent = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  return `${(bytes / 1024 ** exponent).toFixed(exponent ? 1 : 0)} ${units[exponent]}`;
}

function formatNumber(value) {
  return Number(value || 0).toLocaleString("zh-CN");
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

window.addEventListener("DOMContentLoaded", init);
