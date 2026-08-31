const hasTauri = Boolean(window.__TAURI__?.core?.invoke);

function nowPath() {
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\..+/, "").replace("T", "-");
  return `work/rhs-studio-${stamp}.rhsraw`;
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

function validatePreviewPipeline(config) {
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
    if (node.kind === "threshold" && !(node.channelIndex >= 0 && node.channelIndex < 16)) {
      throw new Error(`节点 ${node.id}：通道必须在 1 到 16 之间`);
    }
  }
}

class MockBackend {
  constructor() {
    this.startedAt = performance.now() - 118_000;
    this.sequence = 3420;
    this.mode = "acquiring";
    this.recording = false;
    this.recordingPath = null;
    this.pipeline = defaultPipeline();
    this.events = [
      this.event("info", "system", "RHS Studio preview backend ready"),
      this.event("success", "device", "RHS2116 detected on Port D, stream 6"),
      this.event("success", "acquisition", "Acquisition started at 30000 Hz, 16 channels; stimulation disabled")
    ];
  }

  event(level, category, message) {
    return {
      id: (this.events?.length || 0) + 1,
      elapsedMs: Math.max(0, performance.now() - this.startedAt),
      level,
      category,
      message
    };
  }

  status() {
    const elapsed = this.mode === "acquiring" || this.mode === "recording"
      ? performance.now() - this.startedAt
      : 0;
    const samples = Math.floor(elapsed * 30);
    return {
      mode: this.recording ? "recording" : this.mode,
      health: "nominal",
      device: {
        serial: "DEMO-RHS2116",
        chipName: "RHS2116",
        port: "D",
        stream: 6,
        channels: 16,
        sampleRateHz: 30000,
        bitfile: "demo/ConfigRHSController.bit"
      },
      recording: this.recording,
      recordingPath: this.recordingPath,
      elapsedMs: elapsed,
      metrics: {
        blocks: this.sequence,
        samplesPerChannel: samples,
        bytesRead: samples * 16 * 2,
        timestampGaps: 0,
        emptyReads: 4,
        queueOverflows: 0,
        observedSampleRateHz: 29998.6,
        thresholdEvents: this.pipeline.nodes.some((node) => node.kind === "threshold" && node.enabled) ? 12 : 0,
        latestSequence: this.sequence
      },
      pipeline: this.pipeline,
      stimulationInterlock: "bench-validation-required",
      lastError: null
    };
  }

  waveform() {
    if (!this.mode.includes("acquir") && this.mode !== "recording") return null;
    this.sequence += 1;
    const pointCount = 360;
    const channelCount = 16;
    const minimums = [];
    const maximums = [];
    const phase = this.sequence * 0.11;
    for (let point = 0; point < pointCount; point += 1) {
      for (let channel = 0; channel < channelCount; channel += 1) {
        const carrier = Math.sin(point * (0.055 + channel * 0.0018) + phase + channel * 0.37);
        const slow = Math.sin(point * 0.011 + phase * 0.4 + channel);
        const spikeAt = (point + channel * 23 + this.sequence * 3) % 211;
        const spike = spikeAt < 4 ? (4 - spikeAt) * (channel % 2 ? -2200 : 2600) : 0;
        const center = 32768 + carrier * (420 + channel * 24) + slow * 180 + spike;
        const spread = 55 + Math.abs(Math.sin(point * 0.17 + channel)) * 75;
        minimums.push(Math.max(0, Math.round(center - spread)));
        maximums.push(Math.min(65535, Math.round(center + spread)));
      }
    }
    return {
      sequence: this.sequence,
      firstSampleIndex: this.sequence * 1024,
      sampleRateHz: 30000,
      binWidthSamples: 3,
      pointCount,
      channelCount,
      minimums,
      maximums,
      flags: 0
    };
  }

  async invoke(command, args = {}) {
    switch (command) {
      case "get_status": return this.status();
      case "get_pipeline_registry": return JSON.parse(JSON.stringify(pipelineRegistry));
      case "poll_waveform": return this.waveform();
      case "poll_events": return this.events.filter((event) => event.id > (args.afterId || 0));
      case "scan_device":
        this.mode = "ready";
        this.events.push(this.event("success", "device", "RHS2116 detected on Port D, stream 6"));
        return this.status().device;
      case "start_acquisition":
        this.mode = "acquiring";
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
        this.events.push(this.event("record", "storage", `Recording started: ${this.recordingPath}`));
        return { path: this.recordingPath, blocks: 0, complete: false };
      case "stop_recording":
        this.recording = false;
        this.events.push(this.event("success", "storage", "Recording finalized: 146 blocks"));
        return { path: this.recordingPath, blocks: 146, complete: true };
      case "update_pipeline":
        validatePreviewPipeline(args.config);
        this.pipeline = JSON.parse(JSON.stringify(args.config));
        this.events.push(this.event("info", "pipeline", "Display processing chain rebuilt"));
        return this.status();
      case "emergency_stop":
        this.events.push(this.event("critical", "stimulation", "Emergency stop sent; all stimulation remains disabled"));
        return this.status();
      default: throw new Error(`Unknown mock command: ${command}`);
    }
  }
}

const mock = new MockBackend();

export const bridge = {
  isPreview: !hasTauri,
  defaultRecordingPath: nowPath,
  invoke(command, args = {}) {
    return hasTauri ? window.__TAURI__.core.invoke(command, args) : mock.invoke(command, args);
  }
};
