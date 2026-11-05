use rhs_core::{SampleBlock, SampleFlags};
use rhs_device::{
    AcquisitionHandle, AcquisitionStimEvent, AcquisitionStimState, AcquisitionWorkerConfig,
    RhsDevice, ScheduledStimRequest, StimConfig,
};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Options {
    electrode: String,
    stream: usize,
    chip_channel: u8,
    operator: String,
    audit: PathBuf,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut electrode = None;
        let mut operator = None;
        let mut audit = None;
        let mut no_bio = false;
        let mut real_output = false;
        let mut args = args.iter();
        while let Some(option) = args.next() {
            match option.as_str() {
                "--electrode" if electrode.is_none() => {
                    electrode = Some(args.next().ok_or("missing electrode")?.clone())
                }
                "--operator" if operator.is_none() => {
                    operator = Some(args.next().ok_or("missing operator")?.trim().to_owned())
                }
                "--audit" if audit.is_none() => {
                    audit = Some(PathBuf::from(args.next().ok_or("missing audit path")?))
                }
                "--confirm-no-bio" if !no_bio => no_bio = true,
                "--confirm-real-output" if !real_output => real_output = true,
                _ => {
                    return Err(
                        format!("unknown or duplicate functional-check option: {option}").into(),
                    )
                }
            }
        }
        if !no_bio || !real_output {
            return Err("functional check requires --confirm-no-bio and --confirm-real-output; it is not a software simulation".into());
        }
        let electrode = electrode.ok_or("an explicit B/C electrode is required")?;
        let (stream, chip_channel) = physical_address(&electrode)?;
        let operator = operator
            .filter(|name| {
                !name.is_empty() && name.len() <= 80 && !name.chars().any(char::is_control)
            })
            .ok_or("operator must be non-empty, at most 80 bytes, without control characters")?;
        Ok(Self {
            electrode,
            stream,
            chip_channel,
            operator,
            audit: audit.ok_or("an explicit audit path is required")?,
        })
    }

    fn preset(&self) -> StimConfig {
        let mask = 1_u16 << self.chip_channel;
        let mut config = StimConfig {
            channel: i32::from(self.chip_channel),
            channel_mask: mask,
            trigger: 0,
            pulses: 1,
            magnitude_steps: 1,
            stim_step_size: rhs_device::StimStepSize::N10,
            phase_steps: 6,
            interphase_steps: 3,
            period_steps: 3_000,
            stream_masks: [0; 8],
        };
        config.stream_masks[self.stream] = mask;
        config
    }
}

fn physical_address(electrode: &str) -> Result<(usize, u8)> {
    let bytes = electrode.as_bytes();
    if bytes.len() != 5
        || !matches!(bytes[0], b'B' | b'C')
        || bytes[1] != b'-'
        || !bytes[2..].iter().all(u8::is_ascii_digit)
    {
        return Err("electrode must be B-000..B-031 or C-000..C-031".into());
    }
    let port_channel: u8 = electrode[2..].parse()?;
    if port_channel > 31 {
        return Err("port-local channel must be 000..031".into());
    }
    let base_stream = if bytes[0] == b'B' { 2 } else { 4 };
    Ok((
        base_stream + usize::from(port_channel / 16),
        port_channel % 16,
    ))
}

struct Audit {
    file: File,
    started: Instant,
}

impl Audit {
    fn record(&mut self, record: Value) -> Result<()> {
        let line = json!({
            "unixMs": SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            "elapsedMs": self.started.elapsed().as_millis(),
            "record": record
        });
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        Ok(())
    }
}

#[derive(Default)]
struct Feedback {
    stage: usize,
    target: u64,
    completion: Option<u64>,
    flagged_blocks: u64,
    blocks: u64,
}

impl Feedback {
    fn event(&mut self, event: &AcquisitionStimEvent) -> Result<()> {
        const EXPECTED: [AcquisitionStimState; 4] = [
            AcquisitionStimState::Queued,
            AcquisitionStimState::Triggered,
            AcquisitionStimState::Observed,
            AcquisitionStimState::Completed,
        ];
        if event.command_id != 1
            || event.target_sample_index != self.target
            || EXPECTED.get(self.stage) != Some(&event.state)
        {
            return Err(format!(
                "unexpected hardware feedback {:?}: {}",
                event.state, event.message
            )
            .into());
        }
        if matches!(
            event.state,
            AcquisitionStimState::Observed | AcquisitionStimState::Completed
        ) && event.sample_index.is_none()
        {
            return Err("FPGA feedback is missing its sample index".into());
        }
        self.stage += 1;
        if event.state == AcquisitionStimState::Completed {
            self.completion = event.sample_index;
        }
        Ok(())
    }

