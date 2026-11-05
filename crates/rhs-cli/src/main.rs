mod functional_check;

use rhs_core::{SampleBlock, SampleFlags};
use rhs_device::{
    AcquisitionConfig, AcquisitionHandle, AcquisitionWorkerConfig, RhsDevice, ScanReport,
    StimConfig, StimHardwareSession, StimPoll, StimStepSize, WorkerStatus,
};
use rhs_nodes::{
    ArtifactBlanker, BandPassProcessor, BlankingConfig, BlankingReplacement, MeasuredProcessor,
    MeasuredSink, NodeMetricsSnapshot, NotchProcessor, ThresholdConfig, ThresholdDirection,
    ThresholdProcessor, ViewerSink,
};
use rhs_pipeline::{
    bounded_queue, CancellationToken, PipelineError, PipelineRunner, Processor, Sink, Source,
    SourceError, SyntheticConfig, SyntheticSource,
};
use rhs_stim::{
    ArmRequest, DryRunExecutor, ExecutionState, RejectionReason, StimCommand, StimExecutor,
    StimOutcome, StimParameters, StimPreset, StimSafetyPolicy, StimService, StimServiceState,
};
use rhs_storage::{sha256_file, FileReplaySource, RawRecorderSink};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

fn main() {
    if let Err(error) = run() {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        args.push("scan".to_string());
    }

    let command = args.remove(0);
    match command.as_str() {
        "scan" => {
            let (device, _) = parse_common(args)?;
            let report = device.scan()?;
            print_report(&report);
        }
        "stim" => {
            let (device, args) = parse_common(args)?;
            let options = parse_stim(args)?;
            run_hardware_stim(device, options)?;
        }
        "stim-service-check" => {
            if !args.is_empty() {
                return Err("stim-service-check does not accept options".into());
            }
            run_stim_service_check()?;
        }
        "stim64-functional-check" => {
            let (device, rest) = parse_common(args)?;
            functional_check::run(device, rest)?;
        }
        "stim-session-check" => {
            let (device, rest) = parse_common(args)?;
            run_stim_session_check(device, parse_stim_session_target(&rest)?)?;
        }
        "sync-session-check" | "sync64-session-check" => {
            let (device, rest) = parse_common(args)?;
            let channel_mask = parse_sync_channel_mask(rest)?;
            run_sync_session_check(
                device,
                channel_mask,
                if command == "sync64-session-check" {
                    64
                } else {
                    16
                },
            )?;
        }
        "acquire" => {
            let (device, args) = parse_common(args)?;
            let options = parse_acquire(args)?;
            run_acquire(device, options)?;
        }
        "record" => {
            let (device, args) = parse_common(args)?;
            let options = parse_record(args)?;
            run_record(device, options)?;
        }
        "replay-check" => {
            let input = parse_replay(args)?;
            run_replay_check(input)?;
        }
        "viewer-check" => {
            let (device, args) = parse_common(args)?;
            let options = parse_acquire(args)?;
            run_viewer_check(device, options)?;
        }
        "synthetic-check" => {
            let blocks = parse_synthetic(args)?;
            run_synthetic_check(blocks)?;
        }
        "help" | "--help" | "-h" => print_usage(),
        other => return Err(format!("unknown command: {other}").into()),
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct AcquireOptions {
    seconds: u64,
    blocks_per_read: u8,
    queue_capacity: usize,
    channel_count: usize,
}

#[derive(Debug, Clone)]
struct ChannelSignalStats {
    count: u64,
    minimum: u16,
    maximum: u16,
    mean: f64,
    m2: f64,
    exact_rail_samples: u64,
    near_rail_samples: u64,
    step_count: u64,
    large_steps: u64,
    upward_center_crossings: u64,
    maximum_step: u16,
    previous: Option<u16>,
    common_sum: f64,
    common_sum_sq: f64,
    sample_common_sum: f64,
}

impl Default for ChannelSignalStats {
    fn default() -> Self {
        Self {
            count: 0,
            minimum: u16::MAX,
            maximum: u16::MIN,
            mean: 0.0,
            m2: 0.0,
            exact_rail_samples: 0,
            near_rail_samples: 0,
            step_count: 0,
            large_steps: 0,
            upward_center_crossings: 0,
            maximum_step: 0,
            previous: None,
            common_sum: 0.0,
            common_sum_sq: 0.0,
            sample_common_sum: 0.0,
        }
    }
}

impl ChannelSignalStats {
    const NEAR_RAIL_CODES: u16 = 512;
    const LARGE_STEP_CODES: u16 = 8_192;

    fn observe(&mut self, sample: u16, common_mode: f64) {
        self.count += 1;
        self.minimum = self.minimum.min(sample);
        self.maximum = self.maximum.max(sample);

        let value = f64::from(sample);
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        self.m2 += delta * (value - self.mean);

        if sample == u16::MIN || sample == u16::MAX {
            self.exact_rail_samples += 1;
        }
        if sample <= Self::NEAR_RAIL_CODES || sample >= u16::MAX - Self::NEAR_RAIL_CODES {
            self.near_rail_samples += 1;
        }

        if let Some(previous) = self.previous {
            let step = sample.abs_diff(previous);
            self.step_count += 1;
            self.maximum_step = self.maximum_step.max(step);
            if step >= Self::LARGE_STEP_CODES {
                self.large_steps += 1;
            }
            if previous < 32_768 && sample >= 32_768 {
                self.upward_center_crossings += 1;
            }
        }
        self.previous = Some(sample);

        self.common_sum += common_mode;
        self.common_sum_sq += common_mode * common_mode;
        self.sample_common_sum += value * common_mode;
    }

    fn standard_deviation(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        (self.m2 / (self.count - 1) as f64).sqrt()
    }

    fn rail_percent(&self) -> f64 {
        percent(self.near_rail_samples, self.count)
    }

    fn exact_rail_percent(&self) -> f64 {
        percent(self.exact_rail_samples, self.count)
    }

    fn large_step_percent(&self) -> f64 {
        percent(self.large_steps, self.step_count)
    }

    fn common_mode_correlation(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        let count = self.count as f64;
        let sample_sum = self.mean * count;
        let sample_sum_sq = self.m2 + count * self.mean * self.mean;
        let numerator = count * self.sample_common_sum - sample_sum * self.common_sum;
        let sample_term = count * sample_sum_sq - sample_sum * sample_sum;
        let common_term = count * self.common_sum_sq - self.common_sum * self.common_sum;
        let denominator = (sample_term * common_term).sqrt();
        if denominator <= f64::EPSILON {
            0.0
        } else {
            (numerator / denominator).clamp(-1.0, 1.0)
        }
    }

    fn spans_almost_full_scale(&self) -> bool {
        self.minimum <= Self::NEAR_RAIL_CODES && self.maximum >= u16::MAX - Self::NEAR_RAIL_CODES
    }

    fn touches_a_rail(&self) -> bool {
        self.minimum <= Self::NEAR_RAIL_CODES || self.maximum >= u16::MAX - Self::NEAR_RAIL_CODES
    }

    fn center_crossing_hz(&self, elapsed: Duration) -> f64 {
        self.upward_center_crossings as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
    }
}

fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * part as f64 / total as f64
    }
}

fn observe_signal_block(
    block: &SampleBlock,
    channel_stats: &mut Vec<ChannelSignalStats>,
) -> Result<(), Box<dyn std::error::Error>> {
    let channel_count = block.channels.len();
    if channel_count == 0 {
        return Err("acquisition block has no channels".into());
    }
    if channel_stats.is_empty() {
        channel_stats.resize_with(channel_count, ChannelSignalStats::default);
    } else if channel_stats.len() != channel_count {
        return Err(format!(
            "channel count changed during acquisition: {} to {channel_count}",
            channel_stats.len()
        )
        .into());
    }

    let mut frames = block.samples.chunks_exact(channel_count);
    for frame in &mut frames {
        let common_mode =
            frame.iter().map(|sample| f64::from(*sample)).sum::<f64>() / channel_count as f64;
        for (stats, sample) in channel_stats.iter_mut().zip(frame) {
            stats.observe(*sample, common_mode);
        }
    }
    if !frames.remainder().is_empty() {
        return Err("sample buffer is not sample-major interleaved".into());
    }
    Ok(())
}

