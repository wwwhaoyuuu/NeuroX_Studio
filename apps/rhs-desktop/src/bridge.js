import { timestampRecordingPath } from "./recording.js";

const hasTauri = Boolean(window.__TAURI__?.core?.invoke);

function nowPath() {
  return timestampRecordingPath("work/");
}

function triggeredPath() {
  return timestampRecordingPath("work/", "triggered");
}

const numberParameter = (key, label, unit, minimum, maximum, step, displayOffset = 0) => ({
  key, label, control: "number", unit, minimum, maximum, step, displayOffset, choices: []
});

const choiceParameter = (key, label, choices) => ({
  key, label, control: "select", unit: null, minimum: null, maximum: null,
  step: null, displayOffset: 0, choices
});

const pipelineRegistry = {
  schemaVersion: 1,
  sourceType: "sample-block",
  nodes: [
    {
      kind: "artifact-blanking",
      label: "伪迹消隐",
      description: "TTL 上升沿后替换一段采样，抑制刺激伪迹。",
      icon: "eraser",
      tone: "amber",
      inputType: "sample-block",
      outputType: "sample-block",
      preservesSampleRate: true,
      preservesChannels: true,
      maxInstances: 8,
      parameters: [
        numberParameter("ttlMask", "TTL 掩码", null, 1, 65535, 1),
        numberParameter("blankSamples", "消隐长度", "samples", 1, 30000, 1),
        choiceParameter("replacement", "替换方式", [
          { value: "hold-last", label: "保持前值" },
          { value: "midpoint", label: "ADC 中点" }
        ])
      ],
      defaultNode: {
        kind: "artifact-blanking", id: "artifact-blanking-template", enabled: true,
        ttlMask: 1, blankSamples: 90, replacement: "hold-last"
      }
    },
    {
      kind: "notch",
      label: "陷波滤波",
      description: "抑制指定频率的窄带工频干扰。",
      icon: "waves",
      tone: "teal",
      inputType: "sample-block",
      outputType: "sample-block",
      preservesSampleRate: true,
      preservesChannels: true,
      maxInstances: 8,
      parameters: [
        numberParameter("frequencyHz", "中心频率", "Hz", 1, 14999, 1),
        numberParameter("qualityFactor", "品质因数 Q", null, 0.1, 1000, 0.1)
      ],
      defaultNode: {
        kind: "notch", id: "notch-template", enabled: true,
        frequencyHz: 50, qualityFactor: 20
      }
    },
    {
      kind: "band-pass",
      label: "带通滤波",
      description: "保留低、高截止频率之间的信号。",
      icon: "sliders-horizontal",
      tone: "blue",
      inputType: "sample-block",
      outputType: "sample-block",
      preservesSampleRate: true,
      preservesChannels: true,
      maxInstances: 8,
      parameters: [
        numberParameter("lowCutHz", "低截止", "Hz", 1, 14998, 1),
        numberParameter("highCutHz", "高截止", "Hz", 2, 14999, 1)
      ],
      defaultNode: {
        kind: "band-pass", id: "band-pass-template", enabled: true,
        lowCutHz: 300, highCutHz: 3000
      }
    },
    {
      kind: "threshold",
      label: "阈值检测",
      description: "在指定通道越过阈值时产生带采样点的事件。",
      icon: "scan-line",
      tone: "coral",
      inputType: "sample-block",
      outputType: "sample-block",
      preservesSampleRate: true,
      preservesChannels: true,
      maxInstances: 8,
      parameters: [
        numberParameter("channelIndex", "通道", null, 1, 16, 1, 1),
        numberParameter("threshold", "阈值", "ADC", 0, 65535, 1),
        choiceParameter("direction", "方向", [
          { value: "rising-above", label: "向上越过" },
          { value: "falling-below", label: "向下越过" }
        ]),
        numberParameter("refractorySamples", "不应期", "samples", 0, 300000, 1)
      ],
      defaultNode: {
        kind: "threshold", id: "threshold-template", enabled: true,
        channelIndex: 0, threshold: 40000, direction: "rising-above", refractorySamples: 300
      }
    }
  ]
};

function defaultPipeline() {
  return {
    version: pipelineRegistry.schemaVersion,
    nodes: pipelineRegistry.nodes.map((definition) => ({
      ...JSON.parse(JSON.stringify(definition.defaultNode)),
      id: `${definition.kind}-1`,
      enabled: definition.kind === "artifact-blanking" || definition.kind === "notch"
    }))
  };
}

