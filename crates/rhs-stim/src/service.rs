use crate::{
    ArmRequest, ArmSession, ExecutionState, RejectionReason, StimCommand, StimExecutor,
    StimOutcome, StimPreset, StimResult, StimSafetyPolicy, StimServiceState,
};
use rhs_core::{Event, EventKind};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error as StdError;
use std::fmt;

struct ActiveCommand {
    command: StimCommand,
    pulses: u64,
}

pub struct StimService {
    policy: StimSafetyPolicy,
    presets: BTreeMap<String, StimPreset>,
    arm_session: Option<ArmSession>,
    pending: VecDeque<StimCommand>,
    active: Option<ActiveCommand>,
    seen_command_ids: BTreeSet<u64>,
    last_execution_started_us: Option<u64>,
    fault: Option<String>,
    next_arm_session_id: u64,
    next_event_sequence: u64,
    events: Vec<Event>,
    results: Vec<StimResult>,
}

impl StimService {
    pub fn new(policy: StimSafetyPolicy, presets: Vec<StimPreset>) -> Result<Self, StimError> {
        validate_policy(&policy)?;
        if presets.is_empty() {
            return Err(StimError::new("at least one approved preset is required"));
        }
        let mut indexed = BTreeMap::new();
        for preset in presets {
            validate_preset(&policy, &preset)?;
            let id = preset.id.clone();
            if indexed.insert(id.clone(), preset).is_some() {
                return Err(StimError::new(format!("duplicate preset id: {id}")));
            }
        }
        Ok(Self {
            policy,
            presets: indexed,
            arm_session: None,
            pending: VecDeque::new(),
            active: None,
            seen_command_ids: BTreeSet::new(),
            last_execution_started_us: None,
            fault: None,
            next_arm_session_id: 1,
            next_event_sequence: 0,
            events: Vec::new(),
            results: Vec::new(),
        })
    }

    pub fn state(&self) -> StimServiceState {
        if self.fault.is_some() {
            StimServiceState::Fault
        } else if self.active.is_some() {
            StimServiceState::Running
        } else if self.arm_session.is_some() {
            StimServiceState::Armed
        } else {
            StimServiceState::Disarmed
        }
    }

    pub fn policy(&self) -> &StimSafetyPolicy {
        &self.policy
    }

    pub fn presets(&self) -> impl Iterator<Item = &StimPreset> {
        self.presets.values()
    }

