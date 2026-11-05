use rhs_device::{
    AcquisitionHandle, AcquisitionStimEvent, AcquisitionStimState, ScheduledStimRequest, StimConfig,
};
use rhs_stim::{ExecutionState, StimExecutor, StimParameters};
use std::collections::VecDeque;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

pub trait AcquisitionStimControl {
    fn next_sample_index(&self) -> u64;
    fn configured_stimulation(&self) -> Option<StimConfig>;
    fn schedule_stim(&mut self, request: ScheduledStimRequest) -> Result<(), String>;
    fn cancel_stim(&mut self, command_id: u64) -> Result<(), String>;
    fn emergency_stop(&mut self) -> Result<(), String>;
    fn try_stim_event(&mut self) -> Result<AcquisitionStimEvent, TryRecvError>;
    fn recv_stim_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<AcquisitionStimEvent, RecvTimeoutError>;
}

impl AcquisitionStimControl for AcquisitionHandle {
    fn next_sample_index(&self) -> u64 {
        AcquisitionHandle::next_sample_index(self)
    }

    fn configured_stimulation(&self) -> Option<StimConfig> {
        AcquisitionHandle::configured_stimulation(self).cloned()
    }

    fn schedule_stim(&mut self, request: ScheduledStimRequest) -> Result<(), String> {
        AcquisitionHandle::schedule_stim(self, request).map_err(|error| error.to_string())
    }

    fn cancel_stim(&mut self, command_id: u64) -> Result<(), String> {
        AcquisitionHandle::cancel_stim(self, command_id).map_err(|error| error.to_string())
    }

    fn emergency_stop(&mut self) -> Result<(), String> {
        AcquisitionHandle::emergency_stop(self).map_err(|error| error.to_string())
    }

    fn try_stim_event(&mut self) -> Result<AcquisitionStimEvent, TryRecvError> {
        AcquisitionHandle::try_stim_event(self)
    }

    fn recv_stim_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<AcquisitionStimEvent, RecvTimeoutError> {
        AcquisitionHandle::recv_stim_event_timeout(self, timeout)
    }
}

#[derive(Debug, Clone)]
pub struct SynchronizedStimExecutorConfig {
    pub lead_samples: u64,
    pub deadline_slack_samples: u64,
    pub cancel_timeout: Duration,
}

impl Default for SynchronizedStimExecutorConfig {
    fn default() -> Self {
        Self {
            lead_samples: 1,
            deadline_slack_samples: 3_000,
            cancel_timeout: Duration::from_millis(500),
        }
    }
}

impl SynchronizedStimExecutorConfig {
    fn validate(&self) -> Result<(), String> {
        if self.deadline_slack_samples == 0 {
            return Err("deadline_slack_samples must be greater than zero".to_owned());
        }
        if self.cancel_timeout.is_zero() {
            return Err("cancel_timeout must be greater than zero".to_owned());
        }
        Ok(())
    }
}

struct ActiveCommand {
    hardware_command_id: u64,
    target_sample_index: u64,
    observed_sample_index: Option<u64>,
}

pub struct SynchronizedStimExecutor<C> {
    control: C,
    config: SynchronizedStimExecutorConfig,
    next_hardware_command_id: u64,
    active: Option<ActiveCommand>,
    audit_events: Vec<AcquisitionStimEvent>,
    deferred_events: VecDeque<AcquisitionStimEvent>,
}

impl<C: AcquisitionStimControl> SynchronizedStimExecutor<C> {
    pub fn new(control: C, config: SynchronizedStimExecutorConfig) -> Result<Self, String> {
        config.validate()?;
        if control.configured_stimulation().is_none() {
            return Err("acquisition control has no preconfigured stimulation preset".to_owned());
        }
        Ok(Self {
            control,
            config,
            next_hardware_command_id: 1,
            active: None,
            audit_events: Vec::new(),
            deferred_events: VecDeque::new(),
        })
    }

    pub fn control(&self) -> &C {
        &self.control
    }

    pub fn control_mut(&mut self) -> &mut C {
        &mut self.control
    }

    pub fn into_control(self) -> C {
        self.control
    }

    pub fn drain_hardware_events(&mut self) -> Vec<AcquisitionStimEvent> {
        std::mem::take(&mut self.audit_events)
    }

