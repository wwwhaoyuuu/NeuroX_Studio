use crate::ADC_MIDPOINT;
use rhs_core::{SampleBlock, SampleFlags};
use rhs_pipeline::{PipelineError, Processor};
use std::f64::consts::{PI, SQRT_2};

#[derive(Debug, Clone, Copy)]
struct BiquadCoefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    fn process(&mut self, input: f64, coefficients: BiquadCoefficients) -> f64 {
        let output = coefficients.b0 * input + self.z1;
        self.z1 = coefficients.b1 * input - coefficients.a1 * output + self.z2;
        self.z2 = coefficients.b2 * input - coefficients.a2 * output;
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamShape {
    sample_rate_hz: u32,
    channel_count: usize,
}

pub struct NotchProcessor {
    frequency_hz: f64,
    quality_factor: f64,
    shape: Option<StreamShape>,
    coefficients: Option<BiquadCoefficients>,
    states: Vec<BiquadState>,
}

impl NotchProcessor {
    pub fn new(frequency_hz: f64, quality_factor: f64) -> Result<Self, PipelineError> {
        if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
            return Err(PipelineError::new(
                "notch",
                "frequency_hz must be finite and greater than zero",
            ));
        }
        if !quality_factor.is_finite() || quality_factor <= 0.0 {
            return Err(PipelineError::new(
                "notch",
                "quality_factor must be finite and greater than zero",
            ));
        }
        Ok(Self {
            frequency_hz,
            quality_factor,
            shape: None,
            coefficients: None,
            states: Vec::new(),
        })
    }

    fn configure(&mut self, block: &SampleBlock) -> Result<(), PipelineError> {
        let shape = StreamShape {
            sample_rate_hz: block.sample_rate_hz,
            channel_count: block.channel_count(),
        };
        if let Some(existing) = self.shape {
            if existing != shape {
                return Err(PipelineError::new(
                    "notch",
                    "sample rate or channel count changed while the filter was running",
                ));
            }
            return Ok(());
        }
        if self.frequency_hz >= block.sample_rate_hz as f64 / 2.0 {
            return Err(PipelineError::new(
                "notch",
                "notch frequency must be below Nyquist",
            ));
        }
        self.coefficients = Some(notch_coefficients(
            block.sample_rate_hz as f64,
            self.frequency_hz,
            self.quality_factor,
        ));
        self.states = vec![BiquadState::default(); block.channel_count()];
        self.shape = Some(shape);
        Ok(())
    }
}

impl Processor for NotchProcessor {
    fn name(&self) -> &str {
        "notch"
    }

    fn process(&mut self, mut block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        block
            .validate()
            .map_err(|error| PipelineError::new("notch", error.to_string()))?;
        self.configure(&block)?;
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            self.states.fill(BiquadState::default());
        }
        let coefficients = self.coefficients.expect("notch must be configured");
        let channel_count = block.channel_count();
        for sample in block.samples.chunks_exact_mut(channel_count) {
            for (channel, raw) in sample.iter_mut().enumerate() {
                let centered = *raw as f64 - ADC_MIDPOINT;
                *raw = encode_centered(self.states[channel].process(centered, coefficients));
            }
        }
        Ok(Some(block))
    }
}

pub struct BandPassProcessor {
    low_cut_hz: f64,
    high_cut_hz: f64,
    shape: Option<StreamShape>,
    high_pass: Option<BiquadCoefficients>,
    low_pass: Option<BiquadCoefficients>,
    high_pass_states: Vec<BiquadState>,
    low_pass_states: Vec<BiquadState>,
}

impl BandPassProcessor {
    pub fn new(low_cut_hz: f64, high_cut_hz: f64) -> Result<Self, PipelineError> {
        if !low_cut_hz.is_finite()
            || !high_cut_hz.is_finite()
            || low_cut_hz <= 0.0
            || high_cut_hz <= low_cut_hz
        {
            return Err(PipelineError::new(
                "band-pass",
                "cutoffs must be finite and satisfy 0 < low_cut_hz < high_cut_hz",
            ));
        }
        Ok(Self {
            low_cut_hz,
            high_cut_hz,
            shape: None,
            high_pass: None,
            low_pass: None,
            high_pass_states: Vec::new(),
            low_pass_states: Vec::new(),
        })
    }

