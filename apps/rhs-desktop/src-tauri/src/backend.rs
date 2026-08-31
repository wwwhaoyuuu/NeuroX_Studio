use rhs_core::SampleBlock;
use rhs_device::{
    AcquisitionConfig, AcquisitionHandle, AcquisitionInfo, AcquisitionWorkerConfig, RhsDevice,
    WorkerStatus,
};
use rhs_nodes::{
    ArtifactBlanker, BandPassProcessor, BlankingConfig, BlankingReplacement, NotchProcessor,
    ThresholdConfig, ThresholdDirection, ThresholdEventReceiver, ThresholdProcessor,
};
use rhs_pipeline::{Processor, Sink};
use rhs_storage::RawRecorderSink;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};

const EVENT_LIMIT: usize = 200;
const WAVEFORM_POINTS: usize = 360;
const PIPELINE_SCHEMA_VERSION: u32 = 1;
const RHS_CHANNEL_COUNT: usize = 16;
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformFrame {
    sequence: u64,
    first_sample_index: u64,
    sample_rate_hz: u32,
    bin_width_samples: u32,
    point_count: usize,
    channel_count: usize,
    minimums: Vec<u16>,
    maximums: Vec<u16>,
    flags: u32,
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingView {
    path: String,
    blocks: u64,
    complete: bool,
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
    last_error: Option<String>,
    latest_waveform: Option<WaveformFrame>,
    events: VecDeque<EventView>,
    next_event_id: u64,
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
            last_error: None,
            latest_waveform: None,
            events: VecDeque::new(),
            next_event_id: 1,
        }
    }
}

