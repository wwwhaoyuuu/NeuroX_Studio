use rhs_core::{ChannelId, SampleBlock, SampleFlags};
use rhs_nodes::BandPassProcessor;
use rhs_pipeline::Processor;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

const UV_PER_ADC: f64 = 0.195;
const EVENT_CAPACITY: usize = 4096;
const POLL_LIMIT: usize = 512;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SpikeThresholdMode {
    #[default]
    Fixed,
    Noise,
}

fn default_noise_multiplier() -> f64 {
    5.0
}
fn default_calibration_ms() -> u32 {
    1000
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeConfig {
    pub enabled: bool,
    pub threshold_uv: f64,
    #[serde(default)]
    pub threshold_mode: SpikeThresholdMode,
    #[serde(default = "default_noise_multiplier")]
    pub noise_multiplier: f64,
    #[serde(default = "default_calibration_ms")]
    pub calibration_ms: u32,
    pub refractory_ms: f64,
    pub pre_ms: f64,
    pub post_ms: f64,
    pub band_pass: bool,
    pub recording_path: Option<String>,
}

impl Default for SpikeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_uv: -50.0,
            threshold_mode: SpikeThresholdMode::Noise,
            noise_multiplier: default_noise_multiplier(),
            calibration_ms: default_calibration_ms(),
            refractory_ms: 3.0,
            pre_ms: 1.0,
            post_ms: 2.0,
            band_pass: true,
            recording_path: None,
        }
    }
}

impl SpikeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.noise_multiplier.is_finite()
            || !(2.0..=12.0).contains(&self.noise_multiplier)
            || !(250..=10_000).contains(&self.calibration_ms)
        {
            return Err("Noise multiplier must be 2..12; calibration must be 250..10000 ms".into());
        }
        if !self.threshold_uv.is_finite() || !(1.0..=6000.0).contains(&self.threshold_uv.abs()) {
            return Err("Spike threshold magnitude must be 1..6000 uV".into());
        }
        if !self.pre_ms.is_finite()
            || !(0.2..=3.0).contains(&self.pre_ms)
            || !self.post_ms.is_finite()
            || !(0.5..=5.0).contains(&self.post_ms)
            || !self.refractory_ms.is_finite()
            || !(self.post_ms..=100.0).contains(&self.refractory_ms)
        {
            return Err("Spike pre-window: 0.2..3 ms; post-window: 0.5..5 ms; refractory: post-window..100 ms".into());
        }
        if self
            .recording_path
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err("Spike recording path must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeEvent {
    pub id: u64,
    pub channel_index: usize,
    pub stream: u8,
    pub chip_channel: u8,
    pub sample_index: u64,
    pub peak_sample_index: u64,
    pub time_ms: f64,
    pub peak_uv: f64,
    pub display_peak_uv: f64,
    pub threshold_uv: f64,
    pub noise_uv: Option<f64>,
    pub peak_to_peak_uv: f64,
    pub sample_rate_hz: u32,
    pub pre_samples: usize,
    pub waveform_uv: Vec<f64>,
}

#[derive(Default)]
pub struct SpikeStore {
    pub config: SpikeConfig,
    pub epoch: u64,
    pub running: bool,
    pub error: Option<String>,
    pub counts: Vec<u64>,
    pub latest_sample_index: u64,
    pub sample_rate_hz: u32,
    pub discarded_incomplete: u64,
    pub discontinuities: u64,
    pub recorded_events: u64,
    pub recording_complete: bool,
    pub thresholds: Vec<SpikeThresholdView>,
    first_id: u64,
    events: VecDeque<SpikeEvent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeBatch {
    config: SpikeConfig,
    epoch: u64,
    running: bool,
    error: Option<String>,
    counts: Vec<u64>,
    latest_sample_index: u64,
    sample_rate_hz: u32,
    discarded_incomplete: u64,
    discontinuities: u64,
    recorded_events: u64,
    recording_complete: bool,
    thresholds: Vec<SpikeThresholdView>,
    missed_events: u64,
    events: Vec<SpikeEvent>,
}

impl SpikeStore {
    pub fn begin(&mut self) {
        self.begin_segment(1);
    }

