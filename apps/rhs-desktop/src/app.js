import { bridge } from "./bridge.js";
import { createSpikePanel } from "./spikes.js";
import { timestampRecordingPath, recordingWorkspace, playbackWorkspace, analysisExportDirectory } from "./recording.js";
import { buildImpedanceCsv, impedanceChannelOrder, impedanceControls, impedanceLayout } from "./impedance.js";
import {
  appendSignalHistory,
  buildIsiHistogram,
  buildPsth,
  buildSpectrogram,
  buildThresholdRaster,
  frameChannelActivity,
  frameChannelSeries,
  spikeActivitySummary,
  powerSpectrum
} from "./analysis.js";
import {
  DEFAULT_MEA_PROFILE_ID,
  MEA_PROFILES,
  STIM_PARAMETER_PRESETS,
  channelSelectionPreset,
  deviceDisplayName,
  getMeaProfile,
  getStimParameterPreset,
  normalizeChannelSelection
} from "./mea.js";
import { RHS_UV_PER_ADC, WaveformRenderer, normalizeWaveformRange } from "./waveform.js";
import { acquisitionFaultNotice, isHardwareOutputMode, shouldPreloadStimulation, stimulationControls, stimulationConfirmations } from "./stim-routing.js";
import {
  buildCsv,
  buildExcelWorkbook,
  buildHtmlReport,
  configuredProfile,
  normalizeChannelSettings
} from "./workspace-data.js";

const byId = (id) => document.getElementById(id);
const renderer = new WaveformRenderer(byId("waveform"), byId("channel-labels"), {
  pager: byId("channel-pager"),
  previousPageButton: byId("previous-channel-page"),
  nextPageButton: byId("next-channel-page"),
  pageLabel: byId("channel-page-label"),
  pageSize: 64,
  onFocusChannel: focusWaveformChannel,
  spikeTooltip: byId("waveform-spike-tooltip"),
  onSpikeMarkers: (count) => {
    byId("waveform-spike-count").textContent = `${count} 点`;
    byId("waveform-spike-count").hidden = !byId("waveform-spike-markers").checked;
  }
});
const state = {
  status: null,
  pipelineRegistry: null,
  pipelineDraft: null,
  pipelinePresets: [],
  expandedNodes: new Set(),
  latestEventId: 0,
  events: [],
  panels: { left: true, right: true },
  waveformRanges: {},
  displayPaused: false,
  meaProfileId: DEFAULT_MEA_PROFILE_ID,
  channelSettings: {},
  channelEditorDraft: null,
  stimConfigs: {},
  stimDraft: null,
  gameFocusChannel: null,
  activityFocusChannel: null,
  latestFrame: null,
  waveformCursor: null,
  waveformGeneration: null,
  waveformGapCount: 0,
  signalHistory: null,
  analysisChannel: 0,
  channelActivity: [],
  experimentPresets: [],
  audio: {
    enabled: false,
    context: null,
    gainNode: null,
    compressor: null,
    nextStartTime: 0,
    lastSequence: null
  },
  pending: new Set()
};
const spikePanel = createSpikePanel({ bridge, profile: currentMeaProfile,
  status: () => state.status, notify: toast, onUpdate: () => {
    renderer.setSpikeEvents(spikePanel.history);
    const batch = spikePanel.history.batch;
    renderer.setDetectionThresholds(batch);
    if (state.activityEpoch !== batch?.epoch || state.activitySample !== batch?.latestSampleIndex) {
      state.activityEpoch = batch?.epoch; state.activitySample = batch?.latestSampleIndex; state.activityClockAt = performance.now();
    }
    byId("waveform-spike-state").textContent = batch?.error ? "检测错误" : !batch?.config.enabled ? "检测未开启"
      : batch.thresholds?.some((value) => value.state === "calibrating") ? "噪声校准中" : batch.running ? "检测中" : "检测已停止";
    const threshold = batch?.thresholds?.[renderer.focusChannel];
    byId("waveform-spike-threshold").textContent = renderer.focusChannel !== null && Number.isFinite(threshold?.thresholdUv)
      ? `阈值 ${threshold.thresholdUv.toFixed(1)} µV${batch.config.bandPass ? " · 检测带通" : ""}` : "";
    if (!state.spikeAnalysisAt || performance.now() - state.spikeAnalysisAt >= 250) {
      state.spikeAnalysisAt = performance.now();
      renderEventAnalysis();
    }
  } });

async function init() {
  byId("recording-path").value = bridge.defaultRecordingPath();
  byId("trigger-recording-path").value = bridge.defaultTriggeredRecordingPath();
  byId("playback-path").value = byId("recording-path").value;
  initializeMeaProfiles();
  initializeDeviceTools();
  bindControls();
  spikePanel.bind();
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
  await initializePipeline();
  if (!restoreRecoverySnapshot()) saveRecoverySnapshot();
  if (byId("recording-auto-name").checked) byId("recording-path").value = timestampRecordingPath(byId("recording-path").value);
  if (byId("trigger-recording-auto-name").checked) byId("trigger-recording-path").value = timestampRecordingPath(byId("trigger-recording-path").value, "triggered");
  initializeExperimentPresets();
  refreshEvents();
  refreshWaveform();
  spikePanel.poll();
  window.setInterval(refreshStatus, 500);
  window.setInterval(refreshEvents, 650);
  window.setInterval(refreshWaveform, 50);
  window.setInterval(() => {
    const interval = byId("activity-view").classList.contains("active") || (!state.displayPaused && byId("waveform-view").classList.contains("active")) ? 45 : 240;
    if (!state.spikePollAt || performance.now() - state.spikePollAt >= interval) {
      state.spikePollAt = performance.now();
      spikePanel.poll();
    }
  }, 50);
  window.setInterval(() => { if (byId("activity-view").classList.contains("active")) renderActivityView(); }, 50);
}

