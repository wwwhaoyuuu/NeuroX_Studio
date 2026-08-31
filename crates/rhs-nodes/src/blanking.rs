use rhs_core::{SampleBlock, SampleFlags};
use rhs_pipeline::{PipelineError, Processor};

const ADC_MIDPOINT_U16: u16 = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlankingReplacement {
    HoldLast,
    Midpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlankingConfig {
    pub ttl_mask: u16,
    pub blank_samples_after_rising_edge: u32,
    pub replacement: BlankingReplacement,
}

pub struct ArtifactBlanker {
    config: BlankingConfig,
    channel_count: Option<usize>,
    last_good: Vec<u16>,
    last_ttl_high: bool,
    remaining_blank_samples: u32,
}

impl ArtifactBlanker {
    pub fn new(config: BlankingConfig) -> Result<Self, PipelineError> {
        if config.ttl_mask == 0 {
            return Err(PipelineError::new(
                "artifact-blanker",
                "ttl_mask must select at least one TTL bit",
            ));
        }
        if config.blank_samples_after_rising_edge == 0 {
            return Err(PipelineError::new(
                "artifact-blanker",
                "blank_samples_after_rising_edge must be greater than zero",
            ));
        }
        Ok(Self {
            config,
            channel_count: None,
            last_good: Vec::new(),
            last_ttl_high: false,
            remaining_blank_samples: 0,
        })
    }

    fn configure(&mut self, channel_count: usize) -> Result<(), PipelineError> {
        if let Some(existing) = self.channel_count {
            if existing != channel_count {
                return Err(PipelineError::new(
                    "artifact-blanker",
                    "channel count changed while blanking was running",
                ));
            }
            return Ok(());
        }
        self.channel_count = Some(channel_count);
        self.last_good = vec![ADC_MIDPOINT_U16; channel_count];
        Ok(())
    }

    fn reset(&mut self) {
        self.last_good.fill(ADC_MIDPOINT_U16);
        self.last_ttl_high = false;
        self.remaining_blank_samples = 0;
    }
}

impl Processor for ArtifactBlanker {
    fn name(&self) -> &str {
        "artifact-blanker"
    }

    fn process(&mut self, mut block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        block
            .validate()
            .map_err(|error| PipelineError::new("artifact-blanker", error.to_string()))?;
        let channel_count = block.channel_count();
        self.configure(channel_count)?;
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            self.reset();
        }

        let mut blanked = false;
        for sample_index in 0..block.samples_per_channel as usize {
            let ttl_high = block
                .ttl_in
                .get(sample_index)
                .is_some_and(|value| value & self.config.ttl_mask != 0);
            if ttl_high && !self.last_ttl_high {
                self.remaining_blank_samples = self.config.blank_samples_after_rising_edge;
            }

            let offset = sample_index * channel_count;
            let sample = &mut block.samples[offset..offset + channel_count];
            if self.remaining_blank_samples > 0 {
                for (channel, raw) in sample.iter_mut().enumerate() {
                    *raw = match self.config.replacement {
                        BlankingReplacement::HoldLast => self.last_good[channel],
                        BlankingReplacement::Midpoint => ADC_MIDPOINT_U16,
                    };
                }
                self.remaining_blank_samples -= 1;
                blanked = true;
            } else {
                self.last_good.copy_from_slice(sample);
            }
            self.last_ttl_high = ttl_high;
        }
        if blanked {
            block.flags.insert(SampleFlags::STIM_ARTIFACT);
        }
        Ok(Some(block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::ChannelId;

    fn block(sequence: u64, first_sample: u64, samples: Vec<u16>, ttl: Vec<u16>) -> SampleBlock {
        SampleBlock::new(
            "test",
            sequence,
            first_sample,
            30_000,
            vec![ChannelId {
                stream: 0,
                channel: 0,
            }],
            samples.len() as u32,
            samples,
            ttl,
            SampleFlags::empty(),
        )
        .unwrap()
    }

    #[test]
    fn blanks_a_bounded_window_after_ttl_rising_edge() {
        let mut blanker = ArtifactBlanker::new(BlankingConfig {
            ttl_mask: 1,
            blank_samples_after_rising_edge: 3,
            replacement: BlankingReplacement::HoldLast,
        })
        .unwrap();
        let output = blanker
            .process(block(
                0,
                0,
                vec![100, 101, 102, 103, 104],
                vec![0, 1, 1, 0, 0],
            ))
            .unwrap()
            .unwrap();

        assert_eq!(output.samples, vec![100, 100, 100, 100, 104]);
        assert!(output.flags.contains(SampleFlags::STIM_ARTIFACT));
    }

    #[test]
    fn blanking_window_continues_across_blocks() {
        let mut blanker = ArtifactBlanker::new(BlankingConfig {
            ttl_mask: 1,
            blank_samples_after_rising_edge: 3,
            replacement: BlankingReplacement::Midpoint,
        })
        .unwrap();
        let first = blanker
            .process(block(0, 0, vec![100, 101], vec![0, 1]))
            .unwrap()
            .unwrap();
        let second = blanker
            .process(block(1, 2, vec![102, 103, 104], vec![0, 0, 0]))
            .unwrap()
            .unwrap();

        assert_eq!(first.samples, vec![100, ADC_MIDPOINT_U16]);
        assert_eq!(
            second.samples,
            vec![ADC_MIDPOINT_U16, ADC_MIDPOINT_U16, 104]
        );
        assert!(second.flags.contains(SampleFlags::STIM_ARTIFACT));
    }
}