    pub fn begin_segment(&mut self, first_id: u64) {
        let epoch = self.epoch.saturating_add(1);
        *self = Self {
            config: self.config.clone(),
            epoch,
            running: self.config.enabled,
            first_id,
            ..Self::default()
        };
    }

    pub fn poll(&self, epoch: u64, after_id: u64) -> SpikeBatch {
        let after_id = if epoch == self.epoch {
            after_id
        } else {
            self.first_id.saturating_sub(1)
        };
        let missed_events = self.events.front().map_or(0, |first| {
            first.id.saturating_sub(after_id.saturating_add(1))
        });
        SpikeBatch {
            config: self.config.clone(),
            epoch: self.epoch,
            running: self.running,
            error: self.error.clone(),
            counts: self.counts.clone(),
            latest_sample_index: self.latest_sample_index,
            sample_rate_hz: self.sample_rate_hz,
            discarded_incomplete: self.discarded_incomplete,
            discontinuities: self.discontinuities,
            recorded_events: self.recorded_events,
            recording_complete: self.recording_complete,
            thresholds: self.thresholds.clone(),
            missed_events,
            events: self
                .events
                .iter()
                .filter(|event| event.id > after_id)
                .take(POLL_LIMIT)
                .cloned()
                .collect(),
        }
    }

    pub fn append(&mut self, events: Vec<SpikeEvent>) {
        for event in events {
            if self.counts.len() <= event.channel_index {
                self.counts.resize(event.channel_index + 1, 0);
            }
            self.counts[event.channel_index] += 1;
            if self.events.len() == EVENT_CAPACITY {
                self.events.pop_front();
            }
            self.events.push_back(event);
        }
    }
}

struct Candidate {
    sample_index: u64,
    values: Vec<f64>,
    display_values: Vec<f64>,
    threshold_uv: f64,
    noise_uv: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeThresholdView {
    pub threshold_uv: Option<f64>,
    pub noise_uv: Option<f64>,
    pub baseline_uv: f64,
    pub state: &'static str,
}

#[derive(Default)]
struct NoiseCalibration {
    samples: Vec<f64>,
    clipped: usize,
    total: usize,
    result: Option<SpikeThresholdView>,
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let middle = values.len() / 2;
    let even = values.len().is_multiple_of(2);
    let (lower, center, _) = values.select_nth_unstable_by(middle, f64::total_cmp);
    if even {
        (*center + lower.iter().copied().fold(f64::NEG_INFINITY, f64::max)) / 2.0
    } else {
        *center
    }
}

impl NoiseCalibration {
    fn finish(&mut self, multiplier: f64, sign: f64) {
        let baseline = median(&mut self.samples);
        let mut deviations: Vec<_> = self
            .samples
            .iter()
            .map(|value| (value - baseline).abs())
            .collect();
        let sigma = median(&mut deviations) / 0.6744897501960817;
        let state = if self.total > 0 && self.clipped * 100 > self.total {
            "saturated"
        } else if self.samples.len() < 128 {
            "insufficient"
        } else if sigma < UV_PER_ADC * 0.5 {
            "flat"
        } else {
            "ready"
        };
        self.result = Some(SpikeThresholdView {
            threshold_uv: (state == "ready").then_some(baseline + sign * multiplier * sigma),
            noise_uv: Some(sigma),
            baseline_uv: baseline,
            state,
        });
        self.samples.clear();
        self.samples.shrink_to_fit();
    }
}

#[derive(Default)]
struct ChannelState {
    history: VecDeque<f64>,
    previous: Option<f64>,
    next_allowed: u64,
    candidate: Option<Candidate>,
    noise: NoiseCalibration,
}

pub struct SpikeAnalyzer {
    config: SpikeConfig,
    channels: Vec<ChannelState>,
    shape: Option<(u32, Vec<ChannelId>)>,
    expected_sample: Option<u64>,
    filter: Option<BandPassProcessor>,
    warm_until: u64,
    calibrate_until: u64,
    next_id: u64,
    recorder: Option<BufWriter<File>>,
    last_flush: Instant,
    pub recording_error: Option<String>,
    pub recorded_events: u64,
    pub discarded_incomplete: u64,
    pub discontinuities: u64,
}

impl SpikeAnalyzer {
    pub fn restart_segment(&mut self) -> u64 {
        self.discarded_incomplete += self
            .channels
            .iter()
            .filter(|channel| channel.candidate.is_some())
            .count() as u64;
        self.channels.clear();
        self.shape = None;
        self.expected_sample = None;
        self.filter = None;
        self.next_id
    }

