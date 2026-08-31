use crate::ADC_MIDPOINT;
use rhs_core::{SampleBlock, SampleFlags};
use rhs_pipeline::{PipelineError, Processor};

pub struct EnvelopeProcessor {
    factor: usize,
    output_sequence: u64,
    next_output_index: Option<u64>,
}

impl EnvelopeProcessor {
    pub fn new(factor: usize) -> Result<Self, PipelineError> {
        if factor == 0 {
            return Err(PipelineError::new(
                "envelope",
                "decimation factor must be greater than zero",
            ));
        }
        Ok(Self {
            factor,
            output_sequence: 0,
            next_output_index: None,
        })
    }

    pub fn factor(&self) -> usize {
        self.factor
    }
}

impl Processor for EnvelopeProcessor {
    fn name(&self) -> &str {
        "envelope"
    }

    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        block
            .validate()
            .map_err(|error| PipelineError::new("envelope", error.to_string()))?;
        let input_samples = block.samples_per_channel as usize;
        if !input_samples.is_multiple_of(self.factor) {
            return Err(PipelineError::new(
                "envelope",
                format!(
                    "samples_per_channel {input_samples} must be divisible by factor {}",
                    self.factor
                ),
            ));
        }
        if !block.sample_rate_hz.is_multiple_of(self.factor as u32) {
            return Err(PipelineError::new(
                "envelope",
                format!(
                    "sample rate {} must be divisible by factor {}",
                    block.sample_rate_hz, self.factor
                ),
            ));
        }

        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            self.next_output_index = None;
        }
        let output_samples = input_samples / self.factor;
        let channel_count = block.channel_count();
        let mut samples = Vec::with_capacity(output_samples * channel_count);
        let mut ttl_in = Vec::with_capacity(output_samples);
        for bin in 0..output_samples {
            let first = bin * self.factor;
            let end = first + self.factor;
            for channel in 0..channel_count {
                let mut peak = 0.0_f64;
                for sample in first..end {
                    let raw = block.samples[sample * channel_count + channel] as f64;
                    peak = peak.max((raw - ADC_MIDPOINT).abs());
                }
                samples.push(peak.round().clamp(0.0, u16::MAX as f64) as u16);
            }
            let ttl = if block.ttl_in.is_empty() {
                0
            } else {
                block.ttl_in[first..end]
                    .iter()
                    .copied()
                    .fold(0_u16, |combined, value| combined | value)
            };
            ttl_in.push(ttl);
        }

        let first_output_index = self
            .next_output_index
            .unwrap_or(block.first_sample_index / self.factor as u64);
        self.next_output_index = Some(first_output_index.saturating_add(output_samples as u64));
        let output = SampleBlock::new(
            format!("{}:envelope", block.source_id),
            self.output_sequence,
            first_output_index,
            block.sample_rate_hz / self.factor as u32,
            block.channels,
            output_samples as u32,
            samples,
            ttl_in,
            block.flags,
        )
        .map_err(|error| PipelineError::new("envelope", error.to_string()))?;
        self.output_sequence = self.output_sequence.saturating_add(1);
        Ok(Some(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::{ChannelId, SampleFlags};

    #[test]
    fn creates_peak_envelope_and_combines_ttl() {
        let block = SampleBlock::new(
            "test",
            7,
            40,
            8_000,
            vec![
                ChannelId {
                    stream: 0,
                    channel: 0,
                },
                ChannelId {
                    stream: 0,
                    channel: 1,
                },
            ],
            8,
            vec![
                32_768, 32_768, 32_770, 32_765, 32_764, 32_772, 32_769, 32_767, 32_758, 32_768,
                32_768, 32_788, 32_773, 32_766, 32_765, 32_771,
            ],
            vec![0, 1, 0, 0, 0, 0, 2, 0],
            SampleFlags::empty(),
        )
        .unwrap();
        let mut processor = EnvelopeProcessor::new(4).unwrap();
        let output = processor.process(block).unwrap().unwrap();

        assert_eq!(output.sequence, 0);
        assert_eq!(output.first_sample_index, 10);
        assert_eq!(output.sample_rate_hz, 2_000);
        assert_eq!(output.samples_per_channel, 2);
        assert_eq!(output.samples, vec![4, 4, 10, 20]);
        assert_eq!(output.ttl_in, vec![1, 2]);
    }

    #[test]
    fn rejects_non_divisible_blocks() {
        let block = SampleBlock::new(
            "test",
            0,
            0,
            30_000,
            vec![ChannelId {
                stream: 0,
                channel: 0,
            }],
            3,
            vec![32_768; 3],
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap();
        let error = EnvelopeProcessor::new(2)
            .unwrap()
            .process(block)
            .unwrap_err();
        assert!(error.to_string().contains("divisible"));
    }
}