    fn block(&mut self, block: &SampleBlock) -> Result<()> {
        if block.flags.contains(SampleFlags::DISCONTINUITY) {
            return Err("sample discontinuity during functional check".into());
        }
        let expected: Vec<_> = (2..=5)
            .flat_map(|stream| (0..16).map(move |channel| rhs_core::ChannelId { stream, channel }))
            .collect();
        if block.channels != expected {
            return Err("acquired physical channel IDs differ from B2/B3/C4/C5 layout".into());
        }
        self.blocks += 1;
        self.flagged_blocks += u64::from(block.flags.contains(SampleFlags::STIM_ARTIFACT));
        Ok(())
    }
}

pub fn run(device: RhsDevice, args: Vec<String>) -> Result<()> {
    let options = Options::parse(&args)?;
    let requested_serial = device
        .requested_serial()
        .filter(|serial| !serial.trim().is_empty())
        .ok_or("functional check requires an explicit device serial")?
        .to_owned();
    let config = options.preset();
    config.validate()?;
    let mut audit = Audit {
        file: OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&options.audit)?,
        started: Instant::now(),
    };
    audit.record(json!({
        "event": "preflight", "mode": "no-bio-hardware-functional-check", "operator": options.operator,
        "requestedSerial": requested_serial, "electrode": options.electrode, "stream": options.stream,
        "chipChannel": options.chip_channel, "streamMasks": config.stream_masks,
        "noBiologicalSampleConfirmed": true, "realHardwareOutputAcknowledged": true,
        "nominalCurrentNa": 10, "phaseWidthUs": 200, "interphaseUs": 100, "pulseCount": 1,
        "currentMeasured": false
    }))?;
    println!("REAL hardware functional check: serial={requested_serial}, electrode={}, stream={}, chip_channel={}", options.electrode, options.stream, options.chip_channel);
    println!("fixed preset: 10 nA, 200 us/phase, 100 us gap, one negative-first biphasic pulse; no retries");
    println!("audit: {}", options.audit.display());

    let handle = device.spawn_acquisition(AcquisitionWorkerConfig {
        channel_count: 64,
        stimulation: Some(config),
        ..AcquisitionWorkerConfig::default()
    })?;
    // Every exit after opening the controller, including audit/feedback failures, stops output.
    let outcome = capture(&handle, &requested_serial, &mut audit);
    let _ = handle.emergency_stop();
    let stopped = handle.stop();
    let passed = outcome.is_ok()
        && stopped
            .as_ref()
            .is_ok_and(|stats| stats.timestamp_gaps == 0 && stats.queue_overflows == 0);
    audit.record(json!({
        "event": "stopped", "pass": passed,
        "functionalError": outcome.as_ref().err().map(ToString::to_string),
        "stopError": stopped.as_ref().err().map(ToString::to_string),
        "timestampGaps": stopped.as_ref().ok().map(|stats| stats.timestamp_gaps),
        "queueOverflows": stopped.as_ref().ok().map(|stats| stats.queue_overflows),
        "currentMeasured": false
    }))?;
    let feedback = outcome?;
    let stats = stopped?;
    if !passed {
        return Err("functional check failed acquisition safety counters".into());
    }
    println!("FPGA feedback sequence: Queued -> Triggered -> Observed -> Completed");
    println!(
        "blocks={} stim_flagged_blocks={} timestamp_gaps={} queue_overflows={}",
        feedback.blocks, feedback.flagged_blocks, stats.timestamp_gaps, stats.queue_overflows
    );
    println!("PASS: one selected-channel trigger and FPGA stim_on feedback completed; output stopped; actual current was not measured");
    Ok(())
}