function validatePreviewPipeline(config, channelCount = 16) {
  if (config?.version !== pipelineRegistry.schemaVersion || !Array.isArray(config.nodes)) {
    throw new Error("不支持的管线配置版本");
  }
  const ids = new Set();
  for (const node of config.nodes) {
    if (!node.id || ids.has(node.id)) throw new Error(`节点 ID 重复或为空：${node.id || "(empty)"}`);
    ids.add(node.id);
    if (!pipelineRegistry.nodes.some((definition) => definition.kind === node.kind)) {
      throw new Error(`不支持的节点：${node.kind}`);
    }
    if (node.kind === "notch" && !(node.frequencyHz > 0 && node.frequencyHz < 15000)) {
      throw new Error(`节点 ${node.id}：频率必须低于 Nyquist (15000 Hz)`);
    }
    if (node.kind === "band-pass" && !(node.lowCutHz > 0 && node.highCutHz > node.lowCutHz && node.highCutHz < 15000)) {
      throw new Error(`节点 ${node.id}：截止频率必须满足 0 < low < high < 15000`);
    }
    if (node.kind === "threshold" && !(node.channelIndex >= 0 && node.channelIndex < channelCount)) {
      throw new Error(`节点 ${node.id}：通道必须在 1 到 ${channelCount} 之间`);
    }
  }
}

class MockBackend {
  constructor() {
    this.startedAt = performance.now() - 118_000;
    this.sequence = 3420;
    this.waveformGeneration = 0;
    this.channelCount = 16;
    this.mode = "acquiring";
    this.recording = false;
    this.recordingPath = null;
    this.playback = null;
    this.playbackTickAt = null;
    this.triggeredRecording = null;
    this.stimulation = {
      state: "disarmed", outputMode: "interlocked", armId: null, selectedChannels: 0,
      pendingCommands: 0, completedCommands: 0, lastCommandId: null, lastTargetSampleIndex: null
    };
    this.io = {
      digitalInput: 0, digitalOutput: 0, digitalEdges: 0,
      analogOutputs: [0, 0], outputMode: "interlocked"
    };
    this.impedance = {
      state: "idle", frequencyHz: 1000, channels: [], message: ""
    };
    this.remoteApi = {
      state: "stopped", address: "127.0.0.1", clientsServed: 0, protocolVersion: 1
    };
    this.sessionMetadata = { sessionName: "", operator: "", subjectId: "", notes: "" };
    this.spikeConfig = { enabled: false, thresholdUv: -50, thresholdMode: "noise", noiseMultiplier: 5, calibrationMs: 1000, refractoryMs: 3, preMs: 1, postMs: 2, bandPass: true, recordingPath: null };
    this.previewCalibrationStart = 0;
    this.spikeEpoch = 1;
    this.spikeEvents = [];
    this.spikeCounts = [];
    this.spikeTick = 0;
    this.previewNextSpike = [];
    this.pipeline = defaultPipeline();
    this.events = [
      this.event("info", "system", "NeuroX Studio preview backend ready"),
      this.event("success", "device", "Synthetic MEA preview source ready"),
      this.event("success", "acquisition", "Preview acquisition started at 30000 Hz, 16 channels; stimulation disabled")
    ];
  }

  event(level, category, message, details = {}) {
    return {
      id: (this.events?.length || 0) + 1,
      elapsedMs: Math.max(0, performance.now() - this.startedAt),
      level,
      category,
      message,
      sampleIndex: details.sampleIndex ?? null,
      channelIndex: details.channelIndex ?? null
    };
  }