    fn configure(&mut self, block: &SampleBlock) -> Result<(), PipelineError> {
        let shape = StreamShape {
            sample_rate_hz: block.sample_rate_hz,
            channel_count: block.channel_count(),
        };
        if let Some(existing) = self.shape {
            if existing != shape {
                return Err(PipelineError::new(
                    "band-pass",
                    "sample rate or channel count changed while the filter was running",
                ));
            }
            return Ok(());
        }
        if self.high_cut_hz >= block.sample_rate_hz as f64 / 2.0 {
            return Err(PipelineError::new(
                "band-pass",
                "high_cut_hz must be below Nyquist",
            ));
        }
        let sample_rate = block.sample_rate_hz as f64;
        self.high_pass = Some(high_pass_coefficients(sample_rate, self.low_cut_hz));
        self.low_pass = Some(low_pass_coefficients(sample_rate, self.high_cut_hz));
        self.high_pass_states = vec![BiquadState::default(); block.channel_count()];
        self.low_pass_states = vec![BiquadState::default(); block.channel_count()];
        self.shape = Some(shape);
        Ok(())
    }
}

impl Processor for BandPassProcessor {
    fn name(&self) -> &str {
        "band-pass"
    }

    fn process(&mut self, mut block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        block
            .validate()
            .map_err(|error| PipelineError::new("band-pass", error.to_string()))?;
        self.configure(&block)?;
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            self.high_pass_states.fill(BiquadState::default());
            self.low_pass_states.fill(BiquadState::default());
        }
        let high_pass = self.high_pass.expect("band-pass must be configured");
        let low_pass = self.low_pass.expect("band-pass must be configured");
        let channel_count = block.channel_count();
        for sample in block.samples.chunks_exact_mut(channel_count) {
            for (channel, raw) in sample.iter_mut().enumerate() {
                let centered = *raw as f64 - ADC_MIDPOINT;
                let high_passed = self.high_pass_states[channel].process(centered, high_pass);
                let filtered = self.low_pass_states[channel].process(high_passed, low_pass);
                *raw = encode_centered(filtered);
            }
        }
        Ok(Some(block))
    }
}

fn encode_centered(value: f64) -> u16 {
    (value + ADC_MIDPOINT).round().clamp(0.0, u16::MAX as f64) as u16
}

fn notch_coefficients(sample_rate: f64, frequency: f64, quality: f64) -> BiquadCoefficients {
    let omega = 2.0 * PI * frequency / sample_rate;
    let alpha = omega.sin() / (2.0 * quality);
    normalize(
        1.0,
        -2.0 * omega.cos(),
        1.0,
        1.0 + alpha,
        -2.0 * omega.cos(),
        1.0 - alpha,
    )
}

fn low_pass_coefficients(sample_rate: f64, frequency: f64) -> BiquadCoefficients {
    let omega = 2.0 * PI * frequency / sample_rate;
    let alpha = omega.sin() / SQRT_2;
    let cosine = omega.cos();
    normalize(
        (1.0 - cosine) / 2.0,
        1.0 - cosine,
        (1.0 - cosine) / 2.0,
        1.0 + alpha,
        -2.0 * cosine,
        1.0 - alpha,
    )
}

fn high_pass_coefficients(sample_rate: f64, frequency: f64) -> BiquadCoefficients {
    let omega = 2.0 * PI * frequency / sample_rate;
    let alpha = omega.sin() / SQRT_2;
    let cosine = omega.cos();
    normalize(
        (1.0 + cosine) / 2.0,
        -(1.0 + cosine),
        (1.0 + cosine) / 2.0,
        1.0 + alpha,
        -2.0 * cosine,
        1.0 - alpha,
    )
}