fn print_signal_diagnostics(channel_stats: &[ChannelSignalStats], elapsed: Duration) {
    println!("signal_quality (unsigned 16-bit ADC codes; center=32768):");
    println!(
        "channel  min    max    mean      stddev   near_rail  exact_rail  large_step  center_hz  common_corr"
    );
    for (channel, stats) in channel_stats.iter().enumerate() {
        println!(
            "{channel:>7}  {:>5}  {:>5}  {:>8.1}  {:>8.1}  {:>8.3}%  {:>9.3}%  {:>9.3}%  {:>9.1}  {:>11.3}",
            stats.minimum,
            stats.maximum,
            stats.mean,
            stats.standard_deviation(),
            stats.rail_percent(),
            stats.exact_rail_percent(),
            stats.large_step_percent(),
            stats.center_crossing_hz(elapsed),
            stats.common_mode_correlation(),
        );
    }

    let rail_touch_channels = channel_stats
        .iter()
        .filter(|stats| stats.touches_a_rail())
        .count();
    let full_scale_channels = channel_stats
        .iter()
        .filter(|stats| stats.spans_almost_full_scale())
        .count();
    let strongly_common_channels = channel_stats
        .iter()
        .filter(|stats| stats.common_mode_correlation().abs() >= 0.8)
        .count();
    let large_steps = channel_stats
        .iter()
        .map(|stats| stats.large_steps)
        .sum::<u64>();
    let mut center_frequencies = channel_stats
        .iter()
        .map(|stats| stats.center_crossing_hz(elapsed))
        .collect::<Vec<_>>();
    center_frequencies.sort_by(f64::total_cmp);
    let median_center_hz = center_frequencies
        .get(center_frequencies.len() / 2)
        .copied()
        .unwrap_or(0.0);
    println!(
        "signal_summary: rail_touch_channels={}/{} full_scale_channels={}/{} strongly_common_channels={}/{} median_center_hz={:.1} large_steps_per_second={:.1}",
        rail_touch_channels,
        channel_stats.len(),
        full_scale_channels,
        channel_stats.len(),
        strongly_common_channels,
        channel_stats.len(),
        median_center_hz,
        large_steps as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
    );
    if rail_touch_channels * 2 >= channel_stats.len().max(1) {
        println!(
            "SIGNAL WARNING: most channels touch an ADC rail; this is saturation, not a normal low-noise baseline"
        );
    }
}

#[derive(Debug)]
struct RecordOptions {
    output: PathBuf,
    acquisition: AcquireOptions,
}

fn parse_record(args: Vec<String>) -> Result<RecordOptions, Box<dyn std::error::Error>> {
    let mut output = None;
    let mut acquisition_args = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--output" {
            i += 1;
            output = Some(PathBuf::from(
                args.get(i).ok_or("--output requires a path")?,
            ));
        } else {
            acquisition_args.push(args[i].clone());
        }
        i += 1;
    }

    Ok(RecordOptions {
        output: output.ok_or("record requires --output PATH")?,
        acquisition: parse_acquire(acquisition_args)?,
    })
}

fn parse_replay(args: Vec<String>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut input = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                i += 1;
                input = Some(PathBuf::from(args.get(i).ok_or("--input requires a path")?));
            }
            other => return Err(format!("unknown replay-check option: {other}").into()),
        }
        i += 1;
    }
    input.ok_or_else(|| "replay-check requires --input PATH".into())
}

fn parse_acquire(args: Vec<String>) -> Result<AcquireOptions, Box<dyn std::error::Error>> {
    let mut options = AcquireOptions {
        seconds: 5,
        blocks_per_read: 8,
        queue_capacity: 8,
        channel_count: 16,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--channels" => {
                i += 1;
                options.channel_count =
                    args.get(i).ok_or("--channels requires a value")?.parse()?;
            }
            "--seconds" => {
                i += 1;
                options.seconds = args.get(i).ok_or("--seconds requires a value")?.parse()?;
            }
            "--blocks-per-read" => {
                i += 1;
                options.blocks_per_read = args
                    .get(i)
                    .ok_or("--blocks-per-read requires a value")?
                    .parse()?;
            }
            "--queue-capacity" => {
                i += 1;
                options.queue_capacity = args
                    .get(i)
                    .ok_or("--queue-capacity requires a value")?
                    .parse()?;
            }
            other => return Err(format!("unknown acquire option: {other}").into()),
        }
        i += 1;
    }

    if !matches!(options.channel_count, 16 | 64) {
        return Err("--channels must be 16 or 64".into());
    }
    if !(1..=3_600).contains(&options.seconds) {
        return Err("--seconds must be 1..3600".into());
    }
    if !(1..=32).contains(&options.blocks_per_read) {
        return Err("--blocks-per-read must be 1..32".into());
    }
    if options.queue_capacity == 0 {
        return Err("--queue-capacity must be greater than zero".into());
    }
    Ok(options)
}

fn run_acquire(
    device: RhsDevice,
    options: AcquireOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting read-only acquisition diagnostic");
    println!(
        "duration={}s blocks_per_read={} queue_capacity={} stimulation=disabled",
        options.seconds, options.blocks_per_read, options.queue_capacity
    );

    let worker_config = AcquisitionWorkerConfig {
        channel_count: options.channel_count,
        acquisition: AcquisitionConfig {
            blocks_per_read: options.blocks_per_read,
        },
        data_queue_capacity: options.queue_capacity,
        ..AcquisitionWorkerConfig::default()
    };
    let handle = device.spawn_acquisition(worker_config)?;
    if let Ok(WorkerStatus::Started(info)) = handle.try_status() {
        println!(
            "device ready: port={} stream={} channels={} sample_rate={}Hz samples_per_batch={}",
            char::from(b'A' + info.target_port),
            info.target_stream,
            info.channel_count,
            info.sample_rate_hz,
            info.samples_per_block * info.blocks_per_read
        );
    }

    let start = Instant::now();
    let deadline = start + Duration::from_secs(options.seconds);
    let mut blocks = 0_u64;
    let mut samples_per_channel = 0_u64;
    let mut expected_sequence = 0_u64;
    let mut expected_first_sample = None;
    let mut raw_min = u16::MAX;
    let mut raw_max = u16::MIN;
    let mut channel_stats = Vec::new();

    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(block) => {
                validate_acquired_block(&block, expected_sequence, expected_first_sample)?;
                expected_sequence += 1;
                expected_first_sample = Some(block.last_sample_index().saturating_add(1));
                blocks += 1;
                samples_per_channel += block.samples_per_channel as u64;
                observe_signal_block(&block, &mut channel_stats)?;
                if let Some(minimum) = block.samples.iter().min() {
                    raw_min = raw_min.min(*minimum);
                }
                if let Some(maximum) = block.samples.iter().max() {
                    raw_max = raw_max.max(*maximum);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(WorkerStatus::Fault(error)) = handle.try_status() {
                    return Err(error.into());
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Ok(WorkerStatus::Fault(error)) = handle.try_status() {
                    return Err(error.into());
                }
                return Err("acquisition worker disconnected unexpectedly".into());
            }
        }
    }

    let elapsed = start.elapsed();
    let stats = handle.stop()?;
    let sample_rate = samples_per_channel as f64 / elapsed.as_secs_f64();
    println!("acquisition complete");
    println!("elapsed_seconds: {:.3}", elapsed.as_secs_f64());
    println!("sample_blocks: {blocks}");
    println!("samples_per_channel: {samples_per_channel}");
    println!("observed_samples_per_second: {:.1}", sample_rate);
    println!("raw_sample_range: {raw_min}..{raw_max}");
    println!("bytes_read: {}", stats.bytes_read);
    println!("timestamp_gaps: {}", stats.timestamp_gaps);
    println!("queue_overflows: {}", stats.queue_overflows);
    println!("empty_reads: {}", stats.empty_reads);
    print_signal_diagnostics(&channel_stats, elapsed);

    if blocks == 0 {
        return Err("no acquisition blocks were received".into());
    }
    if stats.timestamp_gaps != 0 {
        return Err(format!("detected {} hardware timestamp gaps", stats.timestamp_gaps).into());
    }
    if stats.queue_overflows != 0 {
        return Err(format!("detected {} raw queue overflows", stats.queue_overflows).into());
    }
    println!("PASS: read-only acquisition data remained contiguous");
    Ok(())
}