  status() {
    if (this.playback?.state === "playing" && this.playbackTickAt !== null) {
      const deltaMs = performance.now() - this.playbackTickAt;
      this.playback.positionSamples = Math.min(
        this.playback.totalSamples,
        this.playback.positionSamples + Math.round(deltaMs * this.playback.sampleRateHz * this.playback.speed / 1000)
      );
      this.playbackTickAt = performance.now();
      if (this.playback.positionSamples >= this.playback.totalSamples) {
        this.playback.state = "ended";
        this.mode = "playback-paused";
        this.playbackTickAt = null;
      }
      this.playback.positionMs = Math.round(this.playback.positionSamples * 1000 / this.playback.sampleRateHz);
    }
    const elapsed = this.playback
      ? this.playback.positionMs
      : this.mode === "acquiring" || this.mode === "recording"
        ? performance.now() - this.startedAt
        : 0;
    const samples = Math.floor(elapsed * 30);
    const previewChips = Array.from({ length: Math.ceil(this.channelCount / 16) }, (_, index) => ({
      chipName: "Synthetic preview",
      port: "SIM",
      stream: index,
      channels: Math.min(16, this.channelCount - index * 16)
    }));
    return {
      mode: this.playback ? this.mode : this.recording ? "recording" : this.mode,
      health: "nominal",
      device: this.playback ? null : {
        serial: "PREVIEW",
        chipName: "Synthetic MEA",
        port: "SIM",
        stream: 0,
        channels: this.channelCount,
        sampleRateHz: 30000,
        bitfile: "preview/synthetic",
        chips: previewChips,
        capabilities: {
          detectedChipCount: previewChips.length,
          detectedChannelCount: this.channelCount,
          acquisitionStreamLimit: Math.ceil(this.channelCount / 16),
          acquisitionChannelLimit: this.channelCount,
          stimulationStreamLimit: Math.ceil(this.channelCount / 16),
          stimulationChannelLimit: this.channelCount
        }
      },
      recording: this.recording,
      recordingPath: this.recordingPath,
      elapsedMs: elapsed,
      metrics: {
        blocks: this.sequence,
        samplesPerChannel: samples,
        bytesRead: samples * this.channelCount * 2,
        timestampGaps: 0,
        emptyReads: 4,
        queueOverflows: 0,
        observedSampleRateHz: 29998.6,
        thresholdEvents: this.pipeline.nodes.some((node) => node.kind === "threshold" && node.enabled) ? 12 : 0,
        latestSequence: this.sequence
      },
      pipeline: this.pipeline,
      playback: this.playback ? { ...this.playback } : null,
      triggeredRecording: this.triggeredRecording ? { ...this.triggeredRecording } : null,
      stimulation: { ...this.stimulation },
      io: { ...this.io, analogOutputs: [...this.io.analogOutputs] },
      impedance: { ...this.impedance, channels: this.impedance.channels.map((channel) => ({ ...channel })) },
      remoteApi: { ...this.remoteApi },
      sessionMetadata: { ...this.sessionMetadata },
      stimulationInterlock: "bench-validation-required",
      lastError: null
    };
  }

  spikeThresholds() {
    if (!this.spikeConfig.enabled) return [];
    const calibrating = (this.sequence + 1) * 1080 < this.previewCalibrationStart + (this.spikeConfig.calibrationMs || 1000) * 30;
    return Array.from({ length: this.channelCount }, (_, channel) => this.spikeConfig.thresholdMode === "noise"
      ? { thresholdUv: calibrating ? null : Math.sign(this.spikeConfig.thresholdUv) * this.spikeConfig.noiseMultiplier * (6 + channel * 0.05),
        noiseUv: calibrating ? null : 6 + channel * 0.05, baselineUv: 0, state: calibrating ? "calibrating" : "ready" }
      : { thresholdUv: this.spikeConfig.thresholdUv, noiseUv: null, baselineUv: 0, state: "fixed" });
  }

