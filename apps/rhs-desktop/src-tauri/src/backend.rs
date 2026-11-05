use crate::recording::{configuration_entries, read_configuration};
use crate::spikes::{SpikeAnalyzer, SpikeBatch, SpikeConfig, SpikeStore};
use rhs_core::SampleBlock;
use rhs_device::{
    AcquisitionConfig, AcquisitionHandle, AcquisitionInfo, AcquisitionWorkerConfig, ChipInfo,
    RhsDevice, ScanReport, StimConfig, StimStepSize,
};
use rhs_nodes::{
    ArtifactBlanker, BandPassProcessor, BlankingConfig, BlankingReplacement, NotchProcessor,
    ThresholdConfig, ThresholdDirection, ThresholdEventReceiver, ThresholdProcessor,
};
use rhs_pipeline::{Processor, Sink, Source};
use rhs_storage::{FileReplaySource, RawRecorderSink, RecordingInfo};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

const EVENT_LIMIT: usize = 200;
const WAVEFORM_POINTS: usize = 360;
const PIPELINE_SCHEMA_VERSION: u32 = 1;
const RHS_CHANNEL_COUNT: usize = 16;
const MAX_SOFTWARE_CHANNEL_COUNT: usize = 120;
const RHS_SAMPLE_RATE_HZ: u32 = 30_000;
const MAX_PIPELINE_NODES: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    serial: String,
    chip_name: String,
    port: String,
    stream: u8,
    channels: usize,
    sample_rate_hz: u32,
    bitfile: String,
    chips: Vec<DeviceChipView>,
    capabilities: DeviceCapabilitiesView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceChipView {
    chip_name: String,
    port: String,
    stream: u8,
    channels: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCapabilitiesView {
    detected_chip_count: usize,
    detected_channel_count: usize,
    acquisition_stream_limit: usize,
    acquisition_channel_limit: usize,
    stimulation_stream_limit: usize,
    stimulation_channel_limit: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineConfig {
    version: u32,
    nodes: Vec<PipelineNodeConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum PipelineNodeConfig {
    ArtifactBlanking {
        id: String,
        enabled: bool,
        ttl_mask: u16,
        blank_samples: u32,
        replacement: BlankingReplacementConfig,
    },
    Notch {
        id: String,
        enabled: bool,
        frequency_hz: f64,
        quality_factor: f64,
    },
    BandPass {
        id: String,
        enabled: bool,
        low_cut_hz: f64,
        high_cut_hz: f64,
    },
    Threshold {
        id: String,
        enabled: bool,
        channel_index: usize,
        threshold: u16,
        direction: ThresholdDirectionConfig,
        refractory_samples: u32,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BlankingReplacementConfig {
    HoldLast,
    Midpoint,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThresholdDirectionConfig {
    RisingAbove,
    FallingBelow,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineRegistryView {
    schema_version: u32,
    source_type: &'static str,
    nodes: Vec<PipelineNodeDefinitionView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineNodeDefinitionView {
    kind: &'static str,
    label: &'static str,
    description: &'static str,
    icon: &'static str,
    tone: &'static str,
    input_type: &'static str,
    output_type: &'static str,
    preserves_sample_rate: bool,
    preserves_channels: bool,
    max_instances: usize,
    parameters: Vec<PipelineParameterView>,
    default_node: PipelineNodeConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineParameterView {
    key: &'static str,
    label: &'static str,
    control: &'static str,
    unit: Option<&'static str>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    step: Option<f64>,
    display_offset: f64,
    choices: Vec<PipelineChoiceView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineChoiceView {
    value: &'static str,
    label: &'static str,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            version: PIPELINE_SCHEMA_VERSION,
            nodes: vec![
                PipelineNodeConfig::ArtifactBlanking {
                    id: "artifact-blanking-1".to_owned(),
                    enabled: true,
                    ttl_mask: 1,
                    blank_samples: 90,
                    replacement: BlankingReplacementConfig::HoldLast,
                },
                PipelineNodeConfig::Notch {
                    id: "notch-1".to_owned(),
                    enabled: true,
                    frequency_hz: 50.0,
                    quality_factor: 20.0,
                },
                PipelineNodeConfig::BandPass {
                    id: "band-pass-1".to_owned(),
                    enabled: false,
                    low_cut_hz: 300.0,
                    high_cut_hz: 3_000.0,
                },
                PipelineNodeConfig::Threshold {
                    id: "threshold-1".to_owned(),
                    enabled: false,
                    channel_index: 0,
                    threshold: 40_000,
                    direction: ThresholdDirectionConfig::RisingAbove,
                    refractory_samples: 300,
                },
            ],
        }
    }
}

impl PipelineConfig {
    #[cfg(test)]
    fn validate(self) -> Result<Self, String> {
        self.validate_for(RHS_SAMPLE_RATE_HZ, RHS_CHANNEL_COUNT)?;
        Ok(self)
    }

    fn validate_for(&self, sample_rate_hz: u32, channel_count: usize) -> Result<(), String> {
        if self.version != PIPELINE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported pipeline version {}; expected {}",
                self.version, PIPELINE_SCHEMA_VERSION
            ));
        }
        if self.nodes.len() > MAX_PIPELINE_NODES {
            return Err(format!(
                "pipeline contains {} nodes; maximum is {MAX_PIPELINE_NODES}",
                self.nodes.len()
            ));
        }
        let mut ids = HashSet::new();
        let nyquist = sample_rate_hz as f64 / 2.0;
        for node in &self.nodes {
            let id = node.id();
            if id.trim().is_empty() {
                return Err("pipeline node id must not be empty".to_owned());
            }
            if !ids.insert(id) {
                return Err(format!("duplicate pipeline node id: {id}"));
            }
            match node {
                PipelineNodeConfig::ArtifactBlanking {
                    ttl_mask,
                    blank_samples,
                    ..
                } => {
                    if *ttl_mask == 0 {
                        return Err(format!("node {id}: ttlMask must be greater than zero"));
                    }
                    if *blank_samples == 0 || *blank_samples > sample_rate_hz {
                        return Err(format!(
                            "node {id}: blankSamples must be between 1 and {sample_rate_hz}"
                        ));
                    }
                }
                PipelineNodeConfig::Notch {
                    frequency_hz,
                    quality_factor,
                    ..
                } => {
                    if !frequency_hz.is_finite() || *frequency_hz <= 0.0 || *frequency_hz >= nyquist
                    {
                        return Err(format!(
                            "node {id}: frequencyHz must be greater than zero and below Nyquist ({nyquist} Hz)"
                        ));
                    }
                    if !quality_factor.is_finite()
                        || *quality_factor < 0.1
                        || *quality_factor > 1_000.0
                    {
                        return Err(format!(
                            "node {id}: qualityFactor must be between 0.1 and 1000"
                        ));
                    }
                }
                PipelineNodeConfig::BandPass {
                    low_cut_hz,
                    high_cut_hz,
                    ..
                } => {
                    if !low_cut_hz.is_finite()
                        || !high_cut_hz.is_finite()
                        || *low_cut_hz <= 0.0
                        || *high_cut_hz <= *low_cut_hz
                        || *high_cut_hz >= nyquist
                    {
                        return Err(format!(
                            "node {id}: cutoffs must satisfy 0 < lowCutHz < highCutHz < {nyquist}"
                        ));
                    }
                }
                PipelineNodeConfig::Threshold {
                    channel_index,
                    refractory_samples,
                    ..
                } => {
                    if *channel_index >= channel_count {
                        return Err(format!(
                            "node {id}: channelIndex must be between 0 and {}",
                            channel_count.saturating_sub(1)
                        ));
                    }
                    if *refractory_samples > sample_rate_hz.saturating_mul(10) {
                        return Err(format!(
                            "node {id}: refractorySamples may not exceed 10 seconds"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl PipelineNodeConfig {
    fn id(&self) -> &str {
        match self {
            Self::ArtifactBlanking { id, .. }
            | Self::Notch { id, .. }
            | Self::BandPass { id, .. }
            | Self::Threshold { id, .. } => id,
        }
    }

    fn enabled(&self) -> bool {
        match self {
            Self::ArtifactBlanking { enabled, .. }
            | Self::Notch { enabled, .. }
            | Self::BandPass { enabled, .. }
            | Self::Threshold { enabled, .. } => *enabled,
        }
    }
}

fn numeric_parameter(
    key: &'static str,
    label: &'static str,
    unit: Option<&'static str>,
    minimum: f64,
    maximum: f64,
    step: f64,
    display_offset: f64,
) -> PipelineParameterView {
    PipelineParameterView {
        key,
        label,
        control: "number",
        unit,
        minimum: Some(minimum),
        maximum: Some(maximum),
        step: Some(step),
        display_offset,
        choices: Vec::new(),
    }
}

fn choice_parameter(
    key: &'static str,
    label: &'static str,
    choices: Vec<PipelineChoiceView>,
) -> PipelineParameterView {
    PipelineParameterView {
        key,
        label,
        control: "select",
        unit: None,
        minimum: None,
        maximum: None,
        step: None,
        display_offset: 0.0,
        choices,
    }
}

fn pipeline_registry() -> PipelineRegistryView {
    let common = |kind, label, description, icon, tone, parameters, default_node| {
        PipelineNodeDefinitionView {
            kind,
            label,
            description,
            icon,
            tone,
            input_type: "sample-block",
            output_type: "sample-block",
            preserves_sample_rate: true,
            preserves_channels: true,
            max_instances: 8,
            parameters,
            default_node,
        }
    };
    PipelineRegistryView {
        schema_version: PIPELINE_SCHEMA_VERSION,
        source_type: "sample-block",
        nodes: vec![
            common(
                "artifact-blanking",
                "伪迹消隐",
                "TTL 上升沿后替换一段采样，抑制刺激伪迹。",
                "eraser",
                "amber",
                vec![
                    numeric_parameter("ttlMask", "TTL 掩码", None, 1.0, 65_535.0, 1.0, 0.0),
                    numeric_parameter(
                        "blankSamples",
                        "消隐长度",
                        Some("samples"),
                        1.0,
                        RHS_SAMPLE_RATE_HZ as f64,
                        1.0,
                        0.0,
                    ),
                    choice_parameter(
                        "replacement",
                        "替换方式",
                        vec![
                            PipelineChoiceView {
                                value: "hold-last",
                                label: "保持前值",
                            },
                            PipelineChoiceView {
                                value: "midpoint",
                                label: "ADC 中点",
                            },
                        ],
                    ),
                ],
                PipelineNodeConfig::ArtifactBlanking {
                    id: "artifact-blanking-template".to_owned(),
                    enabled: true,
                    ttl_mask: 1,
                    blank_samples: 90,
                    replacement: BlankingReplacementConfig::HoldLast,
                },
            ),
            common(
                "notch",
                "陷波滤波",
                "抑制指定频率的窄带工频干扰。",
                "waves",
                "teal",
                vec![
                    numeric_parameter(
                        "frequencyHz",
                        "中心频率",
                        Some("Hz"),
                        1.0,
                        14_999.0,
                        1.0,
                        0.0,
                    ),
                    numeric_parameter("qualityFactor", "品质因数 Q", None, 0.1, 1_000.0, 0.1, 0.0),
                ],
                PipelineNodeConfig::Notch {
                    id: "notch-template".to_owned(),
                    enabled: true,
                    frequency_hz: 50.0,
                    quality_factor: 20.0,
                },
            ),
            common(
                "band-pass",
                "带通滤波",
                "保留低、高截止频率之间的信号。",
                "sliders-horizontal",
                "blue",
                vec![
                    numeric_parameter("lowCutHz", "低截止", Some("Hz"), 1.0, 14_998.0, 1.0, 0.0),
                    numeric_parameter("highCutHz", "高截止", Some("Hz"), 2.0, 14_999.0, 1.0, 0.0),
                ],
                PipelineNodeConfig::BandPass {
                    id: "band-pass-template".to_owned(),
                    enabled: true,
                    low_cut_hz: 300.0,
                    high_cut_hz: 3_000.0,
                },
            ),
            common(
                "threshold",
                "阈值检测",
                "在指定通道越过阈值时产生带采样点的事件。",
                "scan-line",
                "coral",
                vec![
                    numeric_parameter(
                        "channelIndex",
                        "通道",
                        None,
                        1.0,
                        RHS_CHANNEL_COUNT as f64,
                        1.0,
                        1.0,
                    ),
                    numeric_parameter("threshold", "阈值", Some("ADC"), 0.0, 65_535.0, 1.0, 0.0),
                    choice_parameter(
                        "direction",
                        "方向",
                        vec![
                            PipelineChoiceView {
                                value: "rising-above",
                                label: "向上越过",
                            },
                            PipelineChoiceView {
                                value: "falling-below",
                                label: "向下越过",
                            },
                        ],
                    ),
                    numeric_parameter(
                        "refractorySamples",
                        "不应期",
                        Some("samples"),
                        0.0,
                        (RHS_SAMPLE_RATE_HZ * 10) as f64,
                        1.0,
                        0.0,
                    ),
                ],
                PipelineNodeConfig::Threshold {
                    id: "threshold-template".to_owned(),
                    enabled: true,
                    channel_index: 0,
                    threshold: 40_000,
                    direction: ThresholdDirectionConfig::RisingAbove,
                    refractory_samples: 300,
                },
            ),
        ],
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsView {
    blocks: u64,
    samples_per_channel: u64,
    bytes_read: u64,
    timestamp_gaps: u64,
    empty_reads: u64,
    queue_overflows: u64,
    observed_sample_rate_hz: f64,
    threshold_events: u64,
    latest_sequence: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    id: u64,
    elapsed_ms: u64,
    level: String,
    category: String,
    message: String,
    sample_index: Option<u64>,
    channel_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformFrame {
    sequence: u64,
    spike_epoch: u64,
    first_sample_index: u64,
    sample_rate_hz: u32,
    bin_width_samples: u32,
    sample_count: u32,
    point_count: usize,
    channel_count: usize,
    minimums: Vec<u16>,
    maximums: Vec<u16>,
    first_values: Vec<u16>,
    last_values: Vec<u16>,
    baseline: Vec<BaselineChannelView>,
    flags: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformBatch {
    frames: Vec<WaveformFrame>,
    next_cursor: u64,
    generation: u64,
    dropped_frames: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineChannelView {
    offset_adc: f64,
    noise_adc: f64,
    sample_count: usize,
    quality: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    mode: String,
    health: String,
    device: Option<DeviceView>,
    recording: bool,
    recording_path: Option<String>,
    elapsed_ms: u64,
    metrics: MetricsView,
    pipeline: PipelineConfig,
    playback: Option<PlaybackView>,
    triggered_recording: Option<TriggeredRecordingView>,
    stimulation: StimulationView,
    io: IoView,
    impedance: ImpedanceView,
    remote_api: RemoteApiView,
    session_metadata: SessionMetadata,
    stimulation_interlock: String,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequest {
    bitfile: Option<String>,
    serial: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    bitfile: Option<String>,
    serial: Option<String>,
    blocks_per_read: u8,
    queue_capacity: usize,
    #[serde(default = "default_channel_count")]
    channel_count: usize,
    #[serde(default)]
    stimulation_preset: Option<StimulationConfigRequest>,
    #[serde(default)]
    metadata: SessionMetadata,
}

fn default_channel_count() -> usize {
    16
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    session_name: String,
    operator: String,
    subject_id: String,
    notes: String,
}

impl SessionMetadata {
    fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("session name", &self.session_name),
            ("operator", &self.operator),
            ("subject id", &self.subject_id),
        ] {
            if value.chars().count() > 80 {
                return Err(format!("{label} must not exceed 80 characters"));
            }
        }
        if self.notes.chars().count() > 4_000 {
            return Err("session notes must not exceed 4000 characters".to_owned());
        }
        Ok(())
    }

    fn recording_entries(&self) -> Vec<(String, String)> {
        [
            ("session_name", self.session_name.trim()),
            ("operator", self.operator.trim()),
            ("subject_id", self.subject_id.trim()),
            ("session_notes", self.notes.trim()),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingView {
    path: String,
    blocks: u64,
    complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackView {
    path: String,
    state: String,
    complete: bool,
    sample_rate_hz: u32,
    channel_count: usize,
    block_count: u64,
    position_samples: u64,
    total_samples: u64,
    position_ms: u64,
    duration_ms: u64,
    speed: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingTriggerSource {
    Software,
    Digital,
    Analog,
}

impl RecordingTriggerSource {
    fn label(self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::Digital => "digital",
            Self::Analog => "analog",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggeredRecordingRequest {
    path: String,
    #[serde(default)]
    workspace: Option<serde_json::Value>,
    source: RecordingTriggerSource,
    pre_trigger_ms: u64,
    post_trigger_ms: u64,
    digital_mask: u16,
    analog_channel_index: usize,
    analog_threshold: u16,
    analog_direction: ThresholdDirectionConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggeredRecordingView {
    state: String,
    path: String,
    source: RecordingTriggerSource,
    pre_trigger_ms: u64,
    post_trigger_ms: u64,
    trigger_sample_index: Option<u64>,
    captured_samples_per_channel: u64,
    captured_blocks: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StimulationConfigRequest {
    profile_id: String,
    channel_ids: Vec<String>,
    waveform: String,
    amplitude_na: u32,
    phase_width_us: u32,
    interphase_us: u32,
    frequency_hz: f64,
    pulse_count: u32,
    charge_recovery: bool,
    #[serde(default)]
    bench_confirmation: Option<BenchConfirmation>,
    #[serde(default)]
    output_confirmation: Option<OutputConfirmation>,
}

impl StimulationConfigRequest {
    fn requested_output_mode(&self) -> Result<&'static str, String> {
        match (&self.bench_confirmation, &self.output_confirmation) {
            (Some(_), Some(_)) => {
                Err("select exactly one hardware output confirmation mode".to_owned())
            }
            (_, Some(_)) => Ok("hardware-configurable"),
            (Some(confirmation), _) if confirmation.functional_only => Ok("hardware-functional"),
            (Some(_), _) => Ok("hardware-bench"),
            _ => Ok("interlocked"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfirmation {
    operator: String,
    routing_confirmed: bool,
    parameters_confirmed: bool,
    unverified_output_acknowledged: bool,
}

const CONFIGURABLE_COMMAND_LIMIT: u64 = 16;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammedStimulationView {
    amplitude_na: u32,
    phase_width_us: f64,
    interphase_us: f64,
    frequency_hz: f64,
    pulse_count: i32,
    charge_per_phase_pc: f64,
}

fn programmed_stimulation_view(config: &StimConfig) -> ProgrammedStimulationView {
    let amplitude_na = config.magnitude_steps as u32 * config.stim_step_size.current_na();
    let phase_width_us = f64::from(config.phase_steps) / 0.03;
    ProgrammedStimulationView {
        amplitude_na,
        phase_width_us,
        interphase_us: f64::from(config.interphase_steps) / 0.03,
        frequency_hz: 30_000.0 / f64::from(config.period_steps),
        pulse_count: config.pulses,
        charge_per_phase_pc: f64::from(amplitude_na) * phase_width_us / 1000.0,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchConfirmation {
    sample_disconnected: bool,
    dummy_load_connected: bool,
    measurement_ready: bool,
    operator: String,
    #[serde(default)]
    functional_only: bool,
    #[serde(default)]
    functional_repeat: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StimulationScheduleRequest {
    delay_ms: u64,
    repeat_count: u32,
    repeat_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StimulationView {
    state: String,
    output_mode: String,
    arm_id: Option<u64>,
    selected_channels: usize,
    pending_commands: usize,
    completed_commands: u64,
    submitted_commands: u64,
    functional_trigger_limit: u64,
    command_limit: Option<u64>,
    programmed_parameters: Option<ProgrammedStimulationView>,
    last_command_id: Option<u64>,
    last_target_sample_index: Option<u64>,
    audit_path: Option<String>,
}

impl Default for StimulationView {
    fn default() -> Self {
        Self {
            state: "disarmed".to_owned(),
            output_mode: "interlocked".to_owned(),
            arm_id: None,
            selected_channels: 0,
            pending_commands: 0,
            completed_commands: 0,
            submitted_commands: 0,
            functional_trigger_limit: 1,
            command_limit: None,
            programmed_parameters: None,
            last_command_id: None,
            last_target_sample_index: None,
            audit_path: None,
        }
    }
}

impl StimulationView {
    fn hardware_output(&self) -> bool {
        matches!(
            self.output_mode.as_str(),
            "hardware-bench" | "hardware-functional" | "hardware-configurable"
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IoView {
    digital_input: u16,
    digital_output: u16,
    digital_edges: u64,
    analog_outputs: Vec<f64>,
    output_mode: String,
}

impl Default for IoView {
    fn default() -> Self {
        Self {
            digital_input: 0,
            digital_output: 0,
            digital_edges: 0,
            analog_outputs: vec![0.0, 0.0],
            output_mode: "interlocked".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IoOutputRequest {
    digital_output: u16,
    analog_outputs: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpedanceRequest {
    frequency_hz: f64,
    channel_ids: Vec<String>,
    profile_id: String,
    bitfile: Option<String>,
    serial: Option<String>,
    confirmed_test_signal: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpedanceChannelView {
    channel_id: String,
    magnitude_ohms: Option<f64>,
    phase_degrees: Option<f64>,
    quality: String,
    stream: Option<u8>,
    chip_channel: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpedanceView {
    state: String,
    profile_id: Option<String>,
    frequency_hz: f64,
    channels: Vec<ImpedanceChannelView>,
    message: String,
    actual_frequency_hz: Option<f64>,
    completed_channels: usize,
    source: String,
}

impl Default for ImpedanceView {
    fn default() -> Self {
        Self {
            state: "idle".to_owned(),
            profile_id: None,
            frequency_hz: 1_000.0,
            channels: Vec::new(),
            message: "".to_owned(),
            actual_frequency_hz: None,
            completed_channels: 0,
            source: "none".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteApiView {
    state: String,
    address: String,
    clients_served: u64,
    protocol_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteApiRequest {
    port: u16,
    token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProtocolRequest {
    version: u32,
    token: String,
    command: String,
    after_id: Option<u64>,
}

impl Default for RemoteApiView {
    fn default() -> Self {
        Self {
            state: "stopped".to_owned(),
            address: "127.0.0.1".to_owned(),
            clients_served: 0,
            protocol_version: 1,
        }
    }
}

struct DesktopShared {
    mode: &'static str,
    health: &'static str,
    device: Option<DeviceView>,
    recording: bool,
    recording_path: Option<String>,
    session_started: Option<Instant>,
    metrics: MetricsView,
    pipeline: PipelineConfig,
    playback: Option<PlaybackView>,
    triggered_recording: Option<TriggeredRecordingView>,
    stimulation: StimulationView,
    io: IoView,
    impedance: ImpedanceView,
    remote_api: RemoteApiView,
    session_metadata: SessionMetadata,
    last_error: Option<String>,
    latest_waveform: Option<WaveformFrame>,
    waveform_history: VecDeque<(u64, WaveformFrame)>,
    next_waveform_cursor: u64,
    waveform_generation: u64,
    events: VecDeque<EventView>,
    next_event_id: u64,
    spikes: SpikeStore,
}

impl Default for DesktopShared {
    fn default() -> Self {
        Self {
            mode: "idle",
            health: "nominal",
            device: None,
            recording: false,
            recording_path: None,
            session_started: None,
            metrics: MetricsView::default(),
            pipeline: PipelineConfig::default(),
            playback: None,
            triggered_recording: None,
            stimulation: StimulationView::default(),
            io: IoView::default(),
            impedance: ImpedanceView::default(),
            remote_api: RemoteApiView::default(),
            session_metadata: SessionMetadata::default(),
            last_error: None,
            latest_waveform: None,
            waveform_history: VecDeque::new(),
            next_waveform_cursor: 1,
            waveform_generation: 0,
            events: VecDeque::new(),
            next_event_id: 1,
            spikes: SpikeStore::default(),
        }
    }
}

impl DesktopShared {
    fn clear_waveforms(&mut self) {
        self.latest_waveform = None;
        self.waveform_history.clear();
        self.waveform_generation = self.waveform_generation.saturating_add(1);
    }

    fn publish_waveform(&mut self, mut frame: WaveformFrame) {
        frame.spike_epoch = self.spikes.epoch;
        if self.latest_waveform.as_ref().is_some_and(|previous| {
            previous.channel_count != frame.channel_count
                || previous.sample_rate_hz != frame.sample_rate_hz
                || frame.first_sample_index < previous.first_sample_index
        }) {
            self.clear_waveforms();
        }
        let end_sample = frame.first_sample_index + u64::from(frame.sample_count);
        let retained_samples = u64::from(frame.sample_rate_hz) * 12 / 10;
        let cursor = self.next_waveform_cursor;
        self.next_waveform_cursor = cursor.saturating_add(1);
        self.latest_waveform = Some(frame.clone());
        self.waveform_history.push_back((cursor, frame));
        // Bound display memory independently of raw recording and acquisition.
        while self.waveform_history.len() > 1
            && (self.waveform_history.len() > 512
                || self.waveform_history.front().is_some_and(|(_, oldest)| {
                    end_sample
                        .saturating_sub(oldest.first_sample_index + u64::from(oldest.sample_count))
                        > retained_samples
                }))
        {
            self.waveform_history.pop_front();
        }
    }

    fn waveform_batch(&self, after_cursor: Option<u64>, generation: Option<u64>) -> WaveformBatch {
        let after = after_cursor.filter(|_| generation == Some(self.waveform_generation));
        let dropped_frames = match (after, self.waveform_history.front()) {
            (Some(cursor), Some((oldest, _))) => oldest.saturating_sub(cursor.saturating_add(1)),
            _ => 0,
        };
        WaveformBatch {
            frames: self
                .waveform_history
                .iter()
                .filter(|(cursor, _)| after.is_none_or(|after| *cursor > after))
                .map(|(_, frame)| frame.clone())
                .collect(),
            next_cursor: self.next_waveform_cursor.saturating_sub(1),
            generation: self.waveform_generation,
            dropped_frames,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        if let Some(playback) = &self.playback {
            return playback.position_ms;
        }
        self.session_started
            .map_or(0, |started| elapsed_millis(started.elapsed()))
    }

    fn status(&self) -> StatusView {
        StatusView {
            mode: self.mode.to_owned(),
            health: self.health.to_owned(),
            device: self.device.clone(),
            recording: self.recording,
            recording_path: self.recording_path.clone(),
            elapsed_ms: self.elapsed_ms(),
            metrics: self.metrics.clone(),
            pipeline: self.pipeline.clone(),
            playback: self.playback.clone(),
            triggered_recording: self.triggered_recording.clone(),
            stimulation: self.stimulation.clone(),
            io: self.io.clone(),
            impedance: self.impedance.clone(),
            remote_api: self.remote_api.clone(),
            session_metadata: self.session_metadata.clone(),
            stimulation_interlock: if self.stimulation.hardware_output()
                && self.stimulation.arm_id.is_some()
            {
                "operator-confirmed-bench"
            } else {
                "bench-validation-required"
            }
            .to_owned(),
            last_error: self.last_error.clone(),
        }
    }

    fn push_event(&mut self, level: &str, category: &str, message: impl Into<String>) {
        self.push_event_details(level, category, message, None, None);
    }

    fn push_event_details(
        &mut self,
        level: &str,
        category: &str,
        message: impl Into<String>,
        sample_index: Option<u64>,
        channel_index: Option<usize>,
    ) {
        let event = EventView {
            id: self.next_event_id,
            elapsed_ms: self.elapsed_ms(),
            level: level.to_owned(),
            category: category.to_owned(),
            message: message.into(),
            sample_index,
            channel_index,
        };
        self.next_event_id = self.next_event_id.saturating_add(1);
        if self.events.len() == EVENT_LIMIT {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn set_fault(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.mode = "fault";
        self.health = "fault";
        self.last_error = Some(message.clone());
        self.push_event("error", "system", message);
    }
}

enum SessionCommand {
    StartRecording {
        path: PathBuf,
        workspace: Option<serde_json::Value>,
        reply: mpsc::Sender<Result<RecordingView, String>>,
    },
    StopRecording {
        reply: mpsc::Sender<Result<RecordingView, String>>,
    },
    ArmTriggeredRecording {
        request: TriggeredRecordingRequest,
        reply: mpsc::Sender<Result<TriggeredRecordingView, String>>,
    },
    FireSoftwareTrigger {
        reply: mpsc::Sender<Result<TriggeredRecordingView, String>>,
    },
    CancelTriggeredRecording {
        reply: mpsc::Sender<Result<TriggeredRecordingView, String>>,
    },
    ArmStimulation {
        config: StimulationConfigRequest,
        reply: mpsc::Sender<Result<StimulationView, String>>,
    },
    TriggerStimulation {
        schedule: StimulationScheduleRequest,
        reply: mpsc::Sender<Result<StimulationView, String>>,
    },
    CancelStimulation {
        reply: mpsc::Sender<Result<StimulationView, String>>,
    },
    DisarmStimulation {
        reply: mpsc::Sender<Result<StimulationView, String>>,
    },
    UpdatePipeline {
        config: PipelineConfig,
        reply: mpsc::Sender<Result<(), String>>,
    },
    EmergencyStop {
        reply: mpsc::Sender<Result<(), String>>,
    },
    Stop,
}

struct SessionRuntime {
    commands: SyncSender<SessionCommand>,
    join: JoinHandle<()>,
}

enum PlaybackCommand {
    Play,
    Pause,
    SetSpeed(f64),
    Seek {
        fraction: f64,
        reply: mpsc::Sender<Result<(), String>>,
    },
    UpdatePipeline {
        config: PipelineConfig,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Stop,
}

struct PlaybackRuntime {
    commands: SyncSender<PlaybackCommand>,
    join: JoinHandle<()>,
}

struct RemoteApiRuntime {
    stop: SyncSender<()>,
    join: JoinHandle<()>,
}

struct ImpedanceRuntime {
    cancel: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

pub struct DesktopState {
    shared: Arc<Mutex<DesktopShared>>,
    session: Mutex<Option<SessionRuntime>>,
    playback: Mutex<Option<PlaybackRuntime>>,
    remote_api: Mutex<Option<RemoteApiRuntime>>,
    impedance_runtime: Mutex<Option<ImpedanceRuntime>>,
    hardware_gate: Arc<Mutex<()>>,
}

impl Default for DesktopState {
    fn default() -> Self {
        let mut shared = DesktopShared::default();
        shared.push_event(
            "info",
            "system",
            "NeuroX Studio ready; stimulation output remains interlocked",
        );
        Self {
            shared: Arc::new(Mutex::new(shared)),
            session: Mutex::new(None),
            playback: Mutex::new(None),
            remote_api: Mutex::new(None),
            impedance_runtime: Mutex::new(None),
            hardware_gate: Arc::new(Mutex::new(())),
        }
    }
}

impl DesktopState {
    pub fn shutdown(&self) {
        if let Some(runtime) = self
            .impedance_runtime
            .lock()
            .ok()
            .and_then(|mut task| task.take())
        {
            runtime.cancel.store(true, Ordering::Release);
            let _ = runtime.join.join();
        }
        let runtime = self
            .session
            .lock()
            .ok()
            .and_then(|mut session| session.take());
        if let Some(runtime) = runtime {
            let _ = runtime.commands.send(SessionCommand::Stop);
            let _ = runtime.join.join();
        }
        let playback = self
            .playback
            .lock()
            .ok()
            .and_then(|mut playback| playback.take());
        if let Some(runtime) = playback {
            let _ = runtime.commands.send(PlaybackCommand::Stop);
            let _ = runtime.join.join();
        }
        let remote_api = self
            .remote_api
            .lock()
            .ok()
            .and_then(|mut remote_api| remote_api.take());
        if let Some(runtime) = remote_api {
            let _ = runtime.stop.send(());
            let _ = runtime.join.join();
        }
    }
}

#[tauri::command]
pub fn get_status(state: State<'_, DesktopState>) -> Result<StatusView, String> {
    state
        .shared
        .lock()
        .map(|shared| shared.status())
        .map_err(|_| "desktop state lock was poisoned".to_owned())
}

#[tauri::command]
pub fn configure_io_outputs(
    state: State<'_, DesktopState>,
    request: IoOutputRequest,
) -> Result<IoView, String> {
    if request.analog_outputs.len() != 2
        || request
            .analog_outputs
            .iter()
            .any(|value| !value.is_finite() || !(-3.3..=3.3).contains(value))
    {
        return Err("exactly two analog outputs between -3.3 and 3.3 V are required".to_owned());
    }
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.io.digital_output = request.digital_output;
    shared.io.analog_outputs = request.analog_outputs;
    shared.io.output_mode = "interlocked".to_owned();
    let view = shared.io.clone();
    shared.push_event(
        "warning",
        "io",
        "I/O output configuration updated; physical outputs remain interlocked",
    );
    Ok(view)
}

#[tauri::command]
pub fn prepare_impedance_scan(
    state: State<'_, DesktopState>,
    request: ImpedanceRequest,
) -> Result<ImpedanceView, String> {
    let (channel_count, indices) = validate_impedance_scan(&request)?;
    let _hardware = state
        .hardware_gate
        .try_lock()
        .map_err(|_| "hardware is busy")?;
    ensure_session_idle(&state)?;
    let mut runtime = state
        .impedance_runtime
        .lock()
        .map_err(|_| "impedance runtime lock failed")?;
    if let Some(previous) = runtime.take() {
        previous
            .join
            .join()
            .map_err(|_| "previous impedance task panicked")?;
    }
    let view = ImpedanceView {
        state: "running".to_owned(),
        profile_id: Some(request.profile_id.clone()),
        frequency_hz: request.frequency_hz,
        channels: request
            .channel_ids
            .iter()
            .cloned()
            .map(|channel_id| ImpedanceChannelView {
                channel_id,
                magnitude_ohms: None,
                phase_degrees: None,
                quality: "pending".to_owned(),
                stream: None,
                chip_channel: None,
            })
            .collect(),
        message: "Measuring using the RHS2116 AC impedance test circuit; stimulation disabled"
            .to_owned(),
        actual_frequency_hz: Some(30000.0 / (30000.0 / request.frequency_hz).round()),
        completed_channels: 0,
        source: "hardware".to_owned(),
    };
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.impedance = view.clone();
    shared.push_event(
        "info",
        "impedance",
        "Hardware impedance measurement requested; test AC injection explicitly confirmed",
    );
    drop(shared);
    let shared = Arc::clone(&state.shared);
    let gate = Arc::clone(&state.hardware_gate);
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    let device = make_device(&request.bitfile, &request.serial);
    let frequency = request.frequency_hz;
    let channel_ids = request.channel_ids;
    let join = thread::Builder::new()
        .name("rhs-impedance".to_owned())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _gate = gate.lock().map_err(|_| "hardware lock failed".to_owned())?;
                device
                    .measure_impedances(
                        channel_count,
                        frequency,
                        &indices,
                        &thread_cancel,
                        |measurement| {
                            if let Ok(mut state) = shared.lock() {
                                if let Some(channel) =
                                    state.impedance.channels.iter_mut().find(|channel| {
                                        channel.channel_id
                                            == channel_ids[indices
                                                .iter()
                                                .position(|i| *i == measurement.index)
                                                .expect("validated measurement index")]
                                    })
                                {
                                    channel.magnitude_ohms = (measurement.quality != "invalid")
                                        .then_some(measurement.magnitude_ohms);
                                    channel.phase_degrees = (measurement.quality != "invalid")
                                        .then_some(measurement.phase_degrees);
                                    channel.quality = measurement.quality.to_owned();
                                    channel.stream = Some(measurement.stream);
                                    channel.chip_channel = Some(measurement.channel);
                                }
                                state.impedance.actual_frequency_hz =
                                    Some(measurement.frequency_hz);
                                state.impedance.completed_channels += 1;
                            }
                        },
                    )
                    .map_err(|error| error.to_string())
            }));
            if let Ok(mut state) = shared.lock() {
                let (phase, message) = match outcome {
                    Ok(Ok(true)) => (
                        "complete",
                        "Hardware impedance measurement completed".to_owned(),
                    ),
                    Ok(Ok(false)) => (
                        "cancelled",
                        "Impedance measurement cancelled; test circuit disabled".to_owned(),
                    ),
                    Ok(Err(error)) => ("fault", error),
                    Err(_) => ("fault", "impedance worker panicked".to_owned()),
                };
                state.impedance.state = phase.to_owned();
                state.impedance.message = message.clone();
                state.push_event(
                    if phase == "fault" { "error" } else { "info" },
                    "impedance",
                    message,
                );
            }
        })
        .map_err(|error| {
            if let Ok(mut shared) = state.shared.lock() {
                shared.impedance.state = "fault".to_owned();
                shared.impedance.message = error.to_string();
            }
            error.to_string()
        })?;
    *runtime = Some(ImpedanceRuntime { cancel, join });
    Ok(view)
}

fn validate_impedance_scan(request: &ImpedanceRequest) -> Result<(usize, Vec<usize>), String> {
    if !request.confirmed_test_signal {
        return Err("confirm AC test-signal injection before measuring impedance".to_owned());
    }
    if !request.frequency_hz.is_finite() || !(10.0..=2000.0).contains(&request.frequency_hz) {
        return Err("impedance frequency must be 10..2000 Hz".to_owned());
    }
    let count = match request.profile_id.as_str() {
        "rhs2116-logic-16" => 16,
        "mea-ab-64" => 64,
        _ => {
            return Err(
                "hardware impedance supports RHS2116 16 and MEA 64 profiles only".to_owned(),
            )
        }
    };
    if request.channel_ids.is_empty() || request.channel_ids.len() > count {
        return Err("select valid impedance channels".to_owned());
    }
    let mut indices = Vec::new();
    for id in &request.channel_ids {
        let index = if count == 16 {
            usize::from(rhs2116_hardware_channel(id)?)
        } else {
            let (stream, channel) = mea64_hardware_address(id)?;
            (stream - 2) * 16 + usize::from(channel)
        };
        if indices.contains(&index) {
            return Err("duplicate impedance channel".to_owned());
        }
        indices.push(index);
    }
    Ok((count, indices))
}

#[cfg(test)]
#[test]
fn impedance_probe_map_order_preserves_physical_indices_and_profile_metadata() {
    let request = ImpedanceRequest {
        frequency_hz: 1000.0,
        channel_ids: [
            "B-009", "B-008", "B-005", "B-001", "C-030", "C-026", "C-023", "C-022",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        profile_id: "mea-ab-64".to_owned(),
        bitfile: None,
        serial: None,
        confirmed_test_signal: true,
    };
    assert_eq!(
        validate_impedance_scan(&request).unwrap(),
        (64, vec![9, 8, 5, 1, 62, 58, 55, 54])
    );
    let view = ImpedanceView {
        profile_id: Some(request.profile_id),
        ..ImpedanceView::default()
    };
    assert_eq!(
        serde_json::to_value(view).unwrap()["profileId"],
        "mea-ab-64"
    );
}

#[tauri::command]
pub fn cancel_impedance_scan(state: State<'_, DesktopState>) -> Result<ImpedanceView, String> {
    if let Some(task) = state
        .impedance_runtime
        .lock()
        .map_err(|_| "impedance runtime lock failed")?
        .as_ref()
    {
        task.cancel.store(true, Ordering::Release);
    }
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    if shared.impedance.state == "running" {
        shared.impedance.state = "cancelling".to_owned();
        shared.impedance.message =
            "Cancellation requested; waiting for test circuit cleanup".to_owned();
    }
    let view = shared.impedance.clone();
    shared.push_event("info", "impedance", "Impedance scan cancelled");
    Ok(view)
}

#[tauri::command]
pub async fn start_remote_api(
    app: AppHandle,
    request: RemoteApiRequest,
) -> Result<RemoteApiView, String> {
    tauri::async_runtime::spawn_blocking(move || start_remote_api_inner(&app, request))
        .await
        .map_err(|error| format!("remote API task failed: {error}"))?
}

#[tauri::command]
pub async fn stop_remote_api(app: AppHandle) -> Result<RemoteApiView, String> {
    tauri::async_runtime::spawn_blocking(move || stop_remote_api_inner(&app))
        .await
        .map_err(|error| format!("remote API stop task failed: {error}"))?
}

#[tauri::command]
pub fn get_pipeline_registry() -> PipelineRegistryView {
    pipeline_registry()
}

#[tauri::command]
pub fn poll_waveform(state: State<'_, DesktopState>) -> Result<Option<WaveformFrame>, String> {
    state
        .shared
        .lock()
        .map(|shared| shared.latest_waveform.clone())
        .map_err(|_| "desktop state lock was poisoned".to_owned())
}

#[tauri::command]
pub fn poll_waveforms(
    state: State<'_, DesktopState>,
    after_cursor: Option<u64>,
    generation: Option<u64>,
) -> Result<WaveformBatch, String> {
    state
        .shared
        .lock()
        .map(|shared| shared.waveform_batch(after_cursor, generation))
        .map_err(|_| "desktop state lock was poisoned".to_owned())
}

#[tauri::command]
pub fn poll_events(
    state: State<'_, DesktopState>,
    after_id: u64,
) -> Result<Vec<EventView>, String> {
    state
        .shared
        .lock()
        .map(|shared| {
            shared
                .events
                .iter()
                .filter(|event| event.id > after_id)
                .cloned()
                .collect()
        })
        .map_err(|_| "desktop state lock was poisoned".to_owned())
}

#[tauri::command]
pub fn configure_spikes(state: State<'_, DesktopState>, config: SpikeConfig) -> Result<(), String> {
    config.validate()?;
    ensure_session_idle(&state)?;
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned")?;
    shared.spikes.config = config;
    shared.spikes.begin();
    shared.spikes.running = false;
    shared.push_event(
        "info",
        "spike",
        "Full-rate multichannel spike settings updated",
    );
    Ok(())
}

#[tauri::command]
pub fn poll_spikes(
    state: State<'_, DesktopState>,
    epoch: u64,
    after_id: u64,
) -> Result<SpikeBatch, String> {
    state
        .shared
        .lock()
        .map(|shared| shared.spikes.poll(epoch, after_id))
        .map_err(|_| "desktop state lock was poisoned".to_owned())
}

fn begin_spikes(shared: &Arc<Mutex<DesktopShared>>) -> Option<SpikeAnalyzer> {
    let mut state = shared.lock().ok()?;
    state.spikes.begin();
    if !state.spikes.config.enabled {
        return None;
    }
    match SpikeAnalyzer::new(state.spikes.config.clone()) {
        Ok(analyzer) => {
            state.spikes.error = analyzer.recording_error.clone();
            Some(analyzer)
        }
        Err(error) => {
            state.spikes.running = false;
            state.spikes.error = Some(error);
            None
        }
    }
}

fn restart_spike_segment(analyzer: &mut Option<SpikeAnalyzer>, shared: &Arc<Mutex<DesktopShared>>) {
    if let Some(active) = analyzer.as_mut() {
        let first_id = active.restart_segment();
        if let Ok(mut state) = shared.lock() {
            state.spikes.begin_segment(first_id);
        }
    }
}

fn process_spikes(
    analyzer: &mut Option<SpikeAnalyzer>,
    block: &SampleBlock,
    shared: &Arc<Mutex<DesktopShared>>,
) {
    let Some(active) = analyzer.as_mut() else {
        return;
    };
    let result = active.process(block);
    if let Ok(mut state) = shared.lock() {
        state.spikes.latest_sample_index = block.last_sample_index();
        state.spikes.sample_rate_hz = block.sample_rate_hz;
        state.spikes.thresholds = active.thresholds();
        state.spikes.counts.resize(block.channel_count(), 0);
        state.spikes.discarded_incomplete = active.discarded_incomplete;
        state.spikes.discontinuities = active.discontinuities;
        state.spikes.recorded_events = active.recorded_events;
        state.spikes.error = active.recording_error.clone();
        match result {
            Ok(events) => state.spikes.append(events),
            Err(error) => {
                active.finish();
                state.spikes.error = Some(error.clone());
                state.spikes.running = false;
                state.push_event("error", "spike", error);
                *analyzer = None;
            }
        }
    }
}

fn finish_spikes(analyzer: &mut Option<SpikeAnalyzer>, shared: &Arc<Mutex<DesktopShared>>) {
    if let Some(active) = analyzer.as_mut() {
        active.finish();
        if let Ok(mut state) = shared.lock() {
            state.spikes.running = false;
            state.spikes.discarded_incomplete = active.discarded_incomplete;
            state.spikes.error = active.recording_error.clone();
            state.spikes.recording_complete =
                active.recording_error.is_none() && state.spikes.config.recording_path.is_some();
        }
    }
}

#[tauri::command]
pub async fn scan_device(app: AppHandle, request: DeviceRequest) -> Result<DeviceView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let _hardware = state
            .hardware_gate
            .try_lock()
            .map_err(|_| "hardware is busy")?;
        ensure_session_idle(&state)?;
        update_mode(&state.shared, "connecting", "nominal", None)?;
        let device = make_device(&request.bitfile, &request.serial);
        let bitfile = device.bitfile_path().to_string_lossy().into_owned();
        let report = device.scan().map_err(|error| error.to_string())?;
        if !scan_report_is_usable(&report) {
            let message = format!("device scan failed: {}", report.message);
            update_mode(&state.shared, "fault", "fault", Some(message.clone()))?;
            return Err(message);
        }
        let view = device_view_from_scan(
            &report,
            request.serial.unwrap_or_else(|| "auto-detected".to_owned()),
            bitfile,
        )?;
        let mut shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned".to_owned())?;
        shared.mode = "ready";
        shared.health = "nominal";
        shared.device = Some(view.clone());
        shared.last_error = None;
        shared.push_event(
            "success",
            "device",
            format!(
                "{} detected on Port {}, stream {}",
                view.chip_name, view.port, view.stream
            ),
        );
        Ok(view)
    })
    .await
    .map_err(|error| format!("scan task failed: {error}"))?
}

#[tauri::command]
pub async fn start_acquisition(
    app: AppHandle,
    request: StartRequest,
) -> Result<StatusView, String> {
    tauri::async_runtime::spawn_blocking(move || start_acquisition_inner(&app, request))
        .await
        .map_err(|error| format!("start task failed: {error}"))?
}

fn start_acquisition_inner(app: &AppHandle, request: StartRequest) -> Result<StatusView, String> {
    let desktop = app.state::<DesktopState>();
    let _hardware = desktop
        .hardware_gate
        .try_lock()
        .map_err(|_| "hardware is busy")?;
    if !(1..=32).contains(&request.blocks_per_read) {
        return Err("blocksPerRead must be between 1 and 32".to_owned());
    }
    if request.queue_capacity == 0 || request.queue_capacity > 128 {
        return Err("queueCapacity must be between 1 and 128".to_owned());
    }
    request.metadata.validate()?;
    let audit_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("could not resolve audit directory: {error}"))?
        .join("stimulation-audit");
    let stimulation_preset = request
        .stimulation_preset
        .as_ref()
        .map(rhs2116_hardware_stimulation)
        .transpose()?;
    let state = app.state::<DesktopState>();
    ensure_session_idle(&state)?;
    let mut session_guard = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?;
    if session_guard.is_some() {
        return Err("an acquisition session is already active".to_owned());
    }
    update_mode(&state.shared, "connecting", "nominal", None)?;
    let device = make_device(&request.bitfile, &request.serial);
    let bitfile = device.bitfile_path().to_string_lossy().into_owned();
    let handle = device
        .spawn_acquisition(AcquisitionWorkerConfig {
            channel_count: request.channel_count,
            acquisition: AcquisitionConfig {
                blocks_per_read: request.blocks_per_read,
            },
            data_queue_capacity: request.queue_capacity,
            stimulation: stimulation_preset.clone(),
            ..AcquisitionWorkerConfig::default()
        })
        .map_err(|error| {
            let message = format!("could not start acquisition: {error}");
            let _ = update_mode(&state.shared, "fault", "fault", Some(message.clone()));
            message
        })?;
    let info = handle.info().clone();
    let device_view = device_view_from_info(&info, bitfile);
    let pipeline = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?
        .pipeline
        .clone();
    let (command_tx, command_rx) = mpsc::sync_channel(16);
    let shared = Arc::clone(&state.shared);
    let join = thread::Builder::new()
        .name("rhs-desktop-session".to_owned())
        .spawn(move || run_session(handle, info, pipeline, command_rx, shared, audit_directory))
        .map_err(|error| format!("could not spawn desktop session: {error}"))?;
    *session_guard = Some(SessionRuntime {
        commands: command_tx,
        join,
    });

    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.mode = "acquiring";
    shared.health = "nominal";
    shared.device = Some(device_view.clone());
    shared.recording = false;
    shared.recording_path = None;
    shared.session_started = Some(Instant::now());
    shared.metrics = MetricsView::default();
    shared.session_metadata = request.metadata;
    shared.last_error = None;
    shared.clear_waveforms();
    shared.push_event(
        "success",
        "acquisition",
        format!(
            "Acquisition started at {} Hz, {} channels; {}",
            device_view.sample_rate_hz,
            device_view.channels,
            if stimulation_preset.is_some() {
                "RHS2116 stimulation preset preloaded for selected channels; output disabled"
            } else {
                "stimulation disabled"
            }
        ),
    );
    Ok(shared.status())
}

#[tauri::command]
pub async fn stop_acquisition(app: AppHandle) -> Result<StatusView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let runtime = state
            .session
            .lock()
            .map_err(|_| "session lock was poisoned".to_owned())?
            .take();
        if let Some(runtime) = runtime {
            let _ = runtime.commands.send(SessionCommand::Stop);
            runtime
                .join
                .join()
                .map_err(|_| "desktop session thread panicked".to_owned())?;
        }
        state
            .shared
            .lock()
            .map(|shared| shared.status())
            .map_err(|_| "desktop state lock was poisoned".to_owned())
    })
    .await
    .map_err(|error| format!("stop task failed: {error}"))?
}

#[tauri::command]
pub async fn read_recording_parameters(path: String) -> Result<Option<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let source = FileReplaySource::open(PathBuf::from(path.trim()))
            .map_err(|error| error.to_string())?;
        recording_configuration(source.info())
    })
    .await
    .map_err(|error| format!("read recording parameters task failed: {error}"))?
}

#[tauri::command]
pub async fn open_playback(app: AppHandle, path: String) -> Result<StatusView, String> {
    tauri::async_runtime::spawn_blocking(move || open_playback_inner(&app, path))
        .await
        .map_err(|error| format!("open playback task failed: {error}"))?
}

fn open_playback_inner(app: &AppHandle, path: String) -> Result<StatusView, String> {
    let desktop = app.state::<DesktopState>();
    let _hardware = desktop
        .hardware_gate
        .try_lock()
        .map_err(|_| "hardware is busy")?;
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err("playback path must not be empty".to_owned());
    }
    if path.extension().and_then(|value| value.to_str()) != Some("rhsraw") {
        return Err("playback path must use the .rhsraw extension".to_owned());
    }
    let state = app.state::<DesktopState>();
    ensure_session_idle(&state)?;
    let summary = inspect_playback(&path)?;
    let configuration = recording_configuration(&summary.info)?;
    let mut pipeline = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?
        .pipeline
        .clone();
    if let Some(value) = configuration
        .as_ref()
        .and_then(|value| value["workspace"].get("pipeline"))
    {
        pipeline = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid recorded pipeline: {error}"))?;
    }
    if configuration.is_none()
        && pipeline
            .validate_for(summary.info.sample_rate_hz, summary.info.channels.len())
            .is_err()
    {
        pipeline = PipelineConfig::default();
    }
    pipeline.validate_for(summary.info.sample_rate_hz, summary.info.channels.len())?;
    let spike_config = configuration
        .as_ref()
        .and_then(|value| value.get("spikes"))
        .map(|value| {
            let mut config: SpikeConfig =
                serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
            config.recording_path = None;
            config.validate()?;
            Ok::<_, String>(config)
        })
        .transpose()?;
    let session_metadata = configuration
        .as_ref()
        .and_then(|value| value["workspace"].get("session"))
        .map(|value| {
            let metadata: SessionMetadata =
                serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
            metadata.validate()?;
            Ok::<_, String>(metadata)
        })
        .transpose()?;
    {
        let mut shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned")?;
        shared.pipeline = pipeline.clone();
        if let Some(metadata) = session_metadata {
            shared.session_metadata = metadata;
        }
        // A replay must never re-open a recorded CSV destination or arm hardware output.
        if let Some(config) = spike_config {
            shared.spikes.config = config;
        }
        shared.spikes.config.recording_path = None;
        shared.stimulation = SoftwareStimulationState::default().view;
    }

    let (command_tx, command_rx) = mpsc::sync_channel(16);
    let shared = Arc::clone(&state.shared);
    let playback_path = path.clone();
    let playback_summary = summary.clone();
    let join = thread::Builder::new()
        .name("rhs-desktop-playback".to_owned())
        .spawn(move || {
            run_playback(
                playback_path,
                playback_summary,
                pipeline,
                command_rx,
                shared,
            )
        })
        .map_err(|error| format!("could not spawn playback session: {error}"))?;
    *state
        .playback
        .lock()
        .map_err(|_| "playback lock was poisoned".to_owned())? = Some(PlaybackRuntime {
        commands: command_tx,
        join,
    });

    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.mode = "playback-paused";
    shared.health = "nominal";
    shared.device = None;
    shared.recording = false;
    shared.recording_path = None;
    shared.session_started = None;
    shared.metrics = MetricsView::default();
    shared.clear_waveforms();
    shared.playback = Some(playback_view(&path, &summary, 0, 1.0, "paused"));
    shared.last_error = None;
    shared.push_event(
        "success",
        "playback",
        format!(
            "Playback opened: {} channels, {:.3} seconds",
            summary.info.channels.len(),
            summary.total_samples as f64 / summary.info.sample_rate_hz as f64
        ),
    );
    Ok(shared.status())
}

#[tauri::command]
pub fn play_playback(state: State<'_, DesktopState>) -> Result<StatusView, String> {
    send_playback_command(&state, PlaybackCommand::Play)?;
    get_status(state)
}

#[tauri::command]
pub fn pause_playback(state: State<'_, DesktopState>) -> Result<StatusView, String> {
    send_playback_command(&state, PlaybackCommand::Pause)?;
    get_status(state)
}

#[tauri::command]
pub async fn seek_playback(app: AppHandle, fraction: f64) -> Result<StatusView, String> {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return Err("playback seek fraction must be between 0 and 1".to_owned());
    }
    let state = app.state::<DesktopState>();
    let commands = playback_commands(&state)?;
    let (reply_tx, reply_rx) = mpsc::channel();
    commands
        .send(PlaybackCommand::Seek {
            fraction,
            reply: reply_tx,
        })
        .map_err(|_| "playback session is no longer available".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || {
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "playback seek timed out".to_owned())?
    })
    .await
    .map_err(|error| format!("playback seek task failed: {error}"))??;
    get_status(state)
}

#[tauri::command]
pub fn set_playback_speed(
    state: State<'_, DesktopState>,
    speed: f64,
) -> Result<StatusView, String> {
    if !speed.is_finite() || !(0.25..=4.0).contains(&speed) {
        return Err("playback speed must be between 0.25 and 4.0".to_owned());
    }
    send_playback_command(&state, PlaybackCommand::SetSpeed(speed))?;
    get_status(state)
}

#[tauri::command]
pub async fn close_playback(app: AppHandle) -> Result<StatusView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let runtime = state
            .playback
            .lock()
            .map_err(|_| "playback lock was poisoned".to_owned())?
            .take();
        if let Some(runtime) = runtime {
            let _ = runtime.commands.send(PlaybackCommand::Stop);
            runtime
                .join
                .join()
                .map_err(|_| "playback thread panicked".to_owned())?;
        }
        let mut shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned".to_owned())?;
        shared.playback = None;
        shared.clear_waveforms();
        shared.mode = "idle";
        shared.health = "nominal";
        shared.push_event("info", "playback", "Playback closed");
        Ok(shared.status())
    })
    .await
    .map_err(|error| format!("close playback task failed: {error}"))?
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    path: String,
    workspace: Option<serde_json::Value>,
) -> Result<RecordingView, String> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err("recording path must not be empty".to_owned());
    }
    request_session(&app, move |reply| SessionCommand::StartRecording {
        path,
        workspace,
        reply,
    })
    .await
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> Result<RecordingView, String> {
    request_session(&app, |reply| SessionCommand::StopRecording { reply }).await
}

#[tauri::command]
pub async fn arm_triggered_recording(
    app: AppHandle,
    request: TriggeredRecordingRequest,
) -> Result<TriggeredRecordingView, String> {
    request_session(&app, move |reply| SessionCommand::ArmTriggeredRecording {
        request,
        reply,
    })
    .await
}

#[tauri::command]
pub async fn fire_software_trigger(app: AppHandle) -> Result<TriggeredRecordingView, String> {
    request_session(&app, |reply| SessionCommand::FireSoftwareTrigger { reply }).await
}

#[tauri::command]
pub async fn cancel_triggered_recording(app: AppHandle) -> Result<TriggeredRecordingView, String> {
    request_session(&app, |reply| SessionCommand::CancelTriggeredRecording {
        reply,
    })
    .await
}

#[tauri::command]
pub async fn arm_stimulation(
    app: AppHandle,
    config: StimulationConfigRequest,
) -> Result<StimulationView, String> {
    request_session(&app, move |reply| SessionCommand::ArmStimulation {
        config,
        reply,
    })
    .await
}

#[tauri::command]
pub async fn trigger_stimulation(
    app: AppHandle,
    schedule: StimulationScheduleRequest,
) -> Result<StimulationView, String> {
    request_session(&app, move |reply| SessionCommand::TriggerStimulation {
        schedule,
        reply,
    })
    .await
}

#[tauri::command]
pub async fn cancel_stimulation(app: AppHandle) -> Result<StimulationView, String> {
    request_session(&app, |reply| SessionCommand::CancelStimulation { reply }).await
}

#[tauri::command]
pub async fn disarm_stimulation(app: AppHandle) -> Result<StimulationView, String> {
    request_session(&app, |reply| SessionCommand::DisarmStimulation { reply }).await
}

#[tauri::command]
pub async fn update_pipeline(app: AppHandle, config: PipelineConfig) -> Result<StatusView, String> {
    let state = app.state::<DesktopState>();
    let (sample_rate_hz, channel_count) = {
        let shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned".to_owned())?;
        if let Some(playback) = &shared.playback {
            (playback.sample_rate_hz, playback.channel_count)
        } else if let Some(device) = &shared.device {
            (device.sample_rate_hz, device.channels)
        } else {
            (RHS_SAMPLE_RATE_HZ, MAX_SOFTWARE_CHANNEL_COUNT)
        }
    };
    config.validate_for(sample_rate_hz, channel_count)?;
    let acquisition_command = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?
        .as_ref()
        .map(|runtime| runtime.commands.clone());
    if let Some(command) = acquisition_command {
        let (reply_tx, reply_rx) = mpsc::channel();
        command
            .send(SessionCommand::UpdatePipeline {
                config,
                reply: reply_tx,
            })
            .map_err(|_| "acquisition session is no longer available".to_owned())?;
        tauri::async_runtime::spawn_blocking(move || {
            reply_rx
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "pipeline update timed out".to_owned())?
        })
        .await
        .map_err(|error| format!("pipeline task failed: {error}"))??;
    } else {
        let playback_command = {
            state
                .playback
                .lock()
                .map_err(|_| "playback lock was poisoned".to_owned())?
                .as_ref()
                .map(|runtime| runtime.commands.clone())
        };
        if let Some(command) = playback_command {
            let (reply_tx, reply_rx) = mpsc::channel();
            command
                .send(PlaybackCommand::UpdatePipeline {
                    config,
                    reply: reply_tx,
                })
                .map_err(|_| "playback session is no longer available".to_owned())?;
            tauri::async_runtime::spawn_blocking(move || {
                reply_rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "playback pipeline update timed out".to_owned())?
            })
            .await
            .map_err(|error| format!("playback pipeline task failed: {error}"))??;
        } else {
            let mut shared = state
                .shared
                .lock()
                .map_err(|_| "desktop state lock was poisoned".to_owned())?;
            shared.pipeline = config;
            shared.push_event("info", "pipeline", "Pipeline settings updated");
        }
    }
    get_status(state)
}

#[tauri::command]
pub async fn emergency_stop(app: AppHandle) -> Result<StatusView, String> {
    if let Some(task) = app
        .state::<DesktopState>()
        .impedance_runtime
        .lock()
        .map_err(|_| "impedance runtime lock failed")?
        .as_ref()
    {
        task.cancel.store(true, Ordering::Release);
    }
    let result = request_session(&app, |reply| SessionCommand::EmergencyStop { reply }).await;
    let state = app.state::<DesktopState>();
    if result.is_err() {
        let mut shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned".to_owned())?;
        shared.push_event(
            "warning",
            "stimulation",
            "Emergency stop requested while no hardware session was active",
        );
    }
    get_status(state)
}

async fn request_session<T: Send + 'static>(
    app: &AppHandle,
    make_command: impl FnOnce(mpsc::Sender<Result<T, String>>) -> SessionCommand,
) -> Result<T, String> {
    let state = app.state::<DesktopState>();
    let commands = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?
        .as_ref()
        .map(|runtime| runtime.commands.clone())
        .ok_or_else(|| "no acquisition session is active".to_owned())?;
    let (reply_tx, reply_rx) = mpsc::channel();
    commands
        .send(make_command(reply_tx))
        .map_err(|_| "acquisition session is no longer available".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || {
        reply_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "session command timed out".to_owned())?
    })
    .await
    .map_err(|error| format!("session command task failed: {error}"))?
}

fn start_remote_api_inner(
    app: &AppHandle,
    request: RemoteApiRequest,
) -> Result<RemoteApiView, String> {
    if !(1_024..=65_535).contains(&request.port) {
        return Err("remote API port must be between 1024 and 65535".to_owned());
    }
    if request.token.len() < 12 || request.token.len() > 128 {
        return Err("remote API token must contain between 12 and 128 characters".to_owned());
    }
    let state = app.state::<DesktopState>();
    let mut runtime = state
        .remote_api
        .lock()
        .map_err(|_| "remote API lock was poisoned".to_owned())?;
    if runtime.is_some() {
        return Err("remote API is already running".to_owned());
    }
    let listener = TcpListener::bind(("127.0.0.1", request.port))
        .map_err(|error| format!("could not bind local remote API: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure local remote API: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read local remote API address: {error}"))?
        .to_string();
    let (stop_tx, stop_rx) = mpsc::sync_channel(1);
    let shared = Arc::clone(&state.shared);
    let token = request.token;
    let join = thread::Builder::new()
        .name("rhs-desktop-remote-api".to_owned())
        .spawn(move || run_remote_api(listener, token, stop_rx, shared))
        .map_err(|error| format!("could not spawn local remote API: {error}"))?;
    *runtime = Some(RemoteApiRuntime {
        stop: stop_tx,
        join,
    });
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.remote_api = RemoteApiView {
        state: "running".to_owned(),
        address,
        clients_served: 0,
        protocol_version: 1,
    };
    let view = shared.remote_api.clone();
    shared.push_event(
        "success",
        "remote-api",
        format!("Local API listening at {}", view.address),
    );
    Ok(view)
}

fn stop_remote_api_inner(app: &AppHandle) -> Result<RemoteApiView, String> {
    let state = app.state::<DesktopState>();
    let runtime = state
        .remote_api
        .lock()
        .map_err(|_| "remote API lock was poisoned".to_owned())?
        .take();
    if let Some(runtime) = runtime {
        let _ = runtime.stop.send(());
        runtime
            .join
            .join()
            .map_err(|_| "remote API thread panicked".to_owned())?;
    }
    let mut shared = state
        .shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    let clients_served = shared.remote_api.clients_served;
    shared.remote_api = RemoteApiView {
        clients_served,
        ..RemoteApiView::default()
    };
    let view = shared.remote_api.clone();
    shared.push_event("info", "remote-api", "Local API stopped");
    Ok(view)
}

fn run_remote_api(
    listener: TcpListener,
    token: String,
    stop: Receiver<()>,
    shared: Arc<Mutex<DesktopShared>>,
) {
    loop {
        if stop.try_recv().is_ok() {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = handle_remote_client(&mut stream, &token, &shared);
                if let Ok(mut state) = shared.lock() {
                    state.remote_api.clients_served =
                        state.remote_api.clients_served.saturating_add(1);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.recv_timeout(Duration::from_millis(40)).is_ok() {
                    break;
                }
            }
            Err(error) => {
                if let Ok(mut state) = shared.lock() {
                    state.remote_api.state = "fault".to_owned();
                    state.push_event(
                        "error",
                        "remote-api",
                        format!("Local API accept failed: {error}"),
                    );
                }
                break;
            }
        }
    }
}

fn handle_remote_client(
    stream: &mut TcpStream,
    token: &str,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(750)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_millis(750)))
        .map_err(|error| error.to_string())?;
    let mut bytes = vec![0_u8; 8_192];
    let count = stream.read(&mut bytes).map_err(|error| error.to_string())?;
    let response = if count == 0 {
        serde_json::json!({ "ok": false, "error": "empty request" })
    } else {
        match serde_json::from_slice::<RemoteProtocolRequest>(&bytes[..count]) {
            Ok(request) => remote_protocol_response(request, token, shared),
            Err(_) => serde_json::json!({ "ok": false, "error": "invalid JSON request" }),
        }
    };
    let mut encoded = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .map_err(|error| error.to_string())
}

fn remote_protocol_response(
    request: RemoteProtocolRequest,
    token: &str,
    shared: &Arc<Mutex<DesktopShared>>,
) -> serde_json::Value {
    if request.version != 1 {
        return serde_json::json!({ "ok": false, "error": "unsupported protocol version" });
    }
    if request.token != token {
        return serde_json::json!({ "ok": false, "error": "unauthorized" });
    }
    let state = match shared.lock() {
        Ok(state) => state,
        Err(_) => return serde_json::json!({ "ok": false, "error": "state unavailable" }),
    };
    match request.command.as_str() {
        "ping" => serde_json::json!({ "ok": true, "data": { "protocolVersion": 1 } }),
        "status" => serde_json::json!({ "ok": true, "data": state.status() }),
        "events" => {
            let after_id = request.after_id.unwrap_or(0);
            let events: Vec<_> = state
                .events
                .iter()
                .filter(|event| event.id > after_id)
                .cloned()
                .collect();
            serde_json::json!({ "ok": true, "data": events })
        }
        _ => serde_json::json!({ "ok": false, "error": "unknown command" }),
    }
}

fn playback_commands(state: &DesktopState) -> Result<SyncSender<PlaybackCommand>, String> {
    state
        .playback
        .lock()
        .map_err(|_| "playback lock was poisoned".to_owned())?
        .as_ref()
        .map(|runtime| runtime.commands.clone())
        .ok_or_else(|| "no playback session is open".to_owned())
}

fn send_playback_command(state: &DesktopState, command: PlaybackCommand) -> Result<(), String> {
    playback_commands(state)?
        .send(command)
        .map_err(|_| "playback session is no longer available".to_owned())
}

#[derive(Clone)]
struct PlaybackSummary {
    info: RecordingInfo,
    block_count: u64,
    total_samples: u64,
}

fn recording_configuration(info: &RecordingInfo) -> Result<Option<serde_json::Value>, String> {
    let configuration = read_configuration(&info.metadata)?;
    if let Some(value) = &configuration {
        if value["channelCount"].as_u64() != Some(info.channels.len() as u64)
            || value["sampleRateHz"].as_u64() != Some(info.sample_rate_hz as u64)
        {
            return Err("recorded configuration does not match the data header".into());
        }
    }
    Ok(configuration)
}

fn inspect_playback(path: &Path) -> Result<PlaybackSummary, String> {
    let mut source = FileReplaySource::open(path).map_err(|error| error.to_string())?;
    let info = source.info().clone();
    let mut block_count = 0_u64;
    let mut total_samples = 0_u64;
    while let Some(block) = source.next_block().map_err(|error| error.to_string())? {
        block_count = block_count.saturating_add(1);
        total_samples = total_samples.saturating_add(block.samples_per_channel as u64);
    }
    if block_count == 0 || total_samples == 0 {
        return Err("recording does not contain any data blocks".to_owned());
    }
    Ok(PlaybackSummary {
        info,
        block_count,
        total_samples,
    })
}

fn playback_view(
    path: &Path,
    summary: &PlaybackSummary,
    position_samples: u64,
    speed: f64,
    state: &str,
) -> PlaybackView {
    let sample_rate = summary.info.sample_rate_hz.max(1) as u64;
    PlaybackView {
        path: path.to_string_lossy().into_owned(),
        state: state.to_owned(),
        complete: summary.info.complete,
        sample_rate_hz: summary.info.sample_rate_hz,
        channel_count: summary.info.channels.len(),
        block_count: summary.block_count,
        position_samples: position_samples.min(summary.total_samples),
        total_samples: summary.total_samples,
        position_ms: position_samples
            .min(summary.total_samples)
            .saturating_mul(1_000)
            / sample_rate,
        duration_ms: summary.total_samples.saturating_mul(1_000) / sample_rate,
        speed,
    }
}

fn seek_playback_source(
    path: &Path,
    target_samples: u64,
) -> Result<(FileReplaySource, Option<SampleBlock>, u64, u64), String> {
    let mut source = FileReplaySource::open(path).map_err(|error| error.to_string())?;
    let mut position = 0_u64;
    let mut block_count = 0_u64;
    while let Some(block) = source.next_block().map_err(|error| error.to_string())? {
        let next = position.saturating_add(block.samples_per_channel as u64);
        if next > target_samples {
            return Ok((source, Some(block), position, block_count));
        }
        position = next;
        block_count = block_count.saturating_add(1);
    }
    Ok((source, None, position, block_count))
}

struct ConfiguredProcessor {
    node_id: String,
    processor: Box<dyn Processor>,
}

struct ProcessingChain {
    processors: Vec<ConfiguredProcessor>,
    threshold_events: Vec<ConfiguredThresholdEvents>,
}

struct ConfiguredThresholdEvents {
    channel_index: usize,
    receiver: ThresholdEventReceiver,
}

impl ProcessingChain {
    #[cfg(test)]
    fn new(config: &PipelineConfig) -> Result<Self, String> {
        Self::new_for(config, RHS_SAMPLE_RATE_HZ, RHS_CHANNEL_COUNT)
    }

    fn new_for(
        config: &PipelineConfig,
        sample_rate_hz: u32,
        channel_count: usize,
    ) -> Result<Self, String> {
        config.validate_for(sample_rate_hz, channel_count)?;
        let mut processors = Vec::new();
        let mut threshold_events = Vec::new();
        for node in &config.nodes {
            if !node.enabled() {
                continue;
            }
            let (node_id, processor): (String, Box<dyn Processor>) = match node {
                PipelineNodeConfig::ArtifactBlanking {
                    id,
                    ttl_mask,
                    blank_samples,
                    replacement,
                    ..
                } => {
                    let replacement = match replacement {
                        BlankingReplacementConfig::HoldLast => BlankingReplacement::HoldLast,
                        BlankingReplacementConfig::Midpoint => BlankingReplacement::Midpoint,
                    };
                    let processor = ArtifactBlanker::new(BlankingConfig {
                        ttl_mask: *ttl_mask,
                        blank_samples_after_rising_edge: *blank_samples,
                        replacement,
                    })
                    .map_err(|error| format!("node {id}: {error}"))?;
                    (id.clone(), Box::new(processor))
                }
                PipelineNodeConfig::Notch {
                    id,
                    frequency_hz,
                    quality_factor,
                    ..
                } => {
                    let processor = NotchProcessor::new(*frequency_hz, *quality_factor)
                        .map_err(|error| format!("node {id}: {error}"))?;
                    (id.clone(), Box::new(processor))
                }
                PipelineNodeConfig::BandPass {
                    id,
                    low_cut_hz,
                    high_cut_hz,
                    ..
                } => {
                    let processor = BandPassProcessor::new(*low_cut_hz, *high_cut_hz)
                        .map_err(|error| format!("node {id}: {error}"))?;
                    (id.clone(), Box::new(processor))
                }
                PipelineNodeConfig::Threshold {
                    id,
                    channel_index,
                    threshold,
                    direction,
                    refractory_samples,
                    ..
                } => {
                    let direction = match direction {
                        ThresholdDirectionConfig::RisingAbove => ThresholdDirection::RisingAbove,
                        ThresholdDirectionConfig::FallingBelow => ThresholdDirection::FallingBelow,
                    };
                    let (processor, receiver) = ThresholdProcessor::new(ThresholdConfig {
                        channel_index: *channel_index,
                        threshold: *threshold,
                        direction,
                        refractory_samples: *refractory_samples,
                        event_queue_capacity: 64,
                    })
                    .map_err(|error| format!("node {id}: {error}"))?;
                    threshold_events.push(ConfiguredThresholdEvents {
                        channel_index: *channel_index,
                        receiver,
                    });
                    (id.clone(), Box::new(processor))
                }
            };
            processors.push(ConfiguredProcessor { node_id, processor });
        }
        Ok(Self {
            processors,
            threshold_events,
        })
    }

    fn process(&mut self, block: SampleBlock) -> Result<SampleBlock, String> {
        let mut current = Some(block);
        for configured in &mut self.processors {
            let Some(block) = current.take() else {
                return Err(format!(
                    "node {} ({}) removed the display block",
                    configured.node_id,
                    configured.processor.name()
                ));
            };
            current = configured
                .processor
                .process(block)
                .map_err(|error| format!("node {}: {error}", configured.node_id))?;
        }
        current.ok_or_else(|| "display processing returned no block".to_owned())
    }
}

fn run_playback(
    path: PathBuf,
    summary: PlaybackSummary,
    pipeline: PipelineConfig,
    commands: Receiver<PlaybackCommand>,
    shared: Arc<Mutex<DesktopShared>>,
) {
    let mut source = match FileReplaySource::open(&path) {
        Ok(source) => source,
        Err(error) => {
            if let Ok(mut state) = shared.lock() {
                state.set_fault(format!("could not open playback source: {error}"));
            }
            return;
        }
    };
    let mut processing = match ProcessingChain::new_for(
        &pipeline,
        summary.info.sample_rate_hz,
        summary.info.channels.len(),
    ) {
        Ok(processing) => processing,
        Err(error) => {
            if let Ok(mut state) = shared.lock() {
                state.set_fault(format!("could not build playback pipeline: {error}"));
            }
            return;
        }
    };
    let mut active_pipeline = pipeline;
    let mut pending_block = None::<SampleBlock>;
    let mut position_samples = 0_u64;
    let mut spike_analyzer = begin_spikes(&shared);
    let mut processed_blocks = 0_u64;
    let mut speed = 1.0_f64;
    let mut playing = false;
    let mut stop_requested = false;

    while !stop_requested {
        let command = if playing {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    stop_requested = true;
                    None
                }
            }
        } else {
            match commands.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    stop_requested = true;
                    None
                }
            }
        };

        if let Some(command) = command {
            match command {
                PlaybackCommand::Play => {
                    if position_samples >= summary.total_samples {
                        match seek_playback_source(&path, 0) {
                            Ok((next_source, next_block, position, blocks)) => {
                                source = next_source;
                                pending_block = next_block;
                                position_samples = position;
                                processed_blocks = blocks;
                                match ProcessingChain::new_for(
                                    &active_pipeline,
                                    summary.info.sample_rate_hz,
                                    summary.info.channels.len(),
                                ) {
                                    Ok(next) => {
                                        processing = next;
                                        restart_spike_segment(&mut spike_analyzer, &shared);
                                    }
                                    Err(error) => {
                                        if let Ok(mut state) = shared.lock() {
                                            state.set_fault(format!(
                                                "could not reset playback pipeline: {error}"
                                            ));
                                        }
                                        break;
                                    }
                                }
                            }
                            Err(error) => {
                                if let Ok(mut state) = shared.lock() {
                                    state.set_fault(format!("could not restart playback: {error}"));
                                }
                                break;
                            }
                        }
                    }
                    playing = true;
                    if let Ok(mut state) = shared.lock() {
                        state.mode = "playback";
                        state.health = "nominal";
                        state.last_error = None;
                        state.playback = Some(playback_view(
                            &path,
                            &summary,
                            position_samples,
                            speed,
                            "playing",
                        ));
                        state.push_event("info", "playback", "Playback started");
                    }
                }
                PlaybackCommand::Pause => {
                    playing = false;
                    if let Ok(mut state) = shared.lock() {
                        state.mode = "playback-paused";
                        state.playback = Some(playback_view(
                            &path,
                            &summary,
                            position_samples,
                            speed,
                            "paused",
                        ));
                        state.push_event("info", "playback", "Playback paused");
                    }
                }
                PlaybackCommand::SetSpeed(next_speed) => {
                    speed = next_speed;
                    let playback_state = if position_samples >= summary.total_samples {
                        "ended"
                    } else if playing {
                        "playing"
                    } else {
                        "paused"
                    };
                    if let Ok(mut state) = shared.lock() {
                        state.playback = Some(playback_view(
                            &path,
                            &summary,
                            position_samples,
                            speed,
                            playback_state,
                        ));
                    }
                }
                PlaybackCommand::Seek { fraction, reply } => {
                    let target = (summary.total_samples as f64 * fraction)
                        .round()
                        .clamp(0.0, summary.total_samples as f64)
                        as u64;
                    let result = seek_playback_source(&path, target).and_then(
                        |(next_source, next_block, position, blocks)| {
                            let next_processing = ProcessingChain::new_for(
                                &active_pipeline,
                                summary.info.sample_rate_hz,
                                summary.info.channels.len(),
                            )?;
                            source = next_source;
                            pending_block = next_block;
                            position_samples = position;
                            processed_blocks = blocks;
                            processing = next_processing;
                            restart_spike_segment(&mut spike_analyzer, &shared);
                            if position_samples >= summary.total_samples {
                                playing = false;
                            }
                            if let Ok(mut state) = shared.lock() {
                                state.metrics.blocks = processed_blocks;
                                state.metrics.samples_per_channel = position_samples;
                                state.metrics.bytes_read = position_samples
                                    .saturating_mul(summary.info.channels.len() as u64)
                                    .saturating_mul(2);
                                state.metrics.latest_sequence = 0;
                                state.metrics.observed_sample_rate_hz =
                                    summary.info.sample_rate_hz as f64;
                                state.clear_waveforms();
                                let playback_state = if position_samples >= summary.total_samples {
                                    "ended"
                                } else if playing {
                                    "playing"
                                } else {
                                    "paused"
                                };
                                state.mode = if playing {
                                    "playback"
                                } else {
                                    "playback-paused"
                                };
                                state.playback = Some(playback_view(
                                    &path,
                                    &summary,
                                    position_samples,
                                    speed,
                                    playback_state,
                                ));
                                state.push_event(
                                    "info",
                                    "playback",
                                    format!(
                                        "Playback positioned at {:.3} seconds",
                                        position_samples as f64
                                            / summary.info.sample_rate_hz.max(1) as f64
                                    ),
                                );
                            }
                            Ok(())
                        },
                    );
                    let _ = reply.send(result);
                }
                PlaybackCommand::UpdatePipeline { config, reply } => {
                    let result = ProcessingChain::new_for(
                        &config,
                        summary.info.sample_rate_hz,
                        summary.info.channels.len(),
                    )
                    .map(|next| {
                        processing = next;
                        restart_spike_segment(&mut spike_analyzer, &shared);
                        active_pipeline = config.clone();
                        if let Ok(mut state) = shared.lock() {
                            state.pipeline = config;
                            state.push_event(
                                "info",
                                "pipeline",
                                "Playback processing chain rebuilt",
                            );
                        }
                    });
                    let _ = reply.send(result);
                }
                PlaybackCommand::Stop => stop_requested = true,
            }
        }

        if stop_requested || !playing {
            continue;
        }

        let block = match pending_block.take() {
            Some(block) => Some(block),
            None => match source.next_block() {
                Ok(block) => block,
                Err(error) => {
                    if let Ok(mut state) = shared.lock() {
                        state.set_fault(format!("playback read failed: {error}"));
                        state.playback = Some(playback_view(
                            &path,
                            &summary,
                            position_samples,
                            speed,
                            "fault",
                        ));
                    }
                    break;
                }
            },
        };
        let Some(block) = block else {
            playing = false;
            position_samples = summary.total_samples;
            if let Ok(mut state) = shared.lock() {
                state.mode = "playback-paused";
                state.playback = Some(playback_view(
                    &path,
                    &summary,
                    position_samples,
                    speed,
                    "ended",
                ));
                state.push_event("success", "playback", "Playback reached the end");
            }
            continue;
        };

        let samples_in_block = block.samples_per_channel as u64;
        let baseline = extract_block_baseline(&block);
        let display = match processing.process(block) {
            Ok(display) => display,
            Err(error) => {
                if let Ok(mut state) = shared.lock() {
                    state.set_fault(format!("playback pipeline failed: {error}"));
                    state.playback = Some(playback_view(
                        &path,
                        &summary,
                        position_samples,
                        speed,
                        "fault",
                    ));
                }
                break;
            }
        };
        process_spikes(&mut spike_analyzer, &display, &shared);
        let frame = build_waveform_frame(&display, WAVEFORM_POINTS, baseline);
        position_samples = position_samples
            .saturating_add(samples_in_block)
            .min(summary.total_samples);
        processed_blocks = processed_blocks.saturating_add(1);
        if let Ok(mut state) = shared.lock() {
            state.metrics.blocks = processed_blocks;
            state.metrics.samples_per_channel = position_samples;
            state.metrics.bytes_read = position_samples
                .saturating_mul(summary.info.channels.len() as u64)
                .saturating_mul(2);
            state.metrics.latest_sequence = frame.sequence;
            state.metrics.observed_sample_rate_hz = summary.info.sample_rate_hz as f64;
            state.publish_waveform(frame);
            state.playback = Some(playback_view(
                &path,
                &summary,
                position_samples,
                speed,
                "playing",
            ));
        }
        drain_threshold_events(&processing, &shared);

        let block_seconds =
            samples_in_block as f64 / summary.info.sample_rate_hz.max(1) as f64 / speed.max(0.25);
        if block_seconds > 0.0 {
            thread::sleep(Duration::from_secs_f64(block_seconds.min(0.05)));
        }
    }
    finish_spikes(&mut spike_analyzer, &shared);
}

struct ActiveRecording {
    sink: RawRecorderSink,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerCapturePhase {
    Armed,
    Capturing,
    Complete,
}

struct TriggerCapture {
    request: TriggeredRecordingRequest,
    phase: TriggerCapturePhase,
    pre_trigger_blocks: VecDeque<SampleBlock>,
    buffered_samples: u64,
    active: Option<ActiveRecording>,
    trigger_sample_index: Option<u64>,
    post_samples_remaining: u64,
    captured_samples: u64,
    captured_blocks: u64,
    previous_ttl: u16,
    previous_analog: Option<u16>,
    software_trigger_pending: bool,
}

impl TriggerCapture {
    fn view(&self, state: &str) -> TriggeredRecordingView {
        TriggeredRecordingView {
            state: state.to_owned(),
            path: self.request.path.clone(),
            source: self.request.source,
            pre_trigger_ms: self.request.pre_trigger_ms,
            post_trigger_ms: self.request.post_trigger_ms,
            trigger_sample_index: self.trigger_sample_index,
            captured_samples_per_channel: self.captured_samples,
            captured_blocks: self.captured_blocks,
        }
    }

    fn state_name(&self) -> &'static str {
        if self.software_trigger_pending && self.phase == TriggerCapturePhase::Armed {
            "trigger-pending"
        } else {
            match self.phase {
                TriggerCapturePhase::Armed => "armed",
                TriggerCapturePhase::Capturing => "capturing",
                TriggerCapturePhase::Complete => "complete",
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ScheduledSoftwareStim {
    command_id: u64,
    target_sample_index: u64,
}

#[derive(Clone)]
struct SoftwareStimulationState {
    config: Option<StimulationConfigRequest>,
    pending: VecDeque<ScheduledSoftwareStim>,
    next_arm_id: u64,
    next_command_id: u64,
    view: StimulationView,
    arm_expires_at: Option<Instant>,
    audit: Option<Arc<Mutex<std::fs::File>>>,
    last_functional_submit_at: Option<Instant>,
    next_configurable_submit_at: Option<Instant>,
}

impl Default for SoftwareStimulationState {
    fn default() -> Self {
        Self {
            config: None,
            pending: VecDeque::new(),
            next_arm_id: 1,
            next_command_id: 1,
            view: StimulationView::default(),
            arm_expires_at: None,
            audit: None,
            last_functional_submit_at: None,
            next_configurable_submit_at: None,
        }
    }
}

fn run_session(
    handle: AcquisitionHandle,
    info: AcquisitionInfo,
    pipeline: PipelineConfig,
    commands: Receiver<SessionCommand>,
    shared: Arc<Mutex<DesktopShared>>,
    audit_directory: PathBuf,
) {
    let mut processing =
        match ProcessingChain::new_for(&pipeline, info.sample_rate_hz, info.channel_count) {
            Ok(processing) => processing,
            Err(error) => {
                let _ = handle.stop();
                if let Ok(mut state) = shared.lock() {
                    state.set_fault(format!("could not build display pipeline: {error}"));
                }
                return;
            }
        };
    let mut recorder = None::<ActiveRecording>;
    let mut spike_analyzer = begin_spikes(&shared);
    let mut triggered_recording = None::<TriggerCapture>;
    let mut stimulation = SoftwareStimulationState::default();
    let mut stop_requested = false;
    let mut terminal_fault = None::<String>;

    while !stop_requested {
        if let Err(error) = drain_stimulation_events(&handle, &mut stimulation, &shared) {
            terminal_fault = Some(error);
            break;
        }
        if stimulation
            .arm_expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let _ = handle.emergency_stop();
            if let Err(error) =
                write_stimulation_audit(&stimulation, &serde_json::json!({"event": "arm-expired"}))
            {
                terminal_fault = Some(error);
                break;
            }
            stimulation.arm_expires_at = None;
            stimulation.pending.clear();
            stimulation.config = None;
            stimulation.view.state = "expired".to_owned();
            stimulation.view.arm_id = None;
            stimulation.view.pending_commands = 0;
            if let Ok(mut state) = shared.lock() {
                state.stimulation = stimulation.view.clone();
                state.push_event(
                    "warning",
                    "stimulation",
                    "Hardware Arm expired; output disabled, restart acquisition before rearming",
                );
            }
        }
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::Stop) => {
                    let _ = handle.emergency_stop();
                    if let Err(error) = write_stimulation_audit(
                        &stimulation,
                        &serde_json::json!({"event": "acquisition-stop"}),
                    ) {
                        terminal_fault = Some(error);
                    }
                    stop_requested = true;
                    break;
                }
                Ok(SessionCommand::StartRecording {
                    path,
                    workspace,
                    reply,
                }) => {
                    let result = start_recorder(&mut recorder, &path, &info, &shared, workspace);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::StopRecording { reply }) => {
                    let result = finish_recorder(&mut recorder, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::ArmTriggeredRecording { request, reply }) => {
                    let result =
                        arm_trigger_capture(&mut triggered_recording, request, &info, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::FireSoftwareTrigger { reply }) => {
                    let result = request_software_trigger(&mut triggered_recording, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::CancelTriggeredRecording { reply }) => {
                    let result = cancel_trigger_capture(&mut triggered_recording, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::ArmStimulation { config, reply }) => {
                    if let Err(error) = validate_stimulation_arm_state(&stimulation) {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                    let output_mode = match config.requested_output_mode() {
                        Ok(mode) => mode,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                    let real_output = output_mode != "interlocked";
                    let mut audit = None;
                    let mut audit_path = None;
                    let readiness = if real_output {
                        validate_hardware_arm(
                            &config,
                            &info,
                            handle.configured_stimulation(),
                            handle.stimulation_interlocked(),
                        )
                        .and_then(|_| {
                            let (file, path) =
                                prepare_hardware_audit(&audit_directory, &config, &info)?;
                            audit = Some(file);
                            audit_path = Some(path);
                            Ok(())
                        })
                    } else {
                        Ok(())
                    };
                    let result = readiness.and_then(|_| arm_software_stimulation(
                        &mut stimulation,
                        config,
                        info.channel_count,
                        &shared,
                    )).map(|_| {
                        stimulation.audit = audit;
                        stimulation.view.audit_path = audit_path;
                        stimulation.arm_expires_at = None;
                        if real_output {
                            stimulation.view.output_mode = output_mode.to_owned();
                            stimulation.view.programmed_parameters = handle.configured_stimulation().map(programmed_stimulation_view);
                            stimulation.view.command_limit = (output_mode == "hardware-configurable").then_some(CONFIGURABLE_COMMAND_LIMIT);
                            stimulation.arm_expires_at = Some(Instant::now() + Duration::from_secs(60));
                            if let Ok(mut state) = shared.lock() {
                                state.stimulation = stimulation.view.clone();
                                state.push_event("warning", "stimulation", match output_mode {
                                    "hardware-functional" => "Single-channel no-bio hardware functional check armed; no current measurement; no trigger sent",
                                    "hardware-configurable" => "Configurable research output armed with operator-reviewed parameters; output is unverified; no trigger sent",
                                    _ => "Hardware stimulation armed for a confirmed dummy-load bench; no trigger sent",
                                });
                            }
                        }
                        stimulation.view.clone()
                    });
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::TriggerStimulation { schedule, reply }) => {
                    let previous = stimulation.clone();
                    let real_output = stimulation.view.hardware_output();
                    let mut configurable_interval = None;
                    let readiness = if real_output {
                        (if stimulation.view.output_mode == "hardware-functional" {
                            validate_functional_schedule(
                                &schedule,
                                stimulation.view.submitted_commands,
                                stimulation.view.functional_trigger_limit,
                                stimulation
                                    .last_functional_submit_at
                                    .map(|time| time.elapsed()),
                            )
                        } else {
                            Ok(())
                        })
                        .and_then(|_| {
                            validate_hardware_schedule(&schedule, handle.configured_stimulation())
                        })
                        .and_then(|_| {
                            validate_hardware_submission(
                                &stimulation,
                                &schedule,
                                handle.configured_stimulation(),
                                Instant::now(),
                            )
                            .map(|interval| configurable_interval = interval)
                        })
                    } else {
                        Ok(())
                    };
                    let result = readiness.and_then(|_| schedule_software_stimulation(
                        &mut stimulation,
                        schedule.clone(),
                        handle.next_sample_index(),
                        info.sample_rate_hz,
                        &shared,
                    )).and_then(|view| {
                        if real_output {
                            if let Err(error) = write_stimulation_audit(&stimulation, &serde_json::json!({"event": "schedule-submitted", "commands": stimulation.pending.iter().map(|command| (command.command_id, command.target_sample_index)).collect::<Vec<_>>()})) {
                                let _ = handle.emergency_stop();
                                terminal_fault = Some(error.clone());
                                stop_requested = true;
                                stimulation = previous;
                                if let Ok(mut state) = shared.lock() { state.stimulation = stimulation.view.clone(); }
                                return Err(error);
                            }
                            let requests = stimulation.pending.iter().map(|command| rhs_device::ScheduledStimRequest {
                                command_id: command.command_id,
                                target_sample_index: command.target_sample_index,
                                deadline_sample_index: command.target_sample_index.saturating_add(u64::from(info.sample_rate_hz)),
                            }).collect();
                            if let Err(error) = handle.schedule_stim_batch(requests) {
                                stimulation = previous;
                                if let Ok(mut state) = shared.lock() { state.stimulation = stimulation.view.clone(); }
                                return Err(error.to_string());
                            }
                            if stimulation.view.output_mode == "hardware-functional" {
                                stimulation.last_functional_submit_at = Some(Instant::now());
                            } else if stimulation.view.output_mode == "hardware-configurable" {
                                stimulation.next_configurable_submit_at = configurable_interval.map(|interval| Instant::now() + interval);
                            }
                        }
                        Ok(view)
                    });
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::CancelStimulation { reply }) => {
                    if stimulation.view.hardware_output() {
                        let _ = handle.emergency_stop();
                        if let Err(error) = write_stimulation_audit(
                            &stimulation,
                            &serde_json::json!({"event": "cancel-requested"}),
                        ) {
                            terminal_fault = Some(error.clone());
                            let _ = reply.send(Err(error));
                            stop_requested = true;
                            break;
                        }
                        stimulation.arm_expires_at = None;
                        stimulation.config = None;
                        stimulation.view.arm_id = None;
                    }
                    let result = cancel_software_stimulation(&mut stimulation, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::DisarmStimulation { reply }) => {
                    if stimulation.view.hardware_output() {
                        let _ = handle.emergency_stop();
                        if let Err(error) = write_stimulation_audit(
                            &stimulation,
                            &serde_json::json!({"event": "disarm-requested"}),
                        ) {
                            terminal_fault = Some(error.clone());
                            let _ = reply.send(Err(error));
                            stop_requested = true;
                            break;
                        }
                        stimulation.pending.clear();
                        stimulation.arm_expires_at = None;
                    }
                    let result = disarm_software_stimulation(&mut stimulation, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::UpdatePipeline { config, reply }) => {
                    let result =
                        ProcessingChain::new_for(&config, info.sample_rate_hz, info.channel_count)
                            .map(|next| {
                                processing = next;
                                restart_spike_segment(&mut spike_analyzer, &shared);
                                if let Ok(mut state) = shared.lock() {
                                    state.pipeline = config;
                                    state.push_event(
                                        "info",
                                        "pipeline",
                                        "Display processing chain rebuilt",
                                    );
                                }
                            });
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::EmergencyStop { reply }) => {
                    let result = handle
                        .emergency_stop()
                        .map_err(|error| error.to_string())
                        .and_then(|_| {
                            write_stimulation_audit(
                                &stimulation,
                                &serde_json::json!({"event": "emergency-stop-requested"}),
                            )
                        });
                    stimulation.pending.clear();
                    stimulation.config = None;
                    stimulation.view = StimulationView::default();
                    stimulation.view.state = "emergency-stopped".to_owned();
                    if let Ok(mut state) = shared.lock() {
                        state.stimulation = stimulation.view.clone();
                        state.push_event(
                            "critical",
                            "stimulation",
                            "Emergency stop sent; all stimulation remains disabled",
                        );
                    }
                    let _ = reply.send(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stop_requested = true;
                    break;
                }
            }
        }
        if stop_requested {
            break;
        }

        match handle.recv_timeout(Duration::from_millis(80)) {
            Ok(block) => {
                if let Some(active) = recorder.as_mut() {
                    if let Err(error) = active.sink.consume(&block) {
                        terminal_fault = Some(format!("recording failed: {error}"));
                        break;
                    }
                }
                if let Err(error) =
                    process_trigger_capture(&mut triggered_recording, &block, &info, &shared)
                {
                    terminal_fault = Some(format!("triggered recording failed: {error}"));
                    break;
                }
                if !stimulation.view.hardware_output() {
                    process_software_stimulation(&mut stimulation, &block, &shared);
                }
                update_io_inputs(&block, &shared);
                let baseline = extract_block_baseline(&block);
                let display = match processing.process(block) {
                    Ok(block) => block,
                    Err(error) => {
                        terminal_fault = Some(format!("display pipeline failed: {error}"));
                        break;
                    }
                };
                process_spikes(&mut spike_analyzer, &display, &shared);
                let frame = build_waveform_frame(&display, WAVEFORM_POINTS, baseline);
                let stats = handle.stats();
                if let Ok(mut state) = shared.lock() {
                    state.metrics.blocks = stats.batches;
                    state.metrics.samples_per_channel = stats.samples_per_channel;
                    state.metrics.bytes_read = stats.bytes_read;
                    state.metrics.timestamp_gaps = stats.timestamp_gaps;
                    state.metrics.empty_reads = stats.empty_reads;
                    state.metrics.queue_overflows = stats.queue_overflows;
                    state.metrics.latest_sequence = frame.sequence;
                    state.metrics.observed_sample_rate_hz = state
                        .session_started
                        .map(|started| {
                            stats.samples_per_channel as f64
                                / started.elapsed().as_secs_f64().max(0.001)
                        })
                        .unwrap_or(0.0);
                    state.publish_waveform(frame);
                }
                drain_threshold_events(&processing, &shared);
                if let Err(error) = drain_stimulation_events(&handle, &mut stimulation, &shared) {
                    terminal_fault = Some(error);
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(error) = handle.try_fault() {
                    terminal_fault = Some(error.to_string());
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(error) = handle.try_fault() {
                    terminal_fault = Some(error.to_string());
                } else {
                    terminal_fault = Some("acquisition worker disconnected".to_owned());
                }
                break;
            }
        }
    }

    finish_spikes(&mut spike_analyzer, &shared);
    let _ = handle.emergency_stop();
    if let Err(error) = drain_stimulation_events(&handle, &mut stimulation, &shared) {
        terminal_fault.get_or_insert(error);
    }
    if recorder.is_some() {
        if let Err(error) = finish_recorder(&mut recorder, &shared) {
            terminal_fault.get_or_insert_with(|| format!("recording finalize failed: {error}"));
        }
    }
    if triggered_recording.is_some() {
        if let Err(error) = cancel_trigger_capture(&mut triggered_recording, &shared) {
            terminal_fault
                .get_or_insert_with(|| format!("triggered recording finalize failed: {error}"));
        }
    }
    let final_stats = handle.stats();
    let stop_result = handle.stop();
    if let Err(error) = stop_result {
        if terminal_fault.as_deref() == Some("acquisition worker disconnected") {
            terminal_fault = Some(error.to_string());
        } else {
            terminal_fault.get_or_insert_with(|| error.to_string());
        }
    }
    if let Err(error) = write_stimulation_audit(
        &stimulation,
        &serde_json::json!({"event": "session-ended", "fault": terminal_fault}),
    ) {
        terminal_fault.get_or_insert(error);
    }
    stimulation.pending.clear();
    stimulation.config = None;
    finish_stimulation_session(&mut stimulation, terminal_fault.is_some());
    if let Ok(mut state) = shared.lock() {
        state.metrics.timestamp_gaps = final_stats.timestamp_gaps;
        state.metrics.queue_overflows = final_stats.queue_overflows;
        state.metrics.empty_reads = final_stats.empty_reads;
        state.recording = false;
        state.recording_path = None;
        state.stimulation = stimulation.view.clone();
        if let Some(error) = terminal_fault {
            state.set_fault(error);
        } else {
            state.mode = "ready";
            state.health = "nominal";
            state.push_event("info", "acquisition", "Acquisition stopped cleanly");
        }
    }
}

fn finish_stimulation_session(stimulation: &mut SoftwareStimulationState, faulted: bool) {
    if faulted {
        stimulation.view.state = "fault".to_owned();
        stimulation.view.output_mode = "interlocked".to_owned();
        stimulation.view.arm_id = None;
        stimulation.view.pending_commands = 0;
    } else {
        stimulation.view = StimulationView::default();
    }
}

fn update_io_inputs(block: &SampleBlock, shared: &Arc<Mutex<DesktopShared>>) {
    if block.ttl_in.is_empty() {
        return;
    }
    if let Ok(mut state) = shared.lock() {
        let mut previous = state.io.digital_input;
        let mut edges = 0_u64;
        for value in block.ttl_in.iter().copied() {
            edges = edges.saturating_add((previous ^ value).count_ones() as u64);
            previous = value;
        }
        state.io.digital_input = previous;
        state.io.digital_edges = state.io.digital_edges.saturating_add(edges);
    }
}

fn validate_stimulation_config(config: &StimulationConfigRequest) -> Result<(), String> {
    if config.profile_id.trim().is_empty() {
        return Err("stimulation profile id must not be empty".to_owned());
    }
    if config.channel_ids.is_empty() || config.channel_ids.len() > 120 {
        return Err("stimulation must select between 1 and 120 channels".to_owned());
    }
    let unique_channels: HashSet<_> = config.channel_ids.iter().collect();
    if unique_channels.len() != config.channel_ids.len()
        || config
            .channel_ids
            .iter()
            .any(|channel| channel.trim().is_empty())
    {
        return Err("stimulation channel ids must be unique and non-empty".to_owned());
    }
    if config.profile_id == "rhs2116-logic-16" {
        for channel_id in &config.channel_ids {
            rhs2116_hardware_channel(channel_id)?;
        }
    }
    if !matches!(
        config.waveform.as_str(),
        "biphasic" | "biphasic-delay" | "triphasic"
    ) {
        return Err("unsupported stimulation waveform".to_owned());
    }
    if !(1..=10_000).contains(&config.amplitude_na) {
        return Err("stimulation amplitude must be between 1 and 10000 nA".to_owned());
    }
    if !(1..=10_000).contains(&config.phase_width_us) {
        return Err("stimulation phase width must be between 1 and 10000 us".to_owned());
    }
    if config.interphase_us > 10_000 {
        return Err("stimulation interphase duration must not exceed 10000 us".to_owned());
    }
    if !config.frequency_hz.is_finite() || !(0.1..=1_000.0).contains(&config.frequency_hz) {
        return Err("stimulation frequency must be between 0.1 and 1000 Hz".to_owned());
    }
    if !(1..=10_000).contains(&config.pulse_count) {
        return Err("stimulation pulse count must be between 1 and 10000".to_owned());
    }
    let period_us = 1_000_000.0 / config.frequency_hz;
    let occupied_us = config.phase_width_us as f64 * 2.0 + config.interphase_us as f64;
    if occupied_us > period_us {
        return Err("stimulation phases do not fit inside one pulse period".to_owned());
    }
    if !config.charge_recovery {
        return Err("balanced biphasic output must remain enabled".to_owned());
    }
    Ok(())
}

fn rhs2116_hardware_channel(channel_id: &str) -> Result<u8, String> {
    let number = channel_id
        .strip_prefix("CH-")
        .filter(|digits| digits.len() == 2 && digits.bytes().all(|digit| digit.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u8>().ok())
        .ok_or_else(|| format!("invalid RHS2116 electrode id: {channel_id}"))?;
    if !(1..=16).contains(&number) || format!("CH-{number:02}") != channel_id {
        return Err(format!("invalid RHS2116 electrode id: {channel_id}"));
    }
    Ok(number - 1)
}

fn rhs2116_hardware_stimulation(config: &StimulationConfigRequest) -> Result<StimConfig, String> {
    validate_stimulation_config(config)?;
    if !matches!(config.profile_id.as_str(), "rhs2116-logic-16" | "mea-ab-64") {
        return Err("hardware stimulation supports RHS2116 16 and MEA 64 profiles".to_owned());
    }
    if !matches!(config.waveform.as_str(), "biphasic" | "biphasic-delay") {
        return Err(
            "hardware output currently requires balanced symmetric biphasic pulses".to_owned(),
        );
    }
    if config.waveform == "biphasic-delay" && config.interphase_us == 0 {
        return Err("biphasic-with-delay output requires a nonzero interphase interval".to_owned());
    }
    let (step_size, magnitude) = [
        StimStepSize::N10,
        StimStepSize::N20,
        StimStepSize::N50,
        StimStepSize::N100,
    ]
    .into_iter()
    .find_map(|step| {
        let unit = step.current_na();
        let steps = config.amplitude_na / unit;
        (config.amplitude_na.is_multiple_of(unit) && (1..=255).contains(&steps))
            .then_some((step, steps))
    })
    .ok_or("amplitude cannot be represented exactly by the RHS2116 current DAC")?;
    let mut channel_mask = 0_u16;
    let mut stream_masks = [0_u16; 8];
    for channel_id in &config.channel_ids {
        if config.profile_id == "rhs2116-logic-16" {
            channel_mask |= 1_u16 << rhs2116_hardware_channel(channel_id)?;
        } else {
            let (stream, channel) = mea64_hardware_address(channel_id)?;
            stream_masks[stream] |= 1_u16 << channel;
        }
    }
    if channel_mask == 0 {
        channel_mask = *stream_masks
            .iter()
            .find(|mask| **mask != 0)
            .ok_or("no stimulation channels selected")?;
    }
    let hardware = StimConfig {
        channel: channel_mask.trailing_zeros() as i32,
        channel_mask,
        stream_masks,
        trigger: 0,
        pulses: config.pulse_count as i32,
        magnitude_steps: magnitude as i32,
        stim_step_size: step_size,
        phase_steps: (f64::from(config.phase_width_us) * 0.03).round().max(1.0) as i32,
        interphase_steps: if config.interphase_us == 0 {
            0
        } else {
            (f64::from(config.interphase_us) * 0.03).round().max(1.0) as i32
        },
        period_steps: (30_000.0 / config.frequency_hz).round() as i32,
    };
    hardware.validate().map_err(|error| error.to_string())?;
    Ok(hardware)
}

fn mea64_hardware_address(id: &str) -> Result<(usize, u8), String> {
    let bytes = id.as_bytes();
    if bytes.len() != 5 || bytes[1] != b'-' || !bytes[2..].iter().all(u8::is_ascii_digit) {
        return Err("MEA 64 channel must be B-000..031 or C-000..031".to_owned());
    }
    let index: usize = id[2..].parse().map_err(|_| "invalid MEA 64 channel")?;
    let base = match bytes[0] {
        b'B' => 2,
        b'C' => 4,
        _ => return Err("MEA 64 requires B/C ports".to_owned()),
    };
    if index >= 32 {
        return Err("MEA 64 port channel is out of range".to_owned());
    }
    Ok((base + index / 16, (index % 16) as u8))
}

fn validate_hardware_arm(
    config: &StimulationConfigRequest,
    info: &AcquisitionInfo,
    preloaded: Option<&StimConfig>,
    interlocked: bool,
) -> Result<(), String> {
    if interlocked {
        return Err(
            "emergency stop is latched; restart acquisition before hardware Arm".to_owned(),
        );
    }
    let mode = config.requested_output_mode()?;
    let hardware = rhs2116_hardware_stimulation(config)?;
    if let Some(confirmation) = &config.output_confirmation {
        if !valid_stimulation_operator(&confirmation.operator)
            || !confirmation.routing_confirmed
            || !confirmation.parameters_confirmed
            || !confirmation.unverified_output_acknowledged
        {
            return Err("configurable output requires an operator, confirmed routing and parameters, and explicit acknowledgement that output has not been measured or qualified for biological samples".to_owned());
        }
    } else if let Some(confirmation) = &config.bench_confirmation {
        if !confirmation.sample_disconnected || !valid_stimulation_operator(&confirmation.operator)
        {
            return Err(
                "hardware bench Arm requires disconnected biological samples and an operator"
                    .to_owned(),
            );
        }
        if confirmation.functional_repeat && !confirmation.functional_only {
            return Err(
                "repeated functional validation requires the no-biological-sample functional mode"
                    .to_owned(),
            );
        }
        if confirmation.functional_only {
            if config.channel_ids.len() != 1
                || hardware.pulses != 1
                || hardware.magnitude_steps != 1
                || hardware.stim_step_size != StimStepSize::N10
                || hardware.phase_steps != 6
                || !matches!(hardware.interphase_steps, 0 | 3)
                || hardware.period_steps != 3000
            {
                return Err("no-bio functional check requires one channel, one 10 nA symmetric 200 us/phase pulse, 0 or 100 us gap and 10 Hz period setting".to_owned());
            }
        } else if !confirmation.dummy_load_connected || !confirmation.measurement_ready {
            return Err(
                "measured-bench mode requires connected dummy load and measurement readiness"
                    .to_owned(),
            );
        }
    } else if mode == "interlocked" {
        return Err("hardware output requires an explicit output confirmation".to_owned());
    }
    if preloaded != Some(&hardware) {
        return Err("stimulation selection or parameters differ from the preloaded configuration; stop and restart acquisition to apply them".to_owned());
    }
    if config.profile_id == "mea-ab-64"
        && (info.channel_count != 64 || info.streams != [2, 3, 4, 5])
    {
        return Err("MEA 64 requires the verified B2/B3/C4/C5 acquisition layout".to_owned());
    }
    if config.profile_id == "rhs2116-logic-16" && info.channel_count != 16 {
        return Err("RHS2116 profile requires a single 16-channel session".to_owned());
    }
    Ok(())
}

fn valid_stimulation_operator(operator: &str) -> bool {
    !operator.trim().is_empty()
        && operator.chars().count() <= 80
        && !operator.chars().any(char::is_control)
}

fn hardware_sequence_occupied_ms(preset: &StimConfig) -> Result<u64, String> {
    let period = u64::try_from(preset.period_steps).map_err(|_| "invalid hardware period")?;
    let pulses = u64::try_from(preset.pulses).map_err(|_| "invalid hardware pulse count")?;
    period
        .checked_mul(pulses)
        .and_then(|value| value.checked_add(256))
        .map(|steps| steps.div_ceil(30))
        .ok_or_else(|| "hardware timing overflow".to_owned())
}

fn hardware_schedule_span_ms(
    schedule: &StimulationScheduleRequest,
    preset: &StimConfig,
) -> Result<u64, String> {
    let occupied_ms = hardware_sequence_occupied_ms(preset)?;
    schedule
        .repeat_interval_ms
        .checked_mul(u64::from(schedule.repeat_count.saturating_sub(1)))
        .and_then(|value| value.checked_add(schedule.delay_ms))
        .and_then(|value| value.checked_add(occupied_ms))
        .ok_or_else(|| "hardware scheduling duration overflow".to_owned())
}

fn validate_hardware_schedule(
    schedule: &StimulationScheduleRequest,
    preset: Option<&StimConfig>,
) -> Result<(), String> {
    if !(1..=16).contains(&schedule.repeat_count) {
        return Err("hardware scheduling supports 1..16 finite commands per submission".to_owned());
    }
    let preset = preset.ok_or("no preloaded hardware preset")?;
    let occupied_ms = hardware_sequence_occupied_ms(preset)?;
    if schedule.repeat_count > 1 && schedule.repeat_interval_ms < occupied_ms + 100 {
        return Err(format!(
            "repeat interval must be at least {} ms for this hardware sequence",
            occupied_ms + 100
        ));
    }
    if hardware_schedule_span_ms(schedule, preset)? >= 60_000 {
        return Err("hardware schedule must fit inside the 60-second Arm lifetime".to_owned());
    }
    Ok(())
}

fn validate_hardware_submission(
    state: &SoftwareStimulationState,
    schedule: &StimulationScheduleRequest,
    preset: Option<&StimConfig>,
    now: Instant,
) -> Result<Option<Duration>, String> {
    let preset = preset.ok_or("no preloaded hardware preset")?;
    let span = hardware_schedule_span_ms(schedule, preset)?;
    let remaining = state
        .arm_expires_at
        .ok_or("hardware output is not armed")?
        .saturating_duration_since(now);
    if Duration::from_millis(span).saturating_add(Duration::from_millis(100)) >= remaining {
        return Err(
            "hardware schedule plus stop margin exceeds the remaining Arm lifetime".to_owned(),
        );
    }
    if state.view.output_mode == "hardware-configurable" {
        if state
            .view
            .submitted_commands
            .saturating_add(u64::from(schedule.repeat_count))
            > CONFIGURABLE_COMMAND_LIMIT
        {
            return Err("configurable output permits at most 16 finite commands per Arm; disarm and restart acquisition to begin another session".to_owned());
        }
        if state
            .next_configurable_submit_at
            .is_some_and(|next| now < next)
        {
            return Err("wait for the previous hardware sequence and its refractory interval before triggering again".to_owned());
        }
        return Ok(Some(
            Duration::from_millis(span).saturating_add(Duration::from_millis(100)),
        ));
    }
    Ok(None)
}

fn validate_functional_schedule(
    schedule: &StimulationScheduleRequest,
    submitted: u64,
    limit: u64,
    since_last_submit: Option<Duration>,
) -> Result<(), String> {
    if !matches!(limit, 1 | 3)
        || submitted >= limit
        || schedule.repeat_count != 1
        || schedule.delay_ms > 1000
    {
        return Err(
            "functional check submission budget reached or invalid request; only individual triggers and at most 1000 ms delay are allowed"
                .to_owned(),
        );
    }
    if since_last_submit.is_some_and(|elapsed| elapsed < Duration::from_secs(1)) {
        return Err("wait at least one second between functional triggers".to_owned());
    }
    Ok(())
}

fn validate_stimulation_arm_state(state: &SoftwareStimulationState) -> Result<(), String> {
    if state.view.arm_id.is_some() || !state.pending.is_empty() {
        return Err("stimulation is already armed; disarm before arming again (hardware output requires restarting acquisition after disarm)".to_owned());
    }
    Ok(())
}

fn arm_software_stimulation(
    state: &mut SoftwareStimulationState,
    config: StimulationConfigRequest,
    available_channels: usize,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<StimulationView, String> {
    validate_stimulation_arm_state(state)?;
    validate_stimulation_config(&config)?;
    let required_channels = match config.profile_id.as_str() {
        "rhs2116-logic-16" => 16,
        "mea-ab-64" => 64,
        "mea-abcd-120" => 120,
        _ => return Err("unsupported stimulation MEA profile".to_owned()),
    };
    if available_channels < required_channels {
        return Err(format!(
            "{} requires {required_channels} physical channels; this acquisition has {available_channels}",
            config.profile_id
        ));
    }
    if !state.pending.is_empty() {
        return Err("cancel pending stimulation commands before arming a new config".to_owned());
    }
    let arm_id = state.next_arm_id;
    state.next_arm_id = state.next_arm_id.saturating_add(1);
    state.config = Some(config.clone());
    state.last_functional_submit_at = None;
    state.next_configurable_submit_at = None;
    state.view = StimulationView {
        state: "armed".to_owned(),
        output_mode: "interlocked".to_owned(),
        arm_id: Some(arm_id),
        selected_channels: config.channel_ids.len(),
        pending_commands: 0,
        completed_commands: 0,
        submitted_commands: 0,
        functional_trigger_limit: if config
            .bench_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.functional_repeat)
        {
            3
        } else {
            1
        },
        command_limit: None,
        programmed_parameters: None,
        last_command_id: None,
        last_target_sample_index: None,
        audit_path: None,
    };
    if let Ok(mut shared) = shared.lock() {
        shared.stimulation = state.view.clone();
        shared.push_event(
            "warning",
            "stimulation",
            format!(
                "Stimulation config armed for {} channels; no trigger sent",
                config.channel_ids.len()
            ),
        );
    }
    Ok(state.view.clone())
}

fn schedule_software_stimulation(
    state: &mut SoftwareStimulationState,
    schedule: StimulationScheduleRequest,
    current_sample_index: u64,
    sample_rate_hz: u32,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<StimulationView, String> {
    if state.config.is_none() || state.view.arm_id.is_none() {
        return Err("stimulation must be armed before triggering".to_owned());
    }
    if !state.pending.is_empty() {
        return Err("a stimulation schedule is already pending".to_owned());
    }
    if !(1..=1_000).contains(&schedule.repeat_count) {
        return Err("stimulation repeat count must be between 1 and 1000".to_owned());
    }
    if schedule.repeat_count > 1 && schedule.repeat_interval_ms == 0 {
        return Err("repeat interval must be greater than zero".to_owned());
    }
    if schedule.delay_ms > 3_600_000 || schedule.repeat_interval_ms > 3_600_000 {
        return Err("stimulation delay and repeat interval must not exceed one hour".to_owned());
    }
    let first_target = current_sample_index
        .saturating_add(schedule.delay_ms.saturating_mul(sample_rate_hz as u64) / 1_000);
    let interval_samples = schedule
        .repeat_interval_ms
        .saturating_mul(sample_rate_hz as u64)
        / 1_000;
    for repeat in 0..schedule.repeat_count as u64 {
        let command_id = state.next_command_id;
        state.next_command_id = state.next_command_id.saturating_add(1);
        state.pending.push_back(ScheduledSoftwareStim {
            command_id,
            target_sample_index: first_target
                .saturating_add(repeat.saturating_mul(interval_samples)),
        });
    }
    state.view.state = "scheduled".to_owned();
    state.view.submitted_commands = state
        .view
        .submitted_commands
        .saturating_add(u64::from(schedule.repeat_count));
    state.view.pending_commands = state.pending.len();
    state.view.last_target_sample_index = state
        .pending
        .back()
        .map(|command| command.target_sample_index);
    if let Ok(mut shared) = shared.lock() {
        shared.stimulation = state.view.clone();
        shared.push_event_details(
            "warning",
            "stimulation",
            format!(
                "{} {} stimulation command(s) scheduled",
                schedule.repeat_count,
                if state.view.hardware_output() {
                    state.view.output_mode.as_str()
                } else {
                    "interlocked"
                }
            ),
            Some(first_target),
            None,
        );
    }
    Ok(state.view.clone())
}

fn process_software_stimulation(
    state: &mut SoftwareStimulationState,
    block: &SampleBlock,
    shared: &Arc<Mutex<DesktopShared>>,
) {
    let block_end = block.last_sample_index();
    while state
        .pending
        .front()
        .is_some_and(|command| command.target_sample_index <= block_end)
    {
        let command = state
            .pending
            .pop_front()
            .expect("pending stimulation command was checked");
        state.view.completed_commands = state.view.completed_commands.saturating_add(1);
        state.view.last_command_id = Some(command.command_id);
        state.view.last_target_sample_index = Some(command.target_sample_index);
        state.view.pending_commands = state.pending.len();
        state.view.state = if state.pending.is_empty() {
            "completed".to_owned()
        } else {
            "scheduled".to_owned()
        };
        if let Ok(mut shared) = shared.lock() {
            shared.stimulation = state.view.clone();
            shared.push_event_details(
                "event",
                "stimulation",
                format!(
                    "Interlocked command {} reached its target; no hardware pulse was emitted",
                    command.command_id
                ),
                Some(command.target_sample_index),
                None,
            );
        }
    }
}

fn cancel_software_stimulation(
    state: &mut SoftwareStimulationState,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<StimulationView, String> {
    if state.pending.is_empty() {
        return Err("no stimulation commands are pending".to_owned());
    }
    let cancelled = state.pending.len();
    state.pending.clear();
    state.view.pending_commands = 0;
    state.view.state = "cancelled".to_owned();
    if let Ok(mut shared) = shared.lock() {
        shared.stimulation = state.view.clone();
        shared.push_event(
            "warning",
            "stimulation",
            format!("Cancelled {cancelled} interlocked stimulation command(s)"),
        );
    }
    Ok(state.view.clone())
}

fn disarm_software_stimulation(
    state: &mut SoftwareStimulationState,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<StimulationView, String> {
    if !state.pending.is_empty() {
        return Err("cancel pending stimulation commands before disarming".to_owned());
    }
    state.config = None;
    state.view = StimulationView::default();
    if let Ok(mut shared) = shared.lock() {
        shared.stimulation = state.view.clone();
        shared.push_event("info", "stimulation", "Stimulation config disarmed");
    }
    Ok(state.view.clone())
}

fn validate_triggered_recording_request(
    request: &TriggeredRecordingRequest,
    info: &AcquisitionInfo,
) -> Result<(), String> {
    let path = Path::new(request.path.trim());
    if request.path.trim().is_empty() {
        return Err("triggered recording path must not be empty".to_owned());
    }
    if path.extension().and_then(|value| value.to_str()) != Some("rhsraw") {
        return Err("triggered recording path must use the .rhsraw extension".to_owned());
    }
    if path.exists() {
        return Err("triggered recording path already exists".to_owned());
    }
    if request.pre_trigger_ms > 30_000 {
        return Err("pre-trigger duration must not exceed 30000 ms".to_owned());
    }
    if !(1..=3_600_000).contains(&request.post_trigger_ms) {
        return Err("post-trigger duration must be between 1 and 3600000 ms".to_owned());
    }
    match request.source {
        RecordingTriggerSource::Software => {}
        RecordingTriggerSource::Digital if request.digital_mask == 0 => {
            return Err("digital trigger mask must not be zero".to_owned());
        }
        RecordingTriggerSource::Digital => {}
        RecordingTriggerSource::Analog if request.analog_channel_index >= info.channel_count => {
            return Err(format!(
                "analog trigger channel must be between 0 and {}",
                info.channel_count.saturating_sub(1)
            ));
        }
        RecordingTriggerSource::Analog => {}
    }
    Ok(())
}

fn arm_trigger_capture(
    capture: &mut Option<TriggerCapture>,
    request: TriggeredRecordingRequest,
    info: &AcquisitionInfo,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<TriggeredRecordingView, String> {
    validate_triggered_recording_request(&request, info)?;
    recording_configuration_entries(shared, info, request.workspace.clone())?;
    if capture
        .as_ref()
        .is_some_and(|active| active.phase != TriggerCapturePhase::Complete)
    {
        return Err("a triggered recording is already armed or capturing".to_owned());
    }
    let next = TriggerCapture {
        request,
        phase: TriggerCapturePhase::Armed,
        pre_trigger_blocks: VecDeque::new(),
        buffered_samples: 0,
        active: None,
        trigger_sample_index: None,
        post_samples_remaining: 0,
        captured_samples: 0,
        captured_blocks: 0,
        previous_ttl: 0,
        previous_analog: None,
        software_trigger_pending: false,
    };
    let view = next.view("armed");
    if let Ok(mut state) = shared.lock() {
        state.triggered_recording = Some(view.clone());
        state.push_event(
            "info",
            "trigger-recording",
            format!(
                "Triggered recording armed ({})",
                next.request.source.label()
            ),
        );
    }
    *capture = Some(next);
    Ok(view)
}

fn request_software_trigger(
    capture: &mut Option<TriggerCapture>,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<TriggeredRecordingView, String> {
    let capture = capture
        .as_mut()
        .ok_or_else(|| "no triggered recording is armed".to_owned())?;
    if capture.phase != TriggerCapturePhase::Armed {
        return Err("triggered recording is not waiting for a trigger".to_owned());
    }
    if capture.request.source != RecordingTriggerSource::Software {
        return Err("the armed recording is waiting for a hardware trigger".to_owned());
    }
    capture.software_trigger_pending = true;
    let view = capture.view("trigger-pending");
    if let Ok(mut state) = shared.lock() {
        state.triggered_recording = Some(view.clone());
        state.push_event(
            "info",
            "trigger-recording",
            "Software trigger queued for the next sample block",
        );
    }
    Ok(view)
}

fn cancel_trigger_capture(
    capture: &mut Option<TriggerCapture>,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<TriggeredRecordingView, String> {
    let mut current = capture
        .take()
        .ok_or_else(|| "no triggered recording is active".to_owned())?;
    if let Some(active) = current.active.as_mut() {
        active.sink.finalize().map_err(|error| error.to_string())?;
        current.captured_blocks = active.sink.block_count();
    }
    let view = current.view("cancelled");
    if let Ok(mut state) = shared.lock() {
        state.triggered_recording = Some(view.clone());
        state.push_event(
            "warning",
            "trigger-recording",
            "Triggered recording cancelled",
        );
    }
    Ok(view)
}

fn process_trigger_capture(
    capture: &mut Option<TriggerCapture>,
    block: &SampleBlock,
    info: &AcquisitionInfo,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<(), String> {
    let Some(current) = capture.as_mut() else {
        return Ok(());
    };
    match current.phase {
        TriggerCapturePhase::Complete => return Ok(()),
        TriggerCapturePhase::Capturing => {
            let active = current
                .active
                .as_mut()
                .ok_or_else(|| "triggered recorder is missing its output sink".to_owned())?;
            active
                .sink
                .consume(block)
                .map_err(|error| error.to_string())?;
            current.captured_blocks = active.sink.block_count();
            current.captured_samples = current
                .captured_samples
                .saturating_add(block.samples_per_channel as u64);
            current.post_samples_remaining = current
                .post_samples_remaining
                .saturating_sub(block.samples_per_channel as u64);
            if current.post_samples_remaining == 0 {
                finish_trigger_capture(current, shared)?;
            } else if let Ok(mut state) = shared.lock() {
                state.triggered_recording = Some(current.view("capturing"));
            }
            return Ok(());
        }
        TriggerCapturePhase::Armed => {}
    }

    let trigger_offset = detect_recording_trigger(current, block);
    if let Some(offset) = trigger_offset {
        start_trigger_capture(current, block, offset, info, shared)?;
    } else {
        buffer_pre_trigger_block(current, block, info.sample_rate_hz);
        if let Ok(mut state) = shared.lock() {
            state.triggered_recording = Some(current.view(current.state_name()));
        }
    }
    Ok(())
}

fn detect_recording_trigger(capture: &mut TriggerCapture, block: &SampleBlock) -> Option<usize> {
    match capture.request.source {
        RecordingTriggerSource::Software => {
            if capture.software_trigger_pending {
                capture.software_trigger_pending = false;
                Some(0)
            } else {
                None
            }
        }
        RecordingTriggerSource::Digital => {
            let mask = capture.request.digital_mask;
            let mut previous = capture.previous_ttl;
            let mut trigger = None;
            for (index, value) in block.ttl_in.iter().copied().enumerate() {
                if trigger.is_none() && previous & mask == 0 && value & mask != 0 {
                    trigger = Some(index);
                }
                previous = value;
            }
            capture.previous_ttl = previous;
            trigger
        }
        RecordingTriggerSource::Analog => {
            let channel = capture.request.analog_channel_index;
            let threshold = capture.request.analog_threshold;
            let channel_count = block.channel_count();
            let mut previous = capture.previous_analog;
            let mut trigger = None;
            for sample in 0..block.samples_per_channel as usize {
                let value = block.samples[sample * channel_count + channel];
                let crossed =
                    previous.is_some_and(|before| match capture.request.analog_direction {
                        ThresholdDirectionConfig::RisingAbove => {
                            before < threshold && value >= threshold
                        }
                        ThresholdDirectionConfig::FallingBelow => {
                            before > threshold && value <= threshold
                        }
                    });
                if crossed && trigger.is_none() {
                    trigger = Some(sample);
                }
                previous = Some(value);
            }
            capture.previous_analog = previous;
            trigger
        }
    }
}

fn buffer_pre_trigger_block(
    capture: &mut TriggerCapture,
    block: &SampleBlock,
    sample_rate_hz: u32,
) {
    let target_samples = capture
        .request
        .pre_trigger_ms
        .saturating_mul(sample_rate_hz as u64)
        / 1_000;
    if target_samples == 0 {
        capture.pre_trigger_blocks.clear();
        capture.buffered_samples = 0;
        return;
    }
    capture.buffered_samples = capture
        .buffered_samples
        .saturating_add(block.samples_per_channel as u64);
    capture.pre_trigger_blocks.push_back(block.clone());
    while capture.pre_trigger_blocks.len() > 1 {
        let front_samples = capture
            .pre_trigger_blocks
            .front()
            .map_or(0, |front| front.samples_per_channel as u64);
        if capture.buffered_samples.saturating_sub(front_samples) < target_samples {
            break;
        }
        capture.pre_trigger_blocks.pop_front();
        capture.buffered_samples = capture.buffered_samples.saturating_sub(front_samples);
    }
}

fn start_trigger_capture(
    capture: &mut TriggerCapture,
    block: &SampleBlock,
    trigger_offset: usize,
    info: &AcquisitionInfo,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<(), String> {
    let path = PathBuf::from(capture.request.path.trim());
    let session_metadata = shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?
        .session_metadata
        .recording_entries();
    let mut recording_metadata = vec![
        ("recording_mode".to_owned(), "triggered".to_owned()),
        (
            "trigger_source".to_owned(),
            capture.request.source.label().to_owned(),
        ),
        (
            "pre_trigger_ms".to_owned(),
            capture.request.pre_trigger_ms.to_string(),
        ),
        (
            "post_trigger_ms".to_owned(),
            capture.request.post_trigger_ms.to_string(),
        ),
        ("device_serial".to_owned(), info.device_serial.clone()),
        ("target_port".to_owned(), port_name(info.target_port)),
        ("target_stream".to_owned(), info.target_stream.to_string()),
    ];
    recording_metadata.extend(session_metadata);
    recording_metadata.extend(recording_configuration_entries(
        shared,
        info,
        capture.request.workspace.clone(),
    )?);
    let mut sink = RawRecorderSink::create_with_metadata(&path, recording_metadata)
        .map_err(|error| error.to_string())?;
    for buffered in capture.pre_trigger_blocks.drain(..) {
        sink.consume(&buffered).map_err(|error| error.to_string())?;
    }
    sink.consume(block).map_err(|error| error.to_string())?;
    capture.trigger_sample_index = Some(
        block
            .first_sample_index
            .saturating_add(trigger_offset as u64),
    );
    capture.captured_samples = capture
        .buffered_samples
        .saturating_add(block.samples_per_channel as u64);
    capture.buffered_samples = 0;
    capture.captured_blocks = sink.block_count();
    let post_target = capture
        .request
        .post_trigger_ms
        .saturating_mul(info.sample_rate_hz as u64)
        / 1_000;
    let samples_after_trigger = (block.samples_per_channel as usize)
        .saturating_sub(trigger_offset)
        .max(1) as u64;
    capture.post_samples_remaining = post_target.saturating_sub(samples_after_trigger);
    capture.active = Some(ActiveRecording { sink, path });
    capture.phase = TriggerCapturePhase::Capturing;
    if let Ok(mut state) = shared.lock() {
        state.triggered_recording = Some(capture.view("capturing"));
        state.push_event_details(
            "record",
            "trigger-recording",
            format!(
                "{} trigger accepted; post-trigger capture started",
                capture.request.source.label()
            ),
            capture.trigger_sample_index,
            (capture.request.source == RecordingTriggerSource::Analog)
                .then_some(capture.request.analog_channel_index),
        );
    }
    if capture.post_samples_remaining == 0 {
        finish_trigger_capture(capture, shared)?;
    }
    Ok(())
}

fn finish_trigger_capture(
    capture: &mut TriggerCapture,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<(), String> {
    let active = capture
        .active
        .as_mut()
        .ok_or_else(|| "triggered recorder is missing its output sink".to_owned())?;
    active.sink.finalize().map_err(|error| error.to_string())?;
    capture.captured_blocks = active.sink.block_count();
    capture.phase = TriggerCapturePhase::Complete;
    let view = capture.view("complete");
    if let Ok(mut state) = shared.lock() {
        state.triggered_recording = Some(view);
        state.push_event(
            "success",
            "trigger-recording",
            format!(
                "Triggered recording completed: {} blocks",
                capture.captured_blocks
            ),
        );
    }
    Ok(())
}

fn recording_configuration_entries(
    shared: &Arc<Mutex<DesktopShared>>,
    info: &AcquisitionInfo,
    workspace: Option<serde_json::Value>,
) -> Result<Vec<(String, String)>, String> {
    let shared = shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned")?;
    let mut workspace = workspace.unwrap_or_else(|| serde_json::json!({}));
    if !workspace.is_object() {
        return Err("recording workspace must be an object".into());
    }
    workspace["pipeline"] =
        serde_json::to_value(&shared.pipeline).map_err(|error| error.to_string())?;
    workspace["session"] =
        serde_json::to_value(&shared.session_metadata).map_err(|error| error.to_string())?;
    let configuration = serde_json::json!({
        "schemaVersion": 1,
        "application": "NeuroX Studio",
        "scope": "recording-start",
        "startedAtUnixMs": SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_millis() as u64,
        "sampleRateHz": info.sample_rate_hz,
        "channelCount": info.channel_count,
        "streams": info.streams,
        "deviceSerial": info.device_serial,
        "device": shared.device,
        "data": { "stage": "raw-before-software-processing", "unit": "ADC", "microvoltsPerCount": 0.195 },
        "spikes": shared.spikes.config,
        "spikeThresholdsAtStart": shared.spikes.thresholds,
        "stimulationAtStart": shared.stimulation,
        "workspace": workspace
    });
    configuration_entries(&configuration)
}

fn start_recorder(
    recorder: &mut Option<ActiveRecording>,
    path: &Path,
    info: &AcquisitionInfo,
    shared: &Arc<Mutex<DesktopShared>>,
    workspace: Option<serde_json::Value>,
) -> Result<RecordingView, String> {
    if recorder.is_some() {
        return Err("recording is already active".to_owned());
    }
    if path.extension().and_then(|value| value.to_str()) != Some("rhsraw") {
        return Err("recording path must use the .rhsraw extension".to_owned());
    }
    let session_metadata = shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?
        .session_metadata
        .recording_entries();
    let mut recording_metadata = vec![
        ("device_serial".to_owned(), info.device_serial.clone()),
        ("target_port".to_owned(), port_name(info.target_port)),
        ("target_stream".to_owned(), info.target_stream.to_string()),
        (
            "stimulation".to_owned(),
            "confirmed-bench-capable; see stimulation-audit JSONL and raw artifact flags"
                .to_owned(),
        ),
        ("frontend".to_owned(), "neurox-studio-0.1.0".to_owned()),
    ];
    recording_metadata.extend(session_metadata);
    recording_metadata.extend(recording_configuration_entries(shared, info, workspace)?);
    let sink = RawRecorderSink::create_with_metadata(path, recording_metadata)
        .map_err(|error| error.to_string())?;
    let path_text = path.to_string_lossy().into_owned();
    *recorder = Some(ActiveRecording {
        sink,
        path: path.to_path_buf(),
    });
    if let Ok(mut state) = shared.lock() {
        state.recording = true;
        state.recording_path = Some(path_text.clone());
        state.mode = "recording";
        state.push_event(
            "record",
            "storage",
            format!("Recording started: {path_text}"),
        );
    }
    Ok(RecordingView {
        path: path_text,
        blocks: 0,
        complete: false,
    })
}

fn finish_recorder(
    recorder: &mut Option<ActiveRecording>,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<RecordingView, String> {
    let mut active = recorder
        .take()
        .ok_or_else(|| "recording is not active".to_owned())?;
    if let Err(error) = active.sink.finalize() {
        if let Ok(mut state) = shared.lock() {
            state.recording = false;
            state.recording_path = None;
            state.push_event(
                "error",
                "storage",
                format!("Recording could not be finalized: {error}"),
            );
        }
        return Err(error.to_string());
    }
    let result = RecordingView {
        path: active.path.to_string_lossy().into_owned(),
        blocks: active.sink.block_count(),
        complete: true,
    };
    if let Ok(mut state) = shared.lock() {
        state.recording = false;
        state.recording_path = None;
        state.mode = "acquiring";
        state.push_event(
            "success",
            "storage",
            format!("Recording finalized: {} blocks", result.blocks),
        );
    }
    Ok(result)
}

fn drain_threshold_events(processing: &ProcessingChain, shared: &Arc<Mutex<DesktopShared>>) {
    for configured in &processing.threshold_events {
        while let Ok(event) = configured.receiver.try_recv() {
            if let Ok(mut state) = shared.lock() {
                state.metrics.threshold_events = state.metrics.threshold_events.saturating_add(1);
                state.push_event_details(
                    "event",
                    "threshold",
                    event.message,
                    event.sample_index,
                    Some(configured.channel_index),
                );
            }
        }
    }
}

fn prepare_hardware_audit(
    directory: &Path,
    config: &StimulationConfigRequest,
    info: &AcquisitionInfo,
) -> Result<(Arc<Mutex<std::fs::File>>, String), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("stimulation audit directory is not writable: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path = directory.join(format!("stim-{stamp}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("could not create stimulation audit: {error}"))?;
    let hardware = rhs2116_hardware_stimulation(config)?;
    let record = serde_json::json!({"event": "hardware-arm", "serial": info.device_serial, "streams": info.streams, "requested": config,
        "streamMasks": hardware.stream_masks, "singleChipMask": hardware.channel_mask,
        "amplitudeNa": hardware.magnitude_steps as u32 * hardware.stim_step_size.current_na(),
        "phaseWidthUs": f64::from(hardware.phase_steps) / 0.03, "interphaseUs": f64::from(hardware.interphase_steps) / 0.03,
        "frequencyHz": 30000.0 / f64::from(hardware.period_steps), "pulses": hardware.pulses,
        "activeChargeRecovery": false, "negativePhaseFirst": true,
        "validationMode": match config.requested_output_mode()? {
            "hardware-configurable" => "configurable-research-unverified",
            "hardware-functional" => "no-bio-single-channel-functional",
            _ => "dummy-load-bench",
        },
        "qualifiedForBiologicalUse": false,
        "currentMeasured": false});
    writeln!(file, "{record}")
        .and_then(|_| file.flush())
        .map_err(|error| error.to_string())?;
    Ok((
        Arc::new(Mutex::new(file)),
        path.to_string_lossy().into_owned(),
    ))
}

fn write_stimulation_audit(
    stimulation: &SoftwareStimulationState,
    record: &serde_json::Value,
) -> Result<(), String> {
    if let Some(audit) = &stimulation.audit {
        let mut file = audit.lock().map_err(|_| "stimulation audit lock failed")?;
        writeln!(file, "{record}")
            .and_then(|_| file.flush())
            .map_err(|error| format!("stimulation audit write failed: {error}"))?;
    }
    Ok(())
}

fn drain_stimulation_events(
    handle: &AcquisitionHandle,
    stimulation: &mut SoftwareStimulationState,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<(), String> {
    while let Ok(event) = handle.try_stim_event() {
        if let Err(error) = write_stimulation_audit(
            stimulation,
            &serde_json::json!({"commandId": event.command_id, "state": format!("{:?}", event.state), "targetSampleIndex": event.target_sample_index, "sampleIndex": event.sample_index, "message": event.message}),
        ) {
            let _ = handle.emergency_stop();
            return Err(error);
        }
        use rhs_device::AcquisitionStimState;
        if stimulation.view.hardware_output() {
            stimulation.view.last_command_id = Some(event.command_id);
            stimulation.view.last_target_sample_index = Some(event.target_sample_index);
            match event.state {
                AcquisitionStimState::Completed
                | AcquisitionStimState::Cancelled
                | AcquisitionStimState::EmergencyStopped
                | AcquisitionStimState::Rejected => {
                    stimulation
                        .pending
                        .retain(|pending| pending.command_id != event.command_id);
                    if event.state == AcquisitionStimState::Completed {
                        stimulation.view.completed_commands += 1;
                    }
                    stimulation.view.state = format!("{:?}", event.state).to_lowercase();
                }
                AcquisitionStimState::Fault => {
                    let _ = handle.emergency_stop();
                    stimulation.pending.clear();
                    stimulation.config = None;
                    stimulation.arm_expires_at = None;
                    stimulation.view.arm_id = None;
                    stimulation.view.state = "fault".to_owned();
                }
                AcquisitionStimState::Triggered | AcquisitionStimState::Observed => {
                    if stimulation.view.arm_id.is_some() {
                        stimulation.view.state = format!("{:?}", event.state).to_lowercase();
                    }
                }
                AcquisitionStimState::Queued => {}
            }
            stimulation.view.pending_commands = stimulation.pending.len();
        }
        if let Ok(mut state) = shared.lock() {
            state.stimulation = stimulation.view.clone();
            state.push_event_details(
                "warning",
                "stimulation",
                format!("{:?}: {}", event.state, event.message),
                event.sample_index,
                None,
            );
        }
    }
    Ok(())
}

fn median_sorted(values: &[u16]) -> f64 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (f64::from(values[middle - 1]) + f64::from(values[middle])) / 2.0
    } else {
        f64::from(values[middle])
    }
}

fn extract_block_baseline(block: &SampleBlock) -> Vec<BaselineChannelView> {
    let channel_count = block.channel_count();
    let sample_count = block.samples_per_channel as usize;
    (0..channel_count)
        .map(|channel| {
            let mut values: Vec<u16> = block
                .samples
                .iter()
                .skip(channel)
                .step_by(channel_count)
                .copied()
                .collect();
            let saturated = values.iter().any(|value| *value <= 1 || *value >= 65_534);
            values.sort_unstable();
            let center = median_sorted(&values);
            let mut deviations: Vec<f64> = values
                .iter()
                .map(|value| (f64::from(*value) - center).abs())
                .collect();
            deviations.sort_by(f64::total_cmp);
            let middle = deviations.len() / 2;
            let mad = if deviations.len().is_multiple_of(2) {
                (deviations[middle - 1] + deviations[middle]) / 2.0
            } else {
                deviations[middle]
            };
            let quality = if block.flags.contains(rhs_core::SampleFlags::DISCONTINUITY) {
                "discontinuity"
            } else if block.flags.contains(rhs_core::SampleFlags::STIM_ARTIFACT) {
                "stim-artifact"
            } else if saturated {
                "saturated"
            } else if sample_count < 8 {
                "too-short"
            } else {
                "ok"
            };
            BaselineChannelView {
                offset_adc: center - 32_768.0,
                noise_adc: mad * 1.4826,
                sample_count,
                quality: quality.to_owned(),
            }
        })
        .collect()
}

fn build_waveform_frame(
    block: &SampleBlock,
    max_points: usize,
    baseline: Vec<BaselineChannelView>,
) -> WaveformFrame {
    let input_points = block.samples_per_channel as usize;
    let bin_width = input_points.div_ceil(max_points).max(1);
    let point_count = input_points.div_ceil(bin_width);
    let channel_count = block.channel_count();
    let mut minimums = Vec::with_capacity(point_count * channel_count);
    let mut maximums = Vec::with_capacity(point_count * channel_count);
    let mut first_values = Vec::with_capacity(point_count * channel_count);
    let mut last_values = Vec::with_capacity(point_count * channel_count);
    for point in 0..point_count {
        let start = point * bin_width;
        let end = (start + bin_width).min(input_points);
        for channel in 0..channel_count {
            let mut minimum = u16::MAX;
            let mut maximum = u16::MIN;
            for sample in start..end {
                let raw = block.samples[sample * channel_count + channel];
                minimum = minimum.min(raw);
                maximum = maximum.max(raw);
            }
            minimums.push(minimum);
            maximums.push(maximum);
            first_values.push(block.samples[start * channel_count + channel]);
            last_values.push(block.samples[(end - 1) * channel_count + channel]);
        }
    }
    WaveformFrame {
        sequence: block.sequence,
        spike_epoch: 0,
        first_sample_index: block.first_sample_index,
        sample_rate_hz: block.sample_rate_hz,
        bin_width_samples: bin_width as u32,
        sample_count: block.samples_per_channel,
        point_count,
        channel_count,
        minimums,
        maximums,
        first_values,
        last_values,
        baseline,
        flags: block.flags.bits(),
    }
}

fn make_device(bitfile: &Option<String>, serial: &Option<String>) -> RhsDevice {
    let mut device = match bitfile.as_deref().filter(|value| !value.trim().is_empty()) {
        Some(bitfile) => RhsDevice::new(bitfile),
        None => RhsDevice::default_bitfile(),
    };
    if let Some(serial) = serial.as_deref().filter(|value| !value.trim().is_empty()) {
        device = device.with_serial(serial);
    }
    device
}

fn device_view_from_info(info: &AcquisitionInfo, bitfile: String) -> DeviceView {
    let chip = DeviceChipView {
        chip_name: "RHS2116".to_owned(),
        port: port_name(info.target_port),
        stream: info.target_stream,
        channels: RHS_CHANNEL_COUNT,
    };
    DeviceView {
        serial: info.device_serial.clone(),
        chip_name: chip.chip_name.clone(),
        port: chip.port.clone(),
        stream: info.target_stream,
        channels: info.channel_count,
        sample_rate_hz: info.sample_rate_hz,
        bitfile,
        chips: info
            .streams
            .iter()
            .map(|stream| DeviceChipView {
                chip_name: "RHS2116".to_owned(),
                port: port_name(*stream / 2),
                stream: *stream,
                channels: 16,
            })
            .collect(),
        capabilities: current_device_capabilities(info.streams.len(), info.channel_count),
    }
}

fn device_view_from_scan(
    report: &ScanReport,
    serial: String,
    bitfile: String,
) -> Result<DeviceView, String> {
    let first = report
        .chips
        .first()
        .ok_or_else(|| "device scan did not return any chips".to_owned())?;
    let chips: Vec<_> = report.chips.iter().map(device_chip_view).collect();
    let detected_channel_count = report
        .channels_on_port
        .iter()
        .map(|channels| (*channels).max(0) as usize)
        .sum();

    Ok(DeviceView {
        serial,
        chip_name: first.chip_name.to_owned(),
        port: first.port_name.to_string(),
        stream: first.command_stream.max(0) as u8,
        channels: RHS_CHANNEL_COUNT,
        sample_rate_hz: RHS_SAMPLE_RATE_HZ,
        bitfile,
        chips,
        capabilities: current_device_capabilities(report.chips.len(), detected_channel_count),
    })
}

fn scan_report_is_usable(report: &ScanReport) -> bool {
    report.return_code >= 0 && !report.chips.is_empty()
}

fn device_chip_view(chip: &ChipInfo) -> DeviceChipView {
    DeviceChipView {
        chip_name: chip.chip_name.to_owned(),
        port: chip.port_name.to_string(),
        stream: chip.command_stream.max(0) as u8,
        channels: RHS_CHANNEL_COUNT,
    }
}

fn current_device_capabilities(
    detected_chip_count: usize,
    detected_channel_count: usize,
) -> DeviceCapabilitiesView {
    DeviceCapabilitiesView {
        detected_chip_count,
        detected_channel_count,
        acquisition_stream_limit: 4,
        acquisition_channel_limit: 64,
        stimulation_stream_limit: 4,
        stimulation_channel_limit: 64,
    }
}

fn port_name(port: u8) -> String {
    char::from(b'A'.saturating_add(port)).to_string()
}

fn ensure_session_idle(state: &DesktopState) -> Result<(), String> {
    if matches!(
        state
            .shared
            .lock()
            .map_err(|_| "desktop state lock failed")?
            .impedance
            .state
            .as_str(),
        "running" | "cancelling"
    ) {
        return Err(
            "wait for impedance measurement and cleanup before another hardware operation"
                .to_owned(),
        );
    }
    let session = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?;
    if session.is_some() {
        return Err("stop acquisition before starting another operation".to_owned());
    }
    drop(session);
    let playback = state
        .playback
        .lock()
        .map_err(|_| "playback lock was poisoned".to_owned())?;
    if playback.is_some() {
        return Err("close playback before starting another operation".to_owned());
    }
    Ok(())
}

fn update_mode(
    shared: &Arc<Mutex<DesktopShared>>,
    mode: &'static str,
    health: &'static str,
    error: Option<String>,
) -> Result<(), String> {
    let mut shared = shared
        .lock()
        .map_err(|_| "desktop state lock was poisoned".to_owned())?;
    shared.mode = mode;
    shared.health = health;
    shared.last_error = error;
    Ok(())
}

fn elapsed_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::{ChannelId, SampleFlags};
    use std::fs;

    fn playback_test_path(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rhs-desktop-{label}-{}-{unique}.rhsraw",
            std::process::id()
        ))
    }

    fn write_playback_test_file(path: &Path, block_count: u64, channel_count: usize) {
        let channels = (0..channel_count)
            .map(|channel| ChannelId {
                stream: 0,
                channel: channel as u8,
            })
            .collect::<Vec<_>>();
        let mut recorder = RawRecorderSink::create(path).unwrap();
        for sequence in 0..block_count {
            let samples_per_channel = 100_u32;
            let samples = (0..samples_per_channel as usize * channel_count)
                .map(|index| 32_000 + ((index + sequence as usize) % 1_000) as u16)
                .collect();
            let block = SampleBlock::new(
                "playback-test",
                sequence,
                sequence * samples_per_channel as u64,
                30_000,
                channels.clone(),
                samples_per_channel,
                samples,
                Vec::new(),
                SampleFlags::empty(),
            )
            .unwrap();
            recorder.consume(&block).unwrap();
        }
        recorder.finalize().unwrap();
    }

    fn test_acquisition_info(channel_count: usize) -> AcquisitionInfo {
        AcquisitionInfo {
            sample_rate_hz: 30_000,
            channel_count,
            samples_per_block: 100,
            blocks_per_read: 1,
            target_stream: 0,
            target_port: 0,
            streams: vec![0],
            bytes_per_block: channel_count * 100 * 2,
            pipe_read_error: 0,
            device_serial: "TEST-RHS".to_owned(),
            message: "test".to_owned(),
        }
    }

    fn trigger_test_block(sequence: u64, channel_count: usize) -> SampleBlock {
        SampleBlock::new(
            "trigger-test",
            sequence,
            sequence * 100,
            30_000,
            (0..channel_count)
                .map(|channel| ChannelId {
                    stream: 0,
                    channel: channel as u8,
                })
                .collect(),
            100,
            vec![32_768; 100 * channel_count],
            vec![0; 100],
            SampleFlags::empty(),
        )
        .unwrap()
    }

    #[test]
    fn waveform_frame_is_bounded_and_preserves_channel_extrema() {
        let channels = vec![
            ChannelId {
                stream: 0,
                channel: 0,
            },
            ChannelId {
                stream: 0,
                channel: 1,
            },
        ];
        let mut samples = Vec::new();
        for index in 0..100_u16 {
            samples.push(index);
            samples.push(1_000 + index);
        }
        let block = SampleBlock::new(
            "test",
            3,
            300,
            30_000,
            channels,
            100,
            samples,
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap();
        let frame = build_waveform_frame(&block, 10, extract_block_baseline(&block));
        assert_eq!(frame.point_count, 10);
        assert_eq!(frame.minimums[0], 0);
        assert_eq!(frame.maximums[0], 9);
        assert_eq!(frame.minimums[1], 1_000);
        assert_eq!(frame.maximums[1], 1_009);
        assert_eq!(frame.sample_count, 100);
        assert_eq!(frame.first_values[0], 0);
        assert_eq!(frame.last_values[0], 9);
        let partial = build_waveform_frame(&block, 14, Vec::new());
        assert_eq!(partial.bin_width_samples, 8);
        assert_eq!(partial.sample_count, 100);
        assert_eq!(partial.last_values[24], 99);
    }

    #[test]
    fn waveform_poll_retains_intermediate_frames_without_duplicates() {
        let mut shared = DesktopShared::default();
        for sequence in 0..4 {
            shared.publish_waveform(build_waveform_frame(
                &trigger_test_block(sequence, 64),
                33,
                Vec::new(),
            ));
        }
        let batch = shared.waveform_batch(None, None);
        assert_eq!(batch.frames.len(), 4);
        assert_eq!(batch.frames[3].first_sample_index, 300);
        assert_eq!(batch.frames[0].sample_count, 100);
        let next = shared.waveform_batch(Some(batch.next_cursor), Some(batch.generation));
        assert!(next.frames.is_empty());
        shared.publish_waveform(build_waveform_frame(
            &trigger_test_block(4, 64),
            33,
            Vec::new(),
        ));
        let next = shared.waveform_batch(Some(batch.next_cursor), Some(batch.generation));
        assert_eq!(next.frames.len(), 1);
        assert_eq!(next.frames[0].first_sample_index, 400);
    }

    #[test]
    fn waveform_buffer_is_bounded_and_exposes_slow_reader_loss_and_reset() {
        let mut shared = DesktopShared::default();
        for sequence in 0..600 {
            let mut frame = build_waveform_frame(&trigger_test_block(sequence, 1), 33, Vec::new());
            frame.sample_rate_hz = 1_000_000;
            shared.publish_waveform(frame);
        }
        assert_eq!(shared.waveform_history.len(), 512);
        let batch = shared.waveform_batch(Some(1), Some(shared.waveform_generation));
        assert_eq!(batch.dropped_frames, 87);
        let old_generation = batch.generation;
        shared.clear_waveforms();
        shared.publish_waveform(build_waveform_frame(
            &trigger_test_block(0, 1),
            33,
            Vec::new(),
        ));
        let batch = shared.waveform_batch(Some(batch.next_cursor), Some(old_generation));
        assert_ne!(batch.generation, old_generation);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.dropped_frames, 0);
        assert_eq!(batch.frames[0].first_sample_index, 0);
    }

    #[test]
    fn waveform_buffer_retention_uses_actual_sample_time() {
        let mut shared = DesktopShared::default();
        for sequence in [0, 1, 1000] {
            shared.publish_waveform(build_waveform_frame(
                &trigger_test_block(sequence, 1),
                33,
                Vec::new(),
            ));
        }
        assert_eq!(shared.waveform_history.len(), 1);
        assert_eq!(
            shared.latest_waveform.as_ref().unwrap().first_sample_index,
            100_000
        );
    }

    #[test]
    fn block_baseline_uses_raw_median_and_marks_artifacts() {
        let channels = vec![ChannelId {
            stream: 2,
            channel: 0,
        }];
        let block = SampleBlock::new(
            "baseline-test",
            0,
            0,
            30_000,
            channels.clone(),
            9,
            vec![
                32_766, 32_767, 32_768, 32_769, 32_770, 32_768, 32_768, 32_767, 40_000,
            ],
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap();
        let baseline = extract_block_baseline(&block);
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].offset_adc, 0.0);
        assert!((baseline[0].noise_adc - 1.4826).abs() < 1e-9);
        assert_eq!(baseline[0].sample_count, 9);
        assert_eq!(baseline[0].quality, "ok");

        let flagged = SampleBlock::new(
            "baseline-test",
            1,
            9,
            30_000,
            channels,
            9,
            vec![0; 9],
            Vec::new(),
            SampleFlags::from_bits(SampleFlags::STIM_ARTIFACT),
        )
        .unwrap();
        assert_eq!(extract_block_baseline(&flagged)[0].quality, "stim-artifact");
    }

    #[test]
    fn pipeline_rejects_channels_outside_rhs2116() {
        let mut config = PipelineConfig::default();
        let PipelineNodeConfig::Threshold { channel_index, .. } = &mut config.nodes[3] else {
            panic!("default pipeline must end with a threshold node");
        };
        *channel_index = 16;
        let error = config.validate().unwrap_err();
        assert!(error.contains("0 and 15"));
    }

    #[test]
    fn pipeline_rejects_duplicate_node_ids_and_unknown_versions() {
        let mut duplicate = PipelineConfig::default();
        let first_id = duplicate.nodes[0].id().to_owned();
        let PipelineNodeConfig::Notch { id, .. } = &mut duplicate.nodes[1] else {
            panic!("default pipeline must contain a notch node");
        };
        *id = first_id;
        assert!(duplicate.validate().unwrap_err().contains("duplicate"));

        let future = PipelineConfig {
            version: PIPELINE_SCHEMA_VERSION + 1,
            ..PipelineConfig::default()
        };
        assert!(future.validate().unwrap_err().contains("unsupported"));
    }

    #[test]
    fn pipeline_json_contract_round_trips_with_frontend_field_names() {
        let config = PipelineConfig::default();
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["version"], PIPELINE_SCHEMA_VERSION);
        assert_eq!(value["nodes"][0]["kind"], "artifact-blanking");
        assert_eq!(value["nodes"][0]["ttlMask"], 1);
        assert_eq!(value["nodes"][1]["frequencyHz"], 50.0);
        assert_eq!(value["nodes"][3]["channelIndex"], 0);
        let decoded: PipelineConfig = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn processing_chain_uses_configured_order_and_multiple_threshold_receivers() {
        let config = PipelineConfig {
            version: PIPELINE_SCHEMA_VERSION,
            nodes: vec![
                PipelineNodeConfig::Notch {
                    id: "filter-last".to_owned(),
                    enabled: true,
                    frequency_hz: 60.0,
                    quality_factor: 15.0,
                },
                PipelineNodeConfig::ArtifactBlanking {
                    id: "blank-after-filter".to_owned(),
                    enabled: true,
                    ttl_mask: 1,
                    blank_samples: 30,
                    replacement: BlankingReplacementConfig::Midpoint,
                },
                PipelineNodeConfig::Threshold {
                    id: "threshold-a".to_owned(),
                    enabled: true,
                    channel_index: 0,
                    threshold: 40_000,
                    direction: ThresholdDirectionConfig::RisingAbove,
                    refractory_samples: 100,
                },
                PipelineNodeConfig::Threshold {
                    id: "threshold-b".to_owned(),
                    enabled: true,
                    channel_index: 1,
                    threshold: 20_000,
                    direction: ThresholdDirectionConfig::FallingBelow,
                    refractory_samples: 50,
                },
            ],
        };
        let chain = ProcessingChain::new(&config).unwrap();
        let ids: Vec<_> = chain
            .processors
            .iter()
            .map(|configured| configured.node_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "filter-last",
                "blank-after-filter",
                "threshold-a",
                "threshold-b"
            ]
        );
        assert_eq!(chain.threshold_events.len(), 2);
    }

    #[test]
    fn pipeline_checks_filter_compatibility_before_starting() {
        let config = PipelineConfig {
            version: PIPELINE_SCHEMA_VERSION,
            nodes: vec![PipelineNodeConfig::BandPass {
                id: "bad-band".to_owned(),
                enabled: true,
                low_cut_hz: 300.0,
                high_cut_hz: 15_000.0,
            }],
        };
        let error = ProcessingChain::new(&config).err().unwrap();
        assert!(error.contains("15000"));
    }

    #[test]
    fn status_keeps_stimulation_interlocked() {
        let status = DesktopShared::default().status();
        assert_eq!(status.stimulation_interlock, "bench-validation-required");
    }

    #[test]
    fn scan_view_separates_detected_channels_from_current_session_limit() {
        let report = ScanReport {
            return_code: 1,
            pipe_read_error: 0,
            message: "ok".to_owned(),
            chips: vec![
                ChipInfo {
                    chip_type: 1,
                    chip_name: "RHS2116",
                    port_index: 0,
                    port_name: 'A',
                    command_stream: 0,
                },
                ChipInfo {
                    chip_type: 1,
                    chip_name: "RHS2116",
                    port_index: 0,
                    port_name: 'A',
                    command_stream: 1,
                },
                ChipInfo {
                    chip_type: 1,
                    chip_name: "RHS2116",
                    port_index: 1,
                    port_name: 'B',
                    command_stream: 2,
                },
                ChipInfo {
                    chip_type: 1,
                    chip_name: "RHS2116",
                    port_index: 1,
                    port_name: 'B',
                    command_stream: 3,
                },
            ],
            channels_on_port: [32, 32, 0, 0],
        };

        let view = device_view_from_scan(&report, "test-device".to_owned(), "test.bit".to_owned())
            .unwrap();
        assert!(scan_report_is_usable(&report));
        assert_eq!(view.chips.len(), 4);
        assert_eq!(view.channels, 16);
        assert_eq!(view.capabilities.detected_chip_count, 4);
        assert_eq!(view.capabilities.detected_channel_count, 64);
        assert_eq!(view.capabilities.acquisition_stream_limit, 4);
        assert_eq!(view.capabilities.acquisition_channel_limit, 64);
    }

    #[test]
    fn threshold_event_view_serializes_sample_and_channel_coordinates() {
        let mut shared = DesktopShared::default();
        shared.push_event_details("event", "threshold", "crossing", Some(123_456), Some(7));
        let value = serde_json::to_value(shared.events.back().unwrap()).unwrap();
        assert_eq!(value["sampleIndex"], 123_456);
        assert_eq!(value["channelIndex"], 7);
    }

    #[test]
    fn playback_inspection_and_seek_use_recording_timeline() {
        let path = playback_test_path("seek");
        write_playback_test_file(&path, 3, 4);

        let summary = inspect_playback(&path).unwrap();
        assert_eq!(summary.block_count, 3);
        assert_eq!(summary.total_samples, 300);
        assert_eq!(summary.info.channels.len(), 4);
        assert!(summary.info.complete);

        let (_source, pending, position, blocks) = seek_playback_source(&path, 150).unwrap();
        assert_eq!(position, 100);
        assert_eq!(blocks, 1);
        assert_eq!(pending.unwrap().sequence, 1);

        let (_source, pending, position, blocks) = seek_playback_source(&path, 300).unwrap();
        assert_eq!(position, 300);
        assert_eq!(blocks, 3);
        assert!(pending.is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn playback_pipeline_validation_uses_recorded_channel_count() {
        let config = PipelineConfig {
            version: PIPELINE_SCHEMA_VERSION,
            nodes: vec![PipelineNodeConfig::Threshold {
                id: "recorded-channel".to_owned(),
                enabled: true,
                channel_index: 3,
                threshold: 40_000,
                direction: ThresholdDirectionConfig::RisingAbove,
                refractory_samples: 100,
            }],
        };
        assert!(ProcessingChain::new_for(&config, 30_000, 4).is_ok());
        assert!(ProcessingChain::new_for(&config, 30_000, 3).is_err());
    }

    #[test]
    fn software_trigger_writes_pre_and_post_trigger_blocks() {
        let path = playback_test_path("software-trigger");
        let info = test_acquisition_info(2);
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        shared.lock().unwrap().session_metadata = SessionMetadata {
            session_name: "trigger-session".to_owned(),
            operator: "operator-a".to_owned(),
            subject_id: "subject-7".to_owned(),
            notes: "trigger metadata".to_owned(),
        };
        let request = TriggeredRecordingRequest {
            path: path.to_string_lossy().into_owned(),
            workspace: None,
            source: RecordingTriggerSource::Software,
            pre_trigger_ms: 5,
            post_trigger_ms: 5,
            digital_mask: 1,
            analog_channel_index: 0,
            analog_threshold: 40_000,
            analog_direction: ThresholdDirectionConfig::RisingAbove,
        };
        let mut capture = None;
        arm_trigger_capture(&mut capture, request, &info, &shared).unwrap();
        process_trigger_capture(&mut capture, &trigger_test_block(0, 2), &info, &shared).unwrap();
        process_trigger_capture(&mut capture, &trigger_test_block(1, 2), &info, &shared).unwrap();
        request_software_trigger(&mut capture, &shared).unwrap();
        process_trigger_capture(&mut capture, &trigger_test_block(2, 2), &info, &shared).unwrap();
        process_trigger_capture(&mut capture, &trigger_test_block(3, 2), &info, &shared).unwrap();

        let capture = capture.as_ref().unwrap();
        assert_eq!(capture.phase, TriggerCapturePhase::Complete);
        assert_eq!(capture.trigger_sample_index, Some(200));
        let summary = inspect_playback(&path).unwrap();
        assert_eq!(summary.block_count, 4);
        assert_eq!(summary.total_samples, 400);
        assert!(summary
            .info
            .metadata
            .contains(&("trigger_source".to_owned(), "software".to_owned())));
        assert!(summary
            .info
            .metadata
            .contains(&("session_name".to_owned(), "trigger-session".to_owned())));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn recording_restores_parameters_and_preserves_raw_samples_with_filters_enabled() {
        let path = playback_test_path("configuration-raw-64");
        let mut info = test_acquisition_info(64);
        info.streams = vec![2, 3, 4, 5];
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let workspace = serde_json::json!({
            "meaProfileId": "mea-ab-64",
            "channelSettings": {"mea-ab-64": {"B-009": {"label": "B-09", "row": 0, "column": 0}}},
            "display": {"timeWindowMs": 500, "channelRanges": {"mea-ab-64": {"B-009": {"minUv": -20, "maxUv": 20}}}},
            "stimulation": {"mea-ab-64": {"selectedChannels": ["B-009"], "amplitudeNa": 10, "phaseWidthUs": 200}},
            "pipeline": {"untrustedFrontendDraft": true}
        });
        let mut recorder = None;
        start_recorder(
            &mut recorder,
            &path,
            &info,
            &shared,
            Some(workspace.clone()),
        )
        .unwrap();
        let block = SampleBlock::new(
            "configuration-test",
            0,
            0,
            30_000,
            (0..64)
                .map(|index| ChannelId {
                    stream: 2 + index / 16,
                    channel: index % 16,
                })
                .collect(),
            100,
            (0..6400)
                .map(|index| 32_768 + ((index / 64) % 30) as u16 * 10)
                .collect(),
            vec![0; 100],
            SampleFlags::empty(),
        )
        .unwrap();
        recorder.as_mut().unwrap().sink.consume(&block).unwrap();
        let pipeline = shared.lock().unwrap().pipeline.clone();
        let processed = ProcessingChain::new_for(&pipeline, 30_000, 64)
            .unwrap()
            .process(block.clone())
            .unwrap();
        assert_ne!(processed.samples, block.samples);
        finish_recorder(&mut recorder, &shared).unwrap();
        assert!(
            start_recorder(&mut recorder, &path, &info, &shared, None).is_err(),
            "existing recordings must not be overwritten"
        );
        let mut replay = FileReplaySource::open(&path).unwrap();
        assert_eq!(replay.next_block().unwrap().unwrap().samples, block.samples);
        let configuration = recording_configuration(replay.info()).unwrap().unwrap();
        assert_eq!(configuration["channelCount"], 64);
        assert_eq!(configuration["streams"], serde_json::json!([2, 3, 4, 5]));
        assert_eq!(
            configuration["workspace"]["channelSettings"],
            workspace["channelSettings"]
        );
        assert_eq!(configuration["workspace"]["display"], workspace["display"]);
        assert_eq!(
            configuration["workspace"]["stimulation"],
            workspace["stimulation"]
        );
        assert_eq!(
            configuration["workspace"]["pipeline"],
            serde_json::to_value(pipeline).unwrap()
        );
        assert_eq!(
            configuration["data"]["stage"],
            "raw-before-software-processing"
        );
        let mut mismatched = replay.info().clone();
        mismatched.sample_rate_hz = 20_000;
        assert!(recording_configuration(&mismatched).is_err());
        drop(replay);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn session_metadata_is_validated_and_written_to_raw_recordings() {
        let path = playback_test_path("session-metadata");
        let info = test_acquisition_info(2);
        let shared_state = DesktopShared {
            session_metadata: SessionMetadata {
                session_name: "MEA baseline".to_owned(),
                operator: "operator-b".to_owned(),
                subject_id: "subject-9".to_owned(),
                notes: "quiet baseline".to_owned(),
            },
            ..DesktopShared::default()
        };
        assert!(shared_state.session_metadata.validate().is_ok());
        let shared = Arc::new(Mutex::new(shared_state));
        let mut recorder = None;
        start_recorder(&mut recorder, &path, &info, &shared, None).unwrap();
        recorder
            .as_mut()
            .unwrap()
            .sink
            .consume(&trigger_test_block(0, 2))
            .unwrap();
        finish_recorder(&mut recorder, &shared).unwrap();

        let summary = inspect_playback(&path).unwrap();
        assert!(summary
            .info
            .metadata
            .contains(&("session_name".to_owned(), "MEA baseline".to_owned())));
        assert!(summary
            .info
            .metadata
            .contains(&("subject_id".to_owned(), "subject-9".to_owned())));
        fs::remove_file(path).unwrap();

        let invalid = SessionMetadata {
            notes: "x".repeat(4_001),
            ..SessionMetadata::default()
        };
        assert!(invalid.validate().unwrap_err().contains("4000"));
    }

    #[test]
    fn digital_and_analog_triggers_report_sample_offsets() {
        let mut digital_block = trigger_test_block(0, 2);
        digital_block.ttl_in[37] = 1;
        let mut digital = TriggerCapture {
            request: TriggeredRecordingRequest {
                path: "digital.rhsraw".to_owned(),
                workspace: None,
                source: RecordingTriggerSource::Digital,
                pre_trigger_ms: 0,
                post_trigger_ms: 100,
                digital_mask: 1,
                analog_channel_index: 0,
                analog_threshold: 40_000,
                analog_direction: ThresholdDirectionConfig::RisingAbove,
            },
            phase: TriggerCapturePhase::Armed,
            pre_trigger_blocks: VecDeque::new(),
            buffered_samples: 0,
            active: None,
            trigger_sample_index: None,
            post_samples_remaining: 0,
            captured_samples: 0,
            captured_blocks: 0,
            previous_ttl: 0,
            previous_analog: None,
            software_trigger_pending: false,
        };
        assert_eq!(
            detect_recording_trigger(&mut digital, &digital_block),
            Some(37)
        );

        let mut analog_block = trigger_test_block(0, 2);
        analog_block.samples[36 * 2 + 1] = 39_999;
        analog_block.samples[37 * 2 + 1] = 40_000;
        digital.request.source = RecordingTriggerSource::Analog;
        digital.request.analog_channel_index = 1;
        digital.previous_analog = Some(39_000);
        assert_eq!(
            detect_recording_trigger(&mut digital, &analog_block),
            Some(37)
        );
    }

    fn test_stimulation_config() -> StimulationConfigRequest {
        StimulationConfigRequest {
            profile_id: "rhs2116-logic-16".to_owned(),
            channel_ids: vec!["CH-01".to_owned(), "CH-02".to_owned()],
            waveform: "biphasic".to_owned(),
            amplitude_na: 10,
            phase_width_us: 200,
            interphase_us: 100,
            frequency_hz: 10.0,
            pulse_count: 1,
            charge_recovery: true,
            bench_confirmation: None,
            output_confirmation: None,
        }
    }

    #[test]
    fn configurable_output_requires_separate_explicit_confirmations_and_matching_preload() {
        let mut config = test_stimulation_config();
        config.profile_id = "mea-ab-64".to_owned();
        config.channel_ids = vec!["B-000".to_owned(), "C-031".to_owned()];
        config.amplitude_na = 1000;
        config.phase_width_us = 250;
        config.interphase_us = 80;
        config.frequency_hz = 20.0;
        config.pulse_count = 3;
        config.output_confirmation = Some(OutputConfirmation {
            operator: "offline-configurable-test".to_owned(),
            routing_confirmed: true,
            parameters_confirmed: true,
            unverified_output_acknowledged: true,
        });
        let mut info = test_acquisition_info(64);
        info.streams = vec![2, 3, 4, 5];
        let preloaded = rhs2116_hardware_stimulation(&config).unwrap();
        assert_eq!(
            config.requested_output_mode().unwrap(),
            "hardware-configurable"
        );
        validate_hardware_arm(&config, &info, Some(&preloaded), false).unwrap();
        assert!(validate_hardware_arm(&config, &info, Some(&preloaded), true).is_err());
        assert!(validate_hardware_arm(&config, &info, None, false).is_err());
        for missing in 0..4 {
            let mut changed = config.clone();
            let confirmation = changed.output_confirmation.as_mut().unwrap();
            match missing {
                0 => confirmation.routing_confirmed = false,
                1 => confirmation.parameters_confirmed = false,
                2 => confirmation.unverified_output_acknowledged = false,
                _ => confirmation.operator.clear(),
            }
            assert!(validate_hardware_arm(&changed, &info, Some(&preloaded), false).is_err());
        }
        let mut changed = config.clone();
        changed.amplitude_na = 2000;
        assert!(validate_hardware_arm(&changed, &info, Some(&preloaded), false).is_err());
        let mut mixed = config.clone();
        mixed.bench_confirmation = Some(BenchConfirmation {
            sample_disconnected: true,
            dummy_load_connected: true,
            measurement_ready: true,
            operator: "offline".to_owned(),
            functional_only: false,
            functional_repeat: false,
        });
        assert!(mixed.requested_output_mode().is_err());
        assert!(validate_hardware_arm(&mixed, &info, Some(&preloaded), false).is_err());
        config.output_confirmation = None;
        assert_eq!(config.requested_output_mode().unwrap(), "interlocked");
        assert!(validate_hardware_arm(&config, &info, Some(&preloaded), false).is_err());
    }

    #[test]
    fn configurable_output_preserves_sixteen_channel_routing_and_quantized_values() {
        let mut config = test_stimulation_config();
        config.channel_ids = vec!["CH-01".to_owned(), "CH-16".to_owned()];
        config.waveform = "biphasic-delay".to_owned();
        config.amplitude_na = 1000;
        config.phase_width_us = 250;
        config.interphase_us = 80;
        config.output_confirmation = Some(OutputConfirmation {
            operator: "offline".to_owned(),
            routing_confirmed: true,
            parameters_confirmed: true,
            unverified_output_acknowledged: true,
        });
        let hardware = rhs2116_hardware_stimulation(&config).unwrap();
        validate_hardware_arm(&config, &test_acquisition_info(16), Some(&hardware), false).unwrap();
        assert_eq!(hardware.channel_mask, 0x8001);
        let view = programmed_stimulation_view(&hardware);
        assert_eq!(view.amplitude_na, 1000);
        assert!((view.phase_width_us - 266.6666667).abs() < 0.0001);
        assert_eq!(view.interphase_us, 66.66666666666667);
        assert!((view.charge_per_phase_pc - 266.6666667).abs() < 0.0001);
        config.interphase_us = 0;
        assert!(rhs2116_hardware_stimulation(&config).is_err());
        config.waveform = "triphasic".to_owned();
        assert!(rhs2116_hardware_stimulation(&config).is_err());
    }

    #[test]
    fn configurable_schedule_enforces_budget_refractory_and_remaining_arm_time() {
        let now = Instant::now();
        let hardware = StimConfig::default();
        let mut state = SoftwareStimulationState::default();
        state.view.output_mode = "hardware-configurable".to_owned();
        state.arm_expires_at = Some(now + Duration::from_secs(60));
        let mut schedule = StimulationScheduleRequest {
            delay_ms: 0,
            repeat_count: 1,
            repeat_interval_ms: 1000,
        };
        validate_hardware_schedule(&schedule, Some(&hardware)).unwrap();
        assert_eq!(
            validate_hardware_submission(&state, &schedule, Some(&hardware), now).unwrap(),
            Some(Duration::from_millis(209))
        );
        state.view.submitted_commands = 16;
        assert!(validate_hardware_submission(&state, &schedule, Some(&hardware), now).is_err());
        state.view.submitted_commands = 15;
        schedule.repeat_count = 2;
        assert!(validate_hardware_submission(&state, &schedule, Some(&hardware), now).is_err());
        schedule.repeat_count = 1;
        state.next_configurable_submit_at = Some(now + Duration::from_millis(100));
        assert!(validate_hardware_submission(&state, &schedule, Some(&hardware), now).is_err());
        assert_eq!(
            validate_hardware_submission(
                &state,
                &schedule,
                Some(&hardware),
                now + Duration::from_millis(101),
            )
            .unwrap(),
            Some(Duration::from_millis(209))
        );
        state.next_configurable_submit_at = None;
        state.arm_expires_at = Some(now + Duration::from_millis(150));
        assert!(validate_hardware_submission(&state, &schedule, Some(&hardware), now).is_err());
        schedule.delay_ms = u64::MAX;
        schedule.repeat_count = 16;
        schedule.repeat_interval_ms = u64::MAX;
        assert!(validate_hardware_schedule(&schedule, Some(&hardware)).is_err());
    }

    #[test]
    fn configurable_audit_does_not_claim_measurement_or_fake_bench_confirmations() {
        let directory = playback_test_path("configurable-audit").with_extension("audit-dir");
        let mut config = test_stimulation_config();
        config.output_confirmation = Some(OutputConfirmation {
            operator: "offline-audit-test".to_owned(),
            routing_confirmed: true,
            parameters_confirmed: true,
            unverified_output_acknowledged: true,
        });
        let (file, path) =
            prepare_hardware_audit(&directory, &config, &test_acquisition_info(16)).unwrap();
        drop(file);
        let record: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(record["validationMode"], "configurable-research-unverified");
        assert_eq!(record["currentMeasured"], false);
        assert_eq!(record["qualifiedForBiologicalUse"], false);
        assert!(record["requested"]["benchConfirmation"].is_null());
        assert_eq!(
            record["requested"]["outputConfirmation"]["routingConfirmed"],
            true
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn hardware_preload_maps_selected_rhs2116_electrodes_to_mask() {
        let mut config = test_stimulation_config();
        config.channel_ids = vec!["CH-01".to_owned(), "CH-16".to_owned()];
        let hardware = rhs2116_hardware_stimulation(&config).unwrap();
        assert_eq!(hardware.channel_mask, 0x8001);
        assert_eq!(hardware.channel, 0);
        assert_eq!(hardware.magnitude_steps, 1);
        assert_eq!(hardware.phase_steps, 6);
        assert_eq!(hardware.period_steps, 3_000);
    }

    #[test]
    fn hardware_preload_rejects_unimplemented_waveform_and_magnitude() {
        let mut config = test_stimulation_config();
        config.waveform = "triphasic".to_owned();
        assert!(rhs2116_hardware_stimulation(&config).is_err());
        config.waveform = "biphasic".to_owned();
        config.amplitude_na = 21;
        assert!(rhs2116_hardware_stimulation(&config).is_err());
    }

    #[test]
    fn mea64_routes_every_address_once_and_rejects_unknown_labels() {
        let mut addresses = HashSet::new();
        let mut config = test_stimulation_config();
        config.profile_id = "mea-ab-64".to_owned();
        config.channel_ids.clear();
        for port in ['B', 'C'] {
            for channel in 0..32 {
                let id = format!("{port}-{channel:03}");
                assert!(addresses.insert(mea64_hardware_address(&id).unwrap()));
                config.channel_ids.push(id);
            }
        }
        assert_eq!(addresses.len(), 64);
        assert_eq!(mea64_hardware_address("C-031").unwrap(), (5, 15));
        for id in ["A-000", "B-032", "B-00", "C-0X1", "电极"] {
            assert!(mea64_hardware_address(id).is_err());
        }
        let hardware = rhs2116_hardware_stimulation(&config).unwrap();
        assert_eq!(
            hardware.stream_masks,
            [0, 0, 0xffff, 0xffff, 0xffff, 0xffff, 0, 0]
        );
    }

    #[test]
    fn hardware_arm_checks_confirmation_preload_layout_and_stop_latch() {
        let mut config = test_stimulation_config();
        config.profile_id = "mea-ab-64".to_owned();
        config.channel_ids = vec!["B-000".to_owned(), "C-031".to_owned()];
        let hardware = rhs2116_hardware_stimulation(&config).unwrap();
        let mut info = test_acquisition_info(64);
        info.streams = vec![2, 3, 4, 5];
        assert!(validate_hardware_arm(&config, &info, Some(&hardware), false).is_err());
        config.bench_confirmation = Some(BenchConfirmation {
            sample_disconnected: true,
            dummy_load_connected: true,
            measurement_ready: true,
            operator: "bench-test".to_owned(),
            functional_only: false,
            functional_repeat: false,
        });
        validate_hardware_arm(&config, &info, Some(&hardware), false).unwrap();
        assert!(validate_hardware_arm(&config, &info, Some(&hardware), true).is_err());
        assert!(validate_hardware_arm(&config, &info, None, false).is_err());
        info.streams.swap(0, 1);
        assert!(validate_hardware_arm(&config, &info, Some(&hardware), false).is_err());
    }

    #[test]
    fn hardware_parameters_are_representable_and_sequences_are_finite() {
        let mut config = test_stimulation_config();
        config.amplitude_na = 10_000;
        config.phase_width_us = 250;
        config.interphase_us = 0;
        let hardware = rhs2116_hardware_stimulation(&config).unwrap();
        assert_eq!(
            hardware.magnitude_steps as u32 * hardware.stim_step_size.current_na(),
            10_000
        );
        assert_eq!(hardware.phase_steps, 8);
        assert_eq!(hardware.interphase_steps, 0);
        config.pulse_count = 21;
        assert!(rhs2116_hardware_stimulation(&config).is_err());
        config.pulse_count = 1;
        config.frequency_hz = 0.1;
        assert!(rhs2116_hardware_stimulation(&config).is_err());
    }

    #[test]
    fn functional_arm_requires_no_bio_one_channel_and_a_fixed_low_single_pulse() {
        let mut config = test_stimulation_config();
        config.profile_id = "mea-ab-64".into();
        config.channel_ids = vec!["B-000".into()];
        config.bench_confirmation = Some(BenchConfirmation {
            sample_disconnected: true,
            dummy_load_connected: false,
            measurement_ready: false,
            operator: "functional-test".into(),
            functional_only: true,
            functional_repeat: false,
        });
        let mut info = test_acquisition_info(64);
        info.streams = vec![2, 3, 4, 5];
        let check = |config: &StimulationConfigRequest| {
            let hardware = rhs2116_hardware_stimulation(config).unwrap();
            validate_hardware_arm(config, &info, Some(&hardware), false)
        };
        check(&config).unwrap();
        let mut repeated = config.clone();
        repeated
            .bench_confirmation
            .as_mut()
            .unwrap()
            .functional_repeat = true;
        check(&repeated).unwrap();
        repeated
            .bench_confirmation
            .as_mut()
            .unwrap()
            .sample_disconnected = false;
        assert!(check(&repeated).is_err());
        let mut changed = config.clone();
        changed.bench_confirmation.as_mut().unwrap().functional_only = false;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed
            .bench_confirmation
            .as_mut()
            .unwrap()
            .sample_disconnected = false;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.channel_ids.push("C-000".into());
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.amplitude_na = 20;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.pulse_count = 5;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.phase_width_us = 1000;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.interphase_us = 200;
        assert!(check(&changed).is_err());
        changed = config.clone();
        changed.frequency_hz = 20.0;
        assert!(check(&changed).is_err());
    }

    #[test]
    fn functional_schedule_enforces_explicit_budget_and_cooldown() {
        let mut schedule = StimulationScheduleRequest {
            delay_ms: 0,
            repeat_count: 1,
            repeat_interval_ms: 1000,
        };
        validate_functional_schedule(&schedule, 0, 1, None).unwrap();
        assert!(validate_functional_schedule(&schedule, 1, 1, None).is_err());
        for submitted in 0..3 {
            validate_functional_schedule(&schedule, submitted, 3, Some(Duration::from_secs(1)))
                .unwrap();
        }
        assert!(validate_functional_schedule(&schedule, 3, 3, None).is_err());
        assert!(validate_functional_schedule(&schedule, 0, 100, None).is_err());
        assert!(
            validate_functional_schedule(&schedule, 1, 3, Some(Duration::from_millis(999)))
                .is_err()
        );
        schedule.repeat_count = 2;
        assert!(validate_functional_schedule(&schedule, 0, 3, None).is_err());
        schedule.repeat_count = 1;
        schedule.delay_ms = 1001;
        assert!(validate_functional_schedule(&schedule, 0, 3, None).is_err());
        let mut view = StimulationView::default();
        assert!(!view.hardware_output());
        view.output_mode = "hardware-functional".into();
        assert!(view.hardware_output());
        let confirmation: BenchConfirmation = serde_json::from_value(serde_json::json!({"sampleDisconnected":true,"dummyLoadConnected":true,"measurementReady":true,"operator":"old-client"})).unwrap();
        assert!(!confirmation.functional_only);
        assert!(!confirmation.functional_repeat);
    }

    #[test]
    fn hardware_schedule_rejects_overlap_excessive_queue_and_arm_overrun() {
        let hardware = StimConfig::default();
        let mut schedule = StimulationScheduleRequest {
            delay_ms: 0,
            repeat_count: 16,
            repeat_interval_ms: 1000,
        };
        validate_hardware_schedule(&schedule, Some(&hardware)).unwrap();
        schedule.repeat_count = 17;
        assert!(validate_hardware_schedule(&schedule, Some(&hardware)).is_err());
        schedule.repeat_count = 2;
        schedule.repeat_interval_ms = 100;
        assert!(validate_hardware_schedule(&schedule, Some(&hardware)).is_err());
        schedule.repeat_interval_ms = 60_000;
        assert!(validate_hardware_schedule(&schedule, Some(&hardware)).is_err());
    }

    #[test]
    fn repeated_functional_arm_tracks_submission_budget_without_resetting_it() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let mut stimulation = SoftwareStimulationState::default();
        let mut config = test_stimulation_config();
        config.channel_ids = vec!["CH-01".to_owned()];
        config.bench_confirmation = Some(BenchConfirmation {
            sample_disconnected: true,
            dummy_load_connected: false,
            measurement_ready: false,
            operator: "offline-test".to_owned(),
            functional_only: true,
            functional_repeat: true,
        });
        let view = arm_software_stimulation(&mut stimulation, config.clone(), 16, &shared).unwrap();
        assert_eq!(view.functional_trigger_limit, 3);
        for submitted in 0..3 {
            let schedule = StimulationScheduleRequest {
                delay_ms: 0,
                repeat_count: 1,
                repeat_interval_ms: 1000,
            };
            validate_functional_schedule(&schedule, stimulation.view.submitted_commands, 3, None)
                .unwrap();
            let view = schedule_software_stimulation(
                &mut stimulation,
                schedule,
                submitted * 30_000,
                30_000,
                &shared,
            )
            .unwrap();
            assert_eq!(view.submitted_commands, submitted + 1);
            stimulation.pending.clear();
            stimulation.view.pending_commands = 0;
            stimulation.view.completed_commands += 1;
        }
        let schedule = StimulationScheduleRequest {
            delay_ms: 0,
            repeat_count: 1,
            repeat_interval_ms: 1000,
        };
        assert!(validate_functional_schedule(
            &schedule,
            stimulation.view.submitted_commands,
            3,
            None
        )
        .is_err());
        assert!(arm_software_stimulation(&mut stimulation, config, 16, &shared).is_err());
        assert_eq!(stimulation.view.submitted_commands, 3);
    }

    #[test]
    fn repeated_arm_does_not_reset_an_existing_session() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let mut stimulation = SoftwareStimulationState::default();
        arm_software_stimulation(&mut stimulation, test_stimulation_config(), 16, &shared).unwrap();
        let arm_id = stimulation.view.arm_id;
        stimulation.view.completed_commands = 1;
        stimulation.arm_expires_at = Some(Instant::now() + Duration::from_secs(60));
        let expires = stimulation.arm_expires_at;
        let error =
            arm_software_stimulation(&mut stimulation, test_stimulation_config(), 16, &shared)
                .unwrap_err();
        assert!(error.contains("already armed"));
        assert_eq!(stimulation.view.arm_id, arm_id);
        assert_eq!(stimulation.view.completed_commands, 1);
        assert_eq!(stimulation.arm_expires_at, expires);
        disarm_software_stimulation(&mut stimulation, &shared).unwrap();
        arm_software_stimulation(&mut stimulation, test_stimulation_config(), 16, &shared).unwrap();
        assert_ne!(stimulation.view.arm_id, arm_id);
    }

    #[test]
    fn interlocked_stimulation_scheduler_tracks_targets_and_completion() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let mut stimulation = SoftwareStimulationState::default();
        let armed =
            arm_software_stimulation(&mut stimulation, test_stimulation_config(), 16, &shared)
                .unwrap();
        assert_eq!(armed.state, "armed");
        assert_eq!(armed.output_mode, "interlocked");
        assert_eq!(armed.selected_channels, 2);

        let scheduled = schedule_software_stimulation(
            &mut stimulation,
            StimulationScheduleRequest {
                delay_ms: 10,
                repeat_count: 3,
                repeat_interval_ms: 20,
            },
            1_000,
            30_000,
            &shared,
        )
        .unwrap();
        assert_eq!(scheduled.pending_commands, 3);
        assert_eq!(stimulation.pending[0].target_sample_index, 1_300);
        assert_eq!(stimulation.pending[1].target_sample_index, 1_900);
        assert_eq!(stimulation.pending[2].target_sample_index, 2_500);

        process_software_stimulation(&mut stimulation, &trigger_test_block(13, 2), &shared);
        assert_eq!(stimulation.view.completed_commands, 1);
        process_software_stimulation(&mut stimulation, &trigger_test_block(25, 2), &shared);
        assert_eq!(stimulation.view.completed_commands, 3);
        assert_eq!(stimulation.view.pending_commands, 0);
        assert_eq!(stimulation.view.state, "completed");
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .events
                .iter()
                .filter(|event| event.category == "stimulation" && event.level == "event")
                .count(),
            3
        );
    }

    #[test]
    fn stimulation_validation_rejects_unsafe_or_impossible_configs() {
        let mut config = test_stimulation_config();
        config.charge_recovery = false;
        assert!(validate_stimulation_config(&config)
            .unwrap_err()
            .contains("balanced biphasic"));
        config.charge_recovery = true;
        config.frequency_hz = 1_000.0;
        config.phase_width_us = 600;
        assert!(validate_stimulation_config(&config)
            .unwrap_err()
            .contains("do not fit"));
    }

    #[test]
    fn sixty_four_channel_stimulation_cannot_arm_on_a_sixteen_channel_session() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let mut stimulation = SoftwareStimulationState::default();
        let mut config = test_stimulation_config();
        config.profile_id = "mea-ab-64".to_owned();
        config.channel_ids = vec!["A-000".to_owned(), "B-031".to_owned()];
        let error = arm_software_stimulation(&mut stimulation, config, 16, &shared).unwrap_err();
        assert!(error.contains("requires 64 physical channels"));
        assert!(stimulation.config.is_none());
        assert_eq!(stimulation.view.state, "disarmed");
    }

    #[test]
    fn rhs2116_stimulation_labels_map_exactly_to_hardware_channels() {
        for hardware_channel in 0..16_u8 {
            let label = format!("CH-{:02}", hardware_channel + 1);
            assert_eq!(rhs2116_hardware_channel(&label).unwrap(), hardware_channel);
        }
        for invalid in ["CH-00", "CH-17", "CH-1", "CH-001", "A-000", "CH-0x"] {
            assert!(rhs2116_hardware_channel(invalid).is_err());
        }
        let mut config = test_stimulation_config();
        config.channel_ids = vec!["CH-17".to_owned()];
        assert!(validate_stimulation_config(&config).is_err());
    }

    #[test]
    fn io_input_monitor_counts_bit_transitions() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let mut block = trigger_test_block(0, 2);
        block.ttl_in[10] = 1;
        block.ttl_in[11] = 3;
        block.ttl_in[12] = 1;
        for value in &mut block.ttl_in[13..] {
            *value = 1;
        }
        update_io_inputs(&block, &shared);
        let state = shared.lock().unwrap();
        assert_eq!(state.io.digital_input, 1);
        assert_eq!(state.io.digital_edges, 3);
    }

    #[test]
    fn terminal_stimulation_fault_preserves_completed_counts_and_audit_without_rearming() {
        let mut state = SoftwareStimulationState::default();
        state.view.state = "triggered".to_owned();
        state.view.output_mode = "hardware-configurable".to_owned();
        state.view.arm_id = Some(1);
        state.view.pending_commands = 1;
        state.view.submitted_commands = 3;
        state.view.completed_commands = 2;
        state.view.last_command_id = Some(3);
        state.view.audit_path = Some("test-audit.jsonl".to_owned());
        finish_stimulation_session(&mut state, true);
        assert_eq!(state.view.state, "fault");
        assert_eq!(state.view.output_mode, "interlocked");
        assert_eq!(state.view.arm_id, None);
        assert_eq!(state.view.pending_commands, 0);
        assert_eq!(state.view.submitted_commands, 3);
        assert_eq!(state.view.completed_commands, 2);
        assert_eq!(state.view.last_command_id, Some(3));
        assert_eq!(state.view.audit_path.as_deref(), Some("test-audit.jsonl"));
        assert!(!state.view.hardware_output());
        finish_stimulation_session(&mut state, false);
        assert_eq!(state.view.state, "disarmed");
        assert_eq!(state.view.completed_commands, 0);
    }

    #[test]
    fn impedance_requests_validate_confirmation_profiles_and_all_physical_routes() {
        let mut request = ImpedanceRequest {
            frequency_hz: 1000.0,
            channel_ids: vec!["CH-01".to_owned(), "CH-16".to_owned()],
            profile_id: "rhs2116-logic-16".to_owned(),
            bitfile: None,
            serial: None,
            confirmed_test_signal: false,
        };
        assert!(validate_impedance_scan(&request).is_err());
        request.confirmed_test_signal = true;
        assert_eq!(
            validate_impedance_scan(&request).unwrap(),
            (16, vec![0, 15])
        );
        request.profile_id = "mea-ab-64".to_owned();
        request.channel_ids = ["B", "C"]
            .iter()
            .flat_map(|port| (0..32).map(move |i| format!("{port}-{i:03}")))
            .collect();
        assert_eq!(
            validate_impedance_scan(&request).unwrap(),
            (64, (0..64).collect())
        );
        request.channel_ids.push("B-000".to_owned());
        assert!(validate_impedance_scan(&request).is_err());
        request.channel_ids = vec!["A-000".to_owned()];
        assert!(validate_impedance_scan(&request).is_err());
        request.channel_ids = vec!["B-000".to_owned()];
        for frequency in [f64::NAN, 9.0, 2001.0] {
            request.frequency_hz = frequency;
            assert!(validate_impedance_scan(&request).is_err());
        }
        request.frequency_hz = 1000.0;
        request.profile_id = "mea-abcd-120".to_owned();
        assert!(validate_impedance_scan(&request).is_err());
    }

    #[test]
    fn impedance_running_and_cleanup_exclude_new_hardware_operations() {
        let state = DesktopState::default();
        assert!(ensure_session_idle(&state).is_ok());
        for phase in ["running", "cancelling"] {
            state.shared.lock().unwrap().impedance.state = phase.to_owned();
            assert!(ensure_session_idle(&state).is_err());
        }
        state.shared.lock().unwrap().impedance.state = "complete".to_owned();
        assert!(ensure_session_idle(&state).is_ok());
    }

    #[test]
    fn remote_protocol_requires_version_and_token() {
        let shared = Arc::new(Mutex::new(DesktopShared::default()));
        let response = remote_protocol_response(
            RemoteProtocolRequest {
                version: 1,
                token: "correct-token".to_owned(),
                command: "status".to_owned(),
                after_id: None,
            },
            "correct-token",
            &shared,
        );
        assert_eq!(response["ok"], true);
        assert_eq!(response["data"]["remoteApi"]["protocolVersion"], 1);

        let unauthorized = remote_protocol_response(
            RemoteProtocolRequest {
                version: 1,
                token: "wrong-token".to_owned(),
                command: "events".to_owned(),
                after_id: Some(0),
            },
            "correct-token",
            &shared,
        );
        assert_eq!(unauthorized["ok"], false);
        assert_eq!(unauthorized["error"], "unauthorized");
    }
}