struct TimedHardwareSource<'a> {
    handle: &'a AcquisitionHandle,
    deadline: Instant,
    stop_requested: bool,
}

impl TimedHardwareSource<'_> {
    fn poll_terminal_status(&self) -> Result<bool, SourceError> {
        let mut stopped = false;
        loop {
            match self.handle.try_status() {
                Ok(WorkerStatus::Started(_)) => {}
                Ok(WorkerStatus::Stopped) => stopped = true,
                Ok(WorkerStatus::Fault(error)) => {
                    return Err(SourceError::new(format!(
                        "hardware acquisition fault: {error}"
                    )));
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(stopped),
            }
        }
    }
}

impl Source for TimedHardwareSource<'_> {
    fn next_block(&mut self) -> Result<Option<SampleBlock>, SourceError> {
        loop {
            let now = Instant::now();
            if !self.stop_requested && now >= self.deadline {
                self.handle.request_stop();
                self.stop_requested = true;
            }

            let timeout = if self.stop_requested {
                Duration::from_millis(250)
            } else {
                self.deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(250))
            };
            match self.handle.recv_timeout(timeout) {
                Ok(block) => return Ok(Some(block)),
                Err(RecvTimeoutError::Timeout) => {
                    let stopped = self.poll_terminal_status()?;
                    if stopped {
                        if self.stop_requested {
                            return Ok(None);
                        }
                        return Err(SourceError::new(
                            "hardware acquisition stopped before the requested duration",
                        ));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.poll_terminal_status()?;
                    if self.stop_requested {
                        return Ok(None);
                    }
                    return Err(SourceError::new(
                        "hardware acquisition disconnected unexpectedly",
                    ));
                }
            }
        }
    }
}

#[derive(Default)]
struct ContinuityProcessor {
    expected_sequence: u64,
    expected_first_sample: Option<u64>,
}

impl Processor for ContinuityProcessor {
    fn name(&self) -> &str {
        "continuity-check"
    }

    fn process(&mut self, block: SampleBlock) -> Result<Option<SampleBlock>, PipelineError> {
        validate_acquired_block(&block, self.expected_sequence, self.expected_first_sample)
            .map_err(|error| PipelineError::new("continuity-check", error.to_string()))?;
        self.expected_sequence += 1;
        self.expected_first_sample = Some(block.last_sample_index().saturating_add(1));
        Ok(Some(block))
    }
}

fn run_record(device: RhsDevice, options: RecordOptions) -> Result<(), Box<dyn std::error::Error>> {
    let bitfile_path = device.bitfile_path().to_path_buf();
    let bitfile_sha256 = sha256_file(&bitfile_path)?;
    let worker_config = AcquisitionWorkerConfig {
        channel_count: options.acquisition.channel_count,
        acquisition: AcquisitionConfig {
            blocks_per_read: options.acquisition.blocks_per_read,
        },
        data_queue_capacity: options.acquisition.queue_capacity,
        ..AcquisitionWorkerConfig::default()
    };

    println!("Starting read-only hardware recording pipeline");
    println!(
        "duration={}s blocks_per_read={} queue_capacity={} stimulation=disabled",
        options.acquisition.seconds,
        options.acquisition.blocks_per_read,
        options.acquisition.queue_capacity
    );
    let handle = device.spawn_acquisition(worker_config)?;
    let info = handle.info().clone();
    if info.device_serial.is_empty() {
        return Err("hardware opened without reporting a FrontPanel serial number".into());
    }
    println!(
        "device ready: serial={} port={} stream={} channels={} sample_rate={}Hz",
        info.device_serial,
        char::from(b'A' + info.target_port),
        info.target_stream,
        info.channel_count,
        info.sample_rate_hz
    );

    let metadata = vec![
        ("device_serial".to_owned(), info.device_serial.clone()),
        (
            "bitfile_path".to_owned(),
            bitfile_path.to_string_lossy().into_owned(),
        ),
        ("bitfile_sha256".to_owned(), bitfile_sha256.clone()),
        (
            "target_port".to_owned(),
            char::from(b'A' + info.target_port).to_string(),
        ),
        ("target_stream".to_owned(), info.target_stream.to_string()),
        ("stimulation".to_owned(), "disabled".to_owned()),
    ];
    let recorder = RawRecorderSink::create_with_metadata(&options.output, metadata)?;
    let mut processors: Vec<Box<dyn Processor>> = vec![Box::new(ContinuityProcessor::default())];
    let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(recorder)];
    let runner = PipelineRunner::new(CancellationToken::new());
    let start = Instant::now();
    let run_result = {
        let mut source = TimedHardwareSource {
            handle: &handle,
            deadline: start + Duration::from_secs(options.acquisition.seconds),
            stop_requested: false,
        };
        runner.run(&mut source, &mut processors, &mut sinks)
    };
    drop(sinks);
    let stop_result = handle.stop();
    let report = run_result?;
    let stats = stop_result?;
    let elapsed = start.elapsed();
    let file_bytes = std::fs::metadata(&options.output)?.len();

    println!("recording complete: {}", options.output.display());
    println!("elapsed_seconds: {:.3}", elapsed.as_secs_f64());
    println!("sample_blocks: {}", report.emitted_blocks);
    println!("samples_per_channel: {}", stats.samples_per_channel);
    println!("recording_bytes: {file_bytes}");
    println!("bitfile_sha256: {bitfile_sha256}");
    println!("timestamp_gaps: {}", stats.timestamp_gaps);
    println!("queue_overflows: {}", stats.queue_overflows);
    if report.emitted_blocks == 0 {
        return Err("recording produced no data blocks".into());
    }
    if report.emitted_blocks != stats.batches {
        return Err(format!(
            "pipeline/worker block mismatch: emitted {}, worker produced {}",
            report.emitted_blocks, stats.batches
        )
        .into());
    }
    if stats.timestamp_gaps != 0 || stats.queue_overflows != 0 {
        return Err(format!(
            "recording integrity failure: timestamp_gaps={} queue_overflows={}",
            stats.timestamp_gaps, stats.queue_overflows
        )
        .into());
    }
    println!("PASS: hardware data was recorded contiguously and finalized");
    Ok(())
}

fn run_replay_check(input: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut replay = FileReplaySource::open(&input)?;
    let info = replay.info().clone();
    println!("recording: {}", input.display());
    println!("format_version: {}", info.version);
    println!("complete: {}", info.complete);
    println!("source_id: {}", info.source_id);
    println!("sample_rate_hz: {}", info.sample_rate_hz);
    println!("channels: {}", info.channels.len());
    for (key, value) in &info.metadata {
        println!("metadata.{key}: {value}");
    }

    let mut blocks = 0_u64;
    let mut samples_per_channel = 0_u64;
    let mut expected_sequence = None;
    let mut expected_first_sample = None;
    let mut discontinuities = 0_u64;
    let mut raw_min = u16::MAX;
    let mut raw_max = u16::MIN;
    while let Some(block) = replay.next_block()? {
        block.validate()?;
        if let Some(expected) = expected_sequence {
            if block.sequence != expected {
                return Err(format!(
                    "replay sequence mismatch: expected {expected}, got {}",
                    block.sequence
                )
                .into());
            }
        }
        if let Some(expected) = expected_first_sample {
            if block.first_sample_index != expected
                && !block.flags.contains(SampleFlags::DISCONTINUITY)
            {
                return Err(format!(
                    "replay sample index mismatch: expected {expected}, got {}",
                    block.first_sample_index
                )
                .into());
            }
        }
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            discontinuities += 1;
        }
        expected_sequence = Some(block.sequence.saturating_add(1));
        expected_first_sample = Some(block.last_sample_index().saturating_add(1));
        blocks += 1;
        samples_per_channel += block.samples_per_channel as u64;
        if let Some(minimum) = block.samples.iter().min() {
            raw_min = raw_min.min(*minimum);
        }
        if let Some(maximum) = block.samples.iter().max() {
            raw_max = raw_max.max(*maximum);
        }
    }

    if !info.complete {
        return Err("recording contains a valid recoverable prefix but is not finalized".into());
    }
    if blocks == 0 {
        return Err("recording contains no data blocks".into());
    }
    if let (Some(path), Some(expected_hash)) = (
        metadata_value(&info.metadata, "bitfile_path"),
        metadata_value(&info.metadata, "bitfile_sha256"),
    ) {
        let path = PathBuf::from(path);
        if path.exists() {
            let actual_hash = sha256_file(&path)?;
            if actual_hash != expected_hash {
                return Err(format!(
                    "bitfile fingerprint mismatch: recorded {expected_hash}, current {actual_hash}"
                )
                .into());
            }
            println!("bitfile_fingerprint_check: match");
        } else {
            println!("bitfile_fingerprint_check: skipped (recorded path is unavailable)");
        }
    }
    println!("sample_blocks: {blocks}");
    println!("samples_per_channel: {samples_per_channel}");
    println!("raw_sample_range: {raw_min}..{raw_max}");
    println!("discontinuities: {discontinuities}");
    if discontinuities != 0 {
        return Err(format!("recording contains {discontinuities} discontinuities").into());
    }
    println!("PASS: recording CRC, footer, continuity, and metadata checks succeeded");
    Ok(())
}