  waveform() {
    if (this.playback && this.playback.state !== "playing") return null;
    if (!this.mode.includes("acquir") && this.mode !== "recording" && !this.playback) return null;
    this.sequence += 1;
    const nextDigitalInput = (Math.floor(this.sequence / 25) ^ (Math.floor(this.sequence / 70) << 1)) & 0xffff;
    this.io.digitalEdges += countChangedBits(this.io.digitalInput ^ nextDigitalInput);
    this.io.digitalInput = nextDigitalInput;
    const pointCount = 360;
    const channelCount = this.channelCount;
    const minimums = [];
    const maximums = [];
    const centers = Array.from({ length: channelCount }, () => []);
    const firstValues = [];
    const lastValues = [];
    const thresholds = this.spikeThresholds();
    for (let point = 0; point < pointCount; point += 1) {
      for (let channel = 0; channel < channelCount; channel += 1) {
        const position = this.sequence * pointCount + point;
        const carrier = Math.sin(position * (0.055 + channel * 0.0018) + channel * 0.37);
        const slow = Math.sin(position * 0.011 + channel);
        const spikeAt = this.spikeConfig.enabled
          ? (position + channel * 83) % (12000 + channel * 37)
          : (position + channel * 23) % 211;
        const spike = spikeAt < 4 ? (4 - spikeAt) * (channel % 2 ? -2200 : 2600) : 0;
        const threshold = thresholds[channel];
        const detectedPreview = this.spikeConfig.enabled && spikeAt === 0 && Number.isFinite(threshold.thresholdUv)
          && threshold.thresholdUv < 0 && -(120 + channel * 0.25) <= threshold.thresholdUv;
        const center = this.spikeConfig.enabled
          ? 32768 + (spikeAt < 4
            ? -(120 + channel * 0.25) * (4 - spikeAt) / (4 * 0.195)
            : (carrier * (420 + channel * 24) + slow * 180) * 0.08)
          : 32768 + carrier * (420 + channel * 24) + slow * 180 + spike;
        const sampleIndex = this.sequence * pointCount * 3 + point * 3;
        if (detectedPreview && sampleIndex >= (this.previewNextSpike[channel] || 0)) {
          const pre = Math.round(this.spikeConfig.preMs * 30), post = Math.round(this.spikeConfig.postMs * 30);
          const peakUv = (Math.round(center) - 32768) * 0.195;
          const waveformUv = Array.from({ length: pre + post + 1 }, (_, index) => peakUv * Math.exp(-((index - pre) ** 2) / 24));
          this.spikeEvents.push({ id: (this.spikeEvents.at(-1)?.id || 0) + 1, channelIndex: channel,
            stream: Math.floor(channel / 16), chipChannel: channel % 16, sampleIndex, peakSampleIndex: sampleIndex,
            timeMs: sampleIndex / 30, peakUv, displayPeakUv: peakUv, peakToPeakUv: Math.abs(peakUv),
            thresholdUv: threshold.thresholdUv, noiseUv: threshold.noiseUv,
            sampleRateHz: 30000, preSamples: pre, waveformUv });
          this.spikeCounts[channel] = (this.spikeCounts[channel] || 0) + 1;
          this.previewNextSpike[channel] = sampleIndex + Math.round(this.spikeConfig.refractoryMs * 30);
        }
        centers[channel].push(center);
        const spread = this.spikeConfig.enabled ? 3 : 55 + Math.abs(Math.sin(point * 0.17 + channel)) * 75;
        minimums.push(Math.max(0, Math.round(center - spread)));
        maximums.push(Math.min(65535, Math.round(center + spread)));
        firstValues.push(Math.max(0, Math.min(65535, Math.round(center))));
        lastValues.push(Math.max(0, Math.min(65535, Math.round(center))));
      }
    }
    this.spikeEvents = this.spikeEvents.slice(-4096);
    const baseline = centers.map((samples) => {
      const sorted = samples.slice().sort((left, right) => left - right);
      const middle = Math.floor(sorted.length / 2);
      const center = (sorted[middle - 1] + sorted[middle]) / 2;
      const deviations = samples.map((value) => Math.abs(value - center)).sort((left, right) => left - right);
      return {
        offsetAdc: center - 32768,
        noiseAdc: (deviations[middle - 1] + deviations[middle]) * 0.7413,
        sampleCount: samples.length,
        quality: "preview"
      };
    });
    if (this.sequence % 20 === 0) {
      for (const node of this.pipeline.nodes.filter((candidate) => candidate.kind === "threshold" && candidate.enabled)) {
        const sampleIndex = this.sequence * pointCount * 3 + Math.floor(pointCount / 2) * 3;
        this.events.push(this.event(
          "event",
          "threshold",
          `Threshold crossing on channel ${node.channelIndex + 1}`,
          { sampleIndex, channelIndex: node.channelIndex }
        ));
      }
    }
    return {
      sequence: this.sequence,
      spikeEpoch: this.spikeEpoch,
      firstSampleIndex: this.sequence * pointCount * 3,
      sampleRateHz: 30000,
      binWidthSamples: 3,
      sampleCount: pointCount * 3,
      pointCount,
      channelCount,
      minimums,
      maximums,
      firstValues,
      lastValues,
      baseline,
      flags: 0
    };
  }