function bindControls() {
  byId("waveform-spike-markers").addEventListener("change", (event) => {
    renderer.setSpikeMarkersVisible(event.target.checked);
    scheduleRecoverySave();
  });
  byId("waveform-spike-settings").addEventListener("click", () => {
    document.querySelector('[data-view="spike-view"]').click();
    byId("spike-enabled").focus();
  });
  byId("scan-device").addEventListener("click", () => runAction("scan", async () => {
    clearBenchConfirmation();
    await bridge.invoke("scan_device", { request: deviceRequest() });
    toast("设备扫描完成", "success");
  }));

  byId("start-acquisition").addEventListener("click", () => runAction("start", async () => {
    const preloadRhs2116 = shouldPreloadStimulation(currentMeaProfile().id, activeStimConfig().selectedChannels);
    await bridge.invoke("start_acquisition", {
      request: {
        ...deviceRequest(),
        blocksPerRead: Number(byId("blocks-per-read").value),
        queueCapacity: Number(byId("queue-capacity").value),
        channelCount: currentMeaProfile().channelCount,
        stimulationPreset: preloadRhs2116 ? stimulationConfigRequest() : null,
        metadata: sessionMetadata()
      }
    });
    toast(preloadRhs2116 ? "采集已启动，刺激参数已预装，输出仍锁定" : "采集已启动", "success");
  }));

  byId("stop-acquisition").addEventListener("click", () => runAction("stop", async () => {
    clearBenchConfirmation();
    await bridge.invoke("stop_acquisition");
    toast("采集已停止", "info");
  }));

  byId("record-toggle").addEventListener("click", () => runAction("record", async () => {
    if (state.status?.recording) {
      const result = await bridge.invoke("stop_recording");
      byId("record-blocks").textContent = `${formatNumber(result.blocks)} blocks`;
      toast("记录文件已完成", "success");
    } else {
      if (byId("recording-auto-name").checked) byId("recording-path").value = timestampRecordingPath(byId("recording-path").value);
      const path = byId("recording-path").value.trim();
      const result = await bridge.invoke("start_recording", { path, workspace: recordingWorkspace(experimentWorkspaceSnapshot()) });
      byId("recording-path").value = result.path;
      byId("playback-path").value = result.path;
      toast("开始写入原始数据", "record");
    }
  }));

  byId("trigger-recording-source").addEventListener("change", renderTriggerRecordingSource);
  byId("arm-trigger-recording").addEventListener("click", () => runAction("arm-trigger-recording", async () => {
    if (byId("trigger-recording-auto-name").checked) byId("trigger-recording-path").value = timestampRecordingPath(byId("trigger-recording-path").value, "triggered");
    const result = await bridge.invoke("arm_triggered_recording", { request: triggeredRecordingRequest() });
    byId("playback-path").value = result.path;
    toast("触发记录已就绪", "success");
  }));
  byId("fire-recording-trigger").addEventListener("click", () => runAction("fire-recording-trigger", async () => {
    await bridge.invoke("fire_software_trigger");
    toast("软件触发已发送", "record");
  }));
  byId("cancel-trigger-recording").addEventListener("click", () => runAction("cancel-trigger-recording", async () => {
    await bridge.invoke("cancel_triggered_recording");
    toast("触发记录已取消", "info");
  }));
  renderTriggerRecordingSource();

  byId("open-playback").addEventListener("click", () => runAction("open-playback", async () => {
    const path = byId("playback-path").value.trim();
    const configuration = await bridge.invoke("read_recording_parameters", { path });
    const workspace = playbackWorkspace(configuration, configuration?.channelCount);
    const status = await bridge.invoke("open_playback", { path });
    state.status = status;
    clearBenchConfirmation();
    byId("stim-real-output").checked = false;
    if (workspace) applyExperimentWorkspace(workspace);
    toast(workspace ? "记录已打开，录制起始参数已恢复，刺激输出保持关闭" : "旧记录已打开，文件不含完整工作区参数", "success");
  }));

  byId("playback-toggle").addEventListener("click", () => runAction("playback-toggle", async () => {
    if (state.status?.playback?.state === "playing") {
      await bridge.invoke("pause_playback");
      toast("回放已暂停", "info");
    } else {
      await bridge.invoke("play_playback");
      toast("开始回放", "success");
    }
  }));

  byId("close-playback").addEventListener("click", () => runAction("close-playback", async () => {
    await bridge.invoke("close_playback");
    toast("记录已关闭", "info");
  }));

  byId("playback-speed").addEventListener("change", (event) => runAction("playback-speed", async () => {
    await bridge.invoke("set_playback_speed", { speed: Number(event.target.value) });
  }));

  byId("playback-position").addEventListener("change", (event) => runAction("playback-seek", async () => {
    await bridge.invoke("seek_playback", { fraction: Number(event.target.value) / 1000 });
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
    updatePipeline();
    toast(`MEA 已切换：${currentMeaProfile().name}`, "info");
  });
  byId("open-channel-editor").addEventListener("click", openChannelEditor);
  byId("close-channel-editor").addEventListener("click", closeChannelEditor);
  byId("cancel-channel-editor").addEventListener("click", closeChannelEditor);
  byId("channel-editor-form").addEventListener("submit", saveChannelEditor);
  byId("channel-editor-rows").addEventListener("input", handleChannelEditorInput);
  byId("enable-all-channels").addEventListener("click", () => setAllChannelEditorStates(true));
  byId("disable-all-channels").addEventListener("click", () => setAllChannelEditorStates(false));
  byId("reset-channel-settings").addEventListener("click", resetChannelEditor);
  byId("open-export").addEventListener("click", openExportDialog);
  byId("open-analysis-export").addEventListener("click", openAnalysisExport);
  byId("summary-open-analysis-export").addEventListener("click", () => {
    byId("export-dialog").close();
    openAnalysisExport();
  });
  byId("cancel-analysis-export").addEventListener("click", () => byId("analysis-export-dialog").close());
  byId("analysis-export-source").addEventListener("input", () => {
    const path = byId("analysis-export-source").value.trim();
    if (path && !/[\\/]$/.test(path)) byId("analysis-export-directory").value = analysisExportDirectory(path);
  });
  byId("analysis-export-form").addEventListener("submit", exportAnalysisRecording);
  byId("cancel-export").addEventListener("click", () => byId("export-dialog").close());
  byId("export-form").addEventListener("submit", exportWorkspace);
  byId("open-stim-config").addEventListener("click", openStimConfigDialog);
  byId("stim-validation-mode").addEventListener("change", () => {
    resetStimulationConfirmations();
    if (byId("stim-validation-mode").value.startsWith("functional")) {
      byId("stim-repeat-count").value = "1";
      byId("stim-delay").value = String(Math.min(1000, Number(byId("stim-delay").value) || 0));
    }
    renderStimValidationControls();
  });
  byId("close-stim-config").addEventListener("click", closeStimConfigDialog);
  byId("cancel-stim-config").addEventListener("click", closeStimConfigDialog);
  byId("stim-config-form").addEventListener("submit", saveStimConfig);
  byId("stim-mea-grid").addEventListener("click", handleStimChannelClick);
  document.querySelector(".selection-presets").addEventListener("click", handleStimSelectionPreset);
  byId("stim-parameter-preset").addEventListener("change", applyStimParameterPreset);
  document.querySelector(".stim-form-grid").addEventListener("input", markStimPresetCustom);
  byId("stim-charge-recovery").addEventListener("change", markStimPresetCustom);
  byId("arm-stimulation").addEventListener("click", () => runAction("arm-stimulation", async () => {
    if (bridge.isPreview && byId("stim-real-output").checked) throw new Error("浏览器预览不支持真实输出");
    if (byId("stim-validation-mode").value === "configurable" && !byId("stim-real-output").checked) {
      throw new Error("可配置真实输出需要明确确认真实硬件输出");
    }
    const result = await bridge.invoke("arm_stimulation", { config: stimulationConfigRequest() });
    toast(isHardwareOutputMode(result.outputMode) ? "真实硬件输出已 Arm，等待触发" : "刺激试运行已 Arm，真实输出仍锁定", "success");
  }));
  byId("trigger-stimulation").addEventListener("click", () => runAction("trigger-stimulation", async () => {
    await bridge.invoke("trigger_stimulation", { schedule: stimulationScheduleRequest() });
    toast(isHardwareOutputMode(state.status?.stimulation?.outputMode) ? "真实刺激已提交硬件队列" : "刺激试运行已进入调度队列", "record");
  }));
  byId("cancel-stimulation").addEventListener("click", () => runAction("cancel-stimulation", async () => {
    await bridge.invoke("cancel_stimulation");
    toast("待执行刺激命令已取消", "info");
  }));
  byId("disarm-stimulation").addEventListener("click", () => runAction("disarm-stimulation", async () => {
    await bridge.invoke("disarm_stimulation");
    toast("刺激配置已解除 Arm", "info");
  }));
  byId("game-mea-grid").addEventListener("click", handleGameChannelClick);
  byId("activity-mea-grid").addEventListener("click", handleActivityChannelClick);
  byId("spike-window").addEventListener("change", renderSpikeView);
  byId("activity-metric").addEventListener("change", renderActivityView);
  for (const id of ["activity-window", "activity-flash-ms", "activity-rate-max"]) byId(id).addEventListener("change", () => { renderActivityView(); scheduleRecoverySave(); });
  byId("spectrum-channel").addEventListener("change", handleAnalysisChannelChange);
  byId("statistics-channel").addEventListener("change", handleAnalysisChannelChange);
  byId("amplitude-channel").addEventListener("change", handleAnalysisChannelChange);
  byId("focus-channel").addEventListener("change", handleAnalysisChannelChange);
  byId("waveform-single").addEventListener("click", () => focusWaveformChannel(state.analysisChannel));
  byId("waveform-multi").addEventListener("click", () => focusWaveformChannel(null));
  byId("waveform-layout").addEventListener("change", (event) => {
    renderer.setLayout(event.target.value); renderWaveformAmplitude(); scheduleRecoverySave();
  });
  byId("previous-focus-channel").addEventListener("click", () => stepFocusedChannel(-1));
  byId("next-focus-channel").addEventListener("click", () => stepFocusedChannel(1));
  byId("focus-spectrum").addEventListener("click", () => switchView("spectrum-view"));
  byId("focus-spikes").addEventListener("click", () => {
    switchView("spike-view");
    byId("spike-display").value = "waveforms";
    spikePanel.render();
    byId("spike-channel-filter").value = String(renderer.focusChannel ?? 0);
    spikePanel.render();
  });
  byId("spike-return-waveform").addEventListener("click", () => switchView("waveform-view"));
  for (const id of ["spectrum-return-waveform", "statistics-return-waveform"]) {
    byId(id).addEventListener("click", () => switchView("waveform-view"));
  }
  byId("activity-focus-waveform").addEventListener("click", () => {
    const index = currentMeaProfile().channels.findIndex((channel) => channel.id === state.activityFocusChannel);
    if (index >= 0) { focusWaveformChannel(index); switchView("waveform-view"); }
  });
  byId("spectrum-range").addEventListener("change", renderFrequencyAnalysis);
  byId("isi-range").addEventListener("change", renderEventAnalysis);

  byId("prepare-impedance").addEventListener("click", () => runAction("prepare-impedance", async () => {
    const profile = currentMeaProfile();
    const channelIds = impedanceChannelOrder(profile);
    const impedance = await bridge.invoke("prepare_impedance_scan", {
      request: {
        frequencyHz: clampNumber(byId("impedance-frequency"), 10, 2_000, 1_000),
        channelIds, profileId: profile.id, ...deviceRequest(),
        confirmedTestSignal: byId("impedance-confirmed").checked
      }
    });
    renderImpedanceState(impedance);
    toast("阻抗测量已开始", "info");
  }));
  byId("cancel-impedance").addEventListener("click", () => runAction("cancel-impedance", async () => {
    const impedance = await bridge.invoke("cancel_impedance_scan");
    renderImpedanceState(impedance);
    toast("正在停止测试信号，请等待清理完成", "info");
  }));
  byId("export-impedance").addEventListener("click", () => {
    const view = state.status?.impedance || {};
    downloadTextFile("neurox-impedance.csv", buildImpedanceCsv(view, impedanceProfile(view)), "text/csv;charset=utf-8");
  });
  byId("expand-impedance").addEventListener("click", () => {
    const expanded = byId("device-tools-view").classList.toggle("impedance-expanded");
    const button = byId("expand-impedance");
    button.setAttribute("aria-pressed", String(expanded));
    button.title = expanded ? "还原设备工具布局" : "展开阻抗结果";
    button.innerHTML = `<i data-lucide="${expanded ? "minimize-2" : "maximize-2"}"></i>`;
    window.lucide?.createIcons({ nodes: [button] });
  });
  byId("toggle-audio").addEventListener("click", () => runAction("toggle-audio", toggleAudioMonitor));
  byId("audio-channel").addEventListener("change", () => {
    state.audio.lastSequence = null;
    renderAudioState();
  });
  byId("audio-gain").addEventListener("input", updateAudioGain);
  byId("export-diagnostics").addEventListener("click", exportDiagnostics);

  for (const id of [
    "session-name", "session-operator", "session-subject", "session-notes",
    "bitfile-path", "device-serial", "blocks-per-read", "queue-capacity",
    "recording-path", "recording-auto-name", "trigger-recording-path", "trigger-recording-auto-name", "trigger-recording-source",
    "pre-trigger-ms", "post-trigger-ms", "digital-trigger-mask", "analog-trigger-channel",
    "analog-trigger-threshold", "analog-trigger-direction", "playback-path",
    "stim-delay", "stim-repeat-count", "stim-repeat-interval", "display-gain",
    "time-window", "amplitude-channel", "spectrum-channel", "statistics-channel", "spectrum-range",
    "isi-range", "activity-metric", "spike-window", "audio-channel", "audio-gain"
  ]) {
    byId(id).addEventListener("change", scheduleRecoverySave);
  }
  for (const id of ["session-name", "session-operator", "session-subject", "session-notes"]) {
    byId(id).addEventListener("input", scheduleRecoverySave);
  }
  window.addEventListener("beforeunload", saveRecoverySnapshot);

  byId("experiment-preset").addEventListener("change", renderExperimentPresetActions);
  byId("load-experiment").addEventListener("click", loadSelectedExperimentPreset);
  byId("save-experiment").addEventListener("click", openExperimentPresetDialog);
  byId("delete-experiment").addEventListener("click", deleteSelectedExperimentPreset);
  byId("experiment-preset-form").addEventListener("submit", saveExperimentPreset);
  byId("cancel-experiment-preset").addEventListener("click", () => byId("experiment-preset-dialog").close());

  byId("display-gain").addEventListener("input", (event) => {
    renderer.setGain(event.target.value);
    renderWaveformAmplitude();
  });
  for (const side of ["left", "right"]) {
    byId(`toggle-${side}-panel`).addEventListener("click", () => {
      state.panels[side] = !state.panels[side];
      renderPanelVisibility(); scheduleRecoverySave();
    });
  }
  byId("focus-range-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const range = normalizeWaveformRange({ minimumUv: byId("focus-range-min").valueAsNumber,
      maximumUv: byId("focus-range-max").valueAsNumber });
    byId("focus-range-max").setCustomValidity(range ? "" : "上限必须高于下限，范围至少为 0.1 µV");
    if (!range) { byId("focus-range-max").reportValidity(); return; }
    saveFocusedWaveformRange(range, event.submitter?.id === "all-channel-range-apply");
  });
  for (const id of ["focus-range-min", "focus-range-max"]) {
    byId(id).addEventListener("input", () => byId("focus-range-max").setCustomValidity(""));
  }
  byId("focus-range-reset").addEventListener("click", () => saveFocusedWaveformRange(null));
  byId("all-channel-range-reset").addEventListener("click", () => saveFocusedWaveformRange(null, true));
  byId("time-window").addEventListener("change", (event) => {
    renderer.setTimeWindow(event.target.value);
    renderWaveformAmplitude();
  });
  byId("focus-time-window").addEventListener("change", (event) => {
    byId("time-window").value = event.target.value;
    renderer.setTimeWindow(event.target.value);
    renderWaveformAmplitude(); scheduleRecoverySave();
  });
  byId("pause-display").addEventListener("click", () => {
    state.displayPaused = !state.displayPaused;
    renderer.setPaused(state.displayPaused);
    byId("pause-display").classList.toggle("active", state.displayPaused);
    byId("pause-display").title = state.displayPaused ? "继续波形显示" : "暂停波形显示";
    byId("pause-display").innerHTML = `<i data-lucide="${state.displayPaused ? "play" : "pause"}"></i>`;
    renderWaveformAmplitude();
    window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
  });
  const fitWaveformDisplay = () => {
    if (renderer.focusChannel !== null) {
      const stats = renderer.amplitudeStats(state.analysisChannel);
      if (!stats) { toast("当前通道暂无波形数据", "info"); return; }
      const padding = Math.max(0.1, stats.peakToPeakUv * 0.1);
      saveFocusedWaveformRange({ minimumUv: Number((stats.minimumUv - padding).toFixed(2)),
        maximumUv: Number((stats.maximumUv + padding).toFixed(2)) });
      return;
    }
    const gain = renderer.fitGain();
    byId("display-gain").value = String(gain);
    renderer.setGain(gain);
    renderWaveformAmplitude();
    scheduleRecoverySave();
  };
  byId("fit-waveform").addEventListener("click", fitWaveformDisplay);
  byId("focus-fit-waveform").addEventListener("click", fitWaveformDisplay);

  document.querySelectorAll(".tab").forEach((tab) => {
    tab.addEventListener("click", () => switchView(tab.dataset.view));
  });
}