    pub fn new(config: SpikeConfig) -> Result<Self, String> {
        config.validate()?;
        let mut result = Self {
            config,
            channels: Vec::new(),
            shape: None,
            expected_sample: None,
            filter: None,
            warm_until: 0,
            calibrate_until: 0,
            next_id: 1,
            recorder: None,
            last_flush: Instant::now(),
            recording_error: None,
            recorded_events: 0,
            discarded_incomplete: 0,
            discontinuities: 0,
        };
        if result.config.enabled {
            if let Some(path) = result.config.recording_path.as_ref() {
                let opened = (|| -> std::io::Result<BufWriter<File>> {
                    if let Some(parent) = std::path::Path::new(path)
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                    {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut writer =
                        BufWriter::new(OpenOptions::new().write(true).create_new(true).open(path)?);
                    writeln!(writer, "id,channel_index,stream,chip_channel,crossing_sample,time_ms,peak_sample,peak_uv,peak_to_peak_uv,sample_rate_hz,pre_samples,threshold_uv,band_pass_300_3000,waveform_uv,noise_sigma_uv,threshold_mode,noise_multiplier")?;
                    writer.flush()?;
                    Ok(writer)
                })();
                match opened {
                    Ok(writer) => result.recorder = Some(writer),
                    Err(error) => {
                        result.recording_error = Some(format!("Spike CSV open failed: {error}"))
                    }
                }
            }
        }
        Ok(result)
    }

    fn reset(&mut self, block: &SampleBlock) -> Result<(), String> {
        self.discarded_incomplete += self
            .channels
            .iter()
            .filter(|channel| channel.candidate.is_some())
            .count() as u64;
        self.channels = (0..block.channel_count())
            .map(|_| ChannelState::default())
            .collect();
        self.shape = Some((block.sample_rate_hz, block.channels.clone()));
        self.filter = if self.config.band_pass {
            Some(BandPassProcessor::new(300.0, 3000.0).map_err(|error| error.to_string())?)
        } else {
            None
        };
        self.warm_until = block.first_sample_index
            + if self.config.band_pass {
                u64::from(block.sample_rate_hz / 50)
            } else {
                0
            };
        self.calibrate_until = self.warm_until
            + u64::from(block.sample_rate_hz) * u64::from(self.config.calibration_ms) / 1000;
        Ok(())
    }

    pub fn thresholds(&self) -> Vec<SpikeThresholdView> {
        self.channels
            .iter()
            .map(|channel| {
                if self.config.threshold_mode == SpikeThresholdMode::Fixed {
                    SpikeThresholdView {
                        threshold_uv: Some(self.config.threshold_uv),
                        noise_uv: None,
                        baseline_uv: 0.0,
                        state: "fixed",
                    }
                } else {
                    channel.noise.result.clone().unwrap_or(SpikeThresholdView {
                        threshold_uv: None,
                        noise_uv: None,
                        baseline_uv: 0.0,
                        state: "calibrating",
                    })
                }
            })
            .collect()
    }

    pub fn process(&mut self, input: &SampleBlock) -> Result<Vec<SpikeEvent>, String> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        input.validate().map_err(|error| error.to_string())?;
        let changed = self.shape.as_ref().is_none_or(|(rate, channels)| {
            *rate != input.sample_rate_hz || *channels != input.channels
        });
        let gap = self
            .expected_sample
            .is_some_and(|next| next != input.first_sample_index)
            || input.flags.contains(SampleFlags::DISCONTINUITY);
        let artifact = input.flags.contains(SampleFlags::STIM_ARTIFACT);
        if changed || gap || artifact {
            if self.expected_sample.is_some() {
                self.discontinuities += 1;
            }
            self.reset(input)?;
        }
        self.expected_sample = Some(input.last_sample_index().saturating_add(1));
        if artifact {
            self.expected_sample = None;
            self.shape = None;
            return Ok(Vec::new());
        }
        let filtered;
        let block = if let Some(filter) = self.filter.as_mut() {
            filtered = filter
                .process(input.clone())
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Spike band-pass unexpectedly discarded the block".to_owned())?;
            &filtered
        } else {
            input
        };
        let samples_for_ms = |ms: f64| {
            (ms * f64::from(block.sample_rate_hz) / 1000.0)
                .round()
                .max(1.0) as usize
        };
        let pre = samples_for_ms(self.config.pre_ms);
        let post = samples_for_ms(self.config.post_ms);
        let refractory = samples_for_ms(self.config.refractory_ms).max(post + 1) as u64;
        let negative = self.config.threshold_uv < 0.0;
        let noise_stride =
            (u64::from(block.sample_rate_hz) * u64::from(self.config.calibration_ms) / 1000)
                .div_ceil(4096)
                .max(1);
        let mut events = Vec::new();
        for offset in 0..block.samples_per_channel as usize {
            let index = block.first_sample_index + offset as u64;
            for (channel_index, channel) in self.channels.iter_mut().enumerate() {
                let value =
                    (f64::from(block.samples[offset * block.channel_count() + channel_index])
                        - 32768.0)
                        * UV_PER_ADC;
                let display_value =
                    (f64::from(input.samples[offset * input.channel_count() + channel_index])
                        - 32768.0)
                        * UV_PER_ADC;
                let threshold = if self.config.threshold_mode == SpikeThresholdMode::Noise {
                    if channel.noise.result.is_none() && index >= self.warm_until {
                        if index < self.calibrate_until {
                            channel.noise.total += 1;
                            let word =
                                input.samples[offset * input.channel_count() + channel_index];
                            if word <= 1 || word >= 65534 {
                                channel.noise.clipped += 1;
                            }
                            // Bound memory while sampling across the entire calibration interval.
                            if (index - self.warm_until).is_multiple_of(noise_stride)
                                && channel.noise.samples.len() < 4096
                            {
                                channel.noise.samples.push(value);
                            }
                        } else {
                            channel.noise.finish(
                                self.config.noise_multiplier,
                                self.config.threshold_uv.signum(),
                            );
                            channel.previous = None;
                        }
                    }
                    channel
                        .noise
                        .result
                        .as_ref()
                        .and_then(|value| value.threshold_uv)
                } else {
                    Some(self.config.threshold_uv)
                };
                if let Some(candidate) = channel.candidate.as_mut() {
                    candidate.values.push(value);
                    candidate.display_values.push(display_value);
                }
                if channel
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.values.len() == pre + post + 1)
                {
                    let candidate = channel.candidate.take().expect("candidate checked");
                    let (peak_offset, peak) = candidate
                        .values
                        .iter()
                        .enumerate()
                        .skip(pre)
                        .min_by(|(_, a), (_, b)| {
                            if negative {
                                a.total_cmp(b)
                            } else {
                                b.total_cmp(a)
                            }
                        })
                        .expect("nonempty waveform");
                    let minimum = candidate
                        .values
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min);
                    let maximum = candidate
                        .values
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max);
                    let address = &block.channels[channel_index];
                    events.push(SpikeEvent {
                        id: self.next_id,
                        channel_index,
                        stream: address.stream,
                        chip_channel: address.channel,
                        sample_index: candidate.sample_index,
                        peak_sample_index: candidate.sample_index + (peak_offset - pre) as u64,
                        time_ms: candidate.sample_index as f64 * 1000.0
                            / f64::from(block.sample_rate_hz),
                        peak_uv: *peak,
                        display_peak_uv: candidate.display_values[peak_offset - pre],
                        threshold_uv: candidate.threshold_uv,
                        noise_uv: candidate.noise_uv,
                        peak_to_peak_uv: maximum - minimum,
                        sample_rate_hz: block.sample_rate_hz,
                        pre_samples: pre,
                        waveform_uv: candidate.values,
                    });
                    self.next_id += 1;
                }
                let crossed = threshold.is_some_and(|threshold| {
                    channel.previous.is_some_and(|previous| {
                        if negative {
                            previous > threshold && value <= threshold
                        } else {
                            previous < threshold && value >= threshold
                        }
                    })
                });
                if crossed
                    && index >= self.warm_until
                    && index >= channel.next_allowed
                    && channel.history.len() == pre
                    && channel.candidate.is_none()
                {
                    let mut values: Vec<f64> = channel.history.iter().copied().collect();
                    values.push(value);
                    channel.candidate = Some(Candidate {
                        sample_index: index,
                        values,
                        display_values: vec![display_value],
                        threshold_uv: threshold.expect("threshold checked"),
                        noise_uv: channel
                            .noise
                            .result
                            .as_ref()
                            .and_then(|value| value.noise_uv),
                    });
                    channel.next_allowed = index + refractory;
                }
                if channel.history.len() == pre {
                    channel.history.pop_front();
                }
                channel.history.push_back(value);
                channel.previous = Some(value);
            }
        }
        self.record(&events);
        Ok(events)
    }

    fn record(&mut self, events: &[SpikeEvent]) {
        let Some(writer) = self.recorder.as_mut() else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            for event in events {
                write!(
                    writer,
                    "{},{},{},{},{},{:.6},{},{:.3},{:.3},{},{},{},{},",
                    event.id,
                    event.channel_index,
                    event.stream,
                    event.chip_channel,
                    event.sample_index,
                    event.time_ms,
                    event.peak_sample_index,
                    event.peak_uv,
                    event.peak_to_peak_uv,
                    event.sample_rate_hz,
                    event.pre_samples,
                    event.threshold_uv,
                    self.config.band_pass
                )?;
                for (index, value) in event.waveform_uv.iter().enumerate() {
                    if index != 0 {
                        write!(writer, ";")?;
                    }
                    write!(writer, "{value:.3}")?;
                }
                writeln!(
                    writer,
                    ",{},{},{}",
                    event
                        .noise_uv
                        .map_or(String::new(), |value| format!("{value:.6}")),
                    if self.config.threshold_mode == SpikeThresholdMode::Noise {
                        "noise"
                    } else {
                        "fixed"
                    },
                    self.config.noise_multiplier
                )?;
                self.recorded_events += 1;
            }
            if self.last_flush.elapsed() >= Duration::from_secs(1) {
                writer.flush()?;
                self.last_flush = Instant::now();
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.recording_error = Some(format!(
                "Spike CSV write failed (file may be incomplete): {error}"
            ));
            self.recorder = None;
        }
    }

    pub fn finish(&mut self) {
        self.discarded_incomplete += self
            .channels
            .iter_mut()
            .filter_map(|channel| channel.candidate.take())
            .count() as u64;
        if let Some(mut writer) = self.recorder.take() {
            if let Err(error) = writer.flush() {
                self.recording_error = Some(format!("Spike CSV finalize failed: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SpikeConfig {
        SpikeConfig {
            enabled: true,
            band_pass: false,
            threshold_mode: SpikeThresholdMode::Fixed,
            ..SpikeConfig::default()
        }
    }
    fn block(first: u64, channels: usize, length: usize, pulse: Option<usize>) -> SampleBlock {
        let mut samples = vec![32768; channels * length];
        if let Some(pulse) = pulse {
            for channel in 0..channels {
                samples[pulse * channels + channel] = 32000;
            }
        }
        SampleBlock::new(
            "test",
            0,
            first,
            30_000,
            (0..channels)
                .map(|index| ChannelId {
                    stream: (index / 16) as u8 + 2,
                    channel: (index % 16) as u8,
                })
                .collect(),
            length as u32,
            samples,
            vec![],
            SampleFlags::empty(),
        )
        .unwrap()
    }

    #[test]
    fn noise_thresholds_calibrate_per_channel_and_detect_only_after_calibration() {
        let mut settings = config();
        settings.threshold_mode = SpikeThresholdMode::Noise;
        settings.calibration_ms = 250;
        let mut analyzer = SpikeAnalyzer::new(settings).unwrap();
        let mut input = block(0, 64, 7600, None);
        for sample in 0..7600 {
            for channel in 0..64 {
                input.samples[sample * 64 + channel] =
                    (32768 + ((sample % 31) as i32 - 15) * (channel % 4 + 1) as i32) as u16;
            }
        }
        input.samples[3000 * 64..3001 * 64].fill(31000);
        assert!(analyzer.process(&input).unwrap().is_empty());
        let thresholds = analyzer.thresholds();
        assert_eq!(thresholds.len(), 64);
        assert!(thresholds.iter().all(|value| value.state == "ready"));
        assert!(thresholds[1].noise_uv.unwrap() > thresholds[0].noise_uv.unwrap() * 1.8);
        let events = analyzer.process(&block(7600, 64, 300, Some(100))).unwrap();
        assert_eq!(events.len(), 64);
        for event in events {
            let detection = &thresholds[event.channel_index];
            assert_eq!(Some(event.threshold_uv), detection.threshold_uv);
            assert_eq!(event.noise_uv, detection.noise_uv);
            assert!(
                (event.threshold_uv - (detection.baseline_uv - 5.0 * detection.noise_uv.unwrap()))
                    .abs()
                    < 1e-9
            );
        }
    }

    #[test]
    fn noise_calibration_uses_the_filtered_signal_and_then_detects_a_real_input_pulse() {
        let mut settings = config();
        settings.threshold_mode = SpikeThresholdMode::Noise;
        settings.band_pass = true;
        settings.calibration_ms = 250;
        let mut analyzer = SpikeAnalyzer::new(settings).unwrap();
        let mut input = block(0, 16, 8100, None);
        let mut random = 17_u32;
        for value in &mut input.samples {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = (32768 + ((random >> 16) % 41) as i32 - 20) as u16;
        }
        assert!(analyzer.process(&input).unwrap().is_empty());
        let events = analyzer.process(&block(8100, 16, 500, Some(100))).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.channel_index)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            16
        );
        assert!(events.iter().all(
            |event| event.threshold_uv < 0.0 && event.noise_uv.is_some_and(|sigma| sigma > 0.0)
        ));
    }

    #[test]
    fn flat_saturated_and_discontinuous_inputs_do_not_invent_noise_events() {
        let mut settings = config();
        settings.threshold_mode = SpikeThresholdMode::Noise;
        settings.calibration_ms = 250;
        for saturated in [false, true] {
            let mut analyzer = SpikeAnalyzer::new(settings.clone()).unwrap();
            let mut input = block(0, 1, 8000, None);
            if saturated {
                for (index, value) in input.samples.iter_mut().enumerate() {
                    *value = if index % 2 == 0 { 0 } else { 65535 };
                }
            }
            assert!(analyzer.process(&input).unwrap().is_empty());
            let detection = &analyzer.thresholds()[0];
            assert_eq!(
                detection.state,
                if saturated { "saturated" } else { "flat" }
            );
            assert_eq!(detection.threshold_uv, None);
            assert!(analyzer
                .process(&block(8000, 1, 300, Some(100)))
                .unwrap()
                .is_empty());
            let gap = block(10000, 1, 300, Some(100));
            assert!(analyzer.process(&gap).unwrap().is_empty());
            assert_eq!(analyzer.thresholds()[0].state, "calibrating");
        }
    }

    #[test]
    fn noise_settings_validate_and_legacy_configs_keep_fixed_thresholds() {
        let mut value = serde_json::to_value(config()).unwrap();
        for name in ["thresholdMode", "noiseMultiplier", "calibrationMs"] {
            value.as_object_mut().unwrap().remove(name);
        }
        let legacy: SpikeConfig = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.threshold_mode, SpikeThresholdMode::Fixed);
        assert_eq!(
            SpikeConfig::default().threshold_mode,
            SpikeThresholdMode::Noise
        );
        for multiplier in [f64::NAN, 1.9, 12.1] {
            let mut settings = config();
            settings.noise_multiplier = multiplier;
            assert!(settings.validate().is_err());
        }
        for duration in [0, 249, 10001] {
            let mut settings = config();
            settings.calibration_ms = duration;
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn detects_full_rate_all_64_channels_and_exact_peaks() {
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        let events = analyzer.process(&block(1000, 64, 200, Some(50))).unwrap();
        assert_eq!(events.len(), 64);
        for (channel, event) in events.iter().enumerate() {
            assert_eq!(event.channel_index, channel);
            assert_eq!(event.sample_index, 1050);
            assert_eq!(event.peak_sample_index, 1050);
            assert_eq!(event.waveform_uv.len(), 91);
            assert!((event.peak_uv + 149.76).abs() < 0.001);
            assert_eq!(event.display_peak_uv, event.peak_uv);
            assert_eq!(event.stream, (channel / 16) as u8 + 2);
        }
    }

    #[test]
    fn marker_voltage_uses_display_input_at_the_detector_peak_even_with_own_filter() {
        let mut settings = config();
        settings.band_pass = true;
        settings.threshold_uv = -5.0;
        let mut analyzer = SpikeAnalyzer::new(settings).unwrap();
        let input = block(0, 1, 1200, Some(700));
        let events = analyzer.process(&input).unwrap();
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .any(|event| (event.display_peak_uv - event.peak_uv).abs() > 1.0));
        for event in events {
            let expected =
                (f64::from(input.samples[event.peak_sample_index as usize]) - 32768.0) * UV_PER_ADC;
            assert_eq!(event.display_peak_uv, expected);
        }
    }

    #[test]
    fn snippets_cross_blocks_but_never_cross_gaps_or_artifacts() {
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        assert!(analyzer
            .process(&block(0, 16, 80, Some(60)))
            .unwrap()
            .is_empty());
        let events = analyzer.process(&block(80, 16, 80, None)).unwrap();
        assert_eq!(events.len(), 16);
        assert_eq!(events[0].sample_index, 60);
        assert_eq!(events[0].display_peak_uv, events[0].peak_uv);
        assert!(analyzer
            .process(&block(160, 16, 80, Some(60)))
            .unwrap()
            .is_empty());
        assert!(analyzer
            .process(&block(300, 16, 80, None))
            .unwrap()
            .is_empty());
        assert_eq!(analyzer.discarded_incomplete, 16);
        let mut artifact = block(380, 16, 200, Some(60));
        artifact.flags.insert(SampleFlags::STIM_ARTIFACT);
        assert!(analyzer.process(&artifact).unwrap().is_empty());
    }

    #[test]
    fn refractory_and_polarity_are_respected() {
        let mut input = block(0, 1, 300, Some(50));
        input.samples[80] = 32000;
        input.samples[160] = 32000;
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        let events = analyzer.process(&input).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sample_index)
                .collect::<Vec<_>>(),
            vec![50, 160]
        );
        let mut positive = config();
        positive.threshold_uv = 50.0;
        let mut analyzer = SpikeAnalyzer::new(positive).unwrap();
        assert!(analyzer.process(&input).unwrap().is_empty());
        input.samples[50] = 33536;
        input.first_sample_index = 1000;
        assert_eq!(analyzer.process(&input).unwrap()[0].peak_uv, 149.76);
    }

    #[test]
    fn csv_contains_full_rate_values_and_refuses_overwrite() {
        let path = std::env::temp_dir().join(format!(
            "rhs-spikes-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = config();
        config.recording_path = Some(path.to_string_lossy().into());
        let mut analyzer = SpikeAnalyzer::new(config.clone()).unwrap();
        analyzer.process(&block(0, 16, 200, Some(50))).unwrap();
        analyzer.finish();
        assert!(analyzer.recording_error.is_none());
        let csv = std::fs::read_to_string(&path).unwrap();
        assert_eq!(csv.lines().count(), 17);
        assert!(csv.contains("-149.760"));
        assert!(SpikeAnalyzer::new(config)
            .unwrap()
            .recording_error
            .is_some());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cursor_reports_eviction_and_pending_tail_is_explicit() {
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        let events = analyzer.process(&block(0, 64, 200, Some(50))).unwrap();
        let mut store = SpikeStore::default();
        store.begin();
        for id in 1..=4200 {
            let mut event = events[0].clone();
            event.id = id;
            store.append(vec![event]);
        }
        let batch = store.poll(store.epoch, 0);
        assert_eq!(batch.missed_events, 104);
        assert_eq!(batch.events.len(), POLL_LIMIT);
        assert_eq!(store.counts[0], 4200);
        analyzer.process(&block(200, 64, 80, Some(60))).unwrap();
        analyzer.finish();
        assert_eq!(analyzer.discarded_incomplete, 64);
    }

    #[test]
    fn validates_nonfinite_and_inconsistent_settings() {
        let mut config = config();
        config.threshold_uv = f64::NAN;
        assert!(config.validate().is_err());
        config.threshold_uv = -50.0;
        config.refractory_ms = 1.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn sixteen_and_120_channel_shapes_are_supported() {
        for channels in [16, 120] {
            let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
            assert_eq!(
                analyzer
                    .process(&block(0, channels, 200, Some(50)))
                    .unwrap()
                    .len(),
                channels
            );
        }
    }

    #[test]
    fn full_rate_band_pass_runs_across_continuous_64_channel_blocks() {
        let started = Instant::now();
        let mut settings = config();
        settings.band_pass = true;
        let mut analyzer = SpikeAnalyzer::new(settings).unwrap();
        let mut counts = [0; 64];
        for chunk in 0..30_u64 {
            let mut input = block(chunk * 1000, 64, 1000, None);
            for offset in 790..=810 {
                let x = (offset as f64 - 800.0) / 4.0;
                let value = (32768.0 - 768.0 * (-x * x).exp()).round() as u16;
                input.samples[offset * 64..(offset + 1) * 64].fill(value);
            }
            for event in analyzer.process(&input).unwrap() {
                assert!(event.peak_uv < -50.0);
                assert_eq!(event.waveform_uv.len(), 91);
                counts[event.channel_index] += 1;
            }
        }
        assert!(counts.iter().all(|count| *count == 30));
        assert_eq!(analyzer.discontinuities, 0);
        eprintln!("Spike DSP: 1 second of 64 x 30 kHz data processed in {:?} (test build, no USB/UI/disk)", started.elapsed());
    }

    #[test]
    fn recording_failure_is_visible_without_disabling_detection() {
        let path =
            std::env::temp_dir().join(format!("rhs-spike-readonly-{}.csv", std::process::id()));
        std::fs::write(&path, "test").unwrap();
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        analyzer.recorder = Some(BufWriter::new(File::open(&path).unwrap()));
        analyzer.last_flush = Instant::now() - Duration::from_secs(2);
        assert_eq!(
            analyzer
                .process(&block(0, 16, 200, Some(50)))
                .unwrap()
                .len(),
            16
        );
        assert!(analyzer
            .recording_error
            .as_deref()
            .unwrap()
            .contains("write failed"));
        assert_eq!(
            analyzer
                .process(&block(200, 16, 200, Some(50)))
                .unwrap()
                .len(),
            16
        );
        analyzer.finish();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn seek_segments_clear_history_without_reusing_ids_or_inventing_poll_loss() {
        let mut analyzer = SpikeAnalyzer::new(config()).unwrap();
        let mut store = SpikeStore {
            config: config(),
            ..SpikeStore::default()
        };
        store.begin();
        store.append(analyzer.process(&block(1000, 16, 200, Some(50))).unwrap());
        store.begin_segment(analyzer.restart_segment());
        store.append(analyzer.process(&block(0, 16, 200, Some(50))).unwrap());
        let batch = store.poll(0, 0);
        assert_eq!(batch.missed_events, 0);
        assert_eq!(batch.events[0].id, 17);
        assert_eq!(batch.events[0].sample_index, 50);
        assert_eq!(batch.counts, vec![1; 16]);
    }
}