  async invoke(command, args = {}) {
    switch (command) {
      case "configure_spikes":
        if (["acquiring", "recording", "playback", "playback-paused"].includes(this.mode)) throw new Error("请先停止采集或关闭回放");
        if (args.config.recordingPath) throw new Error("预览模式不能写入连续 CSV 文件，可导出界面缓存");
        if (!Number.isFinite(args.config.thresholdUv) || Math.abs(args.config.thresholdUv) < 1 || Math.abs(args.config.thresholdUv) > 6000
          || !Number.isFinite(args.config.refractoryMs) || args.config.refractoryMs < args.config.postMs || args.config.refractoryMs > 100
          || !Number.isFinite(args.config.preMs) || args.config.preMs < 0.2 || args.config.preMs > 3
          || !Number.isFinite(args.config.postMs) || args.config.postMs < 0.5 || args.config.postMs > 5) throw new Error("无效的尖峰检测参数");
        if (args.config.thresholdMode && !["fixed", "noise"].includes(args.config.thresholdMode)) throw new Error("无效的阈值模式");
        if ((args.config.noiseMultiplier !== undefined && (!Number.isFinite(args.config.noiseMultiplier) || args.config.noiseMultiplier < 2 || args.config.noiseMultiplier > 12))
          || (args.config.calibrationMs !== undefined && (!Number.isInteger(args.config.calibrationMs) || args.config.calibrationMs < 250 || args.config.calibrationMs > 10000))) throw new Error("无效的噪声校准参数");
        this.spikeConfig = { thresholdMode: "fixed", noiseMultiplier: 5, calibrationMs: 1000, ...args.config };
        this.spikeEpoch++; this.spikeEvents = []; this.spikeCounts = []; this.previewNextSpike = [];
        this.previewCalibrationStart = Math.max(0, (this.sequence + 1) * 1080);
        return null;
      case "poll_spikes": {
        const running = this.spikeConfig.enabled && (this.mode === "acquiring" || this.mode === "recording" || this.playback?.state === "playing");
        const afterId = args.epoch === this.spikeEpoch ? args.afterId || 0 : 0;
        return { config: { ...this.spikeConfig }, epoch: this.spikeEpoch, running, error: null, counts: [...this.spikeCounts],
          latestSampleIndex: Math.max(0, (this.sequence + 1) * 1080 - 1), thresholds: this.spikeThresholds(),
          sampleRateHz: 30000, discardedIncomplete: 0, discontinuities: 0, recordedEvents: 0, recordingComplete: false,
          missedEvents: Math.max(0, (this.spikeEvents[0]?.id || 1) - afterId - 1), events: this.spikeEvents.filter((event) => event.id > afterId).slice(0, 512) };
      }
      case "get_status": return this.status();
      case "get_pipeline_registry": return JSON.parse(JSON.stringify(pipelineRegistry));
      case "poll_waveform": return this.waveform();
      case "poll_waveforms": {
        const frame = this.waveform();
        return { frames: frame ? [frame] : [], nextCursor: this.sequence,
          generation: this.waveformGeneration, droppedFrames: 0 };
      }
      case "poll_events": return this.events.filter((event) => event.id > (args.afterId || 0));
      case "scan_device":
        this.mode = "ready";
        this.events.push(this.event("success", "device", "Synthetic MEA preview source ready"));
        return this.status().device;
      case "start_acquisition":
        this.waveformGeneration += 1;
        this.mode = "acquiring";
        this.spikeEpoch++; this.spikeEvents = []; this.spikeCounts = []; this.previewNextSpike = [];
        this.sequence = -1;
        this.previewCalibrationStart = 0;
        this.sessionMetadata = { ...this.sessionMetadata, ...(args.request?.metadata || {}) };
        this.startedAt = performance.now();
        this.events.push(this.event("success", "acquisition", "Acquisition started; stimulation disabled"));
        return this.status();
      case "stop_acquisition":
        this.mode = "ready";
        this.recording = false;
        this.events.push(this.event("info", "acquisition", "Acquisition stopped cleanly"));
        return this.status();
      case "start_recording":
        this.recording = true;
        this.recordingPath = args.path || nowPath();
        this.previewRecordings ||= new Map();
        this.previewRecordings.set(this.recordingPath, { schemaVersion: 1, workspace: JSON.parse(JSON.stringify(args.workspace || {})), channelCount: this.channelCount });
        this.events.push(this.event("record", "storage", `Recording started: ${this.recordingPath}`));
        return { path: this.recordingPath, blocks: 0, complete: false };
      case "stop_recording":
        this.recording = false;
        this.events.push(this.event("success", "storage", "Recording finalized: 146 blocks"));
        return { path: this.recordingPath, blocks: 146, complete: true };
      case "arm_triggered_recording":
        this.triggeredRecording = {
          state: "armed",
          path: args.request.path,
          source: args.request.source,
          preTriggerMs: args.request.preTriggerMs,
          postTriggerMs: args.request.postTriggerMs,
          triggerSampleIndex: null,
          capturedSamplesPerChannel: 0,
          capturedBlocks: 0
        };
        return { ...this.triggeredRecording };
      case "fire_software_trigger":
        if (!this.triggeredRecording || this.triggeredRecording.source !== "software") throw new Error("没有软件触发记录等待触发");
        this.triggeredRecording.state = "capturing";
        this.triggeredRecording.triggerSampleIndex = Math.floor((performance.now() - this.startedAt) * 30);
        this.triggeredRecording.capturedBlocks = 1;
        return { ...this.triggeredRecording };
      case "cancel_triggered_recording":
        if (!this.triggeredRecording) throw new Error("没有触发记录");
        this.triggeredRecording.state = "cancelled";
        return { ...this.triggeredRecording };
      case "arm_stimulation":
        if (args.config.benchConfirmation || args.config.outputConfirmation) {
          throw new Error("浏览器预览不能启用真实硬件输出");
        }
        this.stimulation = {
          state: "armed", outputMode: "interlocked", armId: Date.now(),
          selectedChannels: args.config.channelIds.length, pendingCommands: 0,
          completedCommands: 0, lastCommandId: null, lastTargetSampleIndex: null
        };
        return { ...this.stimulation };
      case "trigger_stimulation":
        if (!this.stimulation.armId) throw new Error("刺激配置尚未 Arm");
        this.stimulation.state = "scheduled";
        this.stimulation.pendingCommands = args.schedule.repeatCount;
        return { ...this.stimulation };
      case "cancel_stimulation":
        this.stimulation.state = "cancelled";
        this.stimulation.pendingCommands = 0;
        return { ...this.stimulation };
      case "disarm_stimulation":
        this.stimulation = {
          state: "disarmed", outputMode: "interlocked", armId: null, selectedChannels: 0,
          pendingCommands: 0, completedCommands: 0, lastCommandId: null, lastTargetSampleIndex: null
        };
        return { ...this.stimulation };
      case "configure_io_outputs":
        if (!Array.isArray(args.request?.analogOutputs) || args.request.analogOutputs.length !== 2
          || args.request.analogOutputs.some((value) => !Number.isFinite(value) || value < -3.3 || value > 3.3)) {
          throw new Error("模拟输出必须是 -3.3 V 到 3.3 V 之间的两个数值");
        }
        this.io.digitalOutput = Number(args.request.digitalOutput || 0) & 0xffff;
        this.io.analogOutputs = args.request.analogOutputs.map(Number);
        this.events.push(this.event("warning", "io", "I/O output configuration updated; physical outputs remain interlocked"));
        return { ...this.io, analogOutputs: [...this.io.analogOutputs] };
      case "prepare_impedance_scan":
        if (args.request?.confirmedTestSignal) throw new Error("预览模式不能执行真实阻抗测量");
        if (!args.request?.channelIds?.length) throw new Error("至少选择一个启用通道");
        this.impedance = {
          state: "hardware-required",
          frequencyHz: Number(args.request.frequencyHz),
          channels: args.request.channelIds.map((channelId) => ({
            channelId, magnitudeOhms: null, phaseDegrees: null, quality: "pending"
          })),
          message: "Impedance request validated; connect hardware to execute measurement"
        };
        this.events.push(this.event("warning", "impedance", "Impedance scan prepared; hardware measurement is not enabled"));
        return { ...this.impedance, channels: this.impedance.channels.map((channel) => ({ ...channel })) };
      case "cancel_impedance_scan":
        this.impedance.state = "cancelled";
        this.impedance.message = "Impedance request cancelled";
        return { ...this.impedance, channels: this.impedance.channels.map((channel) => ({ ...channel })) };
      case "start_remote_api":
        if (String(args.request?.token || "").length < 12) throw new Error("访问令牌至少需要 12 个字符");
        this.remoteApi = {
          ...this.remoteApi,
          state: "running",
          address: `127.0.0.1:${Number(args.request.port)}`
        };
        return { ...this.remoteApi };
      case "stop_remote_api":
        this.remoteApi.state = "stopped";
        this.remoteApi.address = "127.0.0.1";
        return { ...this.remoteApi };
      case "read_recording_parameters":
          return this.previewRecordings?.get(args.path) || null;
      case "export_recording_analysis":
          throw new Error("完整记录导出需要在桌面客户端中选择已完成的真实记录文件；浏览器预览不会生成分析数据");
      case "open_playback":
        this.spikeEpoch++; this.spikeEvents = []; this.spikeCounts = []; this.previewNextSpike = [];
        this.waveformGeneration++; this.sequence = -1;
        this.previewCalibrationStart = 0;
        this.mode = "playback-paused";
        this.playbackTickAt = null;
        this.playback = {
          path: args.path,
          state: "paused",
          complete: true,
          sampleRateHz: 30000,
          channelCount: this.previewRecordings?.get(args.path)?.channelCount || this.channelCount,
          blockCount: 2637,
          positionSamples: 0,
          totalSamples: 2700000,
          positionMs: 0,
          durationMs: 90000,
          speed: 1
        };
        this.events.push(this.event("success", "playback", `Playback opened: ${this.playback.channelCount} channels, 90.000 seconds`));
        return this.status();
      case "play_playback":
        if (!this.playback) throw new Error("没有打开的回放记录");
        if (this.playback.positionSamples >= this.playback.totalSamples) {
          this.playback.positionSamples = 0;
          this.playback.positionMs = 0;
        }
        this.playback.state = "playing";
        this.playbackTickAt = performance.now();
        this.mode = "playback";
        return this.status();
      case "pause_playback":
        if (!this.playback) throw new Error("没有打开的回放记录");
        this.status();
        this.playback.state = "paused";
        this.playbackTickAt = null;
        this.mode = "playback-paused";
        return this.status();
      case "seek_playback":
        if (!this.playback) throw new Error("没有打开的回放记录");
        this.playback.positionSamples = Math.round(this.playback.totalSamples * Math.max(0, Math.min(1, args.fraction)));
        this.spikeEpoch++; this.spikeEvents = []; this.spikeCounts = []; this.previewNextSpike = [];
        this.waveformGeneration++; this.sequence = Math.floor(this.playback.positionSamples / 1080) - 1;
        this.previewCalibrationStart = this.playback.positionSamples;
        this.playback.positionMs = Math.round(this.playback.positionSamples * 1000 / this.playback.sampleRateHz);
        if (this.playback.state === "playing") this.playbackTickAt = performance.now();
        return this.status();
      case "set_playback_speed":
        if (!this.playback) throw new Error("没有打开的回放记录");
        this.status();
        this.playback.speed = args.speed;
        if (this.playback.state === "playing") this.playbackTickAt = performance.now();
        return this.status();
      case "close_playback":
        this.playback = null;
        this.playbackTickAt = null;
        this.mode = "idle";
        return this.status();
      case "update_pipeline":
        validatePreviewPipeline(args.config, this.channelCount);
        this.pipeline = JSON.parse(JSON.stringify(args.config));
        this.spikeEpoch++; this.spikeEvents = []; this.spikeCounts = []; this.previewNextSpike = [];
        this.previewCalibrationStart = Math.max(0, (this.sequence + 1) * 1080);
        this.events.push(this.event("info", "pipeline", "Display processing chain rebuilt"));
        return this.status();
      case "emergency_stop":
        this.events.push(this.event("critical", "stimulation", "Emergency stop sent; all stimulation remains disabled"));
        return this.status();
      default: throw new Error(`Unknown mock command: ${command}`);
    }
  }
}

function countChangedBits(value) {
  let bits = Number(value) & 0xffff;
  let count = 0;
  while (bits) {
    bits &= bits - 1;
    count += 1;
  }
  return count;
}

const mock = new MockBackend();

export const bridge = {
  isPreview: !hasTauri,
  defaultRecordingPath: nowPath,
  defaultTriggeredRecordingPath: triggeredPath,
  setPreviewChannelCount(channelCount) {
    if (!hasTauri) {
      mock.channelCount = Math.min(120, Math.max(1, Math.floor(Number(channelCount) || 16)));
      mock.waveformGeneration += 1;
    }
  },
  invoke(command, args = {}) {
    return hasTauri ? window.__TAURI__.core.invoke(command, args) : mock.invoke(command, args);
  }
};
