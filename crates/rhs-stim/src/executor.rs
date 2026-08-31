use crate::StimParameters;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    Running,
    Completed { message: String },
}

pub trait StimExecutor {
    fn start(&mut self, parameters: &StimParameters) -> Result<(), String>;
    fn poll(&mut self) -> Result<ExecutionState, String>;
    fn cancel(&mut self) -> Result<(), String>;
}

#[derive(Debug)]
pub struct DryRunExecutor {
    polls_before_complete: u32,
    polls_remaining: u32,
    active: bool,
    starts: u64,
    cancels: u64,
    started_parameters: Vec<StimParameters>,
}

impl DryRunExecutor {
    pub fn new(polls_before_complete: u32) -> Self {
        Self {
            polls_before_complete,
            polls_remaining: 0,
            active: false,
            starts: 0,
            cancels: 0,
            started_parameters: Vec::new(),
        }
    }

    pub fn starts(&self) -> u64 {
        self.starts
    }

    pub fn cancels(&self) -> u64 {
        self.cancels
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn started_parameters(&self) -> &[StimParameters] {
        &self.started_parameters
    }
}

impl StimExecutor for DryRunExecutor {
    fn start(&mut self, parameters: &StimParameters) -> Result<(), String> {
        if self.active {
            return Err("dry-run executor already has an active command".to_owned());
        }
        self.active = true;
        self.polls_remaining = self.polls_before_complete;
        self.starts = self.starts.saturating_add(1);
        self.started_parameters.push(parameters.clone());
        Ok(())
    }

    fn poll(&mut self) -> Result<ExecutionState, String> {
        if !self.active {
            return Err("dry-run executor has no active command".to_owned());
        }
        if self.polls_remaining > 0 {
            self.polls_remaining -= 1;
            return Ok(ExecutionState::Running);
        }
        self.active = false;
        Ok(ExecutionState::Completed {
            message: "dry-run command completed without hardware output".to_owned(),
        })
    }

    fn cancel(&mut self) -> Result<(), String> {
        if self.active {
            self.active = false;
            self.cancels = self.cancels.saturating_add(1);
        }
        Ok(())
    }
}