fn run_viewer_check(
    device: RhsDevice,
    options: AcquireOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting real-time processing and viewer diagnostic");
    println!(
        "duration={}s filters=50Hz-notch+300-3000Hz-bandpass viewer_queue=4 stimulation=disabled",
        options.seconds
    );
    let handle = device.spawn_acquisition(AcquisitionWorkerConfig {
        channel_count: options.channel_count,
        acquisition: AcquisitionConfig {
            blocks_per_read: options.blocks_per_read,
        },
        data_queue_capacity: options.queue_capacity,
        ..AcquisitionWorkerConfig::default()
    })?;
    let info = handle.info();
    println!(
        "device ready: serial={} port={} stream={} channels={} sample_rate={}Hz",
        info.device_serial,
        char::from(b'A' + info.target_port),
        info.target_stream,
        info.channel_count,
        info.sample_rate_hz
    );

    let (continuity, continuity_metrics) = MeasuredProcessor::new(ContinuityProcessor::default());
    let (blanker, blanker_metrics) =
        MeasuredProcessor::new(ArtifactBlanker::new(BlankingConfig {
            ttl_mask: 1,
            blank_samples_after_rising_edge: 90,
            replacement: BlankingReplacement::HoldLast,
        })?);
    let (notch, notch_metrics) = MeasuredProcessor::new(NotchProcessor::new(50.0, 20.0)?);
    let (band_pass, band_pass_metrics) =
        MeasuredProcessor::new(BandPassProcessor::new(300.0, 3_000.0)?);
    let (threshold, threshold_events) = ThresholdProcessor::new(ThresholdConfig {
        channel_index: 0,
        threshold: u16::MAX,
        direction: ThresholdDirection::RisingAbove,
        refractory_samples: info.sample_rate_hz,
        event_queue_capacity: 16,
    })?;
    let (threshold, threshold_metrics) = MeasuredProcessor::new(threshold);
    let (viewer, viewer_receiver) = ViewerSink::new(4, 256)?;
    let (viewer, viewer_metrics) = MeasuredSink::new(viewer);

    let mut processors: Vec<Box<dyn Processor>> = vec![
        Box::new(continuity),
        Box::new(blanker),
        Box::new(notch),
        Box::new(band_pass),
        Box::new(threshold),
    ];
    let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(viewer)];
    let runner = PipelineRunner::new(CancellationToken::new());
    let started = Instant::now();
    let run_result = {
        let mut source = TimedHardwareSource {
            handle: &handle,
            deadline: started + Duration::from_secs(options.seconds),
            stop_requested: false,
        };
        runner.run(&mut source, &mut processors, &mut sinks)
    };
    drop(sinks);
    let stop_result = handle.stop();
    let report = run_result?;
    let acquisition_stats = stop_result?;
    let latest_frame = viewer_receiver
        .drain_latest()
        .ok_or("viewer pipeline produced no frames")?;
    let viewer_stats = viewer_receiver.stats();

    println!("elapsed_seconds: {:.3}", started.elapsed().as_secs_f64());
    println!("processed_blocks: {}", report.emitted_blocks);
    println!(
        "samples_per_channel: {}",
        acquisition_stats.samples_per_channel
    );
    println!("timestamp_gaps: {}", acquisition_stats.timestamp_gaps);
    println!(
        "acquisition_queue_overflows: {}",
        acquisition_stats.queue_overflows
    );
    println!("viewer_frames_enqueued: {}", viewer_stats.enqueued);
    println!(
        "viewer_frames_dropped_oldest: {}",
        viewer_stats.dropped_oldest
    );
    println!("viewer_max_depth: {}", viewer_stats.max_depth);
    println!("viewer_latest_points: {}", latest_frame.point_count);
    println!(
        "viewer_max_queue_latency_us: {:.1}",
        viewer_stats.max_queue_latency_ns as f64 / 1_000.0
    );
    println!("threshold_events: {}", threshold_events.generated());
    print_node_metrics("continuity", continuity_metrics.snapshot());
    print_node_metrics("artifact-blanker", blanker_metrics.snapshot());
    print_node_metrics("notch", notch_metrics.snapshot());
    print_node_metrics("band-pass", band_pass_metrics.snapshot());
    print_node_metrics("threshold", threshold_metrics.snapshot());
    print_node_metrics("viewer", viewer_metrics.snapshot());

    if report.emitted_blocks == 0 || report.emitted_blocks != acquisition_stats.batches {
        return Err("real-time pipeline did not process every acquired block".into());
    }
    if acquisition_stats.timestamp_gaps != 0 || acquisition_stats.queue_overflows != 0 {
        return Err("real-time processing caused acquisition data loss".into());
    }
    if viewer_stats.enqueued != report.emitted_blocks || viewer_stats.max_depth > 4 {
        return Err("viewer queue accounting is inconsistent".into());
    }
    if latest_frame.sequence.saturating_add(1) != report.emitted_blocks {
        return Err("viewer did not retain the newest processed frame".into());
    }
    if latest_frame.point_count > 256 {
        return Err("viewer frame exceeded its configured point budget".into());
    }
    println!("PASS: real-time nodes kept acquisition contiguous while stale viewer frames dropped");
    Ok(())
}

fn print_node_metrics(name: &str, metrics: NodeMetricsSnapshot) {
    let average_ns = if metrics.calls == 0 {
        0.0
    } else {
        metrics.total_processing_ns as f64 / metrics.calls as f64
    };
    println!(
        "node.{name}: calls={} errors={} average_us={:.1} max_us={:.1}",
        metrics.calls,
        metrics.errors,
        average_ns / 1_000.0,
        metrics.max_processing_ns as f64 / 1_000.0
    );
}

fn metadata_value<'a>(metadata: &'a [(String, String)], key: &str) -> Option<&'a str> {
    metadata
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

fn validate_acquired_block(
    block: &SampleBlock,
    expected_sequence: u64,
    expected_first_sample: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    block.validate()?;
    if block.sequence != expected_sequence {
        return Err(format!(
            "block sequence mismatch: expected {expected_sequence}, got {}",
            block.sequence
        )
        .into());
    }
    if block.flags.contains(SampleFlags::DISCONTINUITY) {
        return Err(format!(
            "hardware timestamp discontinuity in block {}",
            block.sequence
        )
        .into());
    }
    if let Some(expected) = expected_first_sample {
        if block.first_sample_index != expected {
            return Err(format!(
                "sample index mismatch: expected {expected}, got {}",
                block.first_sample_index
            )
            .into());
        }
    }
    Ok(())
}

fn parse_synthetic(args: Vec<String>) -> Result<u64, Box<dyn std::error::Error>> {
    let mut blocks = 100_u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--blocks" => {
                i += 1;
                blocks = args.get(i).ok_or("--blocks requires a value")?.parse()?;
            }
            other => return Err(format!("unknown synthetic-check option: {other}").into()),
        }
        i += 1;
    }
    if blocks == 0 {
        return Err("--blocks must be greater than zero".into());
    }
    Ok(blocks)
}

