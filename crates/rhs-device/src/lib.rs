mod acquisition;

pub use acquisition::{
    AcquisitionBatch, AcquisitionConfig, AcquisitionHandle, AcquisitionInfo, AcquisitionStats,
    AcquisitionStimEvent, AcquisitionStimState, AcquisitionWorkerConfig, ImpedanceMeasurement,
    ScheduledStimRequest, WorkerStatus,
};
pub use rhs_core::{DeviceError, DeviceErrorCode, StimStepSize};

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

pub const DEFAULT_BITFILE: &str = "ConfigRHSController.bit";
const RHS_SCAN_MAX_RESULTS: usize = 8;

pub fn configured_bitfile() -> PathBuf {
    if let Some(path) = std::env::var_os("RHS_BITFILE").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let bundled = directory.join(DEFAULT_BITFILE);
            if bundled.is_file() {
                return bundled;
            }
        }
    }

    PathBuf::from(DEFAULT_BITFILE)
}

#[repr(C)]
struct RawScanResult {
    return_code: c_int,
    chip_count: c_int,
    chip_type: [c_int; RHS_SCAN_MAX_RESULTS],
    port_index: [c_int; RHS_SCAN_MAX_RESULTS],
    command_stream: [c_int; RHS_SCAN_MAX_RESULTS],
    channels_on_port: [c_int; 4],
    pipe_read_error: c_int,
    message: [c_char; 512],
}

#[repr(C)]
struct RawStimConfig {
    channel: c_int,
    channel_mask: c_int,
    trigger: c_int,
    pulses: c_int,
    magnitude_steps: c_int,
    stim_step_size: c_int,
    phase_steps: c_int,
    interphase_steps: c_int,
    period_steps: c_int,
    stream_masks: [c_int; RHS_SCAN_MAX_RESULTS],
}

#[repr(C)]
struct RawStimSession {
    _private: [u8; 0],
}

