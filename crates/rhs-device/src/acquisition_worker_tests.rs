use super::*;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Default)]
struct Trace {
    triggers: usize,
    cancellations: usize,
    stops: usize,
}

#[derive(Clone, Copy)]
enum Failure {
    None,
    Trigger,
    ReadAfterTrigger,
    ComplianceOnThirdTrigger,
}

struct SimulatedIo {
    trace: Arc<Mutex<Trace>>,
    next: u32,
    marker: Option<u32>,
    failure: Failure,
}

impl AcquisitionIo for SimulatedIo {
    fn read_batch(&mut self) -> Result<Option<AcquisitionBatch>, DeviceError> {
        thread::sleep(Duration::from_millis(2));
        if matches!(self.failure, Failure::ReadAfterTrigger) && self.marker.is_some() {
            return Err(DeviceError::new(
                DeviceErrorCode::UsbReadFailed,
                "injected read failure after trigger",
            ));
        }
        let timestamps: Vec<u32> = (self.next..self.next + 128).collect();
        self.next += 128;
        let stim_on = timestamps
            .iter()
            .map(|sample| {
                u64::from(
                    self.marker
                        .is_some_and(|start| *sample >= start && *sample < start + 15),
                )
            })
            .collect();
        let mut compliance_limit = vec![0; 128];
        if matches!(self.failure, Failure::ComplianceOnThirdTrigger)
            && self.marker.is_some()
            && self.trace.lock().unwrap().triggers == 3
        {
            compliance_limit[8] = 1;
        }
        Ok(Some(AcquisitionBatch {
            timestamps,
            amplifier_samples: vec![32768; 128 * 64],
            ttl_in: vec![0; 128],
            stim_on,
            stim_polarity: vec![0; 128],
            compliance_limit,
            dc_amplifier_samples: vec![0; 128 * 64],
            bytes_read: 128 * 64 * 2,
        }))
    }

    fn trigger_stim(&mut self) -> Result<(), DeviceError> {
        if matches!(self.failure, Failure::Trigger) {
            return Err(DeviceError::new(
                DeviceErrorCode::UsbReadFailed,
                "injected trigger failure",
            ));
        }
        self.trace.lock().unwrap().triggers += 1;
        self.marker = Some(self.next + 4);
        Ok(())
    }

    fn cancel_stim(&mut self) -> Result<(), DeviceError> {
        self.trace.lock().unwrap().cancellations += 1;
        self.marker = None;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), DeviceError> {
        self.trace.lock().unwrap().stops += 1;
        self.marker = None;
        Ok(())
    }
}

fn spawn_simulated(failure: Failure, pulses: i32) -> (AcquisitionHandle, Arc<Mutex<Trace>>) {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let io = SimulatedIo {
        trace: trace.clone(),
        next: 0,
        marker: None,
        failure,
    };
    let info = AcquisitionInfo {
        sample_rate_hz: 30_000,
        channel_count: 64,
        samples_per_block: 128,
        blocks_per_read: 1,
        target_stream: 2,
        target_port: 1,
        streams: vec![2, 3, 4, 5],
        bytes_per_block: 128 * 64 * 2,
        pipe_read_error: 0,
        device_serial: "simulated-not-hardware".to_owned(),
        message: String::new(),
    };
    let config = AcquisitionWorkerConfig {
        channel_count: 64,
        stimulation: Some(StimConfig {
            pulses,
            stream_masks: [0, 0, 1, 0, 0, 0, 0, 0],
            ..StimConfig::default()
        }),
        ..AcquisitionWorkerConfig::default()
    };
    let (data_tx, data_rx) = mpsc::sync_channel(8);
    let (status_tx, status_rx) = mpsc::channel();
    let (event_tx, stim_event_rx) = mpsc::sync_channel(32);
    let (command_tx, command_rx) = mpsc::sync_channel(8);
    let stats = Arc::new(AcquisitionStatsInner::default());
    let next_sample_index = Arc::new(AtomicU64::new(0));
    let emergency_latched = Arc::new(AtomicBool::new(false));
    let thread_stats = stats.clone();
    let thread_next = next_sample_index.clone();
    let thread_latched = emergency_latched.clone();
    let thread_info = info.clone();
    let configured_stimulation = config.stimulation.clone();
    let join = thread::spawn(move || {
        run_acquisition_loop(
            io,
            thread_info,
            config,
            data_tx,
            status_tx,
            event_tx,
            command_rx,
            thread_stats,
            thread_next,
            thread_latched,
        )
    });
    (
        AcquisitionHandle {
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
        },
        trace,
    )
}

fn request(command_id: u64, target: u64) -> ScheduledStimRequest {
    ScheduledStimRequest {
        command_id,
        target_sample_index: target,
        deadline_sample_index: target + 30_000,
    }
}

fn wait_for(
    handle: &AcquisitionHandle,
    command_id: u64,
    state: AcquisitionStimState,
) -> Vec<AcquisitionStimEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        while let Ok(event) = handle.try_stim_event() {
            let matched = event.command_id == command_id && event.state == state;
            events.push(event);
            if matched {
                return events;
            }
        }
        match handle.recv_timeout(Duration::from_millis(2)) {
            Ok(block) => assert!(!block.flags.contains(SampleFlags::DISCONTINUITY)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Terminal feedback may be published just before channel teardown.
                while let Ok(event) = handle.try_stim_event() {
                    let matched = event.command_id == command_id && event.state == state;
                    events.push(event);
                    if matched {
                        return events;
                    }
                }
                panic!("worker disconnected before {state:?}: {events:?}");
            }
        }
    }
    panic!("timed out waiting for {state:?}: {events:?}");
}