    fn next_event(&mut self) -> Result<Option<AcquisitionStimEvent>, String> {
        if let Some(event) = self.deferred_events.pop_front() {
            return Ok(Some(event));
        }
        match self.control.try_stim_event() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("acquisition stimulation event channel disconnected".to_owned())
            }
        }
    }

    fn process_active_event(
        &mut self,
        event: AcquisitionStimEvent,
    ) -> Result<Option<ExecutionState>, String> {
        let Some(active) = self.active.as_mut() else {
            self.audit_events.push(event);
            return Ok(None);
        };
        if event.command_id != active.hardware_command_id {
            self.audit_events.push(event);
            return Ok(None);
        }

        match event.state {
            AcquisitionStimState::Queued | AcquisitionStimState::Triggered => {
                self.audit_events.push(event);
                Ok(Some(ExecutionState::Running))
            }
            AcquisitionStimState::Observed => {
                active.observed_sample_index = event.sample_index;
                self.audit_events.push(event);
                Ok(Some(ExecutionState::Running))
            }
            AcquisitionStimState::Completed => {
                let active = self.active.take().expect("active command was checked");
                let observed = active
                    .observed_sample_index
                    .map_or("unknown".to_owned(), |sample| sample.to_string());
                let completion = event
                    .sample_index
                    .map_or("unknown".to_owned(), |sample| sample.to_string());
                self.audit_events.push(event);
                Ok(Some(ExecutionState::Completed {
                    message: format!(
                        "FPGA stimulation observed at sample {observed}, target={}, completed at sample {completion}",
                        active.target_sample_index
                    ),
                }))
            }
            AcquisitionStimState::Rejected | AcquisitionStimState::Fault => {
                self.active = None;
                let message = event.message.clone();
                self.audit_events.push(event);
                Err(message)
            }
            AcquisitionStimState::Cancelled | AcquisitionStimState::EmergencyStopped => {
                self.active = None;
                let message = event.message.clone();
                self.audit_events.push(event);
                Err(message)
            }
        }
    }
}

impl<C: AcquisitionStimControl> StimExecutor for SynchronizedStimExecutor<C> {
    fn start(&mut self, parameters: &StimParameters) -> Result<(), String> {
        if self.active.is_some() {
            return Err("synchronized executor already has an active command".to_owned());
        }
        let requested = stim_config_from_parameters(parameters)?;
        let configured = self
            .control
            .configured_stimulation()
            .ok_or_else(|| "acquisition stimulation preset disappeared".to_owned())?;
        if requested != configured {
            return Err("requested preset does not match the hardware-preloaded preset".to_owned());
        }

        let command_id = self.next_hardware_command_id;
        self.next_hardware_command_id = self.next_hardware_command_id.saturating_add(1);
        let target_sample_index = self
            .control
            .next_sample_index()
            .saturating_add(self.config.lead_samples);
        let request = ScheduledStimRequest {
            command_id,
            target_sample_index,
            deadline_sample_index: target_sample_index
                .saturating_add(self.config.deadline_slack_samples),
        };
        self.control.schedule_stim(request)?;
        self.active = Some(ActiveCommand {
            hardware_command_id: command_id,
            target_sample_index,
            observed_sample_index: None,
        });
        Ok(())
    }

    fn poll(&mut self) -> Result<ExecutionState, String> {
        if self.active.is_none() {
            return Err("synchronized executor has no active command".to_owned());
        }
        let mut state = ExecutionState::Running;
        while let Some(event) = self.next_event()? {
            if let Some(next) = self.process_active_event(event)? {
                state = next;
                if matches!(state, ExecutionState::Completed { .. }) {
                    break;
                }
            }
        }
        Ok(state)
    }