fn capture(handle: &AcquisitionHandle, serial: &str, audit: &mut Audit) -> Result<Feedback> {
    let info = handle.info();
    if info.device_serial != serial
        || info.channel_count != 64
        || info.sample_rate_hz != 30_000
        || info.streams != [2, 3, 4, 5]
    {
        return Err(
            "opened device/layout does not match the requested 64-channel B/C session".into(),
        );
    }
    audit.record(
        json!({"event": "session-ready", "serial": info.device_serial, "streams": info.streams}),
    )?;
    let mut feedback = Feedback::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    while feedback.blocks < 2 {
        if Instant::now() >= deadline {
            return Err("preflight acquisition timed out; no trigger sent".into());
        }
        match handle.recv_timeout(Duration::from_millis(20)) {
            Ok(block) => {
                feedback.block(&block)?;
                if feedback.flagged_blocks != 0 {
                    return Err(
                        "unexpected stimulation marker before trigger; test cancelled".into(),
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("worker disconnected during preflight; no trigger sent".into())
            }
        }
    }
    let stats = handle.stats();
    if stats.timestamp_gaps != 0 || stats.queue_overflows != 0 {
        return Err("preflight acquisition counters are not clean; no trigger sent".into());
    }
    feedback.target = handle
        .next_sample_index()
        .saturating_add(u64::from(info.sample_rate_hz) / 10);
    audit.record(json!({"event": "single-trigger-request", "commandId": 1, "targetSampleIndex": feedback.target}))?;
    handle.schedule_stim(ScheduledStimRequest {
        command_id: 1,
        target_sample_index: feedback.target,
        deadline_sample_index: feedback
            .target
            .saturating_add(u64::from(info.sample_rate_hz)),
    })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() >= deadline {
            return Err("hardware feedback timed out; output will be stopped without retry".into());
        }
        drain_feedback(handle, &mut feedback, audit)?;
        match handle.recv_timeout(Duration::from_millis(20)) {
            Ok(block) => {
                feedback.block(&block)?;
                if feedback.completion.is_some_and(|sample| {
                    block.last_sample_index()
                        >= sample + 2 * info.samples_per_block as u64 * info.blocks_per_read as u64
                }) {
                    if feedback.flagged_blocks == 0 {
                        return Err("completion was reported without any captured stimulation artifact block".into());
                    }
                    return Ok(feedback);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                drain_feedback(handle, &mut feedback, audit)?;
                return Err("worker disconnected before functional check completed".into());
            }
        }
    }
}

fn drain_feedback(
    handle: &AcquisitionHandle,
    feedback: &mut Feedback,
    audit: &mut Audit,
) -> Result<()> {
    while let Ok(event) = handle.try_stim_event() {
        audit.record(json!({"event": "hardware-feedback", "commandId": event.command_id, "state": format!("{:?}", event.state), "targetSampleIndex": event.target_sample_index, "sampleIndex": event.sample_index, "message": event.message}))?;
        println!("{:?}: {}", event.state, event.message);
        feedback.event(&event)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments() -> Vec<String> {
        [
            "--electrode",
            "B-000",
            "--operator",
            "test",
            "--audit",
            "test.jsonl",
            "--confirm-no-bio",
            "--confirm-real-output",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn confirmations_address_and_operator_are_mandatory() {
        assert!(Options::parse(&arguments()).is_ok());
        for missing in ["--confirm-no-bio", "--confirm-real-output"] {
            assert!(Options::parse(
                &arguments()
                    .into_iter()
                    .filter(|arg| arg != missing)
                    .collect::<Vec<_>>()
            )
            .is_err());
        }
        let mut args = arguments();
        args[3] = " ".into();
        assert!(Options::parse(&args).is_err());
        args = arguments();
        args.extend(["--pulses".into(), "20".into()]);
        assert!(Options::parse(&args).is_err());
        assert!(Options::parse(&[]).is_err());
    }

    #[test]
    fn every_electrode_selects_exactly_one_physical_output_and_fixed_pulse() {
        let mut addresses = std::collections::BTreeSet::new();
        for port in ['B', 'C'] {
            for channel in 0..32 {
                let mut args = arguments();
                args[1] = format!("{port}-{channel:03}");
                let options = Options::parse(&args).unwrap();
                let config = options.preset();
                assert_eq!(
                    config
                        .stream_masks
                        .iter()
                        .map(|mask| mask.count_ones())
                        .sum::<u32>(),
                    1
                );
                assert_eq!(
                    config.stream_masks[options.stream],
                    1 << options.chip_channel
                );
                assert_eq!(config.pulses, 1);
                assert_eq!(config.magnitude_steps, 1);
                assert_eq!(config.stim_step_size.current_na(), 10);
                assert_eq!((config.phase_steps, config.interphase_steps), (6, 3));
                assert!(config.validate().is_ok());
                addresses.insert((options.stream, options.chip_channel));
            }
        }
        assert_eq!(addresses.len(), 64);
        assert_eq!(physical_address("C-031").unwrap(), (5, 15));
        for invalid in [
            "A-000",
            "B-032",
            "B-0",
            "B-000,C-000",
            "b-000",
            "D-000",
            "B-999",
        ] {
            assert!(physical_address(invalid).is_err());
        }
    }

    #[test]
    fn success_requires_trigger_observation_and_finite_completion_in_order() {
        let mut feedback = Feedback {
            target: 100,
            ..Feedback::default()
        };
        let event = |state| AcquisitionStimEvent {
            command_id: 1,
            state,
            target_sample_index: 100,
            sample_index: Some(100),
            message: "fixture".into(),
        };
        assert!(feedback
            .event(&event(AcquisitionStimState::Completed))
            .is_err());
        for state in [
            AcquisitionStimState::Queued,
            AcquisitionStimState::Triggered,
            AcquisitionStimState::Observed,
            AcquisitionStimState::Completed,
        ] {
            feedback.event(&event(state)).unwrap();
        }
        assert_eq!(feedback.completion, Some(100));
        assert!(feedback
            .event(&event(AcquisitionStimState::Triggered))
            .is_err());
        assert!(Feedback::default()
            .event(&event(AcquisitionStimState::Fault))
            .is_err());
    }
}