fn run_synthetic_check(block_count: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut source = SyntheticSource::new(SyntheticConfig {
        total_blocks: Some(block_count),
        ..SyntheticConfig::default()
    })?;
    let (producer, consumer) = bounded_queue::<SampleBlock>(4)?;
    let mut received = 0_u64;
    let mut expected_first_sample = 0_u64;

    while let Some(block) = source.next_block()? {
        producer
            .try_send(block)
            .map_err(|error| format!("queue send failed: {error:?}"))?;
        let block = consumer
            .try_recv()
            .map_err(|error| format!("queue receive failed: {error:?}"))?;
        if block.sequence != received || block.first_sample_index != expected_first_sample {
            return Err("synthetic pipeline continuity check failed".into());
        }
        expected_first_sample = block.last_sample_index() + 1;
        received += 1;
    }

    let stats = consumer.stats();
    println!("synthetic_blocks: {received}");
    println!("queue_max_depth: {}", stats.max_depth);
    println!("queue_rejected: {}", stats.rejected);
    if received != block_count || stats.rejected != 0 {
        return Err("synthetic pipeline did not complete cleanly".into());
    }
    println!("PASS: synthetic source and bounded queue remained contiguous");
    Ok(())
}

fn parse_common(args: Vec<String>) -> Result<(RhsDevice, Vec<String>), Box<dyn std::error::Error>> {
    let mut bitfile = rhs_device::configured_bitfile();
    let mut serial = None;
    let mut rest = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bitfile" => {
                i += 1;
                bitfile = PathBuf::from(args.get(i).ok_or("--bitfile requires a path")?);
            }
            "--serial" => {
                i += 1;
                serial = Some(args.get(i).ok_or("--serial requires a value")?.clone());
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }

    let mut device = RhsDevice::new(bitfile);
    if let Some(serial) = serial {
        device = device.with_serial(serial);
    }
    Ok((device, rest))
}

fn run_stim_service_check() -> Result<(), Box<dyn std::error::Error>> {
    let policy = StimSafetyPolicy {
        hardware_timestep_hz: 30_000,
        allowed_channels: vec![0],
        allowed_step_sizes: vec![StimStepSize::N10],
        max_magnitude_steps: 2,
        max_phase_steps: 12,
        max_interphase_steps: 6,
        min_period_steps: 3_000,
        max_period_steps: 3_000,
        max_pulses_per_command: 2,
        max_pulses_per_arm_session: 4,
        max_phase_charge_picocoulombs: 10,
        max_active_duty_ppm: 10_000,
        max_train_duration_us: 200_000,
        min_command_interval_us: 100_000,
        max_arm_duration_us: 1_000_000,
        max_command_lifetime_us: 100_000,
    };
    let preset = StimPreset {
        id: "software-test-single".to_owned(),
        label: "Software-only 10 nA-step single pulse".to_owned(),
        parameters: StimParameters {
            channel: 0,
            trigger: 0,
            pulses: 1,
            magnitude_steps: 1,
            step_size: StimStepSize::N10,
            phase_steps: 6,
            interphase_steps: 3,
            period_steps: 3_000,
        },
    };
    let phase_charge = preset
        .parameters
        .phase_charge_picocoulombs_ceil(policy.hardware_timestep_hz);
    let mut service = StimService::new(policy, vec![preset])?;
    let session = service.arm(
        ArmRequest {
            operator: "rhs-cli-dry-run".to_owned(),
            reason: "validate stimulation safety state machine without hardware output".to_owned(),
            approved_preset_ids: vec!["software-test-single".to_owned()],
        },
        0,
    )?;
    let mut executor = DryRunExecutor::new(0);

    let first = StimCommand {
        id: 1,
        arm_session_id: session.id,
        preset_id: "software-test-single".to_owned(),
        issued_at_us: 10,
        not_before_us: 10,
        deadline_us: 100_010,
    };
    let queued = service.submit(first, 10);
    service.tick(10, &mut executor);
    let executed = service
        .tick(11, &mut executor)
        .ok_or("dry-run command did not complete")?;

    let second = StimCommand {
        id: 2,
        arm_session_id: session.id,
        preset_id: "software-test-single".to_owned(),
        issued_at_us: 120_000,
        not_before_us: 120_000,
        deadline_us: 220_000,
    };
    service.submit(second, 120_000);
    let cancelled = service.cancel(2, 120_001, &mut executor);

    let third = StimCommand {
        id: 3,
        arm_session_id: session.id,
        preset_id: "software-test-single".to_owned(),
        issued_at_us: 250_000,
        not_before_us: 250_000,
        deadline_us: 350_000,
    };
    service.submit(third, 250_000);
    service.tick(250_000, &mut executor);
    let emergency_results = service.emergency_stop(250_001, &mut executor);
    let rejected_after_stop = service.submit(
        StimCommand {
            id: 4,
            arm_session_id: session.id,
            preset_id: "software-test-single".to_owned(),
            issued_at_us: 250_002,
            not_before_us: 250_002,
            deadline_us: 350_002,
        },
        250_002,
    );

    if queued.outcome != StimOutcome::Queued
        || executed.outcome != StimOutcome::Executed
        || cancelled.outcome != StimOutcome::Cancelled
        || emergency_results.len() != 1
        || emergency_results[0].outcome != StimOutcome::EmergencyStopped
        || rejected_after_stop.outcome != StimOutcome::Rejected(RejectionReason::NotArmed)
        || service.state() != StimServiceState::Disarmed
        || executor.starts() != 2
        || executor.cancels() != 1
    {
        return Err("stimulation safety service dry-run produced an unexpected state".into());
    }

    let events = service.drain_events();
    let results = service.drain_results();
    println!("mode: dry-run (no hardware output)");
    println!("arm_session: {}", session.id);
    println!("phase_charge_limit_check_pc: {phase_charge}");
    println!("executor_starts: {}", executor.starts());
    println!("executor_cancels: {}", executor.cancels());
    println!("audit_events: {}", events.len());
    println!("audit_results: {}", results.len());
    println!("final_state: {:?}", service.state());
    println!("PASS: Arm, execute, cancel, Emergency Stop, and post-stop rejection succeeded");
    Ok(())
}

fn parse_stim_session_target(
    args: &[String],
) -> Result<Option<(u8, u8)>, Box<dyn std::error::Error>> {
    match args {
        [] => Ok(None),
        [port_flag, port, stream_flag, stream]
            if port_flag == "--port" && stream_flag == "--stream" =>
        {
            let port = match port.as_str() {
                "A" => 0,
                "B" => 1,
                "C" => 2,
                "D" => 3,
                _ => return Err("--port must be A, B, C or D".into()),
            };
            let stream: u8 = stream.parse()?;
            if stream > 7 {
                return Err("--stream must be 0..7".into());
            }
            Ok(Some((port, stream)))
        }
        _ => Err("stim-session-check accepts optional --port A|B|C|D --stream 0..7".into()),
    }
}

fn run_stim_session_check(
    device: RhsDevice,
    target: Option<(u8, u8)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, ready) = match target {
        Some((port, stream)) => device.open_stim_session_target(port, stream)?,
        None => device.open_stim_session()?,
    };
    if session.is_active() {
        return Err("new stimulation session unexpectedly reported an active command".into());
    }
    let cancelled = session.cancel()?;
    println!("mode: hardware session safety check (no stimulation start)");
    println!("ready: {}", ready.message);
    println!("cancel: {}", cancelled.message);
    println!("pipe_read_error: {}", cancelled.pipe_read_error);
    println!("active: {}", session.is_active());
    if cancelled.return_code != 1 || cancelled.pipe_read_error != 0 || session.is_active() {
        return Err("idle stimulation session did not reach a clean output-disabled state".into());
    }
    println!("PASS: session open, idle cancel, and close completed without starting stimulation");
    Ok(())
}

fn parse_sync_channel_mask(args: Vec<String>) -> Result<u16, Box<dyn std::error::Error>> {
    match args.as_slice() {
        [] => Ok(1),
        [option, value] if option == "--channel-mask" => {
            let parsed = if let Some(hex) = value.strip_prefix("0x") {
                u16::from_str_radix(hex, 16)?
            } else {
                value.parse::<u16>()?
            };
            if parsed == 0 {
                return Err("channel mask must select at least one channel".into());
            }
            Ok(parsed)
        }
        _ => Err("sync-session-check accepts only --channel-mask MASK after device options".into()),
    }
}