#[link(name = "rhs_bridge", kind = "static")]
extern "C" {
    fn rhs_scan_connected_chips(
        bitfile_path: *const c_char,
        serial: *const c_char,
        out: *mut RawScanResult,
    ) -> c_int;

    fn rhs_stim_open(
        bitfile_path: *const c_char,
        serial: *const c_char,
        out_session: *mut *mut RawStimSession,
        out: *mut RawScanResult,
    ) -> c_int;

    fn rhs_stim_open_target(
        bitfile_path: *const c_char,
        serial: *const c_char,
        expected_port: c_int,
        expected_stream: c_int,
        out_session: *mut *mut RawStimSession,
        out: *mut RawScanResult,
    ) -> c_int;

    fn rhs_stim_start(
        session: *mut RawStimSession,
        config: *const RawStimConfig,
        out: *mut RawScanResult,
    ) -> c_int;

    fn rhs_stim_poll(session: *mut RawStimSession, out: *mut RawScanResult) -> c_int;

    fn rhs_stim_cancel(session: *mut RawStimSession, out: *mut RawScanResult) -> c_int;

    fn rhs_stim_close(session: *mut RawStimSession);

    #[cfg(feature = "legacy-direct-stim")]
    fn rhs_run_bounded_stim(
        bitfile_path: *const c_char,
        serial: *const c_char,
        config: *const RawStimConfig,
        out: *mut RawScanResult,
    ) -> c_int;
}

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone)]
pub struct ChipInfo {
    pub chip_type: i32,
    pub chip_name: &'static str,
    pub port_index: i32,
    pub port_name: char,
    pub command_stream: i32,
}

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub return_code: i32,
    pub pipe_read_error: i32,
    pub message: String,
    pub chips: Vec<ChipInfo>,
    pub channels_on_port: [i32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimConfig {
    pub channel: i32,
    pub channel_mask: u16,
    pub trigger: i32,
    pub pulses: i32,
    pub magnitude_steps: i32,
    pub stim_step_size: StimStepSize,
    pub phase_steps: i32,
    pub interphase_steps: i32,
    pub period_steps: i32,
    pub stream_masks: [u16; RHS_SCAN_MAX_RESULTS],
}

#[derive(Debug, Clone)]
pub enum StimPoll {
    Running(ScanReport),
    Completed(ScanReport),
}

pub struct StimHardwareSession {
    raw: NonNull<RawStimSession>,
    active: bool,
}

impl Default for StimConfig {
    fn default() -> Self {
        Self {
            channel: 0,
            channel_mask: 0,
            trigger: 0,
            pulses: 1,
            magnitude_steps: 1,
            stim_step_size: StimStepSize::N10,
            phase_steps: 6,
            interphase_steps: 3,
            period_steps: 3000,
            stream_masks: [0; RHS_SCAN_MAX_RESULTS],
        }
    }
}

impl StimConfig {
    pub fn validate(&self) -> Result<(), Error> {
        validate_stim_config(self)
    }
}

pub struct RhsDevice {
    bitfile: PathBuf,
    serial: Option<String>,
}

impl RhsDevice {
    pub fn new(bitfile: impl Into<PathBuf>) -> Self {
        Self {
            bitfile: bitfile.into(),
            serial: None,
        }
    }

    pub fn default_bitfile() -> Self {
        Self::new(configured_bitfile())
    }

    pub fn with_serial(mut self, serial: impl Into<String>) -> Self {
        self.serial = Some(serial.into());
        self
    }

    pub fn bitfile_path(&self) -> &Path {
        &self.bitfile
    }

    pub fn requested_serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn scan(&self) -> Result<ScanReport, Error> {
        let bitfile = c_path(&self.bitfile)?;
        let serial = c_optional(&self.serial)?;
        let mut raw = RawScanResult::empty();

        unsafe {
            rhs_scan_connected_chips(
                bitfile.as_ptr(),
                serial.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                &mut raw,
            );
        }

        Ok(raw.into_report())
    }

    pub fn open_stim_session(&self) -> Result<(StimHardwareSession, ScanReport), Error> {
        self.open_stim_session_at(None)
    }

    pub fn open_stim_session_target(
        &self,
        port_index: u8,
        stream: u8,
    ) -> Result<(StimHardwareSession, ScanReport), Error> {
        if port_index > 3 || stream > 7 {
            return Err(Error::new(
                "targeted stimulation port or stream is out of range",
            ));
        }
        self.open_stim_session_at(Some((port_index, stream)))
    }

    fn open_stim_session_at(
        &self,
        target: Option<(u8, u8)>,
    ) -> Result<(StimHardwareSession, ScanReport), Error> {
        let bitfile = c_path(&self.bitfile)?;
        let serial = c_optional(&self.serial)?;
        let mut raw_session = std::ptr::null_mut();
        let mut raw_report = RawScanResult::empty();
        let serial_ptr = serial.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        let return_code = unsafe {
            match target {
                Some((port, stream)) => rhs_stim_open_target(
                    bitfile.as_ptr(),
                    serial_ptr,
                    c_int::from(port),
                    c_int::from(stream),
                    &mut raw_session,
                    &mut raw_report,
                ),
                None => rhs_stim_open(
                    bitfile.as_ptr(),
                    serial_ptr,
                    &mut raw_session,
                    &mut raw_report,
                ),
            }
        };
        let report = raw_report.into_report();
        if return_code < 0 || report.return_code < 0 {
            if !raw_session.is_null() {
                unsafe { rhs_stim_close(raw_session) };
            }
            return Err(Error::new(format!(
                "failed to open stimulation session (code {return_code}): {}",
                report.message
            )));
        }
        let raw = NonNull::new(raw_session)
            .ok_or_else(|| Error::new("stimulation bridge returned a null session"))?;
        Ok((StimHardwareSession { raw, active: false }, report))
    }

    #[cfg(feature = "legacy-direct-stim")]
    pub fn run_bounded_stim(&self, config: StimConfig) -> Result<ScanReport, Error> {
        validate_stim_config(&config)?;

        let bitfile = c_path(&self.bitfile)?;
        let serial = c_optional(&self.serial)?;
        let raw_config = RawStimConfig {
            channel: config.channel,
            channel_mask: i32::from(config.channel_mask),
            trigger: config.trigger,
            pulses: config.pulses,
            magnitude_steps: config.magnitude_steps,
            stim_step_size: config.stim_step_size as i32,
            phase_steps: config.phase_steps,
            interphase_steps: config.interphase_steps,
            period_steps: config.period_steps,
            stream_masks: config.stream_masks.map(i32::from),
        };
        let mut raw = RawScanResult::empty();

        unsafe {
            rhs_run_bounded_stim(
                bitfile.as_ptr(),
                serial.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                &raw_config,
                &mut raw,
            );
        }

        Ok(raw.into_report())
    }

    pub fn spawn_acquisition(
        self,
        config: AcquisitionWorkerConfig,
    ) -> Result<AcquisitionHandle, DeviceError> {
        acquisition::spawn(self, config)
    }
}

impl StimHardwareSession {
    pub fn start(&mut self, config: StimConfig) -> Result<ScanReport, Error> {
        if self.active {
            return Err(Error::new("a stimulation command is already active"));
        }
        validate_stim_config(&config)?;
        if config.stream_masks.iter().any(|mask| *mask != 0) {
            return Err(Error::new(
                "multi-stream stimulation requires an acquisition session",
            ));
        }
        let raw_config = RawStimConfig::from(&config);
        let mut raw_report = RawScanResult::empty();
        let return_code =
            unsafe { rhs_stim_start(self.raw.as_ptr(), &raw_config, &mut raw_report) };
        let report = checked_stim_report("start", return_code, raw_report)?;
        if return_code != 2 {
            return Err(Error::new(format!(
                "stimulation start returned unexpected status {return_code}: {}",
                report.message
            )));
        }
        self.active = true;
        Ok(report)
    }

    pub fn poll(&mut self) -> Result<StimPoll, Error> {
        if !self.active {
            return Err(Error::new("no stimulation command is active"));
        }
        let mut raw_report = RawScanResult::empty();
        let return_code = unsafe { rhs_stim_poll(self.raw.as_ptr(), &mut raw_report) };
        let report = checked_stim_report("poll", return_code, raw_report)?;
        match return_code {
            2 => Ok(StimPoll::Running(report)),
            1 => {
                self.active = false;
                Ok(StimPoll::Completed(report))
            }
            _ => Err(Error::new(format!(
                "stimulation poll returned unexpected status {return_code}: {}",
                report.message
            ))),
        }
    }

    pub fn cancel(&mut self) -> Result<ScanReport, Error> {
        let mut raw_report = RawScanResult::empty();
        let return_code = unsafe { rhs_stim_cancel(self.raw.as_ptr(), &mut raw_report) };
        let report = checked_stim_report("cancel", return_code, raw_report)?;
        self.active = false;
        Ok(report)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for StimHardwareSession {
    fn drop(&mut self) {
        unsafe { rhs_stim_close(self.raw.as_ptr()) };
    }
}

impl From<&StimConfig> for RawStimConfig {
    fn from(config: &StimConfig) -> Self {
        Self {
            channel: config.channel,
            channel_mask: i32::from(config.channel_mask),
            trigger: config.trigger,
            pulses: config.pulses,
            magnitude_steps: config.magnitude_steps,
            stim_step_size: config.stim_step_size as i32,
            phase_steps: config.phase_steps,
            interphase_steps: config.interphase_steps,
            period_steps: config.period_steps,
            stream_masks: config.stream_masks.map(i32::from),
        }
    }
}

impl RawScanResult {
    fn empty() -> Self {
        Self {
            return_code: -1000,
            chip_count: 0,
            chip_type: [-1; RHS_SCAN_MAX_RESULTS],
            port_index: [-1; RHS_SCAN_MAX_RESULTS],
            command_stream: [-1; RHS_SCAN_MAX_RESULTS],
            channels_on_port: [0; 4],
            pipe_read_error: 0,
            message: [0; 512],
        }
    }

    fn into_report(self) -> ScanReport {
        let message = unsafe { CStr::from_ptr(self.message.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let mut chips = Vec::new();

        for i in 0..(self.chip_count as usize).min(RHS_SCAN_MAX_RESULTS) {
            let port_index = self.port_index[i];
            chips.push(ChipInfo {
                chip_type: self.chip_type[i],
                chip_name: chip_name(self.chip_type[i]),
                port_index,
                port_name: port_name(port_index),
                command_stream: self.command_stream[i],
            });
        }

        ScanReport {
            return_code: self.return_code,
            pipe_read_error: self.pipe_read_error,
            message,
            chips,
            channels_on_port: self.channels_on_port,
        }
    }
}

fn c_path(path: &Path) -> Result<CString, Error> {
    if !path.exists() {
        return Err(Error::new(format!(
            "bitfile does not exist: {}",
            path.display()
        )));
    }
    CString::new(path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| Error::new("path contains NUL byte"))
}

fn c_optional(value: &Option<String>) -> Result<Option<CString>, Error> {
    value
        .as_ref()
        .map(|s| CString::new(s.as_str()).map_err(|_| Error::new("serial contains NUL byte")))
        .transpose()
}

fn validate_stim_config(config: &StimConfig) -> Result<(), Error> {
    if !(0..=15).contains(&config.channel) {
        return Err(Error::new("channel must be 0..15"));
    }
    if config.channel_mask != 0 && config.channel_mask & (1_u16 << config.channel) == 0 {
        return Err(Error::new("channel_mask must include the primary channel"));
    }
    if !(0..=7).contains(&config.trigger) {
        return Err(Error::new("trigger must be 0..7"));
    }
    if !(1..=20).contains(&config.pulses) {
        return Err(Error::new("pulses must be 1..20"));
    }
    if !(1..=255).contains(&config.magnitude_steps) {
        return Err(Error::new("magnitude_steps must be 1..255"));
    }
    if !(1..=1000).contains(&config.phase_steps) {
        return Err(Error::new("phase_steps must be 1..1000"));
    }
    if !(0..=1000).contains(&config.interphase_steps) {
        return Err(Error::new("interphase_steps must be 0..1000"));
    }
    if !(10..=30_000).contains(&config.period_steps) {
        return Err(Error::new("period_steps must be 10..30000"));
    }
    let end_stim = i64::from(config.phase_steps) * 2 + i64::from(config.interphase_steps);
    let total_steps = i64::from(config.period_steps) * i64::from(config.pulses) + 256;
    if end_stim >= i64::from(config.period_steps) {
        return Err(Error::new(
            "both phases and interphase must fit inside one period",
        ));
    }
    if total_steps > 65_534 {
        return Err(Error::new(
            "stimulation sequence exceeds the FPGA timestep limit",
        ));
    }
    Ok(())
}

fn checked_stim_report(
    operation: &str,
    return_code: i32,
    raw_report: RawScanResult,
) -> Result<ScanReport, Error> {
    let report = raw_report.into_report();
    if return_code < 0 || report.return_code < 0 {
        return Err(Error::new(format!(
            "stimulation {operation} failed (code {return_code}): {}",
            report.message
        )));
    }
    Ok(report)
}

fn chip_name(chip_type: i32) -> &'static str {
    match chip_type {
        32 => "RHS2116",
        1 => "RHD2132",
        2 => "RHD2216",
        4 => "RHD2164",
        1000 => "RHD2164_MISO_B",
        -1 => "NoChip",
        _ => "Unknown",
    }
}

fn port_name(port_index: i32) -> char {
    if (0..=7).contains(&port_index) {
        (b'A' + port_index as u8) as char
    } else {
        '?'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stim_config_fits_bridge_limits() {
        validate_stim_config(&StimConfig::default()).unwrap();
    }

    #[test]
    fn rejects_stim_pulse_that_does_not_fit_period() {
        let config = StimConfig {
            phase_steps: 6,
            interphase_steps: 3,
            period_steps: 15,
            ..StimConfig::default()
        };
        let error = validate_stim_config(&config).unwrap_err();
        assert!(error.to_string().contains("fit inside one period"));
    }

    #[test]
    fn rejects_stim_sequence_beyond_fpga_limit() {
        let config = StimConfig {
            pulses: 20,
            period_steps: 3_300,
            ..StimConfig::default()
        };
        let error = validate_stim_config(&config).unwrap_err();
        assert!(error.to_string().contains("FPGA timestep limit"));
    }

    #[test]
    fn accepts_all_sixteen_channels_in_one_rhs2116_mask() {
        let config = StimConfig {
            channel: 0,
            channel_mask: u16::MAX,
            ..StimConfig::default()
        };
        validate_stim_config(&config).unwrap();
        assert_eq!(RawStimConfig::from(&config).channel_mask, 0xffff);
    }

    #[test]
    fn rejects_mask_that_excludes_primary_channel() {
        let config = StimConfig {
            channel: 3,
            channel_mask: 0b11,
            ..StimConfig::default()
        };
        assert!(validate_stim_config(&config)
            .unwrap_err()
            .to_string()
            .contains("include the primary channel"));
    }
}
