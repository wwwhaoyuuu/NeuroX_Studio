use std::error::Error as StdError;
use std::fmt;

pub const SAMPLE_BLOCK_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleLayout {
    SampleMajorInterleaved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SampleFlags(u32);

impl SampleFlags {
    pub const DISCONTINUITY: u32 = 1 << 0;
    pub const STIM_ARTIFACT: u32 = 1 << 1;
    pub const DEVICE_WARNING: u32 = 1 << 2;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    pub fn insert(&mut self, flag: u32) {
        self.0 |= flag;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelId {
    pub stream: u8,
    pub channel: u8,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StimStepSize {
    N10 = 1,
    N20 = 2,
    N50 = 3,
    N100 = 4,
    N200 = 5,
    N500 = 6,
    U1 = 7,
    U2 = 8,
    U5 = 9,
    U10 = 10,
}

impl StimStepSize {
    pub const fn label(self) -> &'static str {
        match self {
            Self::N10 => "10 nA",
            Self::N20 => "20 nA",
            Self::N50 => "50 nA",
            Self::N100 => "100 nA",
            Self::N200 => "200 nA",
            Self::N500 => "500 nA",
            Self::U1 => "1 uA",
            Self::U2 => "2 uA",
            Self::U5 => "5 uA",
            Self::U10 => "10 uA",
        }
    }

    pub const fn current_na(self) -> u32 {
        match self {
            Self::N10 => 10,
            Self::N20 => 20,
            Self::N50 => 50,
            Self::N100 => 100,
            Self::N200 => 200,
            Self::N500 => 500,
            Self::U1 => 1_000,
            Self::U2 => 2_000,
            Self::U5 => 5_000,
            Self::U10 => 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleBlock {
    pub format_version: u16,
    pub source_id: String,
    pub sequence: u64,
    pub first_sample_index: u64,
    pub sample_rate_hz: u32,
    pub layout: SampleLayout,
    pub channels: Vec<ChannelId>,
    pub samples_per_channel: u32,
    pub samples: Vec<u16>,
    pub ttl_in: Vec<u16>,
    pub flags: SampleFlags,
}

impl SampleBlock {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: impl Into<String>,
        sequence: u64,
        first_sample_index: u64,
        sample_rate_hz: u32,
        channels: Vec<ChannelId>,
        samples_per_channel: u32,
        samples: Vec<u16>,
        ttl_in: Vec<u16>,
        flags: SampleFlags,
    ) -> Result<Self, CoreError> {
        let block = Self {
            format_version: SAMPLE_BLOCK_FORMAT_VERSION,
            source_id: source_id.into(),
            sequence,
            first_sample_index,
            sample_rate_hz,
            layout: SampleLayout::SampleMajorInterleaved,
            channels,
            samples_per_channel,
            samples,
            ttl_in,
            flags,
        };
        block.validate()?;
        Ok(block)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.format_version != SAMPLE_BLOCK_FORMAT_VERSION {
            return Err(CoreError::new(format!(
                "unsupported SampleBlock version: {}",
                self.format_version
            )));
        }
        if self.source_id.is_empty() {
            return Err(CoreError::new("source_id must not be empty"));
        }
        if self.sample_rate_hz == 0 {
            return Err(CoreError::new("sample_rate_hz must be greater than zero"));
        }
        if self.channels.is_empty() {
            return Err(CoreError::new("at least one channel is required"));
        }
        if self.samples_per_channel == 0 {
            return Err(CoreError::new(
                "samples_per_channel must be greater than zero",
            ));
        }

        let expected_samples = self
            .channels
            .len()
            .checked_mul(self.samples_per_channel as usize)
            .ok_or_else(|| CoreError::new("sample dimensions overflow usize"))?;
        if self.samples.len() != expected_samples {
            return Err(CoreError::new(format!(
                "sample length mismatch: expected {expected_samples}, got {}",
                self.samples.len()
            )));
        }
        if !self.ttl_in.is_empty() && self.ttl_in.len() != self.samples_per_channel as usize {
            return Err(CoreError::new(format!(
                "ttl length mismatch: expected {} or 0, got {}",
                self.samples_per_channel,
                self.ttl_in.len()
            )));
        }
        Ok(())
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn sample(&self, channel: usize, sample: usize) -> Option<u16> {
        if channel >= self.channel_count() || sample >= self.samples_per_channel as usize {
            return None;
        }
        self.samples
            .get(sample * self.channel_count() + channel)
            .copied()
    }

    pub fn last_sample_index(&self) -> u64 {
        self.first_sample_index
            .saturating_add(self.samples_per_channel as u64 - 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    DeviceConnected,
    DeviceDisconnected,
    AcquisitionStarted,
    AcquisitionStopped,
    DataDiscontinuity,
    QueueOverflow,
    DeviceFault,
    ThresholdCrossing,
    StimRequested,
    StimExecuted,
    StimRejected,
    StimCancelled,
    StimFailed,
    StimArmed,
    StimDisarmed,
    EmergencyStop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub sample_index: Option<u64>,
    pub kind: EventKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceErrorCode {
    InvalidArgument,
    BitfileMissing,
    NoDevice,
    OpenFailed,
    FpgaConfigurationFailed,
    ChipNotFound,
    NotRunning,
    BufferTooSmall,
    UsbReadFailed,
    InvalidHeader,
    ProtocolError,
    WorkerStopped,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError {
    pub code: DeviceErrorCode,
    pub message: String,
}

impl DeviceError {
    pub fn new(code: DeviceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl StdError for DeviceError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError {
    message: String,
}

impl CoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for CoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(count: u8) -> Vec<ChannelId> {
        (0..count)
            .map(|channel| ChannelId { stream: 6, channel })
            .collect()
    }

    #[test]
    fn validates_and_indexes_sample_major_data() {
        let block = SampleBlock::new(
            "synthetic",
            4,
            100,
            30_000,
            channels(2),
            3,
            vec![10, 11, 20, 21, 30, 31],
            vec![0, 1, 0],
            SampleFlags::empty(),
        )
        .unwrap();

        assert_eq!(block.sample(0, 0), Some(10));
        assert_eq!(block.sample(1, 2), Some(31));
        assert_eq!(block.sample(2, 0), None);
        assert_eq!(block.last_sample_index(), 102);
    }

    #[test]
    fn rejects_sample_length_mismatch() {
        let error = SampleBlock::new(
            "synthetic",
            0,
            0,
            30_000,
            channels(2),
            3,
            vec![1, 2, 3],
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("sample length mismatch"));
    }

    #[test]
    fn rejects_ttl_length_mismatch() {
        let error = SampleBlock::new(
            "synthetic",
            0,
            0,
            30_000,
            channels(1),
            2,
            vec![1, 2],
            vec![0],
            SampleFlags::empty(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("ttl length mismatch"));
    }

    #[test]
    fn sample_flags_are_composable() {
        let mut flags = SampleFlags::empty();
        flags.insert(SampleFlags::DISCONTINUITY);
        flags.insert(SampleFlags::DEVICE_WARNING);

        assert!(flags.contains(SampleFlags::DISCONTINUITY));
        assert!(flags.contains(SampleFlags::DEVICE_WARNING));
        assert!(!flags.contains(SampleFlags::STIM_ARTIFACT));
    }
}