fn run_sync_session_check(
    device: RhsDevice,
    channel_mask: u16,
    channel_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = StimConfig {
        channel: channel_mask.trailing_zeros() as i32,
        channel_mask,
        ..StimConfig::default()
    };
    if channel_count == 64 {
        for stream in [2, 3, 4, 5] {
            config.stream_masks[stream] = channel_mask;
        }
    }
    let handle = device.spawn_acquisition(AcquisitionWorkerConfig {
        channel_count,
        acquisition: AcquisitionConfig { blocks_per_read: 8 },
        data_queue_capacity: 8,
        stimulation: Some(config),
        ..AcquisitionWorkerConfig::default()
    })?;
    println!("mode: synchronized acquisition session (trigger not scheduled)");
    println!(
        "device: serial={} port={} stream={} sample_rate={}Hz",
        handle.info().device_serial,
        char::from(b'A' + handle.info().target_port),
        handle.info().target_stream,
        handle.info().sample_rate_hz
    );
    println!("preset_preloaded: true");
    println!(
        "channels: {} streams: {:?}",
        handle.info().channel_count,
        handle.info().streams
    );
    println!("channel_mask: 0x{channel_mask:04x}");
    println!("stim_cmd_mode: disabled until a scheduled command is due");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut blocks = 0_u64;
    let mut samples = 0_u64;
    let mut stim_flagged_blocks = 0_u64;
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(block) => {
                blocks += 1;
                samples += block.samples_per_channel as u64;
                if block.flags.contains(SampleFlags::STIM_ARTIFACT) {
                    stim_flagged_blocks += 1;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(WorkerStatus::Fault(error)) = handle.try_status() {
                    return Err(error.into());
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("synchronized acquisition worker disconnected".into());
            }
        }
    }

    let mut stim_events = 0_u64;
    while handle.try_stim_event().is_ok() {
        stim_events += 1;
    }
    let next_sample_index = handle.next_sample_index();
    let stats = handle.stop()?;
    println!("sample_blocks: {blocks}");
    println!("samples_per_channel: {samples}");
    println!("next_sample_index: {next_sample_index}");
    println!("stim_flagged_blocks: {stim_flagged_blocks}");
    println!("stim_events: {stim_events}");
    println!("timestamp_gaps: {}", stats.timestamp_gaps);
    println!("queue_overflows: {}", stats.queue_overflows);
    if blocks == 0
        || stim_flagged_blocks != 0
        || stim_events != 0
        || stats.timestamp_gaps != 0
        || stats.queue_overflows != 0
    {
        return Err("synchronized no-trigger session failed its safety invariants".into());
    }
    println!("PASS: preset preload and continuous acquisition produced no stimulation markers");
    Ok(())
}

struct HardwareStimExecutor {
    session: StimHardwareSession,
}

impl HardwareStimExecutor {
    fn new(session: StimHardwareSession) -> Self {
        Self { session }
    }
}

