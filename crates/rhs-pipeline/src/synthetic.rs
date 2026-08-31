use rhs_core::{ChannelId, SampleBlock, SampleFlags};
use std::error::Error as StdError;
use std::fmt;

pub trait Source {
    fn next_block(&mut self) -> Result<Option<SampleBlock>, SourceError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticConfig {
    pub source_id: String,
    pub sample_rate_hz: u32,
    pub channel_count: u8,
    pub samples_per_block: u32,
    pub total_blocks: Option<u64>,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            source_id: "synthetic".to_string(),
            sample_rate_hz: 30_000,
            channel_count: 16,
            samples_per_block: 1_024,
            total_blocks: None,
        }
    }
}

pub struct SyntheticSource {
    config: SyntheticConfig,
    sequence: u64,
    next_sample_index: u64,
    channels: Vec<ChannelId>,
}

impl SyntheticSource {
    pub fn new(config: SyntheticConfig) -> Result<Self, SourceError> {
        if config.source_id.is_empty() {
            return Err(SourceError::new("source_id must not be empty"));
        }
        if config.sample_rate_hz == 0 {
            return Err(SourceError::new("sample_rate_hz must be greater than zero"));
        }
        if config.channel_count == 0 {
            return Err(SourceError::new("channel_count must be greater than zero"));
        }
        if config.samples_per_block == 0 {
            return Err(SourceError::new(
                "samples_per_block must be greater than zero",
            ));
        }

        let channels = (0..config.channel_count)
            .map(|channel| ChannelId { stream: 0, channel })
            .collect();
        Ok(Self {
            config,
            sequence: 0,
            next_sample_index: 0,
            channels,
        })
    }
}

impl Source for SyntheticSource {
    fn next_block(&mut self) -> Result<Option<SampleBlock>, SourceError> {
        if self
            .config
            .total_blocks
            .is_some_and(|total| self.sequence >= total)
        {
            return Ok(None);
        }

        let sample_count = self.config.samples_per_block as usize;
        let channel_count = self.config.channel_count as usize;
        let mut samples = Vec::with_capacity(sample_count * channel_count);
        for sample in 0..sample_count {
            let absolute_sample = self.next_sample_index + sample as u64;
            for channel in 0..channel_count {
                let value = 30_000_u64 + (absolute_sample + channel as u64 * 257) % 4_096;
                samples.push(value as u16);
            }
        }

        let block = SampleBlock::new(
            self.config.source_id.clone(),
            self.sequence,
            self.next_sample_index,
            self.config.sample_rate_hz,
            self.channels.clone(),
            self.config.samples_per_block,
            samples,
            vec![0; sample_count],
            SampleFlags::empty(),
        )
        .map_err(|error| SourceError::new(error.to_string()))?;

        self.sequence += 1;
        self.next_sample_index += self.config.samples_per_block as u64;
        Ok(Some(block))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    message: String,
}

impl SourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for SourceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_deterministic_contiguous_blocks() {
        let mut source = SyntheticSource::new(SyntheticConfig {
            channel_count: 2,
            samples_per_block: 3,
            total_blocks: Some(2),
            ..SyntheticConfig::default()
        })
        .unwrap();

        let first = source.next_block().unwrap().unwrap();
        let second = source.next_block().unwrap().unwrap();

        assert_eq!(first.sequence, 0);
        assert_eq!(first.first_sample_index, 0);
        assert_eq!(first.last_sample_index(), 2);
        assert_eq!(first.sample(0, 0), Some(30_000));
        assert_eq!(first.sample(1, 0), Some(30_257));
        assert_eq!(second.sequence, 1);
        assert_eq!(second.first_sample_index, 3);
        assert!(source.next_block().unwrap().is_none());
    }

    #[test]
    fn rejects_invalid_configuration() {
        let error = SyntheticSource::new(SyntheticConfig {
            channel_count: 0,
            ..SyntheticConfig::default()
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("channel_count"));
    }
}
