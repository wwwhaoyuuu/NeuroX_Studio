use rhs_core::StimStepSize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimParameters {
    pub channel: u8,
    pub trigger: u8,
    pub pulses: u16,
    pub magnitude_steps: u16,
    pub step_size: StimStepSize,
    pub phase_steps: u32,
    pub interphase_steps: u32,
    pub period_steps: u32,
}

impl StimParameters {
    pub fn phase_charge_picocoulombs_ceil(&self, hardware_timestep_hz: u32) -> u64 {
        let numerator = self.step_size.current_na() as u64
            * self.magnitude_steps as u64
            * self.phase_steps as u64
            * 1_000;
        numerator.div_ceil(hardware_timestep_hz as u64)
    }

    pub fn active_duty_ppm(&self) -> u64 {
        (2_u64 * self.phase_steps as u64 * 1_000_000).div_ceil(self.period_steps as u64)
    }

    pub fn train_duration_us_ceil(&self, hardware_timestep_hz: u32) -> u64 {
        (self.period_steps as u64 * self.pulses as u64 * 1_000_000)
            .div_ceil(hardware_timestep_hz as u64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimPreset {
    pub id: String,
    pub label: String,
    pub parameters: StimParameters,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimSafetyPolicy {
    pub hardware_timestep_hz: u32,
    pub allowed_channels: Vec<u8>,
    pub allowed_step_sizes: Vec<StimStepSize>,
    pub max_magnitude_steps: u16,
    pub max_phase_steps: u32,
    pub max_interphase_steps: u32,
    pub min_period_steps: u32,
    pub max_period_steps: u32,
    pub max_pulses_per_command: u16,
    pub max_pulses_per_arm_session: u64,
    pub max_phase_charge_picocoulombs: u64,
    pub max_active_duty_ppm: u32,
    pub max_train_duration_us: u64,
    pub min_command_interval_us: u64,
    pub max_arm_duration_us: u64,
    pub max_command_lifetime_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmRequest {
    pub operator: String,
    pub reason: String,
    pub approved_preset_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmSession {
    pub id: u64,
    pub operator: String,
    pub reason: String,
    pub approved_preset_ids: Vec<String>,
    pub armed_at_us: u64,
    pub expires_at_us: u64,
    pub executed_pulses: u64,
    pub reserved_pulses: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimCommand {
    pub id: u64,
    pub arm_session_id: u64,
    pub preset_id: String,
    pub issued_at_us: u64,
    pub not_before_us: u64,
    pub deadline_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    NotArmed,
    ArmSessionMismatch,
    ArmExpired,
    PresetNotApproved,
    UnknownPreset,
    DuplicateCommand,
    InvalidTimingWindow,
    DeadlineExpired,
    SessionPulseLimit,
    CommandInterval,
    UnknownCommand,
    Faulted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StimOutcome {
    Queued,
    Executed,
    Rejected(RejectionReason),
    Cancelled,
    Failed,
    EmergencyStopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimResult {
    pub command_id: u64,
    pub arm_session_id: Option<u64>,
    pub preset_id: Option<String>,
    pub at_us: u64,
    pub outcome: StimOutcome,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StimServiceState {
    Disarmed,
    Armed,
    Running,
    Fault,
}