    pub fn arm_session(&self) -> Option<&ArmSession> {
        self.arm_session.as_ref()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn active_command_id(&self) -> Option<u64> {
        self.active.as_ref().map(|active| active.command.id)
    }

    pub fn fault_message(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    pub fn arm(&mut self, request: ArmRequest, now_us: u64) -> Result<ArmSession, StimError> {
        if let Some(fault) = &self.fault {
            return Err(StimError::new(format!(
                "cannot arm while service is faulted: {fault}"
            )));
        }
        if self.arm_session.is_some() || self.active.is_some() || !self.pending.is_empty() {
            return Err(StimError::new(
                "service must be disarmed before creating a new arm session",
            ));
        }
        if request.operator.trim().is_empty() || request.reason.trim().is_empty() {
            return Err(StimError::new("arm request requires operator and reason"));
        }
        if request.approved_preset_ids.is_empty() {
            return Err(StimError::new(
                "arm request must approve at least one preset",
            ));
        }
        let mut unique = BTreeSet::new();
        for id in &request.approved_preset_ids {
            if !self.presets.contains_key(id) {
                return Err(StimError::new(format!(
                    "unknown preset in arm request: {id}"
                )));
            }
            if !unique.insert(id.clone()) {
                return Err(StimError::new(format!(
                    "duplicate preset in arm request: {id}"
                )));
            }
        }

        let session = ArmSession {
            id: self.next_arm_session_id,
            operator: request.operator,
            reason: request.reason,
            approved_preset_ids: request.approved_preset_ids,
            armed_at_us: now_us,
            expires_at_us: now_us.saturating_add(self.policy.max_arm_duration_us),
            executed_pulses: 0,
            reserved_pulses: 0,
        };
        self.next_arm_session_id = self.next_arm_session_id.saturating_add(1);
        self.arm_session = Some(session.clone());
        self.push_event(
            EventKind::StimArmed,
            format!(
                "arm_session={} operator={} presets={}",
                session.id,
                session.operator,
                session.approved_preset_ids.join(",")
            ),
        );
        Ok(session)
    }

    pub fn submit(&mut self, command: StimCommand, now_us: u64) -> StimResult {
        if !self.seen_command_ids.insert(command.id) {
            return self.reject(
                &command,
                now_us,
                RejectionReason::DuplicateCommand,
                "command id was already used",
            );
        }
        if self.fault.is_some() {
            return self.reject(
                &command,
                now_us,
                RejectionReason::Faulted,
                "stimulation service is faulted",
            );
        }
        let Some(session) = self.arm_session.as_ref() else {
            return self.reject(
                &command,
                now_us,
                RejectionReason::NotArmed,
                "stimulation service is not armed",
            );
        };
        if now_us > session.expires_at_us {
            return self.reject(
                &command,
                now_us,
                RejectionReason::ArmExpired,
                "arm session expired",
            );
        }
        if command.arm_session_id != session.id {
            return self.reject(
                &command,
                now_us,
                RejectionReason::ArmSessionMismatch,
                "command does not belong to the active arm session",
            );
        }
        let Some(preset) = self.presets.get(&command.preset_id) else {
            return self.reject(
                &command,
                now_us,
                RejectionReason::UnknownPreset,
                "command references an unknown preset",
            );
        };
        if !session
            .approved_preset_ids
            .iter()
            .any(|id| id == &command.preset_id)
        {
            return self.reject(
                &command,
                now_us,
                RejectionReason::PresetNotApproved,
                "preset was not approved for this arm session",
            );
        }
        if command.issued_at_us > now_us
            || command.not_before_us < command.issued_at_us
            || command.deadline_us < command.not_before_us
            || command.deadline_us.saturating_sub(command.issued_at_us)
                > self.policy.max_command_lifetime_us
        {
            return self.reject(
                &command,
                now_us,
                RejectionReason::InvalidTimingWindow,
                "command timing window is invalid or too long",
            );
        }
        if now_us > command.deadline_us {
            return self.reject(
                &command,
                now_us,
                RejectionReason::DeadlineExpired,
                "command deadline already expired",
            );
        }
        let requested_pulses = preset.parameters.pulses as u64;
        if session
            .executed_pulses
            .saturating_add(session.reserved_pulses)
            .saturating_add(requested_pulses)
            > self.policy.max_pulses_per_arm_session
        {
            return self.reject(
                &command,
                now_us,
                RejectionReason::SessionPulseLimit,
                "arm-session cumulative pulse limit would be exceeded",
            );
        }
        if let Some(last_started) = self.last_execution_started_us {
            let earliest = last_started.saturating_add(self.policy.min_command_interval_us);
            if command.deadline_us < earliest {
                return self.reject(
                    &command,
                    now_us,
                    RejectionReason::CommandInterval,
                    "command cannot meet the minimum inter-command interval before its deadline",
                );
            }
        }

        self.arm_session
            .as_mut()
            .expect("arm session was checked")
            .reserved_pulses += requested_pulses;
        self.pending.push_back(command.clone());
        self.record_result(StimResult {
            command_id: command.id,
            arm_session_id: Some(command.arm_session_id),
            preset_id: Some(command.preset_id),
            at_us: now_us,
            outcome: StimOutcome::Queued,
            message: "command accepted into bounded stimulation queue".to_owned(),
        })
    }

    pub fn tick<E: StimExecutor>(&mut self, now_us: u64, executor: &mut E) -> Option<StimResult> {
        if self.fault.is_some() {
            return None;
        }
        if self
            .arm_session
            .as_ref()
            .is_some_and(|session| now_us > session.expires_at_us)
        {
            return self
                .disarm_with_reason(now_us, executor, "arm session expired")
                .into_iter()
                .next();
        }

        if self.active.is_some() {
            return match executor.poll() {
                Ok(ExecutionState::Running) => None,
                Ok(ExecutionState::Completed { message }) => {
                    let active = self.active.take().expect("active command was checked");
                    if let Some(session) = &mut self.arm_session {
                        session.reserved_pulses =
                            session.reserved_pulses.saturating_sub(active.pulses);
                        session.executed_pulses =
                            session.executed_pulses.saturating_add(active.pulses);
                    }
                    Some(self.record_result(StimResult {
                        command_id: active.command.id,
                        arm_session_id: Some(active.command.arm_session_id),
                        preset_id: Some(active.command.preset_id),
                        at_us: now_us,
                        outcome: StimOutcome::Executed,
                        message,
                    }))
                }
                Err(error) => {
                    let active = self.active.take().expect("active command was checked");
                    let cancel_suffix = executor
                        .cancel()
                        .err()
                        .map_or(String::new(), |cancel_error| {
                            format!("; executor cancel also failed: {cancel_error}")
                        });
                    let message = format!("executor poll failed: {error}{cancel_suffix}");
                    let result = self.record_result(StimResult {
                        command_id: active.command.id,
                        arm_session_id: Some(active.command.arm_session_id),
                        preset_id: Some(active.command.preset_id),
                        at_us: now_us,
                        outcome: StimOutcome::Failed,
                        message: message.clone(),
                    });
                    self.force_fault(now_us, message);
                    Some(result)
                }
            };
        }

        if let Some(position) = self
            .pending
            .iter()
            .position(|command| now_us > command.deadline_us)
        {
            let command = self
                .pending
                .remove(position)
                .expect("position came from queue");
            self.release_reserved_pulses(&command.preset_id);
            return Some(self.reject(
                &command,
                now_us,
                RejectionReason::DeadlineExpired,
                "queued command expired before execution",
            ));
        }

        let position = self
            .pending
            .iter()
            .position(|command| command.not_before_us <= now_us)?;
        let command = self.pending.get(position)?.clone();
        if let Some(last_started) = self.last_execution_started_us {
            let earliest = last_started.saturating_add(self.policy.min_command_interval_us);
            if now_us < earliest {
                if command.deadline_us < earliest {
                    let command = self
                        .pending
                        .remove(position)
                        .expect("position came from queue");
                    self.release_reserved_pulses(&command.preset_id);
                    return Some(self.reject(
                        &command,
                        now_us,
                        RejectionReason::CommandInterval,
                        "minimum inter-command interval exceeds command deadline",
                    ));
                }
                return None;
            }
        }

        let command = self
            .pending
            .remove(position)
            .expect("position came from queue");
        let preset = self
            .presets
            .get(&command.preset_id)
            .expect("queued commands always reference a preset")
            .clone();
        match executor.start(&preset.parameters) {
            Ok(()) => {
                self.last_execution_started_us = Some(now_us);
                self.active = Some(ActiveCommand {
                    command,
                    pulses: preset.parameters.pulses as u64,
                });
                None
            }
            Err(error) => {
                self.release_reserved_pulses(&command.preset_id);
                let message = format!("executor start failed: {error}");
                let result = self.record_result(StimResult {
                    command_id: command.id,
                    arm_session_id: Some(command.arm_session_id),
                    preset_id: Some(command.preset_id),
                    at_us: now_us,
                    outcome: StimOutcome::Failed,
                    message: message.clone(),
                });
                self.force_fault(now_us, message);
                Some(result)
            }
        }
    }

    pub fn cancel<E: StimExecutor>(
        &mut self,
        command_id: u64,
        now_us: u64,
        executor: &mut E,
    ) -> StimResult {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.command.id == command_id)
        {
            let active = self.active.take().expect("active command was checked");
            if let Err(error) = executor.cancel() {
                let message = format!("failed to cancel active command: {error}");
                let result = self.record_result(StimResult {
                    command_id,
                    arm_session_id: Some(active.command.arm_session_id),
                    preset_id: Some(active.command.preset_id),
                    at_us: now_us,
                    outcome: StimOutcome::Failed,
                    message: message.clone(),
                });
                self.force_fault(now_us, message);
                return result;
            }
            self.release_reserved_pulses(&active.command.preset_id);
            return self.record_result(StimResult {
                command_id,
                arm_session_id: Some(active.command.arm_session_id),
                preset_id: Some(active.command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Cancelled,
                message: "active stimulation command cancelled".to_owned(),
            });
        }
        if let Some(position) = self
            .pending
            .iter()
            .position(|command| command.id == command_id)
        {
            let command = self
                .pending
                .remove(position)
                .expect("position came from queue");
            self.release_reserved_pulses(&command.preset_id);
            return self.record_result(StimResult {
                command_id,
                arm_session_id: Some(command.arm_session_id),
                preset_id: Some(command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Cancelled,
                message: "queued stimulation command cancelled".to_owned(),
            });
        }
        self.record_result(StimResult {
            command_id,
            arm_session_id: self.arm_session.as_ref().map(|session| session.id),
            preset_id: None,
            at_us: now_us,
            outcome: StimOutcome::Rejected(RejectionReason::UnknownCommand),
            message: "cannot cancel an unknown or completed command".to_owned(),
        })
    }

    pub fn disarm<E: StimExecutor>(&mut self, now_us: u64, executor: &mut E) -> Vec<StimResult> {
        self.disarm_with_reason(now_us, executor, "operator disarmed stimulation")
    }

    pub fn emergency_stop<E: StimExecutor>(
        &mut self,
        now_us: u64,
        executor: &mut E,
    ) -> Vec<StimResult> {
        let cancel_error = executor.cancel().err();
        let mut affected = Vec::new();
        if let Some(active) = self.active.take() {
            affected.push(self.record_result(StimResult {
                command_id: active.command.id,
                arm_session_id: Some(active.command.arm_session_id),
                preset_id: Some(active.command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::EmergencyStopped,
                message: "active command stopped by Emergency Stop".to_owned(),
            }));
        }
        while let Some(command) = self.pending.pop_front() {
            affected.push(self.record_result(StimResult {
                command_id: command.id,
                arm_session_id: Some(command.arm_session_id),
                preset_id: Some(command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::EmergencyStopped,
                message: "queued command removed by Emergency Stop".to_owned(),
            }));
        }
        self.arm_session = None;
        self.push_event(
            EventKind::EmergencyStop,
            "Emergency Stop executed".to_owned(),
        );
        if let Some(error) = cancel_error {
            self.force_fault(
                now_us,
                format!("Emergency Stop executor cancellation failed: {error}"),
            );
        }
        affected
    }

    pub fn device_fault<E: StimExecutor>(
        &mut self,
        message: impl Into<String>,
        now_us: u64,
        executor: &mut E,
    ) {
        let mut message = message.into();
        if let Err(error) = executor.cancel() {
            message.push_str(&format!("; executor cancellation failed: {error}"));
        }
        if let Some(active) = self.active.take() {
            self.record_result(StimResult {
                command_id: active.command.id,
                arm_session_id: Some(active.command.arm_session_id),
                preset_id: Some(active.command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Failed,
                message: format!("device fault: {message}"),
            });
        }
        self.force_fault(now_us, message);
    }

    pub fn clear_fault(&mut self) -> Result<(), StimError> {
        if self.active.is_some() || !self.pending.is_empty() || self.arm_session.is_some() {
            return Err(StimError::new(
                "cannot clear fault while stimulation state is still active",
            ));
        }
        self.fault = None;
        Ok(())
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn drain_results(&mut self) -> Vec<StimResult> {
        std::mem::take(&mut self.results)
    }

    fn disarm_with_reason<E: StimExecutor>(
        &mut self,
        now_us: u64,
        executor: &mut E,
        reason: &str,
    ) -> Vec<StimResult> {
        let cancel_error = if self.active.is_some() {
            executor.cancel().err()
        } else {
            None
        };
        let mut cancelled = Vec::new();
        if let Some(active) = self.active.take() {
            cancelled.push(self.record_result(StimResult {
                command_id: active.command.id,
                arm_session_id: Some(active.command.arm_session_id),
                preset_id: Some(active.command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Cancelled,
                message: reason.to_owned(),
            }));
        }
        while let Some(command) = self.pending.pop_front() {
            cancelled.push(self.record_result(StimResult {
                command_id: command.id,
                arm_session_id: Some(command.arm_session_id),
                preset_id: Some(command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Cancelled,
                message: reason.to_owned(),
            }));
        }
        self.arm_session = None;
        self.push_event(EventKind::StimDisarmed, reason.to_owned());
        if let Some(error) = cancel_error {
            self.force_fault(
                now_us,
                format!("executor cancel failed while disarming: {error}"),
            );
        }
        cancelled
    }

    fn force_fault(&mut self, now_us: u64, message: String) {
        while let Some(command) = self.pending.pop_front() {
            self.record_result(StimResult {
                command_id: command.id,
                arm_session_id: Some(command.arm_session_id),
                preset_id: Some(command.preset_id),
                at_us: now_us,
                outcome: StimOutcome::Cancelled,
                message: format!("cancelled because stimulation service faulted: {message}"),
            });
        }
        self.active = None;
        self.arm_session = None;
        self.fault = Some(message.clone());
        self.push_event(EventKind::DeviceFault, message);
    }

    fn release_reserved_pulses(&mut self, preset_id: &str) {
        let pulses = self
            .presets
            .get(preset_id)
            .map_or(0, |preset| preset.parameters.pulses as u64);
        if let Some(session) = &mut self.arm_session {
            session.reserved_pulses = session.reserved_pulses.saturating_sub(pulses);
        }
    }

    fn reject(
        &mut self,
        command: &StimCommand,
        now_us: u64,
        reason: RejectionReason,
        message: impl Into<String>,
    ) -> StimResult {
        self.record_result(StimResult {
            command_id: command.id,
            arm_session_id: Some(command.arm_session_id),
            preset_id: Some(command.preset_id.clone()),
            at_us: now_us,
            outcome: StimOutcome::Rejected(reason),
            message: message.into(),
        })
    }

    fn record_result(&mut self, result: StimResult) -> StimResult {
        let kind = match result.outcome {
            StimOutcome::Queued => EventKind::StimRequested,
            StimOutcome::Executed => EventKind::StimExecuted,
            StimOutcome::Rejected(_) => EventKind::StimRejected,
            StimOutcome::Cancelled => EventKind::StimCancelled,
            StimOutcome::Failed => EventKind::StimFailed,
            StimOutcome::EmergencyStopped => EventKind::EmergencyStop,
        };
        self.push_event(
            kind,
            format!("command={} {}", result.command_id, result.message),
        );
        self.results.push(result.clone());
        result
    }

    fn push_event(&mut self, kind: EventKind, message: String) {
        self.events.push(Event {
            sequence: self.next_event_sequence,
            sample_index: None,
            kind,
            message,
        });
        self.next_event_sequence = self.next_event_sequence.saturating_add(1);
    }
}

fn validate_policy(policy: &StimSafetyPolicy) -> Result<(), StimError> {
    if policy.hardware_timestep_hz == 0 {
        return Err(StimError::new(
            "hardware_timestep_hz must be greater than zero",
        ));
    }
    if policy.allowed_channels.is_empty()
        || policy.allowed_channels.iter().any(|channel| *channel > 15)
    {
        return Err(StimError::new(
            "allowed_channels must contain RHS channels 0..15",
        ));
    }
    if policy.allowed_step_sizes.is_empty() {
        return Err(StimError::new("allowed_step_sizes must not be empty"));
    }
    if policy.max_magnitude_steps == 0
        || policy.max_magnitude_steps > 255
        || policy.max_phase_steps == 0
        || policy.max_phase_steps > 1_000
        || policy.max_interphase_steps > 1_000
        || policy.min_period_steps < 10
        || policy.max_period_steps > 30_000
        || policy.max_period_steps < policy.min_period_steps
        || policy.max_pulses_per_command == 0
        || policy.max_pulses_per_command > 20
        || policy.max_pulses_per_arm_session == 0
        || policy.max_phase_charge_picocoulombs == 0
        || policy.max_active_duty_ppm == 0
        || policy.max_active_duty_ppm > 1_000_000
        || policy.max_train_duration_us == 0
        || policy.max_arm_duration_us == 0
        || policy.max_command_lifetime_us == 0
    {
        return Err(StimError::new(
            "stimulation safety policy contains invalid or unsupported limits",
        ));
    }
    let unique_channels: BTreeSet<_> = policy.allowed_channels.iter().collect();
    let unique_steps: BTreeSet<_> = policy.allowed_step_sizes.iter().collect();
    if unique_channels.len() != policy.allowed_channels.len()
        || unique_steps.len() != policy.allowed_step_sizes.len()
    {
        return Err(StimError::new(
            "stimulation safety policy contains duplicate allow-list entries",
        ));
    }
    Ok(())
}

fn validate_preset(policy: &StimSafetyPolicy, preset: &StimPreset) -> Result<(), StimError> {
    if preset.id.trim().is_empty() || preset.label.trim().is_empty() {
        return Err(StimError::new(
            "stim preset requires non-empty id and label",
        ));
    }
    let parameters = &preset.parameters;
    if !policy.allowed_channels.contains(&parameters.channel) {
        return Err(StimError::new(format!(
            "preset {} uses a channel outside the safety policy",
            preset.id
        )));
    }
    if parameters.trigger > 7
        || parameters.pulses == 0
        || parameters.pulses > policy.max_pulses_per_command
        || parameters.magnitude_steps == 0
        || parameters.magnitude_steps > policy.max_magnitude_steps
        || !policy.allowed_step_sizes.contains(&parameters.step_size)
        || parameters.phase_steps == 0
        || parameters.phase_steps > policy.max_phase_steps
        || parameters.interphase_steps > policy.max_interphase_steps
        || parameters.period_steps < policy.min_period_steps
        || parameters.period_steps > policy.max_period_steps
    {
        return Err(StimError::new(format!(
            "preset {} exceeds a configured safety limit",
            preset.id
        )));
    }
    let end_stim = parameters
        .phase_steps
        .saturating_mul(2)
        .saturating_add(parameters.interphase_steps);
    let total_steps = parameters
        .period_steps
        .saturating_mul(parameters.pulses as u32)
        .saturating_add(256);
    if end_stim >= parameters.period_steps || total_steps > 65_534 {
        return Err(StimError::new(format!(
            "preset {} does not fit the FPGA sequencer timing limits",
            preset.id
        )));
    }
    if parameters.phase_charge_picocoulombs_ceil(policy.hardware_timestep_hz)
        > policy.max_phase_charge_picocoulombs
    {
        return Err(StimError::new(format!(
            "preset {} exceeds the per-phase charge limit",
            preset.id
        )));
    }
    if parameters.active_duty_ppm() > policy.max_active_duty_ppm as u64 {
        return Err(StimError::new(format!(
            "preset {} exceeds the active duty-cycle limit",
            preset.id
        )));
    }
    if parameters.train_duration_us_ceil(policy.hardware_timestep_hz) > policy.max_train_duration_us
    {
        return Err(StimError::new(format!(
            "preset {} exceeds the train-duration limit",
            preset.id
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimError {
    message: String,
}

impl StimError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for StimError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DryRunExecutor, StimParameters};
    use rhs_core::StimStepSize;

    fn policy() -> StimSafetyPolicy {
        StimSafetyPolicy {
            hardware_timestep_hz: 30_000,
            allowed_channels: vec![0, 1],
            allowed_step_sizes: vec![StimStepSize::N10],
            max_magnitude_steps: 10,
            max_phase_steps: 30,
            max_interphase_steps: 10,
            min_period_steps: 300,
            max_period_steps: 3_000,
            max_pulses_per_command: 3,
            max_pulses_per_arm_session: 4,
            max_phase_charge_picocoulombs: 100,
            max_active_duty_ppm: 200_000,
            max_train_duration_us: 300_000,
            min_command_interval_us: 50_000,
            max_arm_duration_us: 1_000_000,
            max_command_lifetime_us: 100_000,
        }
    }

    fn preset(id: &str, pulses: u16) -> StimPreset {
        StimPreset {
            id: id.to_owned(),
            label: format!("software test preset {id}"),
            parameters: StimParameters {
                channel: 0,
                trigger: 0,
                pulses,
                magnitude_steps: 1,
                step_size: StimStepSize::N10,
                phase_steps: 6,
                interphase_steps: 3,
                period_steps: 300,
            },
        }
    }

    fn service() -> StimService {
        StimService::new(policy(), vec![preset("single", 1), preset("triple", 3)]).unwrap()
    }

    fn arm(service: &mut StimService, now_us: u64) -> ArmSession {
        service
            .arm(
                ArmRequest {
                    operator: "test-operator".to_owned(),
                    reason: "software state-machine test".to_owned(),
                    approved_preset_ids: vec!["single".to_owned(), "triple".to_owned()],
                },
                now_us,
            )
            .unwrap()
    }

    fn command(id: u64, session: u64, preset_id: &str, issued_at_us: u64) -> StimCommand {
        StimCommand {
            id,
            arm_session_id: session,
            preset_id: preset_id.to_owned(),
            issued_at_us,
            not_before_us: issued_at_us,
            deadline_us: issued_at_us + 100_000,
        }
    }

    #[test]
    fn rejects_commands_while_disarmed() {
        let mut service = service();
        let result = service.submit(command(1, 1, "single", 0), 0);
        assert_eq!(
            result.outcome,
            StimOutcome::Rejected(RejectionReason::NotArmed)
        );
    }

    #[test]
    fn executes_approved_preset_and_records_audit_events() {
        let mut service = service();
        let session = arm(&mut service, 0);
        let queued = service.submit(command(1, session.id, "single", 10), 10);
        assert_eq!(queued.outcome, StimOutcome::Queued);
        let mut executor = DryRunExecutor::new(1);
        assert!(service.tick(10, &mut executor).is_none());
        assert_eq!(service.state(), StimServiceState::Running);
        assert!(service.tick(11, &mut executor).is_none());
        let completed = service.tick(12, &mut executor).unwrap();

        assert_eq!(completed.outcome, StimOutcome::Executed);
        assert_eq!(service.state(), StimServiceState::Armed);
        assert_eq!(service.arm_session().unwrap().executed_pulses, 1);
        assert_eq!(executor.starts(), 1);
        let events = service.drain_events();
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::StimArmed));
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::StimRequested));
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::StimExecuted));
    }

    #[test]
    fn rejects_duplicate_command_ids() {
        let mut service = service();
        let session = arm(&mut service, 0);
        service.submit(command(5, session.id, "single", 0), 0);
        let duplicate = service.submit(command(5, session.id, "single", 1), 1);
        assert_eq!(
            duplicate.outcome,
            StimOutcome::Rejected(RejectionReason::DuplicateCommand)
        );
    }

    #[test]
    fn enforces_session_cumulative_pulse_limit() {
        let mut service = service();
        let session = arm(&mut service, 0);
        assert_eq!(
            service
                .submit(command(1, session.id, "triple", 0), 0)
                .outcome,
            StimOutcome::Queued
        );
        let rejected = service.submit(command(2, session.id, "triple", 1), 1);
        assert_eq!(
            rejected.outcome,
            StimOutcome::Rejected(RejectionReason::SessionPulseLimit)
        );
    }

    #[test]
    fn queued_command_expiration_is_explicit() {
        let mut service = service();
        let session = arm(&mut service, 0);
        let mut delayed = command(1, session.id, "single", 0);
        delayed.not_before_us = 50_000;
        delayed.deadline_us = 60_000;
        service.submit(delayed, 0);
        let mut executor = DryRunExecutor::new(0);
        let expired = service.tick(60_001, &mut executor).unwrap();
        assert_eq!(
            expired.outcome,
            StimOutcome::Rejected(RejectionReason::DeadlineExpired)
        );
        assert_eq!(executor.starts(), 0);
    }

    #[test]
    fn can_cancel_queued_and_active_commands() {
        let mut service = service();
        let session = arm(&mut service, 0);
        service.submit(command(1, session.id, "single", 0), 0);
        let mut executor = DryRunExecutor::new(10);
        let queued_cancel = service.cancel(1, 0, &mut executor);
        assert_eq!(queued_cancel.outcome, StimOutcome::Cancelled);
        assert_eq!(executor.cancels(), 0);

        service.submit(command(2, session.id, "single", 1), 1);
        service.tick(1, &mut executor);
        let active_cancel = service.cancel(2, 2, &mut executor);
        assert_eq!(active_cancel.outcome, StimOutcome::Cancelled);
        assert_eq!(executor.cancels(), 1);
        assert_eq!(service.state(), StimServiceState::Armed);
    }

    #[test]
    fn emergency_stop_cancels_everything_and_disarms() {
        let mut service = service();
        let session = arm(&mut service, 0);
        service.submit(command(1, session.id, "single", 0), 0);
        service.submit(command(2, session.id, "single", 1), 1);
        let mut executor = DryRunExecutor::new(10);
        service.tick(0, &mut executor);
        let affected = service.emergency_stop(2, &mut executor);

        assert_eq!(affected.len(), 2);
        assert!(affected
            .iter()
            .all(|result| result.outcome == StimOutcome::EmergencyStopped));
        assert_eq!(service.state(), StimServiceState::Disarmed);
        assert_eq!(executor.cancels(), 1);
    }

    #[test]
    fn device_fault_cancels_and_requires_explicit_clear() {
        let mut service = service();
        let session = arm(&mut service, 0);
        service.submit(command(1, session.id, "single", 0), 0);
        let mut executor = DryRunExecutor::new(10);
        service.tick(0, &mut executor);
        service.device_fault("USB disconnected", 1, &mut executor);

        assert_eq!(service.state(), StimServiceState::Fault);
        assert!(service
            .fault_message()
            .unwrap()
            .contains("USB disconnected"));
        assert!(service
            .arm(
                ArmRequest {
                    operator: "operator".to_owned(),
                    reason: "retry".to_owned(),
                    approved_preset_ids: vec!["single".to_owned()],
                },
                2
            )
            .is_err());
        service.clear_fault().unwrap();
        assert_eq!(service.state(), StimServiceState::Disarmed);
    }

    #[test]
    fn rejects_unsafe_preset_during_service_creation() {
        let mut unsafe_preset = preset("unsafe", 1);
        unsafe_preset.parameters.magnitude_steps = 11;
        let error = StimService::new(policy(), vec![unsafe_preset])
            .err()
            .unwrap();
        assert!(error.to_string().contains("safety limit"));
    }

    #[test]
    fn arm_expiration_cancels_an_active_command_and_disarms() {
        let mut service = service();
        let session = arm(&mut service, 0);
        service.submit(command(1, session.id, "single", 999_999), 999_999);
        let mut executor = DryRunExecutor::new(10);
        service.tick(999_999, &mut executor);
        let expired = service.tick(1_000_001, &mut executor).unwrap();

        assert_eq!(expired.outcome, StimOutcome::Cancelled);
        assert_eq!(service.state(), StimServiceState::Disarmed);
        assert_eq!(executor.cancels(), 1);
    }

    #[test]
    fn minimum_command_interval_delays_the_next_start() {
        let mut service = service();
        let session = arm(&mut service, 0);
        let mut executor = DryRunExecutor::new(0);
        service.submit(command(1, session.id, "single", 0), 0);
        service.tick(0, &mut executor);
        service.tick(1, &mut executor).unwrap();

        service.submit(command(2, session.id, "single", 2), 2);
        assert!(service.tick(49_999, &mut executor).is_none());
        assert_eq!(executor.starts(), 1);
        assert!(service.tick(50_000, &mut executor).is_none());
        assert_eq!(executor.starts(), 2);
        assert_eq!(
            service.tick(50_001, &mut executor).unwrap().outcome,
            StimOutcome::Executed
        );
    }

    #[test]
    fn rejects_preset_above_phase_charge_limit() {
        let mut strict_policy = policy();
        strict_policy.max_magnitude_steps = 255;
        strict_policy.max_phase_charge_picocoulombs = 1;
        let error = StimService::new(strict_policy, vec![preset("too-much-charge", 1)])
            .err()
            .unwrap();
        assert!(error.to_string().contains("per-phase charge"));
    }
}