impl DesktopShared {
    fn elapsed_ms(&self) -> u64 {
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
            stimulation_interlock: "bench-validation-required".to_owned(),
            last_error: self.last_error.clone(),
        }
    }

    fn push_event(&mut self, level: &str, category: &str, message: impl Into<String>) {
        let event = EventView {
            id: self.next_event_id,
            elapsed_ms: self.elapsed_ms(),
            level: level.to_owned(),
            category: category.to_owned(),
            message: message.into(),
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
        reply: mpsc::Sender<Result<RecordingView, String>>,
    },
    StopRecording {
        reply: mpsc::Sender<Result<RecordingView, String>>,
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

pub struct DesktopState {
    shared: Arc<Mutex<DesktopShared>>,
    session: Mutex<Option<SessionRuntime>>,
}

impl Default for DesktopState {
    fn default() -> Self {
        let mut shared = DesktopShared::default();
        shared.push_event(
            "info",
            "system",
            "RHS Studio ready; stimulation output remains interlocked",
        );
        Self {
            shared: Arc::new(Mutex::new(shared)),
            session: Mutex::new(None),
        }
    }
}

impl DesktopState {
    pub fn shutdown(&self) {
        let runtime = self
            .session
            .lock()
            .ok()
            .and_then(|mut session| session.take());
        if let Some(runtime) = runtime {
            let _ = runtime.commands.send(SessionCommand::Stop);
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
pub async fn scan_device(app: AppHandle, request: DeviceRequest) -> Result<DeviceView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        ensure_session_idle(&state)?;
        update_mode(&state.shared, "connecting", "nominal", None)?;
        let device = make_device(&request.bitfile, &request.serial);
        let bitfile = device.bitfile_path().to_string_lossy().into_owned();
        let report = device.scan().map_err(|error| error.to_string())?;
        if report.return_code != 0 || report.chips.is_empty() {
            let message = format!("device scan failed: {}", report.message);
            update_mode(&state.shared, "fault", "fault", Some(message.clone()))?;
            return Err(message);
        }
        let chip = &report.chips[0];
        let view = DeviceView {
            serial: request.serial.unwrap_or_else(|| "auto-detected".to_owned()),
            chip_name: chip.chip_name.to_owned(),
            port: chip.port_name.to_string(),
            stream: chip.command_stream as u8,
            channels: report.channels_on_port[chip.port_index as usize] as usize,
            sample_rate_hz: 30_000,
            bitfile,
        };
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
    if !(1..=32).contains(&request.blocks_per_read) {
        return Err("blocksPerRead must be between 1 and 32".to_owned());
    }
    if request.queue_capacity == 0 || request.queue_capacity > 128 {
        return Err("queueCapacity must be between 1 and 128".to_owned());
    }
    let state = app.state::<DesktopState>();
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
            acquisition: AcquisitionConfig {
                blocks_per_read: request.blocks_per_read,
            },
            data_queue_capacity: request.queue_capacity,
            stimulation: None,
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
        .spawn(move || run_session(handle, info, pipeline, command_rx, shared))
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
    shared.last_error = None;
    shared.latest_waveform = None;
    shared.push_event(
        "success",
        "acquisition",
        format!(
            "Acquisition started at {} Hz, {} channels; stimulation disabled",
            device_view.sample_rate_hz, device_view.channels
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
pub async fn start_recording(app: AppHandle, path: String) -> Result<RecordingView, String> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err("recording path must not be empty".to_owned());
    }
    request_session(&app, move |reply| SessionCommand::StartRecording {
        path,
        reply,
    })
    .await
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> Result<RecordingView, String> {
    request_session(&app, |reply| SessionCommand::StopRecording { reply }).await
}

#[tauri::command]
pub async fn update_pipeline(app: AppHandle, config: PipelineConfig) -> Result<StatusView, String> {
    let config = config.validate()?;
    let state = app.state::<DesktopState>();
    let command = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?
        .as_ref()
        .map(|runtime| runtime.commands.clone());
    if let Some(command) = command {
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
        let mut shared = state
            .shared
            .lock()
            .map_err(|_| "desktop state lock was poisoned".to_owned())?;
        shared.pipeline = config;
        shared.push_event("info", "pipeline", "Pipeline settings updated");
    }
    get_status(state)
}

#[tauri::command]
pub async fn emergency_stop(app: AppHandle) -> Result<StatusView, String> {
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

struct ConfiguredProcessor {
    node_id: String,
    processor: Box<dyn Processor>,
}

struct ProcessingChain {
    processors: Vec<ConfiguredProcessor>,
    threshold_events: Vec<ThresholdEventReceiver>,
}

impl ProcessingChain {
    fn new(config: &PipelineConfig) -> Result<Self, String> {
        config.validate_for(RHS_SAMPLE_RATE_HZ, RHS_CHANNEL_COUNT)?;
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
                    threshold_events.push(receiver);
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

struct ActiveRecording {
    sink: RawRecorderSink,
    path: PathBuf,
}

fn run_session(
    handle: AcquisitionHandle,
    info: AcquisitionInfo,
    pipeline: PipelineConfig,
    commands: Receiver<SessionCommand>,
    shared: Arc<Mutex<DesktopShared>>,
) {
    let mut processing = match ProcessingChain::new(&pipeline) {
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
    let mut stop_requested = false;
    let mut terminal_fault = None::<String>;

    while !stop_requested {
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::Stop) => {
                    stop_requested = true;
                    break;
                }
                Ok(SessionCommand::StartRecording { path, reply }) => {
                    let result = start_recorder(&mut recorder, &path, &info, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::StopRecording { reply }) => {
                    let result = finish_recorder(&mut recorder, &shared);
                    let _ = reply.send(result);
                }
                Ok(SessionCommand::UpdatePipeline { config, reply }) => {
                    let result = ProcessingChain::new(&config).map(|next| {
                        processing = next;
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
                    let result = handle.emergency_stop().map_err(|error| error.to_string());
                    if let Ok(mut state) = shared.lock() {
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
                let display = match processing.process(block) {
                    Ok(block) => block,
                    Err(error) => {
                        terminal_fault = Some(format!("display pipeline failed: {error}"));
                        break;
                    }
                };
                let frame = build_waveform_frame(&display, WAVEFORM_POINTS);
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
                    state.latest_waveform = Some(frame);
                }
                drain_threshold_events(&processing, &shared);
                drain_stimulation_events(&handle, &shared);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(WorkerStatus::Fault(error)) = handle.try_status() {
                    terminal_fault = Some(error.to_string());
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Ok(WorkerStatus::Fault(error)) = handle.try_status() {
                    terminal_fault = Some(error.to_string());
                } else {
                    terminal_fault = Some("acquisition worker disconnected".to_owned());
                }
                break;
            }
        }
    }

    if recorder.is_some() {
        if let Err(error) = finish_recorder(&mut recorder, &shared) {
            terminal_fault.get_or_insert_with(|| format!("recording finalize failed: {error}"));
        }
    }
    let stop_result = handle.stop();
    if let Err(error) = stop_result {
        terminal_fault.get_or_insert_with(|| error.to_string());
    }
    if let Ok(mut state) = shared.lock() {
        state.recording = false;
        state.recording_path = None;
        if let Some(error) = terminal_fault {
            state.set_fault(error);
        } else {
            state.mode = "ready";
            state.health = "nominal";
            state.push_event("info", "acquisition", "Acquisition stopped cleanly");
        }
    }
}

fn start_recorder(
    recorder: &mut Option<ActiveRecording>,
    path: &Path,
    info: &AcquisitionInfo,
    shared: &Arc<Mutex<DesktopShared>>,
) -> Result<RecordingView, String> {
    if recorder.is_some() {
        return Err("recording is already active".to_owned());
    }
    if path.extension().and_then(|value| value.to_str()) != Some("rhsraw") {
        return Err("recording path must use the .rhsraw extension".to_owned());
    }
    let sink = RawRecorderSink::create_with_metadata(
        path,
        vec![
            ("device_serial".to_owned(), info.device_serial.clone()),
            ("target_port".to_owned(), port_name(info.target_port)),
            ("target_stream".to_owned(), info.target_stream.to_string()),
            ("stimulation".to_owned(), "disabled".to_owned()),
            ("frontend".to_owned(), "rhs-studio-0.1.0".to_owned()),
        ],
    )
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
    for receiver in &processing.threshold_events {
        while let Ok(event) = receiver.try_recv() {
            if let Ok(mut state) = shared.lock() {
                state.metrics.threshold_events = state.metrics.threshold_events.saturating_add(1);
                state.push_event("event", "threshold", event.message);
            }
        }
    }
}

fn drain_stimulation_events(handle: &AcquisitionHandle, shared: &Arc<Mutex<DesktopShared>>) {
    while let Ok(event) = handle.try_stim_event() {
        if let Ok(mut state) = shared.lock() {
            state.push_event(
                "warning",
                "stimulation",
                format!("{:?}: {}", event.state, event.message),
            );
        }
    }
}

fn build_waveform_frame(block: &SampleBlock, max_points: usize) -> WaveformFrame {
    let input_points = block.samples_per_channel as usize;
    let bin_width = input_points.div_ceil(max_points).max(1);
    let point_count = input_points.div_ceil(bin_width);
    let channel_count = block.channel_count();
    let mut minimums = Vec::with_capacity(point_count * channel_count);
    let mut maximums = Vec::with_capacity(point_count * channel_count);
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
        }
    }
    WaveformFrame {
        sequence: block.sequence,
        first_sample_index: block.first_sample_index,
        sample_rate_hz: block.sample_rate_hz,
        bin_width_samples: bin_width as u32,
        point_count,
        channel_count,
        minimums,
        maximums,
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
    DeviceView {
        serial: info.device_serial.clone(),
        chip_name: "RHS2116".to_owned(),
        port: port_name(info.target_port),
        stream: info.target_stream,
        channels: info.channel_count,
        sample_rate_hz: info.sample_rate_hz,
        bitfile,
    }
}

fn port_name(port: u8) -> String {
    char::from(b'A'.saturating_add(port)).to_string()
}

fn ensure_session_idle(state: &DesktopState) -> Result<(), String> {
    let session = state
        .session
        .lock()
        .map_err(|_| "session lock was poisoned".to_owned())?;
    if session.is_some() {
        return Err("stop acquisition before scanning the device".to_owned());
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
        let frame = build_waveform_frame(&block, 10);
        assert_eq!(frame.point_count, 10);
        assert_eq!(frame.minimums[0], 0);
        assert_eq!(frame.maximums[0], 9);
        assert_eq!(frame.minimums[1], 1_000);
        assert_eq!(frame.maximums[1], 1_009);
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
}