impl StimExecutor for HardwareStimExecutor {
    fn start(&mut self, parameters: &StimParameters) -> Result<(), String> {
        let config = stim_config_from_parameters(parameters)?;
        self.session
            .start(config)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn poll(&mut self) -> Result<ExecutionState, String> {
        match self.session.poll().map_err(|error| error.to_string())? {
            StimPoll::Running(_) => Ok(ExecutionState::Running),
            StimPoll::Completed(report) => Ok(ExecutionState::Completed {
                message: report.message,
            }),
        }
    }

    fn cancel(&mut self) -> Result<(), String> {
        self.session
            .cancel()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn stim_config_from_parameters(parameters: &StimParameters) -> Result<StimConfig, String> {
    Ok(StimConfig {
        channel: i32::from(parameters.channel),
        channel_mask: 0,
        stream_masks: [0; 8],
        trigger: i32::from(parameters.trigger),
        pulses: i32::from(parameters.pulses),
        magnitude_steps: i32::from(parameters.magnitude_steps),
        stim_step_size: parameters.step_size,
        phase_steps: i32::try_from(parameters.phase_steps)
            .map_err(|_| "phase_steps does not fit the hardware ABI")?,
        interphase_steps: i32::try_from(parameters.interphase_steps)
            .map_err(|_| "interphase_steps does not fit the hardware ABI")?,
        period_steps: i32::try_from(parameters.period_steps)
            .map_err(|_| "period_steps does not fit the hardware ABI")?,
    })
}

#[derive(Debug)]
struct StimOptions {
    preset_id: String,
    operator: String,
    channel: u8,
    expected_port: char,
    expected_stream: u8,
    confirm_dummy_load: bool,
    confirm_measurement_ready: bool,
    emergency_stop_after_ms: Option<u64>,
}

fn parse_stim(args: Vec<String>) -> Result<StimOptions, Box<dyn std::error::Error>> {
    let mut preset_id = "bench-single-v1".to_owned();
    let mut operator = None;
    let mut channel = None;
    let mut expected_port = None;
    let mut expected_stream = None;
    let mut confirm_dummy_load = false;
    let mut confirm_measurement_ready = false;
    let mut emergency_stop_after_ms = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--confirm-dummy-load" => confirm_dummy_load = true,
            "--confirm-measurement-ready" => confirm_measurement_ready = true,
            "--preset" => {
                i += 1;
                preset_id = args.get(i).ok_or("--preset requires a value")?.clone();
            }
            "--operator" => {
                i += 1;
                operator = Some(args.get(i).ok_or("--operator requires a value")?.clone());
            }
            "--channel" => {
                i += 1;
                let number: u8 = args.get(i).ok_or("--channel requires a value")?.parse()?;
                if !(1..=16).contains(&number) {
                    return Err("--channel must be between 1 and 16".into());
                }
                channel = Some(number - 1);
            }
            "--port" => {
                i += 1;
                let port = args.get(i).ok_or("--port requires A, B, C or D")?;
                if !matches!(port.as_str(), "A" | "B" | "C" | "D") {
                    return Err("--port must be A, B, C or D".into());
                }
                expected_port = port.chars().next();
            }
            "--stream" => {
                i += 1;
                let stream: u8 = args.get(i).ok_or("--stream requires 0..7")?.parse()?;
                if stream > 7 {
                    return Err("--stream must be 0..7".into());
                }
                expected_stream = Some(stream);
            }
            "--emergency-stop-after-ms" => {
                i += 1;
                emergency_stop_after_ms = Some(
                    args.get(i)
                        .ok_or("--emergency-stop-after-ms requires a value")?
                        .parse()?,
                );
            }
            other => return Err(format!("unknown stim option: {other}").into()),
        }
        i += 1;
    }

    let operator = operator.ok_or("stim requires --operator NAME for the audit trail")?;
    if operator.trim().is_empty() {
        return Err("--operator cannot be empty".into());
    }
    if !confirm_dummy_load || !confirm_measurement_ready {
        return Err(
            "stim requires both --confirm-dummy-load and --confirm-measurement-ready".into(),
        );
    }
    if preset_id != "bench-single-v1" && preset_id != "bench-train-v1" {
        return Err("--preset must be bench-single-v1 or bench-train-v1".into());
    }
    if let Some(delay_ms) = emergency_stop_after_ms {
        if preset_id != "bench-train-v1" {
            return Err("--emergency-stop-after-ms is only valid with bench-train-v1".into());
        }
        if !(20..=450).contains(&delay_ms) {
            return Err("--emergency-stop-after-ms must be 20..450".into());
        }
    }

    Ok(StimOptions {
        preset_id,
        operator,
        channel: channel.ok_or("stim requires an explicit --channel 1..16")?,
        expected_port: expected_port.ok_or("stim requires an explicit --port A|B|C|D")?,
        expected_stream: expected_stream.ok_or("stim requires an explicit --stream 0..7")?,
        confirm_dummy_load,
        confirm_measurement_ready,
        emergency_stop_after_ms,
    })
}

fn bench_stim_presets(channel: u8) -> Vec<StimPreset> {
    let base = StimParameters {
        channel,
        trigger: 0,
        pulses: 1,
        magnitude_steps: 1,
        step_size: StimStepSize::N10,
        phase_steps: 6,
        interphase_steps: 3,
        period_steps: 3_000,
    };
    let mut train = base.clone();
    train.pulses = 5;
    vec![
        StimPreset {
            id: "bench-single-v1".to_owned(),
            label: "Bench-only 10 nA-step single biphasic pulse".to_owned(),
            parameters: base,
        },
        StimPreset {
            id: "bench-train-v1".to_owned(),
            label: "Bench-only 10 nA-step five-pulse biphasic train".to_owned(),
            parameters: train,
        },
    ]
}

fn bench_stim_policy(channel: u8) -> StimSafetyPolicy {
    StimSafetyPolicy {
        hardware_timestep_hz: 30_000,
        allowed_channels: vec![channel],
        allowed_step_sizes: vec![StimStepSize::N10],
        max_magnitude_steps: 1,
        max_phase_steps: 6,
        max_interphase_steps: 3,
        min_period_steps: 3_000,
        max_period_steps: 3_000,
        max_pulses_per_command: 5,
        max_pulses_per_arm_session: 5,
        max_phase_charge_picocoulombs: 2,
        max_active_duty_ppm: 4_000,
        max_train_duration_us: 500_000,
        min_command_interval_us: 1_000_000,
        max_arm_duration_us: 5_000_000,
        max_command_lifetime_us: 100_000,
    }
}

fn validate_bench_scan(scan: &ScanReport, options: &StimOptions) -> Result<(), String> {
    let unique_streams = scan
        .chips
        .iter()
        .map(|chip| chip.command_stream)
        .collect::<BTreeSet<_>>();
    let valid_multi_chip = scan.chips.len() == 4
        && scan.channels_on_port == [0, 32, 32, 0]
        && ['B', 'C'].iter().all(|port| {
            scan.chips
                .iter()
                .filter(|chip| chip.port_name == *port)
                .count()
                == 2
        });
    let target_present = scan.chips.iter().any(|chip| {
        chip.port_name == options.expected_port
            && chip.command_stream == i32::from(options.expected_stream)
    });
    if scan.return_code < 0
        || scan.pipe_read_error != 0
        || scan.chips.iter().any(|chip| chip.chip_name != "RHS2116")
        || unique_streams.len() != scan.chips.len()
        || !target_present
        || !(scan.chips.len() == 1 || valid_multi_chip)
    {
        return Err(format!(
            "real output requires a matching RHS2116 on port {} stream {} and either one chip or four B/C chips; scan: {}",
            options.expected_port, options.expected_stream, scan.message
        ));
    }
    Ok(())
}

fn run_hardware_stim(
    device: RhsDevice,
    options: StimOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    if !options.confirm_dummy_load || !options.confirm_measurement_ready {
        return Err("physical bench confirmations were not supplied".into());
    }
    if device.requested_serial().is_none() {
        return Err("real output requires --serial to select the exact FrontPanel device".into());
    }
    let scan = device.scan()?;
    validate_bench_scan(&scan, &options)?;

    let presets = bench_stim_presets(options.channel);
    let selected = presets
        .iter()
        .find(|preset| preset.id == options.preset_id)
        .ok_or("selected bench preset is unavailable")?
        .clone();
    let phase_charge = selected.parameters.phase_charge_picocoulombs_ceil(30_000);
    let mut service = StimService::new(bench_stim_policy(options.channel), presets)?;
    let (session, ready) = device.open_stim_session_target(
        (options.expected_port as u8) - b'A',
        options.expected_stream,
    )?;
    if ready.pipe_read_error != 0 {
        return Err(format!(
            "stimulation session reports USB pipe error {}",
            ready.pipe_read_error
        )
        .into());
    }
    let mut executor = HardwareStimExecutor::new(session);
    let clock = Instant::now();
    let arm = service.arm(
        ArmRequest {
            operator: options.operator,
            reason: "dummy-load oscilloscope validation".to_owned(),
            approved_preset_ids: vec![selected.id.clone()],
        },
        0,
    )?;
    let queued = service.submit(
        StimCommand {
            id: 1,
            arm_session_id: arm.id,
            preset_id: selected.id.clone(),
            issued_at_us: 0,
            not_before_us: 0,
            deadline_us: 100_000,
        },
        0,
    );
    if queued.outcome != StimOutcome::Queued {
        return Err(format!("bench command was rejected: {}", queued.message).into());
    }

    println!("mode: REAL OUTPUT, dummy-load measurement only");
    println!("session: {}", ready.message);
    println!("preset: {} ({})", selected.id, selected.label);
    println!(
        "parameters: channel={} pulses={} step={} magnitude_steps={} phase_steps={} interphase_steps={} period_steps={}",
        selected.parameters.channel,
        selected.parameters.pulses,
        selected.parameters.step_size.label(),
        selected.parameters.magnitude_steps,
        selected.parameters.phase_steps,
        selected.parameters.interphase_steps,
        selected.parameters.period_steps
    );
    println!("phase_charge_limit_check_pc: {phase_charge}");

    loop {
        let now_us = u64::try_from(clock.elapsed().as_micros()).unwrap_or(u64::MAX);
        if options
            .emergency_stop_after_ms
            .is_some_and(|delay_ms| clock.elapsed() >= Duration::from_millis(delay_ms))
            && service.active_command_id().is_some()
        {
            let stopped = service.emergency_stop(now_us, &mut executor);
            if stopped.len() != 1
                || stopped[0].outcome != StimOutcome::EmergencyStopped
                || service.state() != StimServiceState::Disarmed
            {
                return Err("Emergency Stop did not produce the expected audit result".into());
            }
            println!(
                "emergency_stop_requested_ms: {}",
                options
                    .emergency_stop_after_ms
                    .expect("checked immediately above")
            );
            println!("result: {}", stopped[0].message);
            println!("audit_events: {}", service.drain_events().len());
            println!("audit_results: {}", service.drain_results().len());
            println!("PASS: active bench train was cancelled and the service disarmed");
            return Ok(());
        }
        if let Some(result) = service.tick(now_us, &mut executor) {
            match result.outcome {
                StimOutcome::Executed => {
                    service.disarm(now_us, &mut executor);
                    println!("result: {}", result.message);
                    println!("audit_events: {}", service.drain_events().len());
                    println!("audit_results: {}", service.drain_results().len());
                    println!("PASS: finite bench preset completed through the safety service");
                    return Ok(());
                }
                _ => return Err(format!("stimulation did not complete: {}", result.message).into()),
            }
        }
        if clock.elapsed() > Duration::from_secs(3) {
            service.emergency_stop(now_us, &mut executor);
            return Err("stimulation timed out; Emergency Stop was issued".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn print_report(report: &rhs_device::ScanReport) {
    println!("return_code: {}", report.return_code);
    println!("pipe_read_error: {}", report.pipe_read_error);
    println!("message: {}", report.message);
    println!("chips: {}", report.chips.len());
    for chip in &report.chips {
        println!(
            "- {} type={} port={} command_stream={}",
            chip.chip_name, chip.chip_type, chip.port_name, chip.command_stream
        );
    }
    println!(
        "channels_on_port: A={} B={} C={} D={}",
        report.channels_on_port[0],
        report.channels_on_port[1],
        report.channels_on_port[2],
        report.channels_on_port[3]
    );
}

fn print_usage() {
    println!("rhs-cli stim64-functional-check --serial SERIAL --electrode B-000 --operator NAME --audit PATH --confirm-no-bio --confirm-real-output");
    println!("rhs-cli scan [--bitfile PATH] [--serial SERIAL]");
    println!("rhs-cli acquire [--channels 16|64] [--seconds 5] [--blocks-per-read 8] [--queue-capacity 8]");
    println!(
        "rhs-cli record --output PATH [--seconds 5] [--blocks-per-read 8] [--queue-capacity 8]"
    );
    println!("rhs-cli replay-check --input PATH");
    println!("rhs-cli viewer-check [--seconds 5] [--blocks-per-read 8] [--queue-capacity 8]");
    println!("rhs-cli synthetic-check [--blocks 100]");
    println!("rhs-cli stim-service-check");
    println!("rhs-cli stim-session-check [--bitfile PATH] [--serial SERIAL] [--port A|B|C|D --stream 0..7]");
    println!("rhs-cli sync-session-check [--bitfile PATH] [--serial SERIAL] [--channel-mask MASK]");
    println!(
        "rhs-cli sync64-session-check [--bitfile PATH] [--serial SERIAL] [--channel-mask MASK]"
    );
    println!(
        "rhs-cli stim --serial SERIAL --port A|B|C|D --stream 0..7 --channel 1..16 --preset bench-single-v1|bench-train-v1 --operator NAME \\"
    );
    println!("  --confirm-dummy-load --confirm-measurement-ready [--emergency-stop-after-ms 150]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_diagnostics_accept_only_supported_channel_counts() {
        assert_eq!(parse_acquire(vec![]).unwrap().channel_count, 16);
        assert_eq!(
            parse_acquire(vec!["--channels".into(), "64".into()])
                .unwrap()
                .channel_count,
            64
        );
        assert!(parse_acquire(vec!["--channels".into(), "120".into()]).is_err());
        assert!(parse_acquire(vec!["--channels".into()]).is_err());
    }

    #[test]
    fn session_check_target_requires_explicit_port_and_stream() {
        assert_eq!(parse_stim_session_target(&[]).unwrap(), None);
        assert_eq!(
            parse_stim_session_target(&[
                "--port".into(),
                "C".into(),
                "--stream".into(),
                "5".into()
            ])
            .unwrap(),
            Some((2, 5))
        );
        assert!(parse_stim_session_target(&["--port".into(), "B".into()]).is_err());
        assert!(parse_stim_session_target(&[
            "--port".into(),
            "C".into(),
            "--stream".into(),
            "8".into()
        ])
        .is_err());
    }

    #[test]
    fn sync_check_accepts_sixteen_channel_mask_without_triggering() {
        assert_eq!(parse_sync_channel_mask(vec![]).unwrap(), 1);
        assert_eq!(
            parse_sync_channel_mask(vec!["--channel-mask".into(), "0xffff".into()]).unwrap(),
            u16::MAX
        );
        assert!(parse_sync_channel_mask(vec!["--channel-mask".into(), "0".into()]).is_err());
        assert!(parse_sync_channel_mask(vec!["--channel-mask".into(), "0x10000".into()]).is_err());
    }

    #[test]
    fn signal_stats_detect_rail_saturation_and_common_mode() {
        let mut stats = ChannelSignalStats::default();
        for sample in [0, 16_384, 32_768, 49_152, 65_535] {
            stats.observe(sample, f64::from(sample));
        }

        assert!(stats.touches_a_rail());
        assert!(stats.spans_almost_full_scale());
        assert_eq!(stats.exact_rail_samples, 2);
        assert!((stats.common_mode_correlation() - 1.0).abs() < 1e-12);
        assert_eq!(stats.upward_center_crossings, 1);
    }

    #[test]
    fn stim_requires_both_physical_confirmations() {
        let error = parse_stim(vec![
            "--operator".to_owned(),
            "tester".to_owned(),
            "--confirm-dummy-load".to_owned(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("both"));
    }

    #[test]
    fn stim_requires_explicit_port_stream_and_channel() {
        let base = vec![
            "--operator".to_owned(),
            "tester".to_owned(),
            "--confirm-dummy-load".to_owned(),
            "--confirm-measurement-ready".to_owned(),
        ];
        assert!(parse_stim(base.clone())
            .unwrap_err()
            .to_string()
            .contains("--channel"));
        let mut with_channel = base;
        with_channel.extend(["--channel".to_owned(), "1".to_owned()]);
        assert!(parse_stim(with_channel.clone())
            .unwrap_err()
            .to_string()
            .contains("--port"));
        with_channel.extend(["--port".to_owned(), "B".to_owned()]);
        assert!(parse_stim(with_channel.clone())
            .unwrap_err()
            .to_string()
            .contains("--stream"));
        with_channel.extend(["--stream".to_owned(), "2".to_owned()]);
        let options = parse_stim(with_channel).unwrap();
        assert_eq!(options.expected_port, 'B');
        assert_eq!(options.expected_stream, 2);
    }

    #[test]
    fn stim_only_accepts_fixed_bench_presets() {
        let error = parse_stim(vec![
            "--operator".to_owned(),
            "tester".to_owned(),
            "--preset".to_owned(),
            "custom".to_owned(),
            "--confirm-dummy-load".to_owned(),
            "--confirm-measurement-ready".to_owned(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("bench-single-v1"));
    }

    #[test]
    fn bench_train_maps_to_bounded_hardware_config() {
        let train = bench_stim_presets(0)
            .into_iter()
            .find(|preset| preset.id == "bench-train-v1")
            .unwrap();
        let config = stim_config_from_parameters(&train.parameters).unwrap();
        assert_eq!(config.channel, 0);
        assert_eq!(config.pulses, 5);
        assert_eq!(config.magnitude_steps, 1);
        assert_eq!(config.stim_step_size, StimStepSize::N10);
    }

    #[test]
    fn emergency_stop_delay_is_bounded_and_train_only() {
        let error = parse_stim(vec![
            "--operator".to_owned(),
            "tester".to_owned(),
            "--emergency-stop-after-ms".to_owned(),
            "150".to_owned(),
            "--confirm-dummy-load".to_owned(),
            "--confirm-measurement-ready".to_owned(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("bench-train-v1"));

        let options = parse_stim(vec![
            "--operator".to_owned(),
            "tester".to_owned(),
            "--port".to_owned(),
            "A".to_owned(),
            "--stream".to_owned(),
            "0".to_owned(),
            "--channel".to_owned(),
            "1".to_owned(),
            "--preset".to_owned(),
            "bench-train-v1".to_owned(),
            "--emergency-stop-after-ms".to_owned(),
            "150".to_owned(),
            "--confirm-dummy-load".to_owned(),
            "--confirm-measurement-ready".to_owned(),
        ])
        .unwrap();
        assert_eq!(options.emergency_stop_after_ms, Some(150));
    }

    #[test]
    fn all_sixteen_bench_channels_map_to_unique_rhs2116_outputs() {
        for number in 1..=16_u8 {
            let options = parse_stim(vec![
                "--operator".to_owned(),
                "tester".to_owned(),
                "--port".to_owned(),
                "A".to_owned(),
                "--stream".to_owned(),
                "0".to_owned(),
                "--channel".to_owned(),
                number.to_string(),
                "--confirm-dummy-load".to_owned(),
                "--confirm-measurement-ready".to_owned(),
            ])
            .unwrap();
            assert_eq!(options.channel, number - 1);
            assert_eq!(options.expected_port, 'A');
            assert_eq!(options.expected_stream, 0);
            let preset = bench_stim_presets(options.channel).remove(0);
            assert_eq!(preset.parameters.channel, number - 1);
            assert_eq!(
                stim_config_from_parameters(&preset.parameters)
                    .unwrap()
                    .channel,
                i32::from(number - 1)
            );
            assert_eq!(
                bench_stim_policy(options.channel).allowed_channels,
                vec![number - 1]
            );
        }
        for invalid in ["0", "17"] {
            let error = parse_stim(vec![
                "--operator".to_owned(),
                "tester".to_owned(),
                "--channel".to_owned(),
                invalid.to_owned(),
                "--confirm-dummy-load".to_owned(),
                "--confirm-measurement-ready".to_owned(),
            ])
            .unwrap_err();
            assert!(error.to_string().contains("between 1 and 16"));
        }
    }

    #[test]
    fn four_chip_bench_scan_requires_exact_target_and_distinct_streams() {
        let options = parse_stim(vec![
            "--operator".into(),
            "tester".into(),
            "--port".into(),
            "C".into(),
            "--stream".into(),
            "5".into(),
            "--channel".into(),
            "16".into(),
            "--confirm-dummy-load".into(),
            "--confirm-measurement-ready".into(),
        ])
        .unwrap();
        let chip = |port_name, command_stream| rhs_device::ChipInfo {
            chip_type: 0,
            chip_name: "RHS2116",
            port_index: if port_name == 'B' { 1 } else { 2 },
            port_name,
            command_stream,
        };
        let mut scan = ScanReport {
            return_code: 1,
            pipe_read_error: 0,
            message: "four RHS2116 chips".into(),
            chips: vec![chip('B', 2), chip('B', 3), chip('C', 4), chip('C', 5)],
            channels_on_port: [0, 32, 32, 0],
        };
        assert!(validate_bench_scan(&scan, &options).is_ok());
        scan.chips[3].command_stream = 4;
        assert!(validate_bench_scan(&scan, &options).is_err());
        scan.chips[3].command_stream = 5;
        scan.chips[3].port_name = 'B';
        assert!(validate_bench_scan(&scan, &options).is_err());
        scan.chips[3].port_name = 'C';
        scan.chips[3].chip_name = "RHD2132";
        assert!(validate_bench_scan(&scan, &options).is_err());
        scan.chips[3].chip_name = "RHS2116";
        scan.channels_on_port[2] = 16;
        assert!(validate_bench_scan(&scan, &options).is_err());
        scan.channels_on_port[2] = 32;
        scan.pipe_read_error = 1;
        assert!(validate_bench_scan(&scan, &options).is_err());
    }
}
