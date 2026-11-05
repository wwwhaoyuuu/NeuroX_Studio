use super::{c_optional, c_path, validate_stim_config, RawStimConfig, RhsDevice, StimConfig};
use rhs_core::{ChannelId, DeviceError, DeviceErrorCode, SampleBlock, SampleFlags};
use std::collections::{BTreeSet, VecDeque};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_STIM_COMMAND_IDS_PER_WORKER: usize = 4096;

#[repr(C)]
struct RawImpedanceResult {
    return_code: c_int,
    stream: c_int,
    channel: c_int,
    quality: c_int,
    frequency_hz: f64,
    magnitude_ohms: f64,
    phase_degrees: f64,
    message: [c_char; 512],
}

#[derive(Debug, Clone)]
pub struct ImpedanceMeasurement {
    pub index: usize,
    pub stream: u8,
    pub channel: u8,
    pub frequency_hz: f64,
    pub magnitude_ohms: f64,
    pub phase_degrees: f64,
    pub quality: &'static str,
}

struct ImpedanceSession(NonNull<std::ffi::c_void>);
impl Drop for ImpedanceSession {
    fn drop(&mut self) {
        unsafe { rhs_impedance_close(self.0.as_ptr()) };
    }
}

extern "C" fn impedance_cancelled(context: *mut std::ffi::c_void) -> c_int {
    // The synchronous native call borrows this AtomicBool until it returns.
    unsafe { &*context.cast::<AtomicBool>() }.load(Ordering::Acquire) as c_int
}

impl RhsDevice {
    pub fn measure_impedances(
        &self,
        channel_count: usize,
        frequency: f64,
        indices: &[usize],
        cancelled: &AtomicBool,
        mut progress: impl FnMut(ImpedanceMeasurement),
    ) -> Result<bool, DeviceError> {
        validate_impedance_request(channel_count, frequency, indices)?;
        if cancelled.load(Ordering::Acquire) {
            return Ok(false);
        }
        let bitfile = c_path(&self.bitfile).map_err(|error| {
            DeviceError::new(DeviceErrorCode::InvalidArgument, error.to_string())
        })?;
        let serial = c_optional(&self.serial).map_err(|error| {
            DeviceError::new(DeviceErrorCode::InvalidArgument, error.to_string())
        })?;
        let mut raw = std::ptr::null_mut();
        let mut info = RawAcquisitionInfo::empty();
        let code = unsafe {
            rhs_impedance_open(
                bitfile.as_ptr(),
                serial.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                channel_count as c_int,
                frequency,
                &mut raw,
                &mut info,
            )
        };
        if code != 1 {
            return Err(error_from_code(code, info.message_string()));
        }
        let session = ImpedanceSession(NonNull::new(raw).ok_or_else(|| {
            DeviceError::new(DeviceErrorCode::Internal, "missing impedance session")
        })?);
        for &index in indices {
            if cancelled.load(Ordering::Acquire) {
                return Ok(false);
            }
            let mut result = RawImpedanceResult {
                return_code: 0,
                stream: 0,
                channel: 0,
                quality: 0,
                frequency_hz: 0.0,
                magnitude_ohms: 0.0,
                phase_degrees: 0.0,
                message: [0; 512],
            };
            let code = unsafe {
                rhs_impedance_measure(
                    session.0.as_ptr(),
                    index as c_int,
                    impedance_cancelled,
                    (cancelled as *const AtomicBool).cast_mut().cast(),
                    &mut result,
                )
            };
            if code == -2 {
                return Ok(false);
            }
            if code != 1 {
                return Err(error_from_code(
                    code,
                    unsafe { CStr::from_ptr(result.message.as_ptr()) }
                        .to_string_lossy()
                        .into_owned(),
                ));
            }
            if !(0..8).contains(&result.stream)
                || !(0..16).contains(&result.channel)
                || !result.frequency_hz.is_finite()
                || !result.magnitude_ohms.is_finite()
                || !result.phase_degrees.is_finite()
            {
                return Err(DeviceError::new(
                    DeviceErrorCode::ProtocolError,
                    "invalid impedance result",
                ));
            }
            progress(ImpedanceMeasurement {
                index,
                stream: result.stream as u8,
                channel: result.channel as u8,
                frequency_hz: result.frequency_hz,
                magnitude_ohms: result.magnitude_ohms,
                phase_degrees: result.phase_degrees,
                quality: match result.quality {
                    0 => "measured",
                    1 => "saturated",
                    2 => "low-signal",
                    _ => "invalid",
                },
            });
        }
        Ok(!cancelled.load(Ordering::Acquire))
    }
}

fn validate_impedance_request(
    channels: usize,
    frequency: f64,
    indices: &[usize],
) -> Result<(), DeviceError> {
    let unique: BTreeSet<_> = indices.iter().collect();
    if !matches!(channels, 16 | 64)
        || !frequency.is_finite()
        || !(10.0..=2000.0).contains(&frequency)
        || indices.is_empty()
        || unique.len() != indices.len()
        || indices.iter().any(|i| *i >= channels)
    {
        return Err(DeviceError::new(
            DeviceErrorCode::InvalidArgument,
            "impedance requires 16/64-channel layout, unique valid channels and 10..2000 Hz",
        ));
    }
    Ok(())
}

#[repr(C)]
struct RawAcquisitionSession {
    _private: [u8; 0],
}

#[repr(C)]
struct RawAcquisitionInfo {
    return_code: c_int,
    sample_rate_hz: c_int,
    channel_count: c_int,
    samples_per_block: c_int,
    blocks_per_read: c_int,
    target_stream: c_int,
    target_port: c_int,
    bytes_per_block: c_int,
    pipe_read_error: c_int,
    device_serial: [c_char; 64],
    message: [c_char; 512],
}

#[repr(C)]
struct RawAcquisitionReadResult {
    return_code: c_int,
    block_count: c_int,
    samples_per_channel: c_int,
    invalid_header_count: c_int,
    first_timestamp: u32,
    last_timestamp: u32,
    pipe_read_error: c_int,
    bytes_read: c_int,
    message: [c_char; 512],
}

#[repr(C)]
struct RawAcquisitionStimResult {
    return_code: c_int,
    active: c_int,
    pipe_read_error: c_int,
    message: [c_char; 512],
}