fn normalize(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> BiquadCoefficients {
    BiquadCoefficients {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_core::{ChannelId, SampleFlags};

    const SAMPLE_RATE: u32 = 30_000;
    const SAMPLE_COUNT: usize = 60_000;
    const ANALYSIS_SKIP: usize = 15_000;

    fn signal(tones: &[(f64, f64)]) -> SampleBlock {
        let samples = (0..SAMPLE_COUNT)
            .map(|sample| {
                let time = sample as f64 / SAMPLE_RATE as f64;
                let value = tones
                    .iter()
                    .fold(ADC_MIDPOINT, |sum, (frequency, amplitude)| {
                        sum + amplitude * (2.0 * PI * frequency * time).sin()
                    });
                value.round().clamp(0.0, u16::MAX as f64) as u16
            })
            .collect();
        SampleBlock::new(
            "reference",
            0,
            0,
            SAMPLE_RATE,
            vec![ChannelId {
                stream: 0,
                channel: 0,
            }],
            SAMPLE_COUNT as u32,
            samples,
            Vec::new(),
            SampleFlags::empty(),
        )
        .unwrap()
    }

    fn tone_amplitude(samples: &[u16], frequency: f64) -> f64 {
        let samples = &samples[ANALYSIS_SKIP..];
        let (real, imaginary) =
            samples
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(real, imaginary), (index, raw)| {
                    let absolute_index = index + ANALYSIS_SKIP;
                    let phase = 2.0 * PI * frequency * absolute_index as f64 / SAMPLE_RATE as f64;
                    let centered = *raw as f64 - ADC_MIDPOINT;
                    (
                        real + centered * phase.cos(),
                        imaginary - centered * phase.sin(),
                    )
                });
        2.0 * real.hypot(imaginary) / samples.len() as f64
    }

    #[test]
    fn notch_attenuates_50_hz_and_preserves_500_hz() {
        let input = signal(&[(50.0, 3_000.0), (500.0, 1_000.0)]);
        let input_50 = tone_amplitude(&input.samples, 50.0);
        let input_500 = tone_amplitude(&input.samples, 500.0);
        let mut processor = NotchProcessor::new(50.0, 20.0).unwrap();
        let output = processor.process(input).unwrap().unwrap();

        assert!(tone_amplitude(&output.samples, 50.0) < input_50 * 0.1);
        assert!(tone_amplitude(&output.samples, 500.0) > input_500 * 0.9);
    }

    #[test]
    fn band_pass_preserves_midband_and_attenuates_out_of_band_tones() {
        let input = signal(&[(50.0, 1_200.0), (1_000.0, 1_200.0), (8_000.0, 1_200.0)]);
        let input_50 = tone_amplitude(&input.samples, 50.0);
        let input_1000 = tone_amplitude(&input.samples, 1_000.0);
        let input_8000 = tone_amplitude(&input.samples, 8_000.0);
        let mut processor = BandPassProcessor::new(300.0, 3_000.0).unwrap();
        let output = processor.process(input).unwrap().unwrap();

        assert!(tone_amplitude(&output.samples, 50.0) < input_50 * 0.1);
        assert!(tone_amplitude(&output.samples, 1_000.0) > input_1000 * 0.8);
        assert!(tone_amplitude(&output.samples, 8_000.0) < input_8000 * 0.15);
    }

    #[test]
    fn rejects_cutoffs_above_nyquist() {
        let input = signal(&[(1_000.0, 100.0)]);
        let error = BandPassProcessor::new(300.0, 20_000.0)
            .unwrap()
            .process(input)
            .unwrap_err();
        assert!(error.to_string().contains("Nyquist"));
    }

    #[test]
    fn segmented_online_processing_matches_single_block_offline_processing() {
        let input = signal(&[(50.0, 800.0), (1_000.0, 1_200.0), (8_000.0, 500.0)]);
        let mut offline_notch = NotchProcessor::new(50.0, 20.0).unwrap();
        let mut offline_band_pass = BandPassProcessor::new(300.0, 3_000.0).unwrap();
        let offline = offline_band_pass
            .process(offline_notch.process(input.clone()).unwrap().unwrap())
            .unwrap()
            .unwrap();

        let mut online_notch = NotchProcessor::new(50.0, 20.0).unwrap();
        let mut online_band_pass = BandPassProcessor::new(300.0, 3_000.0).unwrap();
        let mut online_samples = Vec::with_capacity(input.samples.len());
        for (sequence, chunk) in input.samples.chunks(1_024).enumerate() {
            let block = SampleBlock::new(
                "reference",
                sequence as u64,
                (sequence * 1_024) as u64,
                SAMPLE_RATE,
                input.channels.clone(),
                chunk.len() as u32,
                chunk.to_vec(),
                Vec::new(),
                SampleFlags::empty(),
            )
            .unwrap();
            let filtered = online_band_pass
                .process(online_notch.process(block).unwrap().unwrap())
                .unwrap()
                .unwrap();
            online_samples.extend(filtered.samples);
        }

        assert_eq!(online_samples, offline.samples);
    }
}