#[test]
fn production_loop_repeats_three_finite_commands_without_losing_acquisition() {
    let (handle, trace) = spawn_simulated(Failure::None, 1);
    for command_id in 1..=3 {
        let target = handle.next_sample_index() + 256;
        handle.schedule_stim(request(command_id, target)).unwrap();
        let events = wait_for(&handle, command_id, AcquisitionStimState::Completed);
        assert_eq!(
            events.iter().map(|event| event.state).collect::<Vec<_>>(),
            vec![
                AcquisitionStimState::Queued,
                AcquisitionStimState::Triggered,
                AcquisitionStimState::Observed,
                AcquisitionStimState::Completed,
            ]
        );
        assert!(events.iter().all(|event| event.command_id == command_id));
    }
    for _ in 0..4 {
        handle.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    let stats = handle.stop().unwrap();
    assert_eq!(stats.timestamp_gaps, 0);
    assert_eq!(stats.queue_overflows, 0);
    let trace = trace.lock().unwrap();
    assert_eq!(trace.triggers, 3);
    assert!(trace.cancellations >= 3);
    assert_eq!(trace.stops, 1);
}

#[test]
fn production_loop_emergency_cancels_queued_command_without_any_trigger() {
    let (handle, trace) = spawn_simulated(Failure::None, 1);
    handle.schedule_stim(request(1, 100_000)).unwrap();
    wait_for(&handle, 1, AcquisitionStimState::Queued);
    handle.emergency_stop().unwrap();
    wait_for(&handle, 1, AcquisitionStimState::EmergencyStopped);
    assert!(handle.stimulation_interlocked());
    assert!(handle.schedule_stim(request(2, 200_000)).is_err());
    handle.stop().unwrap();
    assert_eq!(trace.lock().unwrap().triggers, 0);
}

#[test]
fn production_loop_third_trigger_compliance_stops_output_and_faults_future_commands() {
    let (handle, trace) = spawn_simulated(Failure::ComplianceOnThirdTrigger, 1);
    for command_id in 1..=2 {
        let target = handle.next_sample_index() + 256;
        handle.schedule_stim(request(command_id, target)).unwrap();
        wait_for(&handle, command_id, AcquisitionStimState::Completed);
    }
    let target = handle.next_sample_index() + 256;
    handle
        .schedule_stim_batch(vec![request(3, target), request(4, target + 100_000)])
        .unwrap();
    let mut events = wait_for(&handle, 3, AcquisitionStimState::Fault);
    assert!(handle.stimulation_interlocked());
    assert!(handle.schedule_stim(request(5, target + 200_000)).is_err());
    events.extend(wait_for(&handle, 4, AcquisitionStimState::Fault));
    assert!(events.iter().any(|event| event.command_id == 3
        && event.message.contains("channels=B-000")
        && event.state == AcquisitionStimState::Fault));
    assert!(!events
        .iter()
        .any(|event| event.state == AcquisitionStimState::Completed));
    let error = handle.stop().unwrap_err();
    assert!(error.message.contains("compliance warning"));
    assert!(error.message.contains("channels=B-000"));
    let trace = trace.lock().unwrap();
    assert_eq!(trace.triggers, 3);
    assert!(trace.cancellations >= 3);
    assert_eq!(trace.stops, 1);
}

#[test]
fn production_loop_emergency_cancels_active_and_future_commands() {
    let (handle, trace) = spawn_simulated(Failure::None, 20);
    handle
        .schedule_stim_batch(vec![request(1, 256), request(2, 100_000)])
        .unwrap();
    wait_for(&handle, 1, AcquisitionStimState::Triggered);
    handle.emergency_stop().unwrap();
    let events = wait_for(&handle, 2, AcquisitionStimState::EmergencyStopped);
    assert!(events.iter().any(
        |event| event.command_id == 1 && event.state == AcquisitionStimState::EmergencyStopped
    ));
    assert!(!events
        .iter()
        .any(|event| event.state == AcquisitionStimState::Completed));
    assert!(handle.schedule_stim(request(3, 200_000)).is_err());
    handle.stop().unwrap();
    let trace = trace.lock().unwrap();
    assert_eq!(trace.triggers, 1);
    assert!(trace.cancellations > 0);
    assert_eq!(trace.stops, 1);
}

#[test]
fn production_loop_reports_trigger_or_read_fault_without_false_completion() {
    for (failure, detail) in [
        (Failure::Trigger, "injected trigger failure"),
        (
            Failure::ReadAfterTrigger,
            "injected read failure after trigger",
        ),
    ] {
        let (handle, trace) = spawn_simulated(failure, 1);
        handle.schedule_stim(request(1, 256)).unwrap();
        let events = wait_for(&handle, 1, AcquisitionStimState::Fault);
        assert!(events.last().unwrap().message.contains(detail));
        assert!(!events
            .iter()
            .any(|event| event.state == AcquisitionStimState::Completed));
        assert!(handle.stimulation_interlocked());
        assert!(handle.stop().unwrap_err().message.contains(detail));
        assert_eq!(trace.lock().unwrap().stops, 1);
    }
}