    fn cancel(&mut self) -> Result<(), String> {
        let Some(command_id) = self
            .active
            .as_ref()
            .map(|active| active.hardware_command_id)
        else {
            return self.control.emergency_stop();
        };
        self.control.cancel_stim(command_id)?;
        let deadline = Instant::now() + self.config.cancel_timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(
                    "timed out waiting for hardware cancellation acknowledgement".to_owned(),
                );
            }
            match self
                .control
                .recv_stim_event_timeout(deadline.saturating_duration_since(now))
            {
                Ok(event) if event.command_id == command_id => match event.state {
                    AcquisitionStimState::Cancelled | AcquisitionStimState::EmergencyStopped => {
                        self.audit_events.push(event);
                        self.active = None;
                        return Ok(());
                    }
                    AcquisitionStimState::Fault | AcquisitionStimState::Rejected => {
                        let message = event.message.clone();
                        self.audit_events.push(event);
                        self.active = None;
                        return Err(message);
                    }
                    _ => self.audit_events.push(event),
                },
                Ok(event) => self.deferred_events.push_back(event),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(
                        "timed out waiting for hardware cancellation acknowledgement".to_owned(),
                    );
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("acquisition stimulation event channel disconnected".to_owned());
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_device::StimStepSize;

    struct FakeControl {
        next_sample_index: u64,
        configured: StimConfig,
        scheduled: Vec<ScheduledStimRequest>,
        cancelled: Vec<u64>,
        events: VecDeque<AcquisitionStimEvent>,
    }

    impl FakeControl {
        fn new() -> Self {
            Self {
                next_sample_index: 1_000,
                configured: StimConfig::default(),
                scheduled: Vec::new(),
                cancelled: Vec::new(),
                events: VecDeque::new(),
            }
        }
    }

    impl AcquisitionStimControl for FakeControl {
        fn next_sample_index(&self) -> u64 {
            self.next_sample_index
        }

        fn configured_stimulation(&self) -> Option<StimConfig> {
            Some(self.configured.clone())
        }

        fn schedule_stim(&mut self, request: ScheduledStimRequest) -> Result<(), String> {
            self.scheduled.push(request);
            Ok(())
        }

        fn cancel_stim(&mut self, command_id: u64) -> Result<(), String> {
            self.cancelled.push(command_id);
            self.events.push_back(AcquisitionStimEvent {
                command_id,
                state: AcquisitionStimState::Cancelled,
                target_sample_index: 1_001,
                sample_index: Some(1_010),
                message: "cancelled".to_owned(),
            });
            Ok(())
        }

        fn emergency_stop(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn try_stim_event(&mut self) -> Result<AcquisitionStimEvent, TryRecvError> {
            self.events.pop_front().ok_or(TryRecvError::Empty)
        }

        fn recv_stim_event_timeout(
            &mut self,
            _timeout: Duration,
        ) -> Result<AcquisitionStimEvent, RecvTimeoutError> {
            self.events.pop_front().ok_or(RecvTimeoutError::Timeout)
        }
    }

    fn parameters() -> StimParameters {
        StimParameters {
            channel: 0,
            trigger: 0,
            pulses: 1,
            magnitude_steps: 1,
            step_size: StimStepSize::N10,
            phase_steps: 6,
            interphase_steps: 3,
            period_steps: 3_000,
        }
    }

    #[test]
    fn schedules_against_sample_clock_and_reports_observed_completion() {
        let mut executor = SynchronizedStimExecutor::new(
            FakeControl::new(),
            SynchronizedStimExecutorConfig::default(),
        )
        .unwrap();
        executor.start(&parameters()).unwrap();
        assert_eq!(executor.control().scheduled[0].target_sample_index, 1_001);

        executor.control_mut().events.extend([
            AcquisitionStimEvent {
                command_id: 1,
                state: AcquisitionStimState::Observed,
                target_sample_index: 1_001,
                sample_index: Some(1_004),
                message: "observed".to_owned(),
            },
            AcquisitionStimEvent {
                command_id: 1,
                state: AcquisitionStimState::Completed,
                target_sample_index: 1_001,
                sample_index: Some(1_019),
                message: "completed".to_owned(),
            },
        ]);
        let state = executor.poll().unwrap();
        assert_eq!(
            state,
            ExecutionState::Completed {
                message: "FPGA stimulation observed at sample 1004, target=1001, completed at sample 1019"
                    .to_owned()
            }
        );
    }

    #[test]
    fn rejects_parameters_that_do_not_match_preloaded_hardware() {
        let mut executor = SynchronizedStimExecutor::new(
            FakeControl::new(),
            SynchronizedStimExecutorConfig::default(),
        )
        .unwrap();
        let mut different = parameters();
        different.pulses = 2;
        let error = executor.start(&different).unwrap_err();
        assert!(error.contains("preloaded"));
    }

    #[test]
    fn waits_for_cancellation_acknowledgement() {
        let mut executor = SynchronizedStimExecutor::new(
            FakeControl::new(),
            SynchronizedStimExecutorConfig::default(),
        )
        .unwrap();
        executor.start(&parameters()).unwrap();
        executor.cancel().unwrap();
        assert_eq!(executor.control().cancelled, vec![1]);
        assert_eq!(executor.drain_hardware_events().len(), 1);
    }
}