#[link(name = "rhs_bridge", kind = "static")]
extern "C" {
    fn rhs_impedance_open(
        bitfile: *const c_char,
        serial: *const c_char,
        channels: c_int,
        frequency: f64,
        out: *mut *mut std::ffi::c_void,
        info: *mut RawAcquisitionInfo,
    ) -> c_int;
    fn rhs_impedance_measure(
        session: *mut std::ffi::c_void,
        index: c_int,
        cancelled: extern "C" fn(*mut std::ffi::c_void) -> c_int,
        context: *mut std::ffi::c_void,
        result: *mut RawImpedanceResult,
    ) -> c_int;
    fn rhs_impedance_close(session: *mut std::ffi::c_void);
    fn rhs_acquisition_open_channels(
        bitfile_path: *const c_char,
        serial: *const c_char,
        blocks_per_read: c_int,
        channel_count: c_int,
        out_session: *mut *mut RawAcquisitionSession,
        out_info: *mut RawAcquisitionInfo,
    ) -> c_int;

    fn rhs_acquisition_streams(
        session: *mut RawAcquisitionSession,
        streams: *mut c_int,
        capacity: usize,
    ) -> c_int;

    fn rhs_acquisition_start(
        session: *mut RawAcquisitionSession,
        out_info: *mut RawAcquisitionInfo,
    ) -> c_int;

    fn rhs_acquisition_configure_stim(
        session: *mut RawAcquisitionSession,
        config: *const RawStimConfig,
        out_result: *mut RawAcquisitionStimResult,
    ) -> c_int;

    fn rhs_acquisition_trigger_stim(
        session: *mut RawAcquisitionSession,
        out_result: *mut RawAcquisitionStimResult,
    ) -> c_int;

    fn rhs_acquisition_cancel_stim(
        session: *mut RawAcquisitionSession,
        out_result: *mut RawAcquisitionStimResult,
    ) -> c_int;

    fn rhs_acquisition_read(
        session: *mut RawAcquisitionSession,
        timestamps: *mut u32,
        timestamp_capacity: usize,
        amplifier_samples: *mut u16,
        amplifier_capacity: usize,
        ttl_in: *mut u16,
        ttl_capacity: usize,
        stim_on: *mut u64,
        stim_on_capacity: usize,
        stim_polarity: *mut u64,
        stim_polarity_capacity: usize,
        compliance_limit: *mut u64,
        compliance_capacity: usize,
        dc_amplifier_samples: *mut u16,
        dc_amplifier_capacity: usize,
        out_result: *mut RawAcquisitionReadResult,
    ) -> c_int;

    fn rhs_acquisition_stop(
        session: *mut RawAcquisitionSession,
        out_info: *mut RawAcquisitionInfo,
    ) -> c_int;

    fn rhs_acquisition_close(session: *mut RawAcquisitionSession);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcquisitionConfig {
    pub blocks_per_read: u8,
}

impl Default for AcquisitionConfig {
    fn default() -> Self {
        Self { blocks_per_read: 8 }
    }
}

impl AcquisitionConfig {
    fn validate(self) -> Result<Self, DeviceError> {
        if !(1..=32).contains(&self.blocks_per_read) {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidArgument,
                "blocks_per_read must be 1..32",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionInfo {
    pub sample_rate_hz: u32,
    pub channel_count: usize,
    pub samples_per_block: usize,
    pub blocks_per_read: usize,
    pub target_stream: u8,
    pub target_port: u8,
    pub streams: Vec<u8>,
    pub bytes_per_block: usize,
    pub pipe_read_error: i32,
    pub device_serial: String,
    pub message: String,
}

#[derive(Debug)]
pub struct AcquisitionBatch {
    pub timestamps: Vec<u32>,
    pub amplifier_samples: Vec<u16>,
    pub ttl_in: Vec<u16>,
    pub stim_on: Vec<u64>,
    pub stim_polarity: Vec<u64>,
    pub compliance_limit: Vec<u64>,
    pub dc_amplifier_samples: Vec<u16>,
    pub bytes_read: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionStimState {
    Queued,
    Triggered,
    Observed,
    Completed,
    Cancelled,
    EmergencyStopped,
    Rejected,
    Fault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledStimRequest {
    pub command_id: u64,
    pub target_sample_index: u64,
    pub deadline_sample_index: u64,
}

impl ScheduledStimRequest {
    fn validate(&self) -> Result<(), DeviceError> {
        if self.command_id == 0 {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidArgument,
                "stimulation command_id must be non-zero",
            ));
        }
        if self.deadline_sample_index < self.target_sample_index {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidArgument,
                "stimulation deadline must not precede its target sample",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionStimEvent {
    pub command_id: u64,
    pub state: AcquisitionStimState,
    pub target_sample_index: u64,
    pub sample_index: Option<u64>,
    pub message: String,
}

struct AcquisitionSession {
    raw: NonNull<RawAcquisitionSession>,
    info: AcquisitionInfo,
    running: bool,
}

impl AcquisitionSession {
    fn open(
        device: &RhsDevice,
        config: AcquisitionConfig,
        channel_count: usize,
    ) -> Result<Self, DeviceError> {
        let config = config.validate()?;
        let bitfile = c_path(&device.bitfile).map_err(|error| {
            DeviceError::new(DeviceErrorCode::BitfileMissing, error.to_string())
        })?;
        let serial = c_optional(&device.serial).map_err(|error| {
            DeviceError::new(DeviceErrorCode::InvalidArgument, error.to_string())
        })?;
        let mut raw_session = std::ptr::null_mut();
        let mut raw_info = RawAcquisitionInfo::empty();

        let code = unsafe {
            rhs_acquisition_open_channels(
                bitfile.as_ptr(),
                serial
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                config.blocks_per_read as c_int,
                channel_count as c_int,
                &mut raw_session,
                &mut raw_info,
            )
        };
        if code != 1 {
            return Err(error_from_code(code, raw_info.message_string()));
        }
        let raw = NonNull::new(raw_session).ok_or_else(|| {
            DeviceError::new(
                DeviceErrorCode::Internal,
                "acquisition open succeeded without returning a session",
            )
        })?;
        let info = raw_info.try_into_info()?;
        let mut session = Self {
            raw,
            info,
            running: false,
        };
        let mut streams = [-1; 8];
        let count = unsafe {
            rhs_acquisition_streams(session.raw.as_ptr(), streams.as_mut_ptr(), streams.len())
        };
        if count < 1 || count as usize != channel_count / 16 {
            return Err(DeviceError::new(
                DeviceErrorCode::ProtocolError,
                "invalid stream layout returned by acquisition bridge",
            ));
        }
        session.info.streams = streams[..count as usize]
            .iter()
            .map(|stream| *stream as u8)
            .collect();
        Ok(session)
    }

    fn start(&mut self) -> Result<(), DeviceError> {
        let mut raw_info = RawAcquisitionInfo::empty();
        let code = unsafe { rhs_acquisition_start(self.raw.as_ptr(), &mut raw_info) };
        if code != 1 {
            return Err(error_from_code(code, raw_info.message_string()));
        }
        self.running = true;
        Ok(())
    }

    fn configure_stim(&mut self, config: &StimConfig) -> Result<(), DeviceError> {
        validate_stim_config(config).map_err(|error| {
            DeviceError::new(DeviceErrorCode::InvalidArgument, error.to_string())
        })?;
        let raw_config = RawStimConfig::from(config);
        let mut raw_result = RawAcquisitionStimResult::empty();
        let code = unsafe {
            rhs_acquisition_configure_stim(self.raw.as_ptr(), &raw_config, &mut raw_result)
        };
        if code != 1 {
            return Err(error_from_code(code, raw_result.message_string()));
        }
        Ok(())
    }

    fn trigger_stim(&mut self) -> Result<(), DeviceError> {
        let mut raw_result = RawAcquisitionStimResult::empty();
        let code = unsafe { rhs_acquisition_trigger_stim(self.raw.as_ptr(), &mut raw_result) };
        if code != 1 {
            return Err(error_from_code(code, raw_result.message_string()));
        }
        Ok(())
    }

    fn cancel_stim(&mut self) -> Result<(), DeviceError> {
        let mut raw_result = RawAcquisitionStimResult::empty();
        let code = unsafe { rhs_acquisition_cancel_stim(self.raw.as_ptr(), &mut raw_result) };
        if code != 1 {
            return Err(error_from_code(code, raw_result.message_string()));
        }
        Ok(())
    }

    fn read_batch(&mut self) -> Result<Option<AcquisitionBatch>, DeviceError> {
        if !self.running {
            return Err(DeviceError::new(
                DeviceErrorCode::NotRunning,
                "acquisition is not running",
            ));
        }

        let sample_count = self.info.samples_per_block * self.info.blocks_per_read;
        let amplifier_count = sample_count * self.info.channel_count;
        let mut timestamps = vec![0_u32; sample_count];
        let mut amplifier_samples = vec![0_u16; amplifier_count];
        let mut ttl_in = vec![0_u16; sample_count];
        let mut stim_on = vec![0_u64; sample_count];
        let mut stim_polarity = vec![0_u64; sample_count];
        let mut compliance_limit = vec![0_u64; sample_count];
        let mut dc_amplifier_samples = vec![0_u16; amplifier_count];
        let mut raw_result = RawAcquisitionReadResult::empty();

        let code = unsafe {
            rhs_acquisition_read(
                self.raw.as_ptr(),
                timestamps.as_mut_ptr(),
                timestamps.len(),
                amplifier_samples.as_mut_ptr(),
                amplifier_samples.len(),
                ttl_in.as_mut_ptr(),
                ttl_in.len(),
                stim_on.as_mut_ptr(),
                stim_on.len(),
                stim_polarity.as_mut_ptr(),
                stim_polarity.len(),
                compliance_limit.as_mut_ptr(),
                compliance_limit.len(),
                dc_amplifier_samples.as_mut_ptr(),
                dc_amplifier_samples.len(),
                &mut raw_result,
            )
        };
        if code == 0 {
            return Ok(None);
        }
        if code != 1 {
            return Err(error_from_code(code, raw_result.message_string()));
        }
        if raw_result.samples_per_channel as usize != sample_count {
            return Err(DeviceError::new(
                DeviceErrorCode::ProtocolError,
                format!(
                    "batch sample count mismatch: expected {sample_count}, got {}",
                    raw_result.samples_per_channel
                ),
            ));
        }
        if raw_result.invalid_header_count != 0 {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidHeader,
                format!(
                    "batch contains {} invalid USB headers",
                    raw_result.invalid_header_count
                ),
            ));
        }

        Ok(Some(AcquisitionBatch {
            timestamps,
            amplifier_samples,
            ttl_in,
            stim_on,
            stim_polarity,
            compliance_limit,
            dc_amplifier_samples,
            bytes_read: raw_result.bytes_read.max(0) as usize,
        }))
    }

    fn stop(&mut self) -> Result<(), DeviceError> {
        if !self.running {
            return Ok(());
        }
        let mut raw_info = RawAcquisitionInfo::empty();
        let code = unsafe { rhs_acquisition_stop(self.raw.as_ptr(), &mut raw_info) };
        self.running = false;
        if code != 1 {
            return Err(error_from_code(code, raw_info.message_string()));
        }
        Ok(())
    }
}

impl Drop for AcquisitionSession {
    fn drop(&mut self) {
        unsafe { rhs_acquisition_close(self.raw.as_ptr()) };
    }
}

#[derive(Debug, Clone)]
pub struct AcquisitionWorkerConfig {
    pub channel_count: usize,
    pub acquisition: AcquisitionConfig,
    pub data_queue_capacity: usize,
    pub command_queue_capacity: usize,
    pub stim_event_queue_capacity: usize,
    pub idle_sleep: Duration,
    pub stimulation: Option<StimConfig>,
}

impl Default for AcquisitionWorkerConfig {
    fn default() -> Self {
        Self {
            channel_count: 16,
            acquisition: AcquisitionConfig::default(),
            data_queue_capacity: 8,
            command_queue_capacity: 8,
            stim_event_queue_capacity: 32,
            idle_sleep: Duration::from_micros(100),
            stimulation: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum WorkerStatus {
    Started(AcquisitionInfo),
    Stopped,
    Fault(DeviceError),
}

#[derive(Default)]
struct AcquisitionStatsInner {
    batches: AtomicU64,
    samples_per_channel: AtomicU64,
    bytes_read: AtomicU64,
    timestamp_gaps: AtomicU64,
    empty_reads: AtomicU64,
    queue_overflows: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AcquisitionStats {
    pub batches: u64,
    pub samples_per_channel: u64,
    pub bytes_read: u64,
    pub timestamp_gaps: u64,
    pub empty_reads: u64,
    pub queue_overflows: u64,
}

impl AcquisitionStatsInner {
    fn snapshot(&self) -> AcquisitionStats {
        AcquisitionStats {
            batches: self.batches.load(Ordering::Relaxed),
            samples_per_channel: self.samples_per_channel.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            timestamp_gaps: self.timestamp_gaps.load(Ordering::Relaxed),
            empty_reads: self.empty_reads.load(Ordering::Relaxed),
            queue_overflows: self.queue_overflows.load(Ordering::Relaxed),
        }
    }
}

enum WorkerCommand {
    ScheduleStim(ScheduledStimRequest),
    ScheduleBatch(Vec<ScheduledStimRequest>),
    CancelStim(u64),
    EmergencyStop,
    Stop,
}

pub struct AcquisitionHandle {
    info: AcquisitionInfo,
    data_rx: Receiver<SampleBlock>,
    status_rx: Receiver<WorkerStatus>,
    stim_event_rx: Receiver<AcquisitionStimEvent>,
    command_tx: SyncSender<WorkerCommand>,
    stats: Arc<AcquisitionStatsInner>,
    next_sample_index: Arc<AtomicU64>,
    emergency_latched: Arc<AtomicBool>,
    configured_stimulation: Option<StimConfig>,
    join: Option<JoinHandle<()>>,
}

impl AcquisitionHandle {
    pub fn info(&self) -> &AcquisitionInfo {
        &self.info
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SampleBlock, RecvTimeoutError> {
        self.data_rx.recv_timeout(timeout)
    }

    pub fn try_status(&self) -> Result<WorkerStatus, TryRecvError> {
        self.status_rx.try_recv()
    }

    pub fn try_fault(&self) -> Option<DeviceError> {
        drain_worker_fault(&self.status_rx)
    }

    pub fn stats(&self) -> AcquisitionStats {
        self.stats.snapshot()
    }

    pub fn next_sample_index(&self) -> u64 {
        self.next_sample_index.load(Ordering::Acquire)
    }

    pub fn configured_stimulation(&self) -> Option<&StimConfig> {
        self.configured_stimulation.as_ref()
    }

    pub fn schedule_stim(&self, request: ScheduledStimRequest) -> Result<(), DeviceError> {
        request.validate()?;
        if self.emergency_latched.load(Ordering::Acquire) {
            return Err(DeviceError::new(
                DeviceErrorCode::NotRunning,
                "stimulation emergency stop is latched; restart acquisition",
            ));
        }
        if self.configured_stimulation.is_none() {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidArgument,
                "acquisition worker has no preconfigured stimulation preset",
            ));
        }
        self.command_tx
            .try_send(WorkerCommand::ScheduleStim(request))
            .map_err(command_send_error)
    }

    pub fn cancel_stim(&self, command_id: u64) -> Result<(), DeviceError> {
        if command_id == 0 {
            return Err(DeviceError::new(
                DeviceErrorCode::InvalidArgument,
                "stimulation command_id must be non-zero",
            ));
        }
        self.command_tx
            .try_send(WorkerCommand::CancelStim(command_id))
            .map_err(command_send_error)
    }

    pub fn emergency_stop(&self) -> Result<(), DeviceError> {
        self.emergency_latched.store(true, Ordering::Release);
        Ok(())
    }

    pub fn stimulation_interlocked(&self) -> bool {
        self.emergency_latched.load(Ordering::Acquire)
    }

    pub fn schedule_stim_batch(
        &self,
        requests: Vec<ScheduledStimRequest>,
    ) -> Result<(), DeviceError> {
        if requests.is_empty()
            || requests.len() > 16
            || self.configured_stimulation.is_none()
            || self.stimulation_interlocked()
        {
            return Err(DeviceError::new(DeviceErrorCode::InvalidArgument, "hardware schedule requires 1..16 commands, a preset and an unlocked acquisition session"));
        }
        for request in &requests {
            request.validate()?;
        }
        self.command_tx
            .try_send(WorkerCommand::ScheduleBatch(requests))
            .map_err(command_send_error)
    }

    pub fn try_stim_event(&self) -> Result<AcquisitionStimEvent, TryRecvError> {
        self.stim_event_rx.try_recv()
    }

    pub fn recv_stim_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<AcquisitionStimEvent, RecvTimeoutError> {
        self.stim_event_rx.recv_timeout(timeout)
    }

    pub fn request_stop(&self) {
        self.emergency_latched.store(true, Ordering::Release);
        let _ = self.command_tx.send(WorkerCommand::Stop);
    }

    pub fn stop(mut self) -> Result<AcquisitionStats, DeviceError> {
        self.request_stop();
        if let Some(join) = self.join.take() {
            join.join().map_err(|payload| {
                let message = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic payload");
                DeviceError::new(
                    DeviceErrorCode::Internal,
                    format!("acquisition worker panicked: {message}"),
                )
            })?;
        }
        if let Some(error) = self.try_fault() {
            return Err(error);
        }
        Ok(self.stats())
    }
}

fn drain_worker_fault(status_rx: &Receiver<WorkerStatus>) -> Option<DeviceError> {
    // Started may still be queued when the data sender disconnects.
    status_rx.try_iter().find_map(|status| match status {
        WorkerStatus::Fault(error) => Some(error),
        _ => None,
    })
}

impl Drop for AcquisitionHandle {
    fn drop(&mut self) {
        self.emergency_latched.store(true, Ordering::Release);
        let _ = self.command_tx.send(WorkerCommand::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(super) fn spawn(
    device: RhsDevice,
    config: AcquisitionWorkerConfig,
) -> Result<AcquisitionHandle, DeviceError> {
    if config.data_queue_capacity == 0 {
        return Err(DeviceError::new(
            DeviceErrorCode::InvalidArgument,
            "data_queue_capacity must be greater than zero",
        ));
    }
    if config.command_queue_capacity == 0 || config.stim_event_queue_capacity == 0 {
        return Err(DeviceError::new(
            DeviceErrorCode::InvalidArgument,
            "command and stimulation event queue capacities must be greater than zero",
        ));
    }
    config.acquisition.validate()?;
    if !matches!(config.channel_count, 16 | 64) {
        return Err(DeviceError::new(
            DeviceErrorCode::InvalidArgument,
            "channel_count must be 16 or 64",
        ));
    }
    if let Some(stimulation) = &config.stimulation {
        validate_stim_config(stimulation).map_err(|error| {
            DeviceError::new(DeviceErrorCode::InvalidArgument, error.to_string())
        })?;
    }

    let (data_tx, data_rx) = mpsc::sync_channel(config.data_queue_capacity);
    let (status_tx, status_rx) = mpsc::channel();
    let (stim_event_tx, stim_event_rx) = mpsc::sync_channel(config.stim_event_queue_capacity);
    let (command_tx, command_rx) = mpsc::sync_channel(config.command_queue_capacity);
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    let stats = Arc::new(AcquisitionStatsInner::default());
    let thread_stats = Arc::clone(&stats);
    let next_sample_index = Arc::new(AtomicU64::new(0));
    let thread_next_sample_index = Arc::clone(&next_sample_index);
    let configured_stimulation = config.stimulation.clone();
    let emergency_latched = Arc::new(AtomicBool::new(false));
    let thread_emergency_latched = Arc::clone(&emergency_latched);

    let join = thread::Builder::new()
        .name("rhs-acquisition".to_string())
        .spawn(move || {
            run_worker(
                device,
                config,
                data_tx,
                status_tx,
                stim_event_tx,
                command_rx,
                startup_tx,
                thread_stats,
                thread_next_sample_index,
                thread_emergency_latched,
            );
        })
        .map_err(|error| {
            DeviceError::new(
                DeviceErrorCode::Internal,
                format!("failed to spawn acquisition worker: {error}"),
            )
        })?;

    match startup_rx.recv() {
        Ok(Ok(info)) => Ok(AcquisitionHandle {
            info,
            data_rx,
            status_rx,
            stim_event_rx,
            command_tx,
            stats,
            next_sample_index,
            emergency_latched,
            configured_stimulation,
            join: Some(join),
        }),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            let _ = join.join();
            Err(DeviceError::new(
                DeviceErrorCode::WorkerStopped,
                "acquisition worker exited during startup",
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    device: RhsDevice,
    config: AcquisitionWorkerConfig,
    data_tx: SyncSender<SampleBlock>,
    status_tx: mpsc::Sender<WorkerStatus>,
    stim_event_tx: SyncSender<AcquisitionStimEvent>,
    command_rx: Receiver<WorkerCommand>,
    startup_tx: SyncSender<Result<AcquisitionInfo, DeviceError>>,
    stats: Arc<AcquisitionStatsInner>,
    next_sample_index_shared: Arc<AtomicU64>,
    emergency_latched: Arc<AtomicBool>,
) {
    let mut session =
        match AcquisitionSession::open(&device, config.acquisition, config.channel_count) {
            Ok(session) => session,
            Err(error) => {
                let _ = startup_tx.send(Err(error));
                return;
            }
        };
    if let Some(stimulation) = &config.stimulation {
        if let Err(error) = session.configure_stim(stimulation) {
            let _ = startup_tx.send(Err(error));
            return;
        }
    }
    if let Err(error) = session.start() {
        let _ = startup_tx.send(Err(error));
        return;
    }

    let info = session.info.clone();
    let _ = startup_tx.send(Ok(info.clone()));
    run_acquisition_loop(
        session,
        info,
        config,
        data_tx,
        status_tx,
        stim_event_tx,
        command_rx,
        stats,
        next_sample_index_shared,
        emergency_latched,
    );
}

trait AcquisitionIo {
    fn read_batch(&mut self) -> Result<Option<AcquisitionBatch>, DeviceError>;
    fn trigger_stim(&mut self) -> Result<(), DeviceError>;
    fn cancel_stim(&mut self) -> Result<(), DeviceError>;
    fn stop(&mut self) -> Result<(), DeviceError>;
}

impl AcquisitionIo for AcquisitionSession {
    fn read_batch(&mut self) -> Result<Option<AcquisitionBatch>, DeviceError> {
        AcquisitionSession::read_batch(self)
    }
    fn trigger_stim(&mut self) -> Result<(), DeviceError> {
        AcquisitionSession::trigger_stim(self)
    }
    fn cancel_stim(&mut self) -> Result<(), DeviceError> {
        AcquisitionSession::cancel_stim(self)
    }
    fn stop(&mut self) -> Result<(), DeviceError> {
        AcquisitionSession::stop(self)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_acquisition_loop(
    mut session: impl AcquisitionIo,
    info: AcquisitionInfo,
    config: AcquisitionWorkerConfig,
    data_tx: SyncSender<SampleBlock>,
    status_tx: mpsc::Sender<WorkerStatus>,
    stim_event_tx: SyncSender<AcquisitionStimEvent>,
    command_rx: Receiver<WorkerCommand>,
    stats: Arc<AcquisitionStatsInner>,
    next_sample_index_shared: Arc<AtomicU64>,
    emergency_latched: Arc<AtomicBool>,
) {
    let _ = status_tx.send(WorkerStatus::Started(info.clone()));

    let mut tracker = TimestampTracker::default();
    let mut block_sequence = 0_u64;
    let channels: Vec<ChannelId> = (0..info.channel_count)
        .map(|channel| ChannelId {
            stream: info.streams[channel / 16],
            channel: (channel % 16) as u8,
        })
        .collect();
    let source_id = format!("rhs-port-{}", char::from(b'A' + info.target_port));
    let mut terminal_fault = None;
    let mut pending_stim = VecDeque::<ScheduledStimRequest>::new();
    let mut active_stim = None::<ActiveAcquisitionStim>;
    let mut seen_stim_ids = BTreeSet::<u64>::new();
    let mut next_sample_index = 0_u64;
    let configured_stimulation = config.stimulation.as_ref();
    let selected_mask =
        configured_stimulation.map_or(0, |stim| stimulation_sample_mask(stim, &info.streams));
    let mut emergency_seen = false;

    'worker: loop {
        loop {
            let command = if emergency_latched.load(Ordering::Acquire) && !emergency_seen {
                emergency_seen = true;
                Ok(WorkerCommand::EmergencyStop)
            } else {
                command_rx.try_recv()
            };
            match command {
                Ok(WorkerCommand::ScheduleBatch(requests)) => {
                    let result = if emergency_seen {
                        Err("emergency stop is latched")
                    } else {
                        enqueue_stim_batch(
                            &mut pending_stim,
                            &mut seen_stim_ids,
                            &requests,
                            next_sample_index,
                            config.command_queue_capacity,
                        )
                    };
                    for request in requests {
                        let event = AcquisitionStimEvent {
                            command_id: request.command_id,
                            state: if result.is_ok() {
                                AcquisitionStimState::Queued
                            } else {
                                AcquisitionStimState::Rejected
                            },
                            target_sample_index: request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: result
                                .err()
                                .unwrap_or("hardware schedule accepted atomically")
                                .to_owned(),
                        };
                        if let Err(error) = send_stim_event(&stim_event_tx, event) {
                            terminal_fault = Some(error);
                            break 'worker;
                        }
                    }
                }
                Ok(WorkerCommand::ScheduleStim(request)) => {
                    let event = if configured_stimulation.is_none() || emergency_seen {
                        AcquisitionStimEvent {
                            command_id: request.command_id,
                            state: AcquisitionStimState::Rejected,
                            target_sample_index: request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: "no stimulation preset was configured before acquisition"
                                .to_owned(),
                        }
                    } else {
                        match enqueue_stim_request(
                            &mut pending_stim,
                            &mut seen_stim_ids,
                            request.clone(),
                            next_sample_index,
                            config.command_queue_capacity,
                        ) {
                            Ok(()) => AcquisitionStimEvent {
                                command_id: request.command_id,
                                state: AcquisitionStimState::Queued,
                                target_sample_index: request.target_sample_index,
                                sample_index: None,
                                message: "stimulation command entered the bounded hardware queue"
                                    .to_owned(),
                            },
                            Err(message) => AcquisitionStimEvent {
                                command_id: request.command_id,
                                state: AcquisitionStimState::Rejected,
                                target_sample_index: request.target_sample_index,
                                sample_index: Some(next_sample_index),
                                message: message.to_owned(),
                            },
                        }
                    };
                    if let Err(error) = send_stim_event(&stim_event_tx, event) {
                        terminal_fault = Some(error);
                        break 'worker;
                    }
                }
                Ok(WorkerCommand::CancelStim(command_id)) => {
                    let event = if active_stim
                        .as_ref()
                        .is_some_and(|active| active.request.command_id == command_id)
                    {
                        if let Err(error) = session.cancel_stim() {
                            terminal_fault = Some(error);
                            break 'worker;
                        }
                        let active = active_stim.take().expect("active command was checked");
                        AcquisitionStimEvent {
                            command_id,
                            state: AcquisitionStimState::Cancelled,
                            target_sample_index: active.request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: "active stimulation cancelled while acquisition continued"
                                .to_owned(),
                        }
                    } else if let Some(position) = pending_stim
                        .iter()
                        .position(|request| request.command_id == command_id)
                    {
                        let request = pending_stim
                            .remove(position)
                            .expect("pending command position was checked");
                        AcquisitionStimEvent {
                            command_id,
                            state: AcquisitionStimState::Cancelled,
                            target_sample_index: request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: "queued stimulation command cancelled".to_owned(),
                        }
                    } else {
                        AcquisitionStimEvent {
                            command_id,
                            state: AcquisitionStimState::Rejected,
                            target_sample_index: 0,
                            sample_index: Some(next_sample_index),
                            message: "cannot cancel an unknown or completed stimulation command"
                                .to_owned(),
                        }
                    };
                    if let Err(error) = send_stim_event(&stim_event_tx, event) {
                        terminal_fault = Some(error);
                        break 'worker;
                    }
                }
                Ok(WorkerCommand::EmergencyStop) => {
                    if configured_stimulation.is_some() {
                        if let Err(error) = session.cancel_stim() {
                            terminal_fault = Some(error);
                            break 'worker;
                        }
                    }
                    if let Some(active) = active_stim.take() {
                        let event = AcquisitionStimEvent {
                            command_id: active.request.command_id,
                            state: AcquisitionStimState::EmergencyStopped,
                            target_sample_index: active.request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: "active stimulation stopped by Emergency Stop".to_owned(),
                        };
                        if let Err(error) = send_stim_event(&stim_event_tx, event) {
                            terminal_fault = Some(error);
                            break 'worker;
                        }
                    }
                    while let Some(request) = pending_stim.pop_front() {
                        let event = AcquisitionStimEvent {
                            command_id: request.command_id,
                            state: AcquisitionStimState::EmergencyStopped,
                            target_sample_index: request.target_sample_index,
                            sample_index: Some(next_sample_index),
                            message: "queued stimulation removed by Emergency Stop".to_owned(),
                        };
                        if let Err(error) = send_stim_event(&stim_event_tx, event) {
                            terminal_fault = Some(error);
                            break 'worker;
                        }
                    }
                }
                Ok(WorkerCommand::Stop) | Err(TryRecvError::Disconnected) => break 'worker,
                Err(TryRecvError::Empty) => break,
            }
        }

        if active_stim.is_none() && !emergency_latched.load(Ordering::Acquire) {
            while let Some(request) = pending_stim.front() {
                if request.deadline_sample_index < next_sample_index {
                    let request = pending_stim
                        .pop_front()
                        .expect("pending stimulation command was checked");
                    let event = AcquisitionStimEvent {
                        command_id: request.command_id,
                        state: AcquisitionStimState::Rejected,
                        target_sample_index: request.target_sample_index,
                        sample_index: Some(next_sample_index),
                        message: "stimulation command expired before its trigger".to_owned(),
                    };
                    if let Err(error) = send_stim_event(&stim_event_tx, event) {
                        terminal_fault = Some(error);
                        break 'worker;
                    }
                    continue;
                }
                if request.target_sample_index > next_sample_index {
                    break;
                }

                let request = pending_stim
                    .pop_front()
                    .expect("pending stimulation command was checked");
                if let Err(error) = session.trigger_stim() {
                    pending_stim.push_front(request);
                    terminal_fault = Some(error);
                    break 'worker;
                }
                let event = AcquisitionStimEvent {
                    command_id: request.command_id,
                    state: AcquisitionStimState::Triggered,
                    target_sample_index: request.target_sample_index,
                    sample_index: None,
                    message: format!(
                        "hardware trigger asserted after consuming sample {}",
                        next_sample_index.saturating_sub(1)
                    ),
                };
                if let Err(error) = send_stim_event(&stim_event_tx, event) {
                    terminal_fault = Some(error);
                    break 'worker;
                }
                active_stim = Some(ActiveAcquisitionStim {
                    request,
                    trigger_after_sample_index: next_sample_index,
                    first_observed_sample_index: None,
                    completion_sample_index: None,
                    observed_mask: 0,
                });
                break;
            }
        }

        let batch = match session.read_batch() {
            Ok(Some(batch)) => batch,
            Ok(None) => {
                stats.empty_reads.fetch_add(1, Ordering::Relaxed);
                thread::sleep(config.idle_sleep);
                continue;
            }
            Err(error) => {
                terminal_fault = Some(error);
                break;
            }
        };

        let mut first_sample_index = 0_u64;
        let mut discontinuity = false;
        let mut gaps = 0_u64;
        for (index, timestamp) in batch.timestamps.iter().copied().enumerate() {
            let observation = tracker.observe(timestamp);
            if index == 0 {
                first_sample_index = observation.sample_index;
            }
            if observation.discontinuity {
                discontinuity = true;
                gaps += 1;
            }
        }
        next_sample_index = first_sample_index.saturating_add(batch.timestamps.len() as u64);
        next_sample_index_shared.store(next_sample_index, Ordering::Release);

        let mut flags = SampleFlags::empty();
        if discontinuity {
            flags.insert(SampleFlags::DISCONTINUITY);
            if active_stim.is_some() || !pending_stim.is_empty() {
                emergency_latched.store(true, Ordering::Release);
                terminal_fault = Some(DeviceError::new(
                    DeviceErrorCode::ProtocolError,
                    "sample timestamp discontinuity while stimulation was pending; output disabled",
                ));
                break;
            }
        }
        if batch.stim_on.iter().any(|mask| *mask != 0) {
            flags.insert(SampleFlags::STIM_ARTIFACT);
        }

        if let Some((offset, compliance_mask)) = first_active_compliance(
            active_stim.is_some(),
            &batch.compliance_limit,
            selected_mask,
        ) {
            let sample_index = first_sample_index.saturating_add(offset as u64);
            let channels = compliance_channels(compliance_mask, &info.streams);
            emergency_latched.store(true, Ordering::Release);
            let active = active_stim.take();
            if configured_stimulation.is_some() {
                if let Err(error) = session.cancel_stim() {
                    terminal_fault = Some(error);
                    break;
                }
            }
            let event = AcquisitionStimEvent {
                command_id: active
                    .as_ref()
                    .map_or(0, |active| active.request.command_id),
                state: AcquisitionStimState::Fault,
                target_sample_index: active
                    .as_ref()
                    .map_or(0, |active| active.request.target_sample_index),
                sample_index: Some(sample_index),
                message: format!(
                    "RHS2116 compliance warning mask=0x{compliance_mask:016x}, channels={channels}; stimulation disabled"
                ),
            };
            if let Err(error) = send_stim_event(&stim_event_tx, event) {
                terminal_fault = Some(error);
                break;
            }
            terminal_fault = Some(DeviceError::new(
                DeviceErrorCode::ProtocolError,
                format!(
                    "RHS2116 compliance warning at sample {sample_index}, mask=0x{compliance_mask:016x}, channels={channels}; acquisition stopped and stimulation disabled"
                ),
            ));
            break;
        }

        if let (Some(active), Some(stimulation)) = (active_stim.as_mut(), configured_stimulation) {
            let channel_mask = stimulation_sample_mask(stimulation, &info.streams);
            active.observed_mask |= batch
                .stim_on
                .iter()
                .fold(0_u64, |mask, sample| mask | (*sample & channel_mask));
            if active.first_observed_sample_index.is_none() {
                if let Some(offset) = batch
                    .stim_on
                    .iter()
                    .position(|mask| mask & channel_mask != 0)
                {
                    let observed = first_sample_index.saturating_add(offset as u64);
                    active.first_observed_sample_index = Some(observed);
                    active.completion_sample_index =
                        Some(stimulation_completion_sample(stimulation, observed));
                    let event = AcquisitionStimEvent {
                        command_id: active.request.command_id,
                        state: AcquisitionStimState::Observed,
                        target_sample_index: active.request.target_sample_index,
                        sample_index: Some(observed),
                        message: format!(
                            "FPGA stim_on marker observed; scheduling error={} samples",
                            observed as i128 - active.request.target_sample_index as i128
                        ),
                    };
                    if let Err(error) = send_stim_event(&stim_event_tx, event) {
                        terminal_fault = Some(error);
                        break;
                    }
                }
            }
        }

        let completed = active_stim.as_ref().and_then(|active| {
            active.completion_sample_index.filter(|completion| {
                *completion < next_sample_index
                    && active.observed_mask & selected_mask == selected_mask
            })
        });
        if let Some(completion_sample_index) = completed {
            if let Err(error) = session.cancel_stim() {
                terminal_fault = Some(error);
                break;
            }
            let active = active_stim
                .take()
                .expect("completion came from active command");
            let event = AcquisitionStimEvent {
                command_id: active.request.command_id,
                state: AcquisitionStimState::Completed,
                target_sample_index: active.request.target_sample_index,
                sample_index: Some(completion_sample_index),
                message: "finite stimulation completed while acquisition remained continuous"
                    .to_owned(),
            };
            if let Err(error) = send_stim_event(&stim_event_tx, event) {
                terminal_fault = Some(error);
                break;
            }
        }

        if active_stim.as_ref().is_some_and(|active| {
            active.observed_mask & selected_mask != selected_mask
                && next_sample_index
                    > active
                        .trigger_after_sample_index
                        .saturating_add(info.sample_rate_hz as u64)
        }) {
            let active = active_stim.take().expect("active command was checked");
            if let Err(error) = session.cancel_stim() {
                terminal_fault = Some(error);
                break;
            }
            let event = AcquisitionStimEvent {
                command_id: active.request.command_id,
                state: AcquisitionStimState::Fault,
                target_sample_index: active.request.target_sample_index,
                sample_index: Some(next_sample_index),
                message: format!(
                    "missing FPGA stim_on markers mask=0x{:016x} within one second; stimulation disabled",
                    selected_mask & !active.observed_mask
                ),
            };
            if let Err(error) = send_stim_event(&stim_event_tx, event) {
                terminal_fault = Some(error);
                break;
            }
            terminal_fault = Some(DeviceError::new(
                DeviceErrorCode::ProtocolError,
                "stimulation trigger produced no FPGA stim_on marker",
            ));
            break;
        }

        let sample_count = batch.timestamps.len() as u32;
        let block = match SampleBlock::new(
            source_id.clone(),
            block_sequence,
            first_sample_index,
            info.sample_rate_hz,
            channels.clone(),
            sample_count,
            batch.amplifier_samples,
            batch.ttl_in,
            flags,
        ) {
            Ok(block) => block,
            Err(error) => {
                terminal_fault = Some(DeviceError::new(
                    DeviceErrorCode::ProtocolError,
                    format!("invalid SampleBlock produced by hardware: {error}"),
                ));
                break;
            }
        };

        match data_tx.try_send(block) {
            Ok(()) => {
                stats.batches.fetch_add(1, Ordering::Relaxed);
                stats
                    .samples_per_channel
                    .fetch_add(sample_count as u64, Ordering::Relaxed);
                stats
                    .bytes_read
                    .fetch_add(batch.bytes_read as u64, Ordering::Relaxed);
                stats.timestamp_gaps.fetch_add(gaps, Ordering::Relaxed);
                block_sequence += 1;
            }
            Err(TrySendError::Full(_)) => {
                stats.queue_overflows.fetch_add(1, Ordering::Relaxed);
                terminal_fault = Some(DeviceError::new(
                    DeviceErrorCode::ProtocolError,
                    "raw acquisition queue is full; acquisition stopped to avoid silent data loss",
                ));
                break;
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }

    if let Err(error) = session.stop() {
        if terminal_fault.is_none() {
            terminal_fault = Some(error);
        }
    }
    if let Some(error) = terminal_fault {
        emergency_latched.store(true, Ordering::Release);
        let requests = active_stim
            .into_iter()
            .map(|active| active.request)
            .chain(pending_stim);
        for request in requests {
            let _ = send_stim_event(
                &stim_event_tx,
                AcquisitionStimEvent {
                    command_id: request.command_id,
                    state: AcquisitionStimState::Fault,
                    target_sample_index: request.target_sample_index,
                    sample_index: Some(next_sample_index),
                    message: format!("acquisition stopped: {error}"),
                },
            );
        }
        let _ = status_tx.send(WorkerStatus::Fault(error));
    } else {
        let _ = status_tx.send(WorkerStatus::Stopped);
    }
}

#[cfg(test)]
#[path = "acquisition_worker_tests.rs"]
mod worker_tests;

struct ActiveAcquisitionStim {
    request: ScheduledStimRequest,
    trigger_after_sample_index: u64,
    first_observed_sample_index: Option<u64>,
    completion_sample_index: Option<u64>,
    observed_mask: u64,
}

fn send_stim_event(
    sender: &SyncSender<AcquisitionStimEvent>,
    event: AcquisitionStimEvent,
) -> Result<(), DeviceError> {
    sender.try_send(event).map_err(|error| match error {
        TrySendError::Full(_) => DeviceError::new(
            DeviceErrorCode::ProtocolError,
            "stimulation event queue is full; critical audit event could not be delivered",
        ),
        TrySendError::Disconnected(_) => DeviceError::new(
            DeviceErrorCode::WorkerStopped,
            "stimulation event receiver disconnected",
        ),
    })
}

fn command_send_error(error: TrySendError<WorkerCommand>) -> DeviceError {
    match error {
        TrySendError::Full(_) => DeviceError::new(
            DeviceErrorCode::ProtocolError,
            "hardware command queue is full",
        ),
        TrySendError::Disconnected(_) => DeviceError::new(
            DeviceErrorCode::WorkerStopped,
            "hardware worker command queue disconnected",
        ),
    }
}

fn enqueue_stim_request(
    pending: &mut VecDeque<ScheduledStimRequest>,
    seen_ids: &mut BTreeSet<u64>,
    request: ScheduledStimRequest,
    next_sample_index: u64,
    capacity: usize,
) -> Result<(), &'static str> {
    if seen_ids.contains(&request.command_id) {
        return Err("duplicate stimulation command id");
    }
    if seen_ids.len() >= MAX_STIM_COMMAND_IDS_PER_WORKER {
        return Err("stimulation command-id history limit reached; restart the worker");
    }
    seen_ids.insert(request.command_id);
    if request.deadline_sample_index < next_sample_index {
        return Err("stimulation command expired before entering the hardware queue");
    }
    if pending.len() >= capacity {
        return Err("bounded hardware stimulation queue is full");
    }
    let position = pending
        .iter()
        .position(|queued| queued.target_sample_index > request.target_sample_index)
        .unwrap_or(pending.len());
    pending.insert(position, request);
    Ok(())
}

fn enqueue_stim_batch(
    pending: &mut VecDeque<ScheduledStimRequest>,
    seen: &mut BTreeSet<u64>,
    requests: &[ScheduledStimRequest],
    next: u64,
    capacity: usize,
) -> Result<(), &'static str> {
    let mut staged_pending = pending.clone();
    let mut staged_seen = seen.clone();
    for request in requests {
        enqueue_stim_request(
            &mut staged_pending,
            &mut staged_seen,
            request.clone(),
            next,
            capacity,
        )?;
    }
    *pending = staged_pending;
    *seen = staged_seen;
    Ok(())
}

fn stimulation_completion_sample(config: &StimConfig, observed_start: u64) -> u64 {
    let end_stim_steps =
        u64::try_from(config.phase_steps * 2 + config.interphase_steps).unwrap_or(u64::MAX);
    let repeat_steps = u64::try_from(config.period_steps * (config.pulses - 1)).unwrap_or(u64::MAX);
    observed_start
        .saturating_add(repeat_steps)
        .saturating_add(end_stim_steps)
}

fn stimulation_sample_mask(config: &StimConfig, streams: &[u8]) -> u64 {
    if config.stream_masks.iter().any(|mask| *mask != 0) {
        streams.iter().enumerate().fold(0, |mask, (index, stream)| {
            mask | (u64::from(config.stream_masks[*stream as usize]) << (index * 16))
        })
    } else if config.channel_mask != 0 {
        u64::from(config.channel_mask)
    } else {
        1_u64 << config.channel
    }
}

fn first_active_compliance(active: bool, masks: &[u64], selected: u64) -> Option<(usize, u64)> {
    active.then(|| {
        masks
            .iter()
            .copied()
            .map(|mask| mask & selected)
            .enumerate()
            .find(|(_, mask)| *mask != 0)
    })?
}

fn compliance_channels(mask: u64, streams: &[u8]) -> String {
    (0..64)
        .filter(|index| mask & (1_u64 << index) != 0)
        .filter_map(|index| {
            streams.get(index / 16).map(|stream| {
                format!(
                    "{}-{:03}",
                    char::from(b'A' + stream / 2),
                    usize::from(stream % 2) * 16 + index % 16
                )
            })
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Default)]
struct TimestampTracker {
    last_raw: Option<u32>,
    last_unwrapped: u64,
}

struct TimestampObservation {
    sample_index: u64,
    discontinuity: bool,
}

impl TimestampTracker {
    fn observe(&mut self, raw: u32) -> TimestampObservation {
        let Some(last_raw) = self.last_raw else {
            self.last_raw = Some(raw);
            self.last_unwrapped = raw as u64;
            return TimestampObservation {
                sample_index: self.last_unwrapped,
                discontinuity: false,
            };
        };

        let delta = raw.wrapping_sub(last_raw) as u64;
        let discontinuity = delta != 1;
        self.last_unwrapped = self.last_unwrapped.saturating_add(delta);
        self.last_raw = Some(raw);
        TimestampObservation {
            sample_index: self.last_unwrapped,
            discontinuity,
        }
    }
}

impl RawAcquisitionInfo {
    fn empty() -> Self {
        Self {
            return_code: -1000,
            sample_rate_hz: 0,
            channel_count: 0,
            samples_per_block: 0,
            blocks_per_read: 0,
            target_stream: -1,
            target_port: -1,
            bytes_per_block: 0,
            pipe_read_error: 0,
            device_serial: [0; 64],
            message: [0; 512],
        }
    }

    fn message_string(&self) -> String {
        unsafe { CStr::from_ptr(self.message.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn try_into_info(&self) -> Result<AcquisitionInfo, DeviceError> {
        if self.sample_rate_hz <= 0
            || self.channel_count <= 0
            || self.samples_per_block <= 0
            || self.blocks_per_read <= 0
            || self.target_stream < 0
            || self.target_port < 0
            || self.bytes_per_block <= 0
        {
            return Err(DeviceError::new(
                DeviceErrorCode::ProtocolError,
                format!("invalid acquisition metadata: {}", self.message_string()),
            ));
        }
        Ok(AcquisitionInfo {
            sample_rate_hz: self.sample_rate_hz as u32,
            channel_count: self.channel_count as usize,
            samples_per_block: self.samples_per_block as usize,
            blocks_per_read: self.blocks_per_read as usize,
            target_stream: self.target_stream as u8,
            target_port: self.target_port as u8,
            streams: Vec::new(),
            bytes_per_block: self.bytes_per_block as usize,
            pipe_read_error: self.pipe_read_error,
            device_serial: unsafe { CStr::from_ptr(self.device_serial.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
            message: self.message_string(),
        })
    }
}

impl RawAcquisitionReadResult {
    fn empty() -> Self {
        Self {
            return_code: -1000,
            block_count: 0,
            samples_per_channel: 0,
            invalid_header_count: 0,
            first_timestamp: 0,
            last_timestamp: 0,
            pipe_read_error: 0,
            bytes_read: 0,
            message: [0; 512],
        }
    }

    fn message_string(&self) -> String {
        unsafe { CStr::from_ptr(self.message.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

impl RawAcquisitionStimResult {
    fn empty() -> Self {
        Self {
            return_code: -1000,
            active: 0,
            pipe_read_error: 0,
            message: [0; 512],
        }
    }

    fn message_string(&self) -> String {
        unsafe { CStr::from_ptr(self.message.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

fn error_from_code(code: i32, message: String) -> DeviceError {
    let error_code = match code {
        -1 => DeviceErrorCode::NoDevice,
        -2 => DeviceErrorCode::OpenFailed,
        -3 => DeviceErrorCode::FpgaConfigurationFailed,
        -102 | -103 => DeviceErrorCode::InvalidArgument,
        -104 => DeviceErrorCode::ProtocolError,
        -105 | -106 => DeviceErrorCode::ChipNotFound,
        -110 => DeviceErrorCode::NotRunning,
        -111 => DeviceErrorCode::BufferTooSmall,
        -112 => DeviceErrorCode::UsbReadFailed,
        -113 => DeviceErrorCode::InvalidHeader,
        _ => DeviceErrorCode::Internal,
    };
    DeviceError::new(error_code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info() -> AcquisitionInfo {
        AcquisitionInfo {
            sample_rate_hz: 30_000,
            channel_count: 16,
            samples_per_block: 128,
            blocks_per_read: 8,
            target_stream: 0,
            target_port: 0,
            streams: vec![0],
            bytes_per_block: 0,
            pipe_read_error: 0,
            device_serial: "offline-test".to_owned(),
            message: String::new(),
        }
    }

    #[test]
    fn terminal_fault_is_not_hidden_behind_started_status() {
        for code in [
            DeviceErrorCode::UsbReadFailed,
            DeviceErrorCode::ProtocolError,
        ] {
            let (tx, rx) = mpsc::channel();
            tx.send(WorkerStatus::Started(test_info())).unwrap();
            let error = DeviceError::new(code, "original terminal fault");
            tx.send(WorkerStatus::Fault(error.clone())).unwrap();
            drop(tx);
            assert_eq!(drain_worker_fault(&rx), Some(error));
            assert_eq!(drain_worker_fault(&rx), None);
        }
    }

    #[test]
    fn empty_or_clean_worker_status_does_not_invent_a_fault() {
        let (tx, rx) = mpsc::channel();
        assert_eq!(drain_worker_fault(&rx), None);
        tx.send(WorkerStatus::Started(test_info())).unwrap();
        tx.send(WorkerStatus::Stopped).unwrap();
        drop(tx);
        assert_eq!(drain_worker_fault(&rx), None);
    }

    fn stopped_test_handle(panics: bool) -> AcquisitionHandle {
        let (_data_tx, data_rx) = mpsc::sync_channel(1);
        let (_event_tx, stim_event_rx) = mpsc::sync_channel(1);
        let (command_tx, command_rx) = mpsc::sync_channel(4);
        let (status_tx, status_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            status_tx.send(WorkerStatus::Started(test_info())).unwrap();
            command_rx.recv().unwrap();
            assert!(!panics, "offline worker panic");
            status_tx
                .send(WorkerStatus::Fault(DeviceError::new(
                    DeviceErrorCode::UsbReadFailed,
                    "original USB fault",
                )))
                .unwrap();
        });
        AcquisitionHandle {
            info: test_info(),
            data_rx,
            status_rx,
            stim_event_rx,
            command_tx,
            stats: Arc::new(AcquisitionStatsInner::default()),
            next_sample_index: Arc::new(AtomicU64::new(0)),
            emergency_latched: Arc::new(AtomicBool::new(false)),
            configured_stimulation: None,
            join: Some(join),
        }
    }

    #[test]
    fn stop_returns_original_worker_fault_after_join() {
        let error = stopped_test_handle(false).stop().unwrap_err();
        assert_eq!(error.code, DeviceErrorCode::UsbReadFailed);
        assert_eq!(error.message, "original USB fault");
    }

    #[test]
    fn stop_preserves_worker_panic_detail() {
        let error = stopped_test_handle(true).stop().unwrap_err();
        assert!(error.message.contains("offline worker panic"));
    }

    #[test]
    fn sixty_four_bit_feedback_and_stream_selection_are_preserved() {
        let config = StimConfig {
            stream_masks: [0, 0, 1, 0, 0, 0x8000, 0, 0],
            ..StimConfig::default()
        };
        assert_eq!(
            stimulation_sample_mask(&config, &[2, 3, 4, 5]),
            1 | (1_u64 << 63)
        );
        assert_eq!(
            first_active_compliance(true, &[0, 1_u64 << 63], 1_u64 << 63),
            Some((1, 1_u64 << 63))
        );
        assert_eq!(
            first_active_compliance(true, &[1_u64 << 62], 1_u64 << 63),
            None
        );
    }

    #[test]
    fn hardware_batch_rejection_is_atomic() {
        let mut pending = VecDeque::new();
        let mut seen = BTreeSet::new();
        let requests = [
            ScheduledStimRequest {
                command_id: 1,
                target_sample_index: 100,
                deadline_sample_index: 200,
            },
            ScheduledStimRequest {
                command_id: 2,
                target_sample_index: 200,
                deadline_sample_index: 300,
            },
        ];
        assert!(enqueue_stim_batch(&mut pending, &mut seen, &requests, 0, 1).is_err());
        assert!(pending.is_empty());
        assert!(seen.is_empty());
        enqueue_stim_batch(&mut pending, &mut seen, &requests, 0, 2).unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn config_rejects_invalid_batch_size() {
        let error = AcquisitionConfig { blocks_per_read: 0 }
            .validate()
            .unwrap_err();
        assert_eq!(error.code, DeviceErrorCode::InvalidArgument);
    }

    #[test]
    fn scheduled_stim_request_requires_ordered_sample_window() {
        let request = ScheduledStimRequest {
            command_id: 1,
            target_sample_index: 100,
            deadline_sample_index: 99,
        };
        let error = request.validate().unwrap_err();
        assert_eq!(error.code, DeviceErrorCode::InvalidArgument);
    }

    #[test]
    fn impedance_input_validation_is_bounded_before_opening_hardware() {
        assert!(validate_impedance_request(64, 1000.0, &(0..64).collect::<Vec<_>>()).is_ok());
        assert!(validate_impedance_request(16, 10.0, &[0, 15]).is_ok());
        for (count, frequency, indices) in [
            (120, 1000.0, vec![0]),
            (16, 9.0, vec![0]),
            (64, f64::NAN, vec![0]),
            (64, 1000.0, vec![64]),
            (16, 1000.0, vec![0, 0]),
            (16, 1000.0, vec![]),
        ] {
            assert!(validate_impedance_request(count, frequency, &indices).is_err());
        }
    }

    #[test]
    fn cancelled_impedance_never_opens_hardware_or_reports_measurements() {
        let cancelled = AtomicBool::new(true);
        let device = RhsDevice::new("missing-impedance-cancellation-test.bit");
        assert!(!device
            .measure_impedances(64, 1000.0, &[0, 63], &cancelled, |_| panic!(
                "cancelled task must not report data"
            ))
            .unwrap());
    }

    #[test]
    fn compliance_addresses_use_selected_stream_order_including_high_bits() {
        assert_eq!(compliance_channels(1, &[2, 3, 4, 5]), "B-000");
        assert_eq!(
            compliance_channels((1_u64 << 63) | (1_u64 << 16), &[2, 3, 4, 5]),
            "B-016,C-031"
        );
        assert_eq!(compliance_channels(1_u64 << 15, &[6]), "D-015");
        assert_eq!(compliance_channels(1, &[3]), "B-016");
        assert_eq!(compliance_channels(0, &[2, 3, 4, 5]), "");
    }

    #[test]
    fn compliance_is_only_actionable_during_active_stimulation() {
        let masks = [0, 0x0004, 0x0008];
        assert_eq!(first_active_compliance(false, &masks, u64::MAX), None);
        assert_eq!(
            first_active_compliance(true, &masks, u64::MAX),
            Some((1, 0x0004))
        );
    }

    #[test]
    fn bounded_stim_queue_orders_targets_and_rejects_duplicates_and_overflow() {
        let mut pending = VecDeque::new();
        let mut seen = BTreeSet::new();
        let request = |command_id, target| ScheduledStimRequest {
            command_id,
            target_sample_index: target,
            deadline_sample_index: target + 10,
        };

        enqueue_stim_request(&mut pending, &mut seen, request(1, 200), 100, 2).unwrap();
        enqueue_stim_request(&mut pending, &mut seen, request(2, 150), 100, 2).unwrap();
        assert_eq!(pending[0].command_id, 2);
        assert_eq!(pending[1].command_id, 1);

        let duplicate =
            enqueue_stim_request(&mut pending, &mut seen, request(1, 250), 100, 2).unwrap_err();
        assert!(duplicate.contains("duplicate"));
        let overflow =
            enqueue_stim_request(&mut pending, &mut seen, request(3, 250), 100, 2).unwrap_err();
        assert!(overflow.contains("full"));
    }

    #[test]
    fn stim_queue_rejects_expired_command_before_trigger() {
        let mut pending = VecDeque::new();
        let mut seen = BTreeSet::new();
        let error = enqueue_stim_request(
            &mut pending,
            &mut seen,
            ScheduledStimRequest {
                command_id: 1,
                target_sample_index: 100,
                deadline_sample_index: 110,
            },
            111,
            2,
        )
        .unwrap_err();
        assert!(error.contains("expired"));
        assert!(pending.is_empty());
    }

    #[test]
    fn completion_sample_uses_observed_marker_and_finite_train_timing() {
        let single = StimConfig::default();
        assert_eq!(stimulation_completion_sample(&single, 10_000), 10_015);

        let train = StimConfig {
            pulses: 5,
            ..StimConfig::default()
        };
        assert_eq!(stimulation_completion_sample(&train, 10_000), 22_015);
    }

    #[test]
    fn timestamp_tracker_accepts_u32_wraparound() {
        let mut tracker = TimestampTracker::default();
        let a = tracker.observe(u32::MAX - 1);
        let b = tracker.observe(u32::MAX);
        let c = tracker.observe(0);

        assert!(!a.discontinuity);
        assert!(!b.discontinuity);
        assert!(!c.discontinuity);
        assert_eq!(c.sample_index, u32::MAX as u64 + 1);
    }

    #[test]
    fn timestamp_tracker_marks_a_gap() {
        let mut tracker = TimestampTracker::default();
        tracker.observe(10);
        let observation = tracker.observe(12);

        assert!(observation.discontinuity);
        assert_eq!(observation.sample_index, 12);
    }
}