function renderPanelVisibility() {
  const workspace = document.querySelector(".workspace");
  for (const side of ["left", "right"]) {
    const open = state.panels[side];
    const description = side === "left" ? "设备与处理链" : "采集与刺激设置";
    const button = byId(`toggle-${side}-panel`);
    const label = `${open ? "收起" : "展开"}${description}`;
    byId(`${side}-panel`).hidden = !open;
    workspace.classList.toggle(`${side}-collapsed`, !open);
    button.setAttribute("aria-expanded", String(open));
    button.setAttribute("aria-label", label); button.title = label;
  }
  window.requestAnimationFrame(() => {
    renderer.resize(); renderFrequencyAnalysis(); renderEventAnalysis();
    renderActivityView(); renderSpikeView();
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
    option.textContent = profile.name;
    select.appendChild(option);
  }
  state.meaProfileId = getMeaProfile(localStorage.getItem(meaProfileKey)).id;
  state.channelSettings = readChannelSettings();
  state.stimConfigs = readStimConfigs();
  populateStimParameterPresets();
  applyMeaProfile();
}

function currentMeaProfile() {
  const profile = getMeaProfile(state.meaProfileId);
  return configuredProfile(profile, state.channelSettings[profile.id]);
}

function persistMeaProfile() {
  try {
    localStorage.setItem(meaProfileKey, state.meaProfileId);
    scheduleRecoverySave();
  } catch (_) {
    // The profile still applies for the current session when storage is unavailable.
  }
}

const channelSettingsKey = "rhs-studio.channel-settings.v1";

function readChannelSettings() {
  try {
    const value = JSON.parse(localStorage.getItem(channelSettingsKey) || "{}");
    return value && typeof value === "object" && !Array.isArray(value) ? value : {};
  } catch (_) {
    return {};
  }
}

function persistChannelSettings() {
  localStorage.setItem(channelSettingsKey, JSON.stringify(state.channelSettings));
  scheduleRecoverySave();
}

function openChannelEditor() {
  const baseProfile = getMeaProfile(state.meaProfileId);
  state.channelEditorDraft = normalizeChannelSettings(baseProfile, state.channelSettings[baseProfile.id]);
  byId("channel-editor-profile").textContent = `${baseProfile.name} · ${baseProfile.rows}×${baseProfile.columns}`;
  renderChannelEditorRows();
  byId("channel-editor-dialog").showModal();
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
}

function closeChannelEditor() {
  state.channelEditorDraft = null;
  byId("channel-editor-dialog").close();
}

function renderChannelEditorRows() {
  const baseProfile = getMeaProfile(state.meaProfileId);
  const draft = state.channelEditorDraft || normalizeChannelSettings(baseProfile);
  const rows = byId("channel-editor-rows");
  rows.replaceChildren();
  for (const channel of baseProfile.channels) {
    const setting = draft[channel.id];
    const row = document.createElement("label");
    row.className = "channel-editor-row";
    row.dataset.channelId = channel.id;
    row.innerHTML = `
      <input data-field="enabled" type="checkbox" ${setting.enabled ? "checked" : ""} aria-label="启用 ${escapeHtml(channel.id)}" />
      <span>${escapeHtml(channel.id)}</span>
      <input data-field="label" type="text" maxlength="32" value="${escapeHtml(setting.label)}" aria-label="${escapeHtml(channel.id)} 显示名称" />
      <input data-field="color" type="color" value="${setting.color}" aria-label="${escapeHtml(channel.id)} 颜色" />
      <input data-field="row" type="number" min="1" max="${baseProfile.rows}" value="${setting.row + 1}" aria-label="${escapeHtml(channel.id)} 行" />
      <input data-field="column" type="number" min="1" max="${baseProfile.columns}" value="${setting.column + 1}" aria-label="${escapeHtml(channel.id)} 列" />`;
    rows.appendChild(row);
  }
  renderChannelEditorCount();
}

function handleChannelEditorInput(event) {
  const row = event.target.closest("[data-channel-id]");
  if (!row || !state.channelEditorDraft) return;
  const setting = state.channelEditorDraft[row.dataset.channelId];
  const field = event.target.dataset.field;
  if (!setting || !field) return;
  if (field === "enabled") setting.enabled = event.target.checked;
  else if (field === "row" || field === "column") setting[field] = Math.max(0, Number(event.target.value) - 1);
  else setting[field] = event.target.value;
  renderChannelEditorCount();
}

function renderChannelEditorCount() {
  const values = Object.values(state.channelEditorDraft || {});
  byId("channel-editor-count").textContent = `${values.filter((setting) => setting.enabled).length} / ${values.length}`;
}

function setAllChannelEditorStates(enabled) {
  if (!state.channelEditorDraft) return;
  for (const setting of Object.values(state.channelEditorDraft)) setting.enabled = enabled;
  byId("channel-editor-rows").querySelectorAll('input[data-field="enabled"]').forEach((input) => { input.checked = enabled; });
  renderChannelEditorCount();
}

function resetChannelEditor() {
  state.channelEditorDraft = normalizeChannelSettings(getMeaProfile(state.meaProfileId));
  renderChannelEditorRows();
}

function saveChannelEditor(event) {
  event.preventDefault();
  if (!state.channelEditorDraft) return;
  const baseProfile = getMeaProfile(state.meaProfileId);
  const positions = new Set();
  for (const setting of Object.values(state.channelEditorDraft)) {
    const position = `${setting.row}:${setting.column}`;
    if (positions.has(position)) {
      toast("阵列位置不能重复", "error");
      return;
    }
    positions.add(position);
  }
  state.channelSettings[baseProfile.id] = normalizeChannelSettings(baseProfile, state.channelEditorDraft);
  try {
    persistChannelSettings();
    closeChannelEditor();
    applyMeaProfile();
    toast("通道与阵列设置已应用", "success");
  } catch (error) {
    toast(`通道设置保存失败：${normalizeError(error)}`, "error");
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
  scheduleRecoverySave();
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
  bridge.setPreviewChannelCount(profile.channelCount);
  const thresholdDefinition = state.pipelineRegistry?.nodes.find((node) => node.kind === "threshold");
  const thresholdChannel = thresholdDefinition?.parameters.find((parameter) => parameter.key === "channelIndex");
  if (thresholdChannel) thresholdChannel.maximum = profile.channelCount;
  byId("mea-profile").value = profile.id;
  byId("source-node-summary").textContent = `${profile.channelCount} 电极 · 30 kHz`;
  const activeLanes = bridge.isPreview
    ? profile.channelCount
    : state.status?.device?.capabilities?.acquisitionChannelLimit || 16;
  byId("plot-empty").querySelector("span").textContent = `${profile.name} · 当前数据后端 ${activeLanes} lanes`;
  byId("waveform").setAttribute("aria-label", `${profile.name} 实时波形`);
  renderer.setChannelNames(profile.channels.map((channel) => channel.label));
  renderer.setChannelColors(profile.channels.map((channel) => channel.color));
  renderer.setChannelEnabled(profile.channels.map((channel) => channel.enabled));
  if (!state.gameFocusChannel || !profile.channels.some((channel) => channel.id === state.gameFocusChannel)) {
    state.gameFocusChannel = config.selectedChannels[0] || profile.channels[0]?.id || null;
  }
  if (!state.activityFocusChannel || !profile.channels.some((channel) => channel.id === state.activityFocusChannel)) {
    state.activityFocusChannel = profile.channels[0]?.id || null;
  }
  renderStimSummary();
  renderGameMap();
  buildActivityMap();
  populateAnalysisChannels();
  populateAudioChannels();
  renderImpedanceState(state.status?.impedance);
  renderActivityView();
  renderSpikeView();
  renderFrequencyAnalysis();
  renderEventAnalysis();
}

function initializeDeviceTools() {
  renderImpedanceState();
  renderAudioState();
}

function impedanceProfile(view = {}) {
  if (!view.profileId || view.profileId === state.meaProfileId) return currentMeaProfile();
  const profile = getMeaProfile(view.profileId);
  return configuredProfile(profile, state.channelSettings[profile.id]);
}

function renderImpedanceState(impedance = state.status?.impedance) {
  const view = impedance || { state: "idle", frequencyHz: 1_000, channels: [], message: "" };
  const profile = impedanceProfile(view);
  const layout = impedanceLayout(profile, view);
  const results = byId("impedance-results");
  results.replaceChildren();
  results.style.setProperty("--impedance-columns", String(layout.columns));
  results.style.setProperty("--impedance-rows", String(layout.rows));
  byId("impedance-profile-name").textContent = profile.shortName;
  for (const entry of layout.entries) {
    const result = entry.result || {};
    const item = document.createElement("div");
    item.className = `impedance-result ${entry.routeMismatch ? "invalid" : result.quality || "pending"}`;
    item.style.gridRow = String(entry.row + 1);
    item.style.gridColumn = String(entry.column + 1);
    item.dataset.channelId = entry.channelId;
    if (entry.siteNumber != null) item.dataset.siteNumber = String(entry.siteNumber);
    const magnitude = entry.routeMismatch ? "地址不符" : Number.isFinite(result.magnitudeOhms)
      ? formatImpedance(result.magnitudeOhms)
      : entry.enabled ? "待测" : "已禁用";
    const quality = { measured: "已测", saturated: "饱和", "low-signal": "信号偏低", invalid: "无效", pending: "待测", preview: "模拟" }[result.quality] || result.quality;
    const phase = !entry.routeMismatch && Number.isFinite(result.phaseDegrees) ? `${result.phaseDegrees.toFixed(1)}°` : "—";
    item.title = `${entry.label} · ${entry.channelId}${entry.siteNumber == null ? "" : ` · 电极 ${entry.siteNumber}`} · ${quality || "待测"}${result.stream == null ? "" : ` · Stream ${result.stream}:CH ${result.chipChannel}`}`;
    const name = document.createElement("span"); name.textContent = entry.label;
    const value = document.createElement("strong"); value.textContent = magnitude;
    const detail = document.createElement("span"); detail.textContent = phase;
    item.append(name, value, detail);
    results.appendChild(item);
  }
  byId("impedance-state").textContent = String(view.state || "idle").toUpperCase();
  const controls = impedanceControls({ ...state.status, impedance: view }, bridge.isPreview);
  byId("prepare-impedance").disabled = controls.measureDisabled;
  byId("cancel-impedance").disabled = controls.cancelDisabled;
  byId("export-impedance").disabled = controls.exportDisabled;
  byId("impedance-frequency").disabled = controls.busy;
  byId("impedance-confirmed").disabled = controls.busy || bridge.isPreview;
  if (controls.busy) byId("impedance-frequency").value = String(view.frequencyHz || 1_000);
  byId("impedance-progress").textContent = `实际频率 ${view.actualFrequencyHz ? `${view.actualFrequencyHz.toFixed(2)} Hz` : "—"} · ${view.completedChannels || 0}/${view.channels?.length || 0} 通道`;
  const mismatches = layout.entries.filter(entry => entry.routeMismatch).length;
  byId("impedance-message").textContent = impedanceMessage(view)
    + (layout.unmappedResults.length ? ` · ${layout.unmappedResults.length} 项结果不属于当前阵列，已保留在 CSV 中` : "")
    + (mismatches ? ` · ${mismatches} 项物理地址不符` : "");
}

function impedanceMessage(view) {
  if (view.source === "synthetic") return "模拟结果，仅用于界面验证，非实测阻抗";
  if (view.state === "hardware-required") return "参数已验证，接入硬件后执行真实阻抗测量";
  if (view.state === "cancelled") return "阻抗任务已取消";
  if (view.state === "complete") return `已完成 ${(view.channels || []).length} 个通道`;
  if (view.state === "running") return "正在测量电极阻抗";
  if (view.state === "cancelling") return "正在停止测试信号并恢复芯片，请等待完成";
  if (view.state === "fault") return `测量失败：${view.message}`;
  if (bridge.isPreview) return "预览模式不连接硬件，不提供实测阻抗";
  return "未创建阻抗任务";
}

function formatImpedance(value) {
  const ohms = Math.max(0, Number(value) || 0);
  if (ohms >= 1_000_000) return `${(ohms / 1_000_000).toFixed(2)} MΩ`;
  if (ohms >= 1_000) return `${(ohms / 1_000).toFixed(1)} kΩ`;
  return `${ohms.toFixed(0)} Ω`;
}

function channelLabel(channelId) {
  return currentMeaProfile().channels.find((channel) => channel.id === channelId)?.label || channelId;
}

function populateAudioChannels() {
  const profile = currentMeaProfile();
  const select = byId("audio-channel");
  const previous = select.value;
  select.replaceChildren();
  profile.channels.forEach((channel, index) => {
    if (!channel.enabled) return;
    const option = document.createElement("option");
    option.value = String(index);
    option.textContent = channel.label;
    select.appendChild(option);
  });
  if ([...select.options].some((option) => option.value === previous)) select.value = previous;
  state.audio.lastSequence = null;
  renderAudioState();
}

async function toggleAudioMonitor() {
  if (state.audio.enabled) {
    state.audio.enabled = false;
    state.audio.nextStartTime = 0;
    await state.audio.context?.suspend();
    renderAudioState();
    toast("信号监听已关闭", "info");
    return;
  }
  const AudioContext = window.AudioContext || window.webkitAudioContext;
  if (!AudioContext) throw new Error("当前系统不支持音频监听");
  if (!state.audio.context) {
    state.audio.context = new AudioContext({ latencyHint: "interactive" });
    state.audio.gainNode = state.audio.context.createGain();
    state.audio.compressor = state.audio.context.createDynamicsCompressor();
    state.audio.compressor.threshold.value = -18;
    state.audio.compressor.knee.value = 12;
    state.audio.compressor.ratio.value = 8;
    state.audio.compressor.attack.value = 0.003;
    state.audio.compressor.release.value = 0.15;
    state.audio.gainNode.connect(state.audio.compressor).connect(state.audio.context.destination);
  }
  updateAudioGain();
  await state.audio.context.resume();
  state.audio.enabled = true;
  state.audio.nextStartTime = 0;
  state.audio.lastSequence = null;
  renderAudioState();
  toast("信号监听已开启", "success");
}

function updateAudioGain() {
  const gain = Number(byId("audio-gain").value) || 0;
  byId("audio-gain-label").textContent = `${gain.toFixed(1)}×`;
  if (state.audio.gainNode && state.audio.context) {
    state.audio.gainNode.gain.setTargetAtTime(gain, state.audio.context.currentTime, 0.01);
  }
}

function renderAudioState() {
  const enabled = state.audio.enabled;
  const selected = Number(byId("audio-channel")?.value || 0);
  const channel = currentMeaProfile().channels[selected];
  byId("audio-state").textContent = enabled ? "ON" : "OFF";
  byId("audio-state").classList.toggle("active", enabled);
  byId("audio-channel-state").textContent = channel?.label || "无可用通道";
  const button = byId("toggle-audio");
  button.querySelector("span").textContent = enabled ? "关闭" : "开启";
  button.innerHTML = `<i data-lucide="${enabled ? "volume-x" : "volume-2"}"></i><span>${enabled ? "关闭" : "开启"}</span>`;
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
  if (!enabled) byId("audio-level-meter").style.setProperty("--audio-level", "0%");
}

function enqueueAudioFrame(frame) {
  const audio = state.audio;
  if (!audio.enabled || !audio.context || audio.context.state !== "running") return;
  if (audio.lastSequence === frame.sequence) return;
  audio.lastSequence = frame.sequence;
  const series = frameChannelSeries(frame, Number(byId("audio-channel").value || 0));
  if (series.samples.length < 2 || series.sampleRateHz <= 0) return;
  const mean = series.samples.reduce((sum, value) => sum + value, 0) / series.samples.length;
  const normalized = series.samples.map((value) => Math.max(-0.95, Math.min(0.95, (value - mean) / 2_048)));
  const rms = Math.sqrt(normalized.reduce((sum, value) => sum + value * value, 0) / normalized.length);
  byId("audio-level-meter").style.setProperty("--audio-level", `${Math.min(100, rms * 260).toFixed(1)}%`);
  const duration = normalized.length / series.sampleRateHz;
  const targetLength = Math.max(2, Math.round(duration * audio.context.sampleRate));
  const buffer = audio.context.createBuffer(1, targetLength, audio.context.sampleRate);
  const output = buffer.getChannelData(0);
  for (let index = 0; index < targetLength; index += 1) {
    const position = index * (normalized.length - 1) / Math.max(1, targetLength - 1);
    const left = Math.floor(position);
    const fraction = position - left;
    output[index] = normalized[left] * (1 - fraction) + normalized[Math.min(normalized.length - 1, left + 1)] * fraction;
  }
  const source = audio.context.createBufferSource();
  source.buffer = buffer;
  source.connect(audio.gainNode);
  if (audio.nextStartTime < audio.context.currentTime || audio.nextStartTime > audio.context.currentTime + 0.3) {
    audio.nextStartTime = audio.context.currentTime + 0.015;
  }
  source.start(audio.nextStartTime);
  audio.nextStartTime += duration;
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

function stimulationConfigRequest() {
  const config = activeStimConfig();
  return {
    profileId: currentMeaProfile().id,
    channelIds: config.selectedChannels,
    waveform: config.waveform,
    amplitudeNa: Math.round(config.amplitudeNa),
    phaseWidthUs: Math.round(config.phaseWidthUs),
    interphaseUs: Math.round(config.interphaseUs),
    frequencyHz: Number(config.frequencyHz),
    pulseCount: Math.round(config.pulseCount),
    chargeRecovery: Boolean(config.chargeRecovery),
    ...stimulationConfirmations(byId("stim-validation-mode").value, byId("stim-real-output").checked, {
      sampleDisconnected: byId("stim-sample-disconnected").checked,
      dummyLoadConnected: byId("stim-dummy-load").checked,
      measurementReady: byId("stim-measurement-ready").checked,
      routingConfirmed: byId("stim-routing-confirmed").checked,
      parametersConfirmed: byId("stim-parameters-confirmed").checked,
      unverifiedOutputAcknowledged: byId("stim-unverified-acknowledged").checked,
      operator: byId("session-operator").value.trim()
    })
  };
}

function resetStimulationConfirmations() {
  for (const id of ["stim-real-output", "stim-sample-disconnected", "stim-dummy-load", "stim-measurement-ready",
    "stim-routing-confirmed", "stim-parameters-confirmed", "stim-unverified-acknowledged"]) byId(id).checked = false;
}

function clearBenchConfirmation() {
  resetStimulationConfirmations();
  byId("stim-validation-mode").value = "measured";
  renderStimValidationControls();
}

function renderStimValidationControls(armed = false, pending = false) {
  const functional = byId("stim-validation-mode").value.startsWith("functional");
  const configurable = byId("stim-validation-mode").value === "configurable";
  byId("stim-validation-mode").disabled = armed || pending;
  byId("stim-measured-confirmations").hidden = functional || configurable;
  byId("stim-disconnected-confirmation").hidden = configurable;
  byId("stim-configurable-confirmations").hidden = !configurable;
  for (const id of ["stim-dummy-load", "stim-measurement-ready"]) byId(id).disabled = functional || configurable || armed || pending;
  byId("stim-sample-disconnected").disabled = configurable || armed || pending;
  for (const id of ["stim-routing-confirmed", "stim-parameters-confirmed", "stim-unverified-acknowledged"]) {
    byId(id).disabled = !configurable || armed || pending;
  }
  for (const id of ["stim-repeat-count", "stim-repeat-interval"]) byId(id).disabled = functional || armed || pending;
}

function stimulationScheduleRequest() {
  return {
    delayMs: Math.max(0, Number(byId("stim-delay").value) || 0),
    repeatCount: Math.max(1, Math.round(Number(byId("stim-repeat-count").value) || 1)),
    repeatIntervalMs: Math.max(0, Number(byId("stim-repeat-interval").value) || 0)
  };
}

function openStimConfigDialog() {
  if (["acquiring", "recording"].includes(state.status?.mode)) {
    toast("请先停止采集，再修改刺激通道与参数", "info");
    return;
  }
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
  byId("stim-array-source").textContent = profile.description;
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
  const hardwareProfile = ["rhs2116-logic-16", "mea-ab-64"].includes(currentMeaProfile().id);
  byId("stim-amplitude").min = hardwareProfile ? "10" : "0";
  byId("stim-amplitude").max = hardwareProfile ? "10000" : "1000000";
  byId("stim-phase-width").max = hardwareProfile ? "10000" : "1000000";
  byId("stim-interphase").max = hardwareProfile ? "10000" : "1000000";
  byId("stim-pulse-count").max = hardwareProfile ? "20" : "10000";
  for (const option of byId("stim-waveform").options) {
    option.disabled = hardwareProfile && option.value === "triphasic";
  }
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
  if (["acquiring", "recording"].includes(state.status?.mode)) {
    toast("采集中不能修改预加载的刺激配置", "error");
    return;
  }
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

function buildActivityMap() {
  const profile = currentMeaProfile();
  const container = byId("activity-mea-grid");
  const channelIndexes = new Map(profile.channels.map((channel, index) => [channel.id, index]));
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
    const cell = document.createElement("button");
    cell.type = "button";
    cell.className = `mea-cell no-data port-${channel.port.toLowerCase()} ${channel.enabled ? "" : "is-disabled"}`;
    cell.dataset.channelId = channel.id;
    cell.dataset.activityIndex = String(channelIndexes.get(channel.id));
    setMeaCellLabel(cell, channel, profile);
    cell.title = channel.siteNumber === null
      ? channel.label
      : `电极 ${channel.siteNumber} · ${channel.label}`;
    cell.disabled = !channel.enabled;
    container.appendChild(cell);
  }
}

function handleActivityChannelClick(event) {
  const button = event.target.closest("button[data-channel-id]");
  if (!button) return;
  state.activityFocusChannel = button.dataset.channelId;
  renderActivityView();
}

function renderActivityView() {
  const profile = currentMeaProfile();
  const activity = state.channelActivity;
  const batch = spikePanel.history.batch;
  const endMs = (batch?.latestSampleIndex || 0) * 1000 / (batch?.sampleRateHz || 30000);
  const validEpoch = state.latestFrame?.spikeEpoch === batch?.epoch;
  const windowMs = Number(byId("activity-window").value);
  const flashMs = Number(byId("activity-flash-ms").value);
  const rateMaximum = Math.max(0.1, Number(byId("activity-rate-max").value) || 20);
  const raster = spikeActivitySummary(validEpoch && batch?.config.enabled ? spikePanel.history.events : [], endMs,
    windowMs, profile.channelCount, performance.now() - (state.activityClockAt || performance.now()), flashMs);
  const metric = byId("activity-metric").value;
  const scores = metric === "spike-rate" ? raster.ratesHz : raster.flashes;
  const normalized = scores.map((value, index) => profile.channels[index]?.enabled === false ? 0
    : metric === "spike-rate" ? Math.min(1, value / rateMaximum) : value);
  const dataChannels = profile.channels.filter((channel, index) => channel.enabled && index < activity.length).length;
  const peakIndex = normalized.reduce((best, score, index) => score > (normalized[best] || 0) ? index : best, 0);
  const hasEvents = normalized[peakIndex] > 0;

  byId("activity-profile-name").textContent = profile.name;
  byId("activity-data-channels").textContent = `${dataChannels} / ${profile.channelCount}`;
  byId("activity-peak-channel").textContent = hasEvents ? profile.channels[peakIndex]?.label || "—" : "—";
  byId("activity-peak-value").textContent = hasEvents ? metric === "spike-rate" ? `${raster.ratesHz[peakIndex].toFixed(2)} Hz` : "近期事件" : "无事件";
  byId("activity-peak-label").textContent = metric === "spike-rate" ? "放电率" : "瞬时事件";
  byId("activity-scale-high").textContent = metric === "spike-rate" ? `${rateMaximum} Hz` : `${flashMs} ms`;

  byId("activity-mea-grid").querySelectorAll("button[data-activity-index]").forEach((cell) => {
    const index = Number(cell.dataset.activityIndex);
    const entry = activity[index];
    const enabled = profile.channels[index]?.enabled !== false;
    cell.classList.toggle("no-data", !entry || !enabled);
    cell.classList.toggle("is-quiet", !normalized[index]);
    cell.classList.toggle("is-focused", cell.dataset.channelId === state.activityFocusChannel);
    if (!entry || !enabled) {
      cell.style.removeProperty("--heat-color");
      cell.title = `电极 ${profile.channels[index]?.siteNumber ?? "—"} · ${profile.channels[index]?.label || cell.dataset.channelId} · 无实时数据`;
      return;
    }
    cell.style.setProperty("--heat-color", activityColor(normalized[index]));
    const channel = profile.channels[index];
    const address = `${channel?.siteNumber === null ? "" : `电极 ${channel?.siteNumber} · `}${channel?.label || cell.dataset.channelId}`;
    cell.title = metric === "spike-rate"
      ? `${address} · 放电率 ${(raster.ratesHz[index] || 0).toFixed(2)} Hz`
      : `${address} · ${normalized[index] ? "近期候选尖峰" : "无近期事件"} · 窗口内 ${raster.counts[index]} 次`;
  });

  const focusIndex = profile.channels.findIndex((channel) => channel.id === state.activityFocusChannel);
  const focus = focusIndex >= 0 ? activity[focusIndex] : null;
  const baseline = focusIndex >= 0 && state.latestFrame?.channelCount === profile.channelCount
    ? state.latestFrame?.baseline?.[focusIndex] : null;
  const qualityLabels = {
    ok: "正常", saturated: "饱和", "stim-artifact": "刺激伪迹",
    discontinuity: "数据中断", "too-short": "片段过短", preview: "模拟数据"
  };
  byId("activity-focus-channel").textContent = profile.channels[focusIndex]?.label || "—";
  byId("activity-focus-range").textContent = focus ? `${Math.round(focus.averagePeakToPeak)} ADC` : "—";
  byId("activity-focus-peak").textContent = focus ? `${Math.round(focus.peakDeviation)} ADC` : "—";
  byId("activity-focus-rate").textContent = focusIndex >= 0 && raster.ratesHz[focusIndex] !== undefined
    ? `${raster.ratesHz[focusIndex].toFixed(2)} Hz`
    : "—";
  byId("activity-focus-baseline").textContent = baseline ? `${(baseline.offsetAdc * RHS_UV_PER_ADC).toFixed(1)} µV` : "—";
  byId("activity-focus-noise").textContent = baseline ? `${(baseline.noiseAdc * RHS_UV_PER_ADC).toFixed(1)} µV` : "—";
  byId("activity-focus-quality").textContent = baseline ? (qualityLabels[baseline.quality] || baseline.quality) : "—";
  const detection = batch?.thresholds?.[focusIndex];
  byId("activity-focus-detection-noise").textContent = Number.isFinite(detection?.noiseUv) ? `${detection.noiseUv.toFixed(2)} µV` : "—";
  byId("activity-focus-threshold").textContent = Number.isFinite(detection?.thresholdUv) ? `${detection.thresholdUv.toFixed(2)} µV` : detection?.state === "calibrating" ? "校准中" : "—";
  byId("activity-status").classList.toggle("live", Boolean(batch?.running));
  byId("activity-status").lastChild.textContent = !batch?.config.enabled ? "尖峰检测未开启" : batch.thresholds?.some((value) => value.state === "calibrating")
    ? "噪声校准中" : batch.running ? `候选尖峰 · ${metric === "spike-rate" ? `${windowMs / 1000} s 窗口` : `${flashMs} ms 闪烁`}` : "检测已停止 · 保留窗口统计";
}

function activityColor(value) {
  const level = Math.min(1, Math.max(0, Number(value) || 0));
  if (!level) return "#1a2225";
  if (level < 0.5) {
    const amount = level / 0.5;
    return `hsl(${Math.round(202 - amount * 39)} 56% ${Math.round(27 + amount * 8)}%)`;
  }
  if (level < 0.8) {
    const amount = (level - 0.5) / 0.3;
    return `hsl(${Math.round(163 - amount * 121)} ${Math.round(56 + amount * 12)}% ${Math.round(35 + amount * 13)}%)`;
  }
  const amount = (level - 0.8) / 0.2;
  return `hsl(${Math.round(42 - amount * 34)} 68% ${Math.round(48 + amount * 5)}%)`;
}

function currentThresholdRaster() {
  const windowMs = Number(byId("spike-window")?.value) || 10_000;
  const batch = spikePanel.history.batch;
  const endElapsedMs = batch ? batch.latestSampleIndex * 1000 / (batch.sampleRateHz || 30000) : 0;
  const dataChannels = state.latestFrame?.channelCount || state.status?.device?.channels || 16;
  return buildThresholdRaster(spikePanel.history.analysisEvents(), endElapsedMs, windowMs, dataChannels);
}

function renderSpikeView() {
  spikePanel.render();
}

function populateAnalysisChannels() {
  const profile = currentMeaProfile();
  const channelCount = Math.max(1, state.latestFrame?.channelCount || profile.channelCount);
  state.analysisChannel = Math.min(state.analysisChannel, channelCount - 1);
  if (profile.channels[state.analysisChannel]?.enabled === false) {
    const firstEnabled = profile.channels.findIndex((channel, index) => index < channelCount && channel.enabled !== false);
    state.analysisChannel = Math.max(0, firstEnabled);
  }
  for (const id of ["amplitude-channel", "spectrum-channel", "statistics-channel", "focus-channel"]) {
    const select = byId(id);
    select.replaceChildren();
    for (let index = 0; index < channelCount; index += 1) {
      const option = document.createElement("option");
      option.value = String(index);
      option.textContent = profile.channels[index]?.label || `CH-${String(index + 1).padStart(2, "0")}`;
      option.disabled = profile.channels[index]?.enabled === false;
      select.appendChild(option);
    }
    select.value = String(state.analysisChannel);
  }
  if (renderer.focusChannel !== null) renderer.setFocusChannel([...byId("focus-channel").options].some((option) => !option.disabled) ? state.analysisChannel : null);
  renderWaveformAmplitude();
}

function handleAnalysisChannelChange(event) {
  state.analysisChannel = Math.max(0, Number(event.target.value) || 0);
  byId("amplitude-channel").value = String(state.analysisChannel);
  byId("spectrum-channel").value = String(state.analysisChannel);
  byId("statistics-channel").value = String(state.analysisChannel);
  byId("focus-channel").value = String(state.analysisChannel);
  if (renderer.focusChannel !== null) renderer.setFocusChannel(state.analysisChannel);
  renderWaveformAmplitude();
  renderFrequencyAnalysis();
  renderEventAnalysis();
}

function formatMicrovolts(value) {
  const absolute = Math.abs(value);
  return `${value < 0 ? "−" : "+"}${absolute < 100 ? absolute.toFixed(1) : Math.round(absolute)} µV`;
}

function renderWaveformAmplitude() {
  renderWaveformFocus();
  byId("gain-label").textContent = `${renderer.gain.toFixed(2)}×`;
  const range = renderer.voltageRange(state.analysisChannel);
  byId("waveform-visible-range").textContent = `${Number(range.minimumUv.toFixed(2))} ～ ${Number(range.maximumUv.toFixed(2))} µV`;
  const stats = renderer.amplitudeStats(state.analysisChannel);
  byId("waveform-minimum").textContent = stats ? formatMicrovolts(stats.minimumUv) : "—";
  byId("waveform-maximum").textContent = stats ? formatMicrovolts(stats.maximumUv) : "—";
  byId("waveform-peak-to-peak").textContent = stats ? `${stats.peakToPeakUv < 100 ? stats.peakToPeakUv.toFixed(1) : Math.round(stats.peakToPeakUv)} µV` : "—";
  byId("waveform-rail-warning").hidden = !stats?.touchesRail;
  byId("waveform-range-clipped").hidden = !stats
    || (stats.minimumUv >= range.minimumUv && stats.maximumUv <= range.maximumUv);
}

function saveFocusedWaveformRange(range, allChannels = false) {
  const profile = currentMeaProfile();
  const id = profile.channels[state.analysisChannel]?.id;
  if (!id) return;
  state.waveformRanges[profile.id] ||= {};
  const ids = allChannels ? profile.channels.map((channel) => channel.id) : [id];
  for (const channelId of ids) {
    if (range) state.waveformRanges[profile.id][channelId] = normalizeWaveformRange(range);
    else delete state.waveformRanges[profile.id][channelId];
  }
  byId("focus-range-max").setCustomValidity("");
  renderer.setChannelRanges(profile.channels.map((channel) => state.waveformRanges[profile.id]?.[channel.id]));
  renderer.setFocusRange(range);
  const visible = renderer.voltageRange(state.analysisChannel);
  byId("focus-range-min").value = String(Number(visible.minimumUv.toFixed(2)));
  byId("focus-range-max").value = String(Number(visible.maximumUv.toFixed(2)));
  renderWaveformAmplitude(); scheduleRecoverySave();
}

function focusWaveformChannel(channel) {
  if (channel !== null) {
    const options = [...byId("focus-channel").options].filter((option) => !option.disabled);
    const selected = options.find((option) => Number(option.value) === Number(channel));
    if (!selected) return;
    state.analysisChannel = Number(selected.value);
    for (const id of ["amplitude-channel", "spectrum-channel", "statistics-channel", "focus-channel"]) byId(id).value = selected.value;
  }
  renderer.setFocusChannel(channel === null ? null : state.analysisChannel);
  renderWaveformAmplitude();
  renderFrequencyAnalysis(); renderEventAnalysis();
  scheduleRecoverySave();
}

function stepFocusedChannel(direction) {
  const options = [...byId("focus-channel").options].filter((option) => !option.disabled);
  const index = options.findIndex((option) => Number(option.value) === state.analysisChannel);
  const next = options[index + direction];
  if (next) focusWaveformChannel(Number(next.value));
}

function renderWaveformFocus() {
  const profile = currentMeaProfile();
  const focused = renderer.focusChannel !== null;
  renderer.setChannelRanges(profile.channels.map((channel) => state.waveformRanges[profile.id]?.[channel.id]));
  const savedRange = normalizeWaveformRange(state.waveformRanges[profile.id]?.[profile.channels[state.analysisChannel]?.id]);
  if (renderer.focusRange?.minimumUv !== savedRange?.minimumUv || renderer.focusRange?.maximumUv !== savedRange?.maximumUv) renderer.setFocusRange(savedRange);
  byId("focus-range-form").hidden = false;
  byId("range-channel-name").textContent = `${profile.channels[state.analysisChannel]?.label || "通道"} 显示范围`;
  byId("waveform-layout-field").hidden = focused;
  const range = renderer.voltageRange(state.analysisChannel);
  const rangeChannel = `${profile.id}:${profile.channels[state.analysisChannel]?.id}`;
  const rangeChannelChanged = byId("focus-range-form").dataset.channel !== rangeChannel;
  if (rangeChannelChanged || !byId("focus-range-form").contains(document.activeElement)) {
    byId("focus-range-form").dataset.channel = rangeChannel;
    if (rangeChannelChanged) byId("focus-range-max").setCustomValidity("");
    byId("focus-range-min").value = String(Number(range.minimumUv.toFixed(2)));
    byId("focus-range-max").value = String(Number(range.maximumUv.toFixed(2)));
  }
  byId("display-gain").disabled = focused && Boolean(savedRange);
  byId("display-gain").title = focused && savedRange ? "当前通道使用自定义微伏范围" : "波形显示增益";
  byId("waveform-view").classList.toggle("is-single-channel", focused);
  byId("waveform-multi").setAttribute("aria-pressed", String(!focused));
  byId("waveform-single").setAttribute("aria-pressed", String(focused));
  byId("focus-navigation").hidden = !focused;
  byId("focus-window-field").hidden = !focused;
  byId("focus-analysis-actions").hidden = !focused;
  for (const id of ["spectrum-return-waveform", "statistics-return-waveform"]) byId(id).hidden = !focused;
  byId("focus-time-window").value = String(renderer.timeWindowMs);
  const options = [...byId("focus-channel").options].filter((option) => !option.disabled);
  const index = options.findIndex((option) => Number(option.value) === state.analysisChannel);
  byId("waveform-single").disabled = index < 0;
  byId("previous-focus-channel").disabled = index <= 0;
  byId("next-focus-channel").disabled = index < 0 || index >= options.length - 1;
  const label = profile.channels[state.analysisChannel]?.label || `CH-${state.analysisChannel + 1}`;
  const hasData = renderer.frame && state.analysisChannel < renderer.frame.channelCount;
  byId("focus-channel-status").textContent = focused ? `${label} · ${hasData ? state.displayPaused ? "显示暂停" : "单通道波形" : "暂无通道数据"}` : "";
  byId("waveform").setAttribute("aria-label", focused ? `${label} 单通道波形` : `${profile.name} 实时波形`);
}

function prepareAnalysisCanvas(id) {
  const canvas = byId(id);
  const rect = canvas.getBoundingClientRect();
  if (rect.width < 10 || rect.height < 10) return null;
  const ratio = Math.min(2, window.devicePixelRatio || 1);
  const width = Math.floor(rect.width);
  const height = Math.floor(rect.height);
  canvas.width = Math.floor(width * ratio);
  canvas.height = Math.floor(height * ratio);
  const context = canvas.getContext("2d");
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  context.fillStyle = "#111719";
  context.fillRect(0, 0, width, height);
  return { context, width, height };
}

function drawAnalysisEmpty(canvasId, message) {
  const prepared = prepareAnalysisCanvas(canvasId);
  if (!prepared) return;
  const { context, width, height } = prepared;
  context.fillStyle = "#667478";
  context.font = '11px "Segoe UI", sans-serif';
  context.textAlign = "center";
  context.fillText(message, width / 2, height / 2);
}

function drawGrid(context, width, height, margins) {
  const left = margins.left;
  const right = width - margins.right;
  const top = margins.top;
  const bottom = height - margins.bottom;
  context.strokeStyle = "rgba(194, 214, 217, 0.10)";
  context.lineWidth = 1;
  for (let index = 0; index <= 4; index += 1) {
    const x = left + (right - left) * index / 4;
    const y = top + (bottom - top) * index / 4;
    context.beginPath();
    context.moveTo(x, top);
    context.lineTo(x, bottom);
    context.stroke();
    context.beginPath();
    context.moveTo(left, y);
    context.lineTo(right, y);
    context.stroke();
  }
  return { left, right, top, bottom };
}

function renderFrequencyAnalysis() {
  if (!byId("spectrum-view").classList.contains("active")) return;
  const history = state.signalHistory;
  const samples = history?.channels?.[state.analysisChannel] || [];
  const spectrum = powerSpectrum(samples, history?.sampleRateHz || 0, 2048);
  if (!spectrum.frequencies.length) {
    byId("spectrum-peak").textContent = "—";
    byId("spectrum-resolution").textContent = "等待信号历史";
    byId("spectrogram-window").textContent = "—";
    drawAnalysisEmpty("spectrum-canvas", "等待足够的连续数据");
    drawAnalysisEmpty("spectrogram-canvas", "等待足够的连续数据");
    return;
  }
  const selectedLimit = Number(byId("spectrum-range").value);
  const nyquist = history.sampleRateHz / 2;
  const frequencyLimit = Math.min(nyquist, selectedLimit > 0 ? selectedLimit : nyquist);
  const visible = spectrum.frequencies
    .map((frequency, index) => ({ frequency, power: spectrum.powerDb[index] }))
    .filter((point) => point.frequency <= frequencyLimit);
  const peak = visible.slice(1).reduce((best, point) => point.power > best.power ? point : best, visible[1] || visible[0]);
  byId("spectrum-peak").textContent = peak ? `${formatFrequency(peak.frequency)} · ${peak.power.toFixed(1)} dB` : "—";
  byId("spectrum-resolution").textContent = `${formatFrequency(spectrum.resolutionHz)} resolution · N=${spectrum.size}`;
  drawSpectrum(visible, frequencyLimit);

  const spectrogram = buildSpectrogram(samples, history.sampleRateHz, { windowSize: 256, hopSize: 64, maxFrames: 96 });
  byId("spectrogram-window").textContent = spectrogram.frames.length
    ? `${spectrogram.frames.length} frames · ${(spectrogram.windowSize / history.sampleRateHz * 1000).toFixed(1)} ms`
    : "—";
  drawSpectrogram(spectrogram, frequencyLimit);
}

function drawSpectrum(points, frequencyLimit) {
  const prepared = prepareAnalysisCanvas("spectrum-canvas");
  if (!prepared || !points.length) return;
  const { context, width, height } = prepared;
  const plot = drawGrid(context, width, height, { left: 44, right: 14, top: 13, bottom: 24 });
  const values = points.map((point) => point.power).filter(Number.isFinite);
  const maximum = Math.max(...values);
  const minimum = Math.min(maximum - 20, Math.max(Math.min(...values), maximum - 90));
  context.beginPath();
  points.forEach((point, index) => {
    const x = plot.left + point.frequency / Math.max(1, frequencyLimit) * (plot.right - plot.left);
    const y = plot.bottom - (point.power - minimum) / Math.max(1, maximum - minimum) * (plot.bottom - plot.top);
    if (index === 0) context.moveTo(x, y);
    else context.lineTo(x, y);
  });
  context.strokeStyle = "#50c5ad";
  context.lineWidth = 1.5;
  context.stroke();
  context.lineTo(plot.right, plot.bottom);
  context.lineTo(plot.left, plot.bottom);
  context.closePath();
  context.fillStyle = "rgba(80, 197, 173, 0.10)";
  context.fill();
  context.fillStyle = "#758589";
  context.font = '9px "Segoe UI", sans-serif';
  context.textAlign = "left";
  context.fillText(`${minimum.toFixed(0)} dB`, 5, plot.bottom);
  context.fillText(`${maximum.toFixed(0)} dB`, 5, plot.top + 4);
  context.fillText("0 Hz", plot.left, height - 7);
  context.textAlign = "right";
  context.fillText(formatFrequency(frequencyLimit), plot.right, height - 7);
}

function drawSpectrogram(spectrogram, frequencyLimit) {
  const prepared = prepareAnalysisCanvas("spectrogram-canvas");
  if (!prepared || !spectrogram.frames.length) return;
  const { context, width, height } = prepared;
  const left = 44;
  const right = width - 14;
  const top = 10;
  const bottom = height - 22;
  const maxBin = Math.max(1, spectrogram.frequencies.findLastIndex((frequency) => frequency <= frequencyLimit));
  const values = spectrogram.frames.flatMap((frame) => frame.slice(0, maxBin + 1)).filter(Number.isFinite).sort((a, b) => a - b);
  const low = values[Math.floor(values.length * 0.08)] ?? -100;
  const high = Math.max(low + 1, values[Math.floor(values.length * 0.98)] ?? -20);
  const cellWidth = (right - left) / spectrogram.frames.length;
  const cellHeight = (bottom - top) / (maxBin + 1);
  spectrogram.frames.forEach((frame, frameIndex) => {
    for (let bin = 0; bin <= maxBin; bin += 1) {
      const normalized = Math.max(0, Math.min(1, (frame[bin] - low) / (high - low)));
      context.fillStyle = spectrogramColor(normalized);
      context.fillRect(
        left + frameIndex * cellWidth,
        bottom - (bin + 1) * cellHeight,
        Math.ceil(cellWidth + 0.5),
        Math.ceil(cellHeight + 0.5)
      );
    }
  });
  context.fillStyle = "#758589";
  context.font = '9px "Segoe UI", sans-serif';
  context.textAlign = "right";
  context.fillText(formatFrequency(frequencyLimit), left - 5, top + 4);
  context.fillText("0 Hz", left - 5, bottom);
  context.fillText("现在", right, height - 6);
}

function spectrogramColor(value) {
  const hue = 205 - Math.min(1, Math.max(0, value)) * 197;
  const lightness = 16 + value * 42;
  return `hsl(${hue} 68% ${lightness}%)`;
}

function renderEventAnalysis() {
  if (!byId("statistics-view").classList.contains("active")) return;
  const maximumIsi = Number(byId("isi-range").value) || 500;
  const spikeEvents = spikePanel.history.analysisEvents();
  const isi = buildIsiHistogram(spikeEvents, state.analysisChannel, maximumIsi, 40);
  byId("isi-count").textContent = `${formatNumber(isi.intervalsMs.length)} intervals`;
  const sortedIntervals = [...isi.intervalsMs].sort((a, b) => a - b);
  const median = sortedIntervals.length ? sortedIntervals[Math.floor(sortedIntervals.length / 2)] : null;
  byId("isi-median").textContent = median === null ? "中位数 —" : `中位数 ${median.toFixed(1)} ms`;
  drawHistogram("isi-canvas", isi.bins, `${maximumIsi} ms`, "#4fc1aa");

  const triggerEvents = state.events.filter((event) =>
    (event.category === "stimulation" || event.category === "trigger-recording")
      && (event.sampleIndex !== null || /triggered|accepted/i.test(event.message || ""))
  );
  const rate = spikePanel.history.batch?.sampleRateHz || 30000;
  const sampleTriggers = triggerEvents.map((event) => ({ ...event,
    elapsedMs: event.sampleIndex === null ? event.elapsedMs : event.sampleIndex * 1000 / rate }));
  const psth = buildPsth(spikeEvents, sampleTriggers, state.analysisChannel, 500, 1000, 50);
  byId("psth-triggers").textContent = `${formatNumber(psth.triggerCount)} triggers`;
  drawHistogram("psth-canvas", psth.ratesHz, "+1 s", "#e7ad52", 1 / 3);

  const snippets = spikePanel.history.snippets(state.analysisChannel, 12).map((event) => event.waveformUv);
  byId("snippet-count").textContent = `${formatNumber(snippets.length)} snippets`;
  drawSnippets(snippets);
}

function drawHistogram(canvasId, bins, endLabel, color, zeroPosition = 0) {
  const prepared = prepareAnalysisCanvas(canvasId);
  if (!prepared) return;
  const { context, width, height } = prepared;
  const plot = drawGrid(context, width, height, { left: 35, right: 12, top: 12, bottom: 22 });
  const maximum = Math.max(0, ...bins);
  if (maximum <= 0) {
    context.fillStyle = "#667478";
    context.font = '10px "Segoe UI", sans-serif';
    context.textAlign = "center";
    context.fillText("暂无可统计事件", (plot.left + plot.right) / 2, (plot.top + plot.bottom) / 2);
  } else {
    const slot = (plot.right - plot.left) / bins.length;
    context.fillStyle = color;
    bins.forEach((value, index) => {
      const barHeight = value / maximum * (plot.bottom - plot.top);
      context.fillRect(plot.left + index * slot + 1, plot.bottom - barHeight, Math.max(1, slot - 2), barHeight);
    });
  }
  if (zeroPosition > 0) {
    const zeroX = plot.left + (plot.right - plot.left) * zeroPosition;
    context.strokeStyle = "rgba(239, 188, 91, 0.8)";
    context.beginPath();
    context.moveTo(zeroX, plot.top);
    context.lineTo(zeroX, plot.bottom);
    context.stroke();
  }
  context.fillStyle = "#758589";
  context.font = '9px "Segoe UI", sans-serif';
  context.textAlign = "left";
  context.fillText(zeroPosition > 0 ? "−0.5 s" : "0", plot.left, height - 6);
  context.textAlign = "right";
  context.fillText(endLabel, plot.right, height - 6);
}

function drawSnippets(snippets) {
  const prepared = prepareAnalysisCanvas("snippet-canvas");
  if (!prepared) return;
  const { context, width, height } = prepared;
  const plot = drawGrid(context, width, height, { left: 35, right: 12, top: 12, bottom: 20 });
  if (!snippets.length) {
    context.fillStyle = "#667478";
    context.font = '10px "Segoe UI", sans-serif';
    context.textAlign = "center";
    context.fillText("当前历史中没有可对齐的尖峰片段", (plot.left + plot.right) / 2, (plot.top + plot.bottom) / 2);
    return;
  }
  const maximum = Math.max(1, ...snippets.flat().map((value) => Math.abs(value)));
  snippets.forEach((snippet, snippetIndex) => {
    context.beginPath();
    snippet.forEach((value, index) => {
      const x = plot.left + index / Math.max(1, snippet.length - 1) * (plot.right - plot.left);
      const y = (plot.top + plot.bottom) / 2 - value / maximum * (plot.bottom - plot.top) * 0.44;
      if (index === 0) context.moveTo(x, y);
      else context.lineTo(x, y);
    });
    context.strokeStyle = `hsla(${160 + snippetIndex * 8} 62% 62% / 0.48)`;
    context.lineWidth = 1;
    context.stroke();
  });
  context.strokeStyle = "rgba(239, 181, 82, 0.8)";
  context.beginPath();
  const spikeConfig = spikePanel.history.batch?.config;
  const zeroX = plot.left + (spikeConfig ? spikeConfig.preMs / (spikeConfig.preMs + spikeConfig.postMs) : 1 / 3) * (plot.right - plot.left);
  context.moveTo(zeroX, plot.top);
  context.lineTo(zeroX, plot.bottom);
  context.stroke();
}

function formatFrequency(value) {
  const frequency = Math.max(0, Number(value) || 0);
  return frequency >= 1000 ? `${(frequency / 1000).toFixed(frequency >= 10000 ? 0 : 1)} kHz` : `${frequency.toFixed(frequency < 10 ? 1 : 0)} Hz`;
}

function setMeaCellLabel(cell, channel, profile) {
  if (profile.id !== "mea-ab-64") { cell.textContent = channel.siteNumber ?? channel.label; return; }
  cell.classList.add("has-port-label");
  const site = document.createElement("span");
  site.className = "mea-site-number"; site.textContent = channel.siteNumber;
  const port = document.createElement("small");
  port.className = "mea-port-label"; port.textContent = channel.label;
  cell.replaceChildren(site, port);
  cell.setAttribute("aria-label", `电极 ${channel.siteNumber} · ${channel.label}`);
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
    setMeaCellLabel(cell, channel, profile);
    cell.title = channel.siteNumber === null
      ? `${channel.label} · row ${channel.row + 1}, column ${channel.column + 1}`
      : `电极 ${channel.siteNumber} · ${channel.label} · row ${channel.row + 1}, column ${channel.column + 1}`;
    if (options.selected?.has(channel.id)) cell.classList.add("is-selected");
    if (options.focused === channel.id) cell.classList.add("is-focused");
    if (channel.enabled === false) {
      cell.classList.add("is-disabled");
      if (cell instanceof HTMLButtonElement) cell.disabled = true;
    }
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
    setPipelineValidation("valid", pipelineValidationMessage());
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

const experimentPresetKey = "rhs-studio.experiment-presets.v1";
const experimentPresetVersion = 1;

function initializeExperimentPresets() {
  try {
    const stored = JSON.parse(localStorage.getItem(experimentPresetKey) || "[]");
    state.experimentPresets = Array.isArray(stored)
      ? stored.filter((preset) => preset?.version === experimentPresetVersion && preset?.name && preset?.workspace)
      : [];
  } catch (_) {
    state.experimentPresets = [];
  }
  renderExperimentPresets();
}

function persistExperimentPresets() {
  localStorage.setItem(experimentPresetKey, JSON.stringify(state.experimentPresets));
}

function renderExperimentPresets(selectedName = "") {
  const select = byId("experiment-preset");
  select.innerHTML = '<option value="">未保存方案</option>';
  for (const preset of state.experimentPresets) {
    const option = document.createElement("option");
    option.value = preset.name;
    option.textContent = preset.name;
    select.appendChild(option);
  }
  select.value = selectedName;
  byId("experiment-saved-count").textContent = String(state.experimentPresets.length);
  renderExperimentPresetActions();
}

function renderExperimentPresetActions() {
  const selected = Boolean(byId("experiment-preset").value);
  byId("load-experiment").disabled = !selected;
  byId("delete-experiment").disabled = !selected;
}

function openExperimentPresetDialog() {
  const selected = byId("experiment-preset").value;
  byId("experiment-preset-name").value = selected || `实验方案 ${state.experimentPresets.length + 1}`;
  byId("experiment-preset-dialog").showModal();
  byId("experiment-preset-name").focus();
  byId("experiment-preset-name").select();
}

function experimentWorkspaceSnapshot() {
  return {
    capturedAt: new Date().toISOString(),
    timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    utcOffsetMinutes: -new Date().getTimezoneOffset(),
    session: sessionMetadata(),
    meaProfileId: currentMeaProfile().id,
    channelSettings: deepClone(state.channelSettings),
    pipeline: deepClone(state.pipelineDraft),
    stimulation: deepClone(state.stimConfigs),
    acquisition: {
      blocksPerRead: Number(byId("blocks-per-read").value),
      queueCapacity: Number(byId("queue-capacity").value),
      bitfile: byId("bitfile-path").value.trim(),
      serial: byId("device-serial").value.trim()
    },
    recording: {
      autoName: byId("recording-auto-name").checked,
      triggerAutoName: byId("trigger-recording-auto-name").checked,
      path: byId("recording-path").value.trim(),
      playbackPath: byId("playback-path").value.trim(),
      triggeredPath: byId("trigger-recording-path").value.trim(),
      triggerSource: byId("trigger-recording-source").value,
      preTriggerMs: Number(byId("pre-trigger-ms").value),
      postTriggerMs: Number(byId("post-trigger-ms").value),
      digitalMask: Number(byId("digital-trigger-mask").value),
      analogChannel: Number(byId("analog-trigger-channel").value),
      analogThreshold: Number(byId("analog-trigger-threshold").value),
      analogDirection: byId("analog-trigger-direction").value
    },
    stimulationSchedule: {
      delayMs: Number(byId("stim-delay").value),
      repeatCount: Number(byId("stim-repeat-count").value),
      repeatIntervalMs: Number(byId("stim-repeat-interval").value)
    },
    display: {
      waveformLayout: byId("waveform-layout").value,
      spikeMarkers: byId("waveform-spike-markers").checked,
      gain: Number(byId("display-gain").value),
      timeWindowMs: Number(byId("time-window").value),
      focusChannel: renderer.focusChannel,
      channelRanges: deepClone(state.waveformRanges),
      panels: { ...state.panels }
    },
    analysis: {
      channel: Number(byId("spectrum-channel").value || 0),
      spectrumRange: Number(byId("spectrum-range").value),
      isiRange: Number(byId("isi-range").value),
      activityMetric: byId("activity-metric").value,
      activityWindow: Number(byId("activity-window").value),
      activityFlashMs: Number(byId("activity-flash-ms").value),
      activityRateMax: Number(byId("activity-rate-max").value),
      spikeWindow: Number(byId("spike-window").value)
    },
    tools: {
      audioChannel: Number(byId("audio-channel").value || 0),
      audioGain: Number(byId("audio-gain").value),
      impedanceFrequency: Number(byId("impedance-frequency").value)
    }
  };
}

function sessionMetadata() {
  return {
    sessionName: byId("session-name").value.trim(),
    operator: byId("session-operator").value.trim(),
    subjectId: byId("session-subject").value.trim(),
    notes: byId("session-notes").value.trim()
  };
}

function saveExperimentPreset(event) {
  event.preventDefault();
  const name = byId("experiment-preset-name").value.trim().slice(0, 40);
  if (!name || !state.pipelineDraft) return;
  const preset = {
    version: experimentPresetVersion,
    name,
    savedAt: new Date().toISOString(),
    workspace: experimentWorkspaceSnapshot()
  };
  const existingIndex = state.experimentPresets.findIndex((candidate) => candidate.name === name);
  if (existingIndex >= 0) state.experimentPresets[existingIndex] = preset;
  else state.experimentPresets.push(preset);
  try {
    persistExperimentPresets();
    renderExperimentPresets(name);
    byId("experiment-preset-dialog").close();
    toast(`实验方案已保存：${name}`, "success");
  } catch (error) {
    toast(`实验方案保存失败：${normalizeError(error)}`, "error");
  }
}

function loadSelectedExperimentPreset() {
  const name = byId("experiment-preset").value;
  const preset = state.experimentPresets.find((candidate) => candidate.name === name);
  if (!preset) return;
  try {
    applyExperimentWorkspace(preset.workspace);
    toast(`实验方案已载入：${name}`, "success");
  } catch (error) {
    toast(`实验方案载入失败：${normalizeError(error)}`, "error");
  }
}

function applyExperimentWorkspace(workspace) {
  if (!workspace || typeof workspace !== "object") throw new Error("工作区内容无效");
  state.pipelineDraft = migratePipelineConfig(workspace.pipeline);
  state.stimConfigs = workspace.stimulation && typeof workspace.stimulation === "object"
    ? deepClone(workspace.stimulation)
    : {};
  state.meaProfileId = getMeaProfile(workspace.meaProfileId).id;
  state.channelSettings = workspace.channelSettings && typeof workspace.channelSettings === "object"
    ? deepClone(workspace.channelSettings)
    : state.channelSettings;
  state.gameFocusChannel = null;
  state.activityFocusChannel = null;

  const metadata = workspace.session || {};
  byId("session-name").value = String(metadata.sessionName || "").slice(0, 80);
  byId("session-operator").value = String(metadata.operator || "").slice(0, 80);
  byId("session-subject").value = String(metadata.subjectId || "").slice(0, 80);
  byId("session-notes").value = String(metadata.notes || "").slice(0, 4_000);

  byId("blocks-per-read").value = String(Math.min(32, Math.max(1, Number(workspace.acquisition?.blocksPerRead) || 8)));
  byId("queue-capacity").value = String(Math.min(128, Math.max(1, Number(workspace.acquisition?.queueCapacity) || 8)));
  if (workspace.acquisition?.bitfile !== undefined) byId("bitfile-path").value = String(workspace.acquisition.bitfile || "");
  if (workspace.acquisition?.serial !== undefined) byId("device-serial").value = String(workspace.acquisition.serial || "");

  const recording = workspace.recording || {};
  byId("recording-auto-name").checked = recording.autoName !== false;
  byId("trigger-recording-auto-name").checked = recording.triggerAutoName !== false;
  if (recording.path) byId("recording-path").value = String(recording.path);
  if (recording.playbackPath) byId("playback-path").value = String(recording.playbackPath);
  if (recording.triggeredPath) byId("trigger-recording-path").value = String(recording.triggeredPath);
  if (["software", "digital", "analog"].includes(recording.triggerSource)) byId("trigger-recording-source").value = recording.triggerSource;
  byId("pre-trigger-ms").value = String(Math.min(30_000, Math.max(0, Number(recording.preTriggerMs) || 0)));
  byId("post-trigger-ms").value = String(Math.min(3_600_000, Math.max(1, Number(recording.postTriggerMs) || 5_000)));
  byId("digital-trigger-mask").value = String(Math.min(65_535, Math.max(1, Number(recording.digitalMask) || 1)));
  byId("analog-trigger-channel").value = String(Math.min(120, Math.max(1, Number(recording.analogChannel) || 1)));
  byId("analog-trigger-threshold").value = String(Math.min(65_535, Math.max(0, Number(recording.analogThreshold) || 40_000)));
  if (["rising-above", "falling-below"].includes(recording.analogDirection)) byId("analog-trigger-direction").value = recording.analogDirection;
  renderTriggerRecordingSource();

  const schedule = workspace.stimulationSchedule || {};
  byId("stim-delay").value = String(Math.min(3_600_000, Math.max(0, Number(schedule.delayMs) || 0)));
  byId("stim-repeat-count").value = String(Math.min(1_000, Math.max(1, Number(schedule.repeatCount) || 1)));
  byId("stim-repeat-interval").value = String(Math.min(3_600_000, Math.max(0, Number(schedule.repeatIntervalMs) || 0)));

  const gain = Math.min(4, Math.max(0.05, Number(workspace.display?.gain) || 1));
  byId("waveform-spike-markers").checked = workspace.display?.spikeMarkers !== false;
  renderer.setSpikeMarkersVisible(byId("waveform-spike-markers").checked);
  const timeWindowMs = [100, 250, 500, 1000].includes(Number(workspace.display?.timeWindowMs))
    ? Number(workspace.display.timeWindowMs)
    : 250;
  byId("display-gain").value = String(gain);
  byId("time-window").value = String(timeWindowMs);
  renderer.setGain(gain);
  renderer.setTimeWindow(timeWindowMs);
  byId("waveform-layout").value = workspace.display?.waveformLayout === "single-16" ? "single-16" : "dual-64";
  renderer.setLayout(byId("waveform-layout").value);
  state.waveformRanges = {};
  for (const profile of MEA_PROFILES) {
    for (const channel of profile.channels) {
      const range = normalizeWaveformRange(workspace.display?.channelRanges?.[profile.id]?.[channel.id]);
      if (range) { state.waveformRanges[profile.id] ||= {}; state.waveformRanges[profile.id][channel.id] = range; }
    }
  }
  state.panels = { left: workspace.display?.panels?.left !== false, right: workspace.display?.panels?.right !== false };
  renderPanelVisibility();
  renderWaveformAmplitude();

  persistMeaProfile();
  persistChannelSettings();
  persistStimConfigs();
  state.expandedNodes.clear();
  applyMeaProfile();
  renderPipelineEditor();

  const analysis = workspace.analysis || {};
  const requestedChannel = Number(analysis.channel);
  const channelOption = String(Number.isFinite(requestedChannel) ? requestedChannel : 0);
  if ([...byId("spectrum-channel").options].some((option) => option.value === channelOption)) {
    byId("spectrum-channel").value = channelOption;
    byId("statistics-channel").value = channelOption;
    state.analysisChannel = Number(channelOption);
  }
  if (["1000", "3000", "5000", "10000", "15000"].includes(String(analysis.spectrumRange))) byId("spectrum-range").value = String(analysis.spectrumRange);
  if (["100", "250", "500", "1000"].includes(String(analysis.isiRange))) byId("isi-range").value = String(analysis.isiRange);
  byId("activity-metric").value = analysis.activityMetric === "spike-rate" ? "spike-rate" : "spike-flash";
  if ([1000, 5000, 10000, 30000].includes(Number(analysis.activityWindow))) byId("activity-window").value = String(analysis.activityWindow);
  if ([100, 200, 500].includes(Number(analysis.activityFlashMs))) byId("activity-flash-ms").value = String(analysis.activityFlashMs);
  byId("activity-rate-max").value = String(Math.min(1000, Math.max(0.1, Number(analysis.activityRateMax) || 20)));
  if (["1000", "5000", "10000", "30000"].includes(String(analysis.spikeWindow))) byId("spike-window").value = String(analysis.spikeWindow);
  const focus = workspace.display?.focusChannel;
  focusWaveformChannel(Number.isInteger(focus) && [...byId("focus-channel").options].some((option) => !option.disabled && Number(option.value) === focus) ? focus : null);

  const tools = workspace.tools || {};
  byId("audio-gain").value = String(Math.min(4, Math.max(0, Number(tools.audioGain) || 0.5)));
  updateAudioGain();
  const audioChannel = String(Number(tools.audioChannel) || 0);
  if ([...byId("audio-channel").options].some((option) => option.value === audioChannel)) byId("audio-channel").value = audioChannel;
  byId("impedance-frequency").value = String(Math.min(2_000, Math.max(10, Number(tools.impedanceFrequency) || 1_000)));
  renderAudioState();
  renderActivityView();
  renderSpikeView();
  renderFrequencyAnalysis();
  renderEventAnalysis();
  updatePipeline();
  scheduleRecoverySave();
}

const recoveryKey = "rhs-studio.workspace-recovery.v1";
const recoveryVersion = 1;
let recoveryTimer;

function scheduleRecoverySave() {
  if (!state.pipelineDraft) return;
  window.clearTimeout(recoveryTimer);
  byId("recovery-status").textContent = "保存中";
  recoveryTimer = window.setTimeout(saveRecoverySnapshot, 250);
}

function saveRecoverySnapshot() {
  if (!state.pipelineDraft) return;
  window.clearTimeout(recoveryTimer);
  try {
    localStorage.setItem(recoveryKey, JSON.stringify({
      version: recoveryVersion,
      savedAt: new Date().toISOString(),
      workspace: experimentWorkspaceSnapshot()
    }));
    byId("recovery-status").textContent = `已保存 ${new Date().toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}`;
  } catch (error) {
    byId("recovery-status").textContent = "保存失败";
  }
}

function restoreRecoverySnapshot() {
  try {
    const snapshot = JSON.parse(localStorage.getItem(recoveryKey) || "null");
    if (!snapshot || snapshot.version !== recoveryVersion || !snapshot.workspace) return false;
    applyExperimentWorkspace(snapshot.workspace);
    byId("recovery-status").textContent = "已恢复";
    toast("已恢复上次工作区配置", "info");
    return true;
  } catch (error) {
    byId("recovery-status").textContent = "恢复失败";
    return false;
  }
}

function deleteSelectedExperimentPreset() {
  const name = byId("experiment-preset").value;
  if (!name || !window.confirm(`删除实验方案“${name}”？`)) return;
  state.experimentPresets = state.experimentPresets.filter((preset) => preset.name !== name);
  try {
    persistExperimentPresets();
    renderExperimentPresets();
    toast(`实验方案已删除：${name}`, "info");
  } catch (error) {
    toast(`实验方案删除失败：${normalizeError(error)}`, "error");
  }
}

function setPipelineValidation(kind, message) {
  const target = byId("pipeline-validation");
  const icon = kind === "valid" ? "circle-check" : kind === "invalid" ? "circle-alert" : "loader-circle";
  target.className = `pipeline-validation ${kind}`;
  target.innerHTML = `<i data-lucide="${icon}"></i><span>${escapeHtml(message)}</span>`;
  window.lucide?.createIcons({ attrs: { "stroke-width": 1.8 } });
}

function pipelineValidationMessage(prefix = "配置有效 · SampleBlock · 30 kHz") {
  const playbackChannels = state.status?.playback?.channelCount;
  const deviceChannels = state.status?.device?.capabilities?.acquisitionChannelLimit
    ?? state.status?.device?.channels;
  const channelCount = playbackChannels
    ?? (bridge.isPreview ? currentMeaProfile().channelCount : deviceChannels)
    ?? currentMeaProfile().channelCount;
  return `${prefix} · ${channelCount} ch`;
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
  scheduleRecoverySave();
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
      setPipelineValidation("valid", pipelineValidationMessage("配置有效 · 已按当前顺序重建"));
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
  if (state.status) renderStatus(state.status);
  try {
    await action();
    await refreshStatus();
    await refreshEvents();
  } catch (error) {
    if (notifyError) toast(normalizeError(error), "error");
  } finally {
    state.pending.delete(key);
    if (state.status) renderStatus(state.status);
    if (state.pending.size === 0) document.body.classList.remove("busy");
  }
}

function openAnalysisExport() {
  const path = byId("playback-path").value.trim() || byId("recording-path").value.trim();
  byId("analysis-export-source").value = path;
  byId("analysis-export-directory").value = path ? analysisExportDirectory(path) : "";
  byId("analysis-export-result").textContent = "";
  byId("analysis-export-dialog").showModal();
}

async function exportAnalysisRecording(event) {
  event.preventDefault();
  const button = byId("submit-analysis-export");
  if (button.disabled) return;
  const output = byId("analysis-export-result");
  button.disabled = true;
  output.textContent = "正在校验并导出完整记录…";
  try {
    const result = await bridge.invoke("export_recording_analysis", {
      path: byId("analysis-export-source").value.trim(),
      directory: byId("analysis-export-directory").value.trim(),
      includeCsv: byId("analysis-export-format").value === "csv"
    });
    output.textContent = `已导出 ${formatNumber(result.samplesPerChannel)} 样本/通道 · ${result.channelCount} 通道 · ${result.directory}`;
    toast("完整原始记录已导出", "success");
  } catch (error) {
    output.textContent = normalizeError(error);
    toast(normalizeError(error), "error");
  } finally {
    button.disabled = false;
  }
}

async function refreshStatus() {
  try {
    const status = await bridge.invoke("get_status");
    const previousError = state.status?.lastError;
    state.status = status;
    renderStatus(status);
    if (status.mode === "fault" && status.lastError && status.lastError !== previousError) {
      toast(status.lastError, "error");
    }
  } catch (error) {
    renderConnectionError(normalizeError(error));
  }
}

let waveformRefreshRunning = false;

async function refreshWaveform() {
  if (state.displayPaused || waveformRefreshRunning) return;
  waveformRefreshRunning = true;
  try {
    const batch = await bridge.invoke("poll_waveforms", { afterCursor: state.waveformCursor, generation: state.waveformGeneration });
    if (state.waveformGeneration !== batch.generation) {
      renderer.clearFrames();
      state.latestFrame = null;
      state.signalHistory = null;
      state.waveformGapCount = 0;
      byId("plot-display-gap").hidden = true;
      state.audio.lastSequence = null;
    }
    state.waveformGeneration = batch.generation;
    const frames = batch.frames;
    if (!frames.length) { state.waveformCursor = batch.nextCursor; return; }
    const previousChannelCount = state.signalHistory?.channels?.length || 0;
    for (const frame of frames) {
      const previous = state.latestFrame;
      if (previous && (frame.firstSampleIndex !== previous.firstSampleIndex + (previous.sampleCount ?? previous.pointCount * previous.binWidthSamples)
          || (frame.flags & 1))) state.waveformGapCount += 1;
      state.signalHistory = appendSignalHistory(state.signalHistory, frame, 16384);
      state.latestFrame = frame;
      enqueueAudioFrame(frame);
    }
    const frame = state.latestFrame;
    state.channelActivity = frameChannelActivity(frame);
    renderer.setFrames(frames);
    state.waveformCursor = batch.nextCursor;
    byId("plot-display-gap").hidden = !state.waveformGapCount && !batch.droppedFrames;
    byId("plot-display-gap").textContent = `显示缺口 ${state.waveformGapCount || 1}`;
    renderWaveformAmplitude();
    renderActivityView();
    if (previousChannelCount !== frame.channelCount) populateAnalysisChannels();
    renderFrequencyAnalysis();
    renderEventAnalysis();
    byId("plot-empty").hidden = true;
    byId("plot-rate").textContent = `${(frame.sampleRateHz / 1000).toFixed(1)} kHz`;
  } catch (_) {
    // The low-rate status channel owns user-visible connection errors.
  } finally {
    waveformRefreshRunning = false;
  }
}

async function refreshEvents() {
  try {
    const events = await bridge.invoke("poll_events", { afterId: state.latestEventId });
    if (!events.length) return;
    state.events.push(...events);
    state.events = state.events.slice(-200);
    state.latestEventId = Math.max(state.latestEventId, ...events.map((event) => event.id));
    renderSpikeView();
    renderActivityView();
    renderEventAnalysis();
  } catch (_) {
    // Status polling reports backend failures; diagnostics retain bounded events.
  }
}

function renderStatus(status) {
  const active = status.mode === "acquiring" || status.mode === "recording";
  const playback = status.playback || null;
  const playbackOpen = Boolean(playback);
  const playbackPlaying = playback?.state === "playing";
  const triggeredRecording = status.triggeredRecording || null;
  const stimulation = status.stimulation || { state: "disarmed", outputMode: "interlocked", pendingCommands: 0, completedCommands: 0 };
  const triggerActive = ["armed", "trigger-pending", "capturing"].includes(triggeredRecording?.state);
  const connected = Boolean(status.device);
  const impedanceBusy = impedanceControls(status).busy;
  byId("mea-profile").disabled = impedanceBusy;
  byId("open-channel-editor").disabled = impedanceBusy;
  document.body.classList.toggle("is-recording", status.recording);
  document.body.classList.toggle("has-fault", status.health === "fault");
  const faultNotice = acquisitionFaultNotice(status);
  byId("acquisition-fault").hidden = !faultNotice;
  byId("acquisition-fault-title").textContent = faultNotice?.title || "";
  byId("acquisition-fault-detail").textContent = faultNotice?.detail || "";
  byId("acquisition-fault-error").textContent = faultNotice?.error || "";

  byId("connection-state").className = `connection-state ${connected || playbackOpen ? "connected" : ""} ${bridge.isPreview ? "preview" : ""}`;
  byId("connection-label").textContent = playbackOpen
    ? `回放 · ${playback.channelCount} ch`
    : bridge.isPreview ? "演示模式" : connected ? `${status.device.serial} · Port ${status.device.port}` : "未连接";
  byId("device-name").textContent = playbackOpen ? "记录回放" : deviceDisplayName(status.device, bridge.isPreview);
  byId("device-route").textContent = playbackOpen
    ? `${(playback.sampleRateHz / 1000).toFixed(1)} kHz · ${formatNumber(playback.blockCount)} blocks`
    : status.device ? deviceRouteLabel(status.device) : "等待扫描";
  byId("device-led").classList.toggle("online", connected || playbackOpen);
  byId("mode-chip").textContent = status.mode.toUpperCase();
  byId("elapsed-time").textContent = formatDuration(status.elapsedMs);

  byId("scan-device").disabled = active || playbackOpen || impedanceBusy;
  byId("start-acquisition").disabled = active || playbackOpen || status.mode === "connecting" || impedanceBusy;
  byId("stop-acquisition").disabled = !active && status.mode !== "fault";
  byId("record-toggle").disabled = !active;
  byId("record-toggle").classList.toggle("active", status.recording);
  byId("record-toggle").querySelector("span").textContent = status.recording ? "停止记录" : "开始记录";
  byId("record-status").textContent = status.recording ? "REC" : "OFF";
  byId("record-status").classList.toggle("active", status.recording);
  byId("health-storage").textContent = status.recording ? "写入中" : "未启用";
  if (triggeredRecording?.state === "capturing") byId("health-storage").textContent = "触发写入";
  byId("health-usb").textContent = playbackOpen ? "文件回放" : active ? "流式传输" : connected ? "就绪" : "待机";
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
  byId("open-playback").disabled = active || playbackOpen || impedanceBusy;
  byId("playback-toggle").disabled = !playbackOpen || playback?.state === "fault";
  byId("close-playback").disabled = !playbackOpen;
  byId("playback-speed").disabled = !playbackOpen;
  byId("playback-position").disabled = !playbackOpen;
  byId("playback-position").value = playbackOpen && playback.totalSamples
    ? String(Math.round((playback.positionSamples / playback.totalSamples) * 1000))
    : "0";
  byId("playback-time").textContent = formatPreciseDuration(playback?.positionMs || 0);
  byId("playback-duration").textContent = formatPreciseDuration(playback?.durationMs || 0);
  byId("playback-speed").value = String(playback?.speed || 1);
  byId("playback-status").textContent = playback?.state?.toUpperCase() || "CLOSED";
  byId("playback-status").classList.toggle("active", playbackPlaying);
  const playbackToggle = byId("playback-toggle");
  playbackToggle.title = playbackPlaying ? "暂停" : "播放";
  playbackToggle.innerHTML = playbackPlaying
    ? '<i data-lucide="pause"></i>'
    : '<i data-lucide="play"></i>';
  window.lucide?.createIcons({ nodes: [playbackToggle], attrs: { "stroke-width": 1.8 } });
  byId("arm-trigger-recording").disabled = !active || triggerActive;
  byId("fire-recording-trigger").disabled = !active
    || !["armed", "trigger-pending"].includes(triggeredRecording?.state)
    || triggeredRecording?.source !== "software";
  byId("cancel-trigger-recording").disabled = !triggerActive;
  byId("trigger-recording-path").disabled = triggerActive;
  byId("recording-path").disabled = Boolean(status.recording);
  byId("recording-auto-name").disabled = Boolean(status.recording);
  byId("trigger-recording-auto-name").disabled = triggerActive;
  byId("trigger-recording-source").disabled = triggerActive;
  byId("trigger-recording-status").textContent = triggeredRecording?.state?.toUpperCase() || "IDLE";
  byId("trigger-recording-status").classList.toggle("active", triggerActive);
  byId("trigger-recording-count").textContent = `${formatNumber(triggeredRecording?.capturedBlocks || 0)} blocks`;
  const stimulationBusy = ["arm-stimulation", "trigger-stimulation", "cancel-stimulation", "disarm-stimulation"]
    .some(key => state.pending.has(key));
  const controls = stimulationControls(stimulation, active, stimulationBusy);
  const stimulationArmed = controls.armed;
  const stimulationPending = controls.pending;
  byId("arm-stimulation").disabled = controls.armDisabled;
  byId("trigger-stimulation").disabled = controls.triggerDisabled;
  byId("cancel-stimulation").disabled = controls.cancelDisabled;
  byId("disarm-stimulation").disabled = controls.disarmDisabled;
  byId("stim-state-label").textContent = String(stimulation.state || "disarmed").toUpperCase();
  byId("stim-output-mode").textContent = String(stimulation.outputMode || "interlocked").toUpperCase();
  const realOutput = isHardwareOutputMode(stimulation.outputMode) && stimulationArmed;
  const functional = stimulation.outputMode === "hardware-functional";
  const configurable = stimulation.outputMode === "hardware-configurable";
  const functionalLimit = stimulation.functionalTriggerLimit || 1;
  const functionalUsed = stimulation.submittedCommands ?? stimulation.completedCommands ?? 0;
  byId("open-stim-config").disabled = active || stimulationArmed || stimulationPending;
  byId("open-stim-config").title = active ? "停止采集后修改通道与参数" : "配置通道与参数";
  byId("stim-safety-title").textContent = realOutput ? (functional ? "单通道联调已 Arm" : configurable ? "可配置真实输出已 Arm" : "真实台架输出已 Arm") : "真实输出已锁定";
  byId("stim-safety-detail").textContent = realOutput
    ? (functional ? (functionalUsed >= functionalLimit && !stimulationPending ? "联调次数已用完 · 输出关闭" : `单通道 · ${functionalUsed}/${functionalLimit} 次 · 60 秒限时`) : configurable ? `未实测验收 · ${stimulation.submittedCommands || 0}/${stimulation.commandLimit || 16} 指令 · 60 秒限时` : "60 秒限时 · 双相有限脉冲")
    : "未 Arm · 输出关闭";
  if (faultNotice) {
    byId("stim-safety-title").textContent = "故障联锁 · 输出已禁用";
    byId("stim-safety-detail").textContent = faultNotice.title;
  }
  byId("stim-real-output").disabled = bridge.isPreview || stimulationPending || stimulationArmed;
  renderStimValidationControls(stimulationArmed, stimulationPending);
  byId("stim-audit-path").textContent = stimulation.auditPath ? `刺激审计：${stimulation.auditPath}` : "";
  byId("stim-command-count").textContent = `${formatNumber(stimulation.pendingCommands || 0)} pending · ${formatNumber(stimulation.completedCommands || 0)} done`;
  const programmed = stimulation.programmedParameters;
  byId("stim-programmed-summary").hidden = !programmed;
  byId("stim-programmed-summary").textContent = programmed
    ? `已编程：${formatNumber(programmed.amplitudeNa)} nA · ${formatNumber(programmed.phaseWidthUs)} µs/相 · 间隔 ${formatNumber(programmed.interphaseUs)} µs · ${formatNumber(programmed.frequencyHz)} Hz · ${programmed.pulseCount} 脉冲 · ${formatNumber(programmed.chargePerPhasePc)} pC/相`
    : "";
  renderImpedanceState(status.impedance);
  byId("diagnostic-summary").textContent = `${status.health.toUpperCase()} · ${formatNumber(metrics.timestampGaps)} gaps · ${formatNumber(metrics.queueOverflows)} overflows`;
  for (const id of ["session-name", "session-operator", "session-subject", "session-notes"]) byId(id).disabled = active;
  renderSpikeView();

  if (status.lastError) renderConnectionError(status.lastError);
}

function triggeredRecordingRequest() {
  return {
    path: byId("trigger-recording-path").value.trim(),
    workspace: recordingWorkspace(experimentWorkspaceSnapshot()),
    source: byId("trigger-recording-source").value,
    preTriggerMs: Number(byId("pre-trigger-ms").value),
    postTriggerMs: Number(byId("post-trigger-ms").value),
    digitalMask: Number(byId("digital-trigger-mask").value),
    analogChannelIndex: Number(byId("analog-trigger-channel").value) - 1,
    analogThreshold: Number(byId("analog-trigger-threshold").value),
    analogDirection: byId("analog-trigger-direction").value
  };
}

function renderTriggerRecordingSource() {
  const source = byId("trigger-recording-source").value;
  document.querySelectorAll(".trigger-source-field").forEach((field) => { field.hidden = true; });
  document.querySelectorAll(`.${source}-trigger-field`).forEach((field) => { field.hidden = false; });
  byId("trigger-recording-source-label").textContent = {
    software: "软件触发",
    digital: "数字输入上升沿",
    analog: "模拟阈值越过"
  }[source];
}

function deviceRouteLabel(device) {
  const capabilities = device.capabilities;
  if (!capabilities) return `Port ${device.port} · Stream ${device.stream}`;
  const active = capabilities.acquisitionChannelLimit ?? device.channels;
  const detected = capabilities.detectedChannelCount ?? active;
  const channelSummary = detected > active
    ? `本次 ${active} · 检测 ${detected} 通道`
    : `${active} 通道`;
  return `Port ${device.port} · Stream ${device.stream} · ${channelSummary}`;
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

function openExportDialog() {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  byId("export-filename").value = `neurox-session-${stamp}`;
  byId("export-dialog").showModal();
  byId("export-filename").focus();
  byId("export-filename").select();
}

function exportDiagnostics() {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const diagnostics = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    application: { name: "NeuroX Studio", version: "0.1.0", backend: bridge.isPreview ? "preview" : "tauri" },
    environment: {
      platform: navigator.platform,
      language: navigator.language,
      hardwareConcurrency: navigator.hardwareConcurrency || null,
      deviceMemoryGb: navigator.deviceMemory || null,
      userAgent: navigator.userAgent
    },
    status: state.status,
    workspace: experimentWorkspaceSnapshot(),
    events: state.events.slice(-200),
    note: "Access tokens and active audio objects are intentionally excluded."
  };
  downloadTextFile(`neurox-diagnostics-${stamp}.json`, JSON.stringify(diagnostics, null, 2), "application/json;charset=utf-8");
  toast("运行诊断已导出", "success");
}

function exportTables() {
  const profile = currentMeaProfile();
  const summaryColumns = [{ key: "item", label: "项目" }, { key: "value", label: "值" }];
  const channelColumns = [
    { key: "id", label: "硬件 ID" }, { key: "label", label: "显示名称" },
    { key: "enabled", label: "启用" }, { key: "port", label: "端口" },
    { key: "row", label: "行" }, { key: "column", label: "列" },
    { key: "color", label: "颜色" }, { key: "averagePeakToPeak", label: "平均峰峰值 ADC" },
    { key: "peakDeviation", label: "峰值偏移 ADC" }, { key: "spikeRateHz", label: "放电率 Hz" }
  ];
  const eventColumns = [
    { key: "id", label: "ID" }, { key: "elapsedMs", label: "时间 ms" },
    { key: "level", label: "级别" }, { key: "category", label: "类别" },
    { key: "channel", label: "通道" }, { key: "sampleIndex", label: "采样点" },
    { key: "message", label: "消息" }
  ];
  const raster = currentThresholdRaster();
  const summary = [
    { item: "导出时间", value: new Date().toISOString() },
    { item: "软件", value: "NeuroX Studio" },
    { item: "会话模式", value: state.status?.mode || "idle" },
    { item: "MEA", value: profile.name },
    { item: "通道数", value: profile.channelCount },
    { item: "样本率", value: state.latestFrame?.sampleRateHz || state.status?.device?.sampleRateHz || 0 },
    { item: "样本/通道", value: state.status?.metrics?.samplesPerChannel || 0 },
    { item: "事件数", value: state.events.length },
    { item: "原始记录", value: state.status?.recordingPath || state.status?.playback?.path || "" }
  ];
  const channels = profile.channels.map((channel, index) => ({
    id: channel.id,
    label: channel.label,
    enabled: channel.enabled ? "是" : "否",
    port: channel.port,
    row: channel.row + 1,
    column: channel.column + 1,
    color: channel.color,
    averagePeakToPeak: Math.round(state.channelActivity[index]?.averagePeakToPeak || 0),
    peakDeviation: Math.round(state.channelActivity[index]?.peakDeviation || 0),
    spikeRateHz: Number((raster.ratesHz[index] || 0).toFixed(4))
  }));
  const events = state.events.map((event) => ({
    id: event.id,
    elapsedMs: Number(event.elapsedMs || 0),
    level: event.level,
    category: event.category,
    channel: event.channelIndex === null || event.channelIndex === undefined
      ? ""
      : profile.channels[event.channelIndex]?.label || `CH-${event.channelIndex + 1}`,
    sampleIndex: event.sampleIndex ?? "",
    message: event.message
  }));
  return {
    summary: { name: "会话摘要", rows: summary, columns: summaryColumns },
    channels: { name: "通道", rows: channels, columns: channelColumns },
    events: { name: "事件", rows: events, columns: eventColumns }
  };
}

function exportWorkspace(event) {
  event.preventDefault();
  const format = byId("export-format").value;
  const baseName = byId("export-filename").value.trim().replace(/[\\/:*?"<>|]+/g, "-").replace(/\.+$/g, "");
  if (!baseName) return;
  const tables = exportTables();
  const formats = {
    json: {
      extension: "json",
      mime: "application/json;charset=utf-8",
      content: () => JSON.stringify({
        schemaVersion: 1,
        exportedAt: new Date().toISOString(),
        status: state.status,
        session: sessionMetadata(),
        mea: currentMeaProfile(),
        channelSettings: state.channelSettings,
        pipeline: state.pipelineDraft,
        stimulation: state.stimConfigs,
        events: state.events,
        channelActivity: state.channelActivity,
        signalHistory: state.signalHistory
      }, null, 2)
    },
    csv: {
      extension: "csv",
      mime: "text/csv;charset=utf-8",
      content: () => `\uFEFF${buildCsv(tables.events.rows, tables.events.columns)}`
    },
    html: {
      extension: "html",
      mime: "text/html;charset=utf-8",
      content: () => buildHtmlReport("NeuroX Studio 实验报告", Object.values(tables))
    },
    xls: {
      extension: "xml",
      mime: "application/vnd.ms-excel;charset=utf-8",
      content: () => buildExcelWorkbook(Object.values(tables))
    }
  };
  const selected = formats[format];
  downloadTextFile(`${baseName}.${selected.extension}`, selected.content(), selected.mime);
  byId("export-dialog").close();
  toast(`已导出 ${baseName}.${selected.extension}`, "success");
}

function downloadTextFile(filename, content, mime) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function switchView(id) {
  document.querySelectorAll(".tab").forEach((tab) => tab.classList.toggle("active", tab.dataset.view === id));
  document.querySelectorAll(".view-panel").forEach((panel) => panel.classList.toggle("active", panel.id === id));
  byId("display-tools").hidden = id !== "waveform-view";
  window.requestAnimationFrame(() => {
    if (id === "spectrum-view") renderFrequencyAnalysis();
    if (id === "statistics-view") renderEventAnalysis();
    if (id === "device-tools-view") {
      renderImpedanceState();
      renderAudioState();
    }
  });
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

function formatPreciseDuration(milliseconds) {
  const value = Math.max(0, Number(milliseconds) || 0);
  const minutes = Math.floor(value / 60000);
  const seconds = Math.floor((value % 60000) / 1000);
  const millis = Math.floor(value % 1000);
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${String(millis).padStart(3, "0")}`;
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
