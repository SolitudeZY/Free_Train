use domain::JobState;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobStateMachine {
    state: JobState,
}

impl Default for JobStateMachine {
    fn default() -> Self {
        Self {
            state: JobState::Draft,
        }
    }
}

impl JobStateMachine {
    pub fn state(self) -> JobState {
        self.state
    }

    pub fn transition(&mut self, next: JobState) -> Result<(), JobStateError> {
        if !is_allowed(self.state, next) {
            return Err(JobStateError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

fn is_allowed(from: JobState, to: JobState) -> bool {
    use JobState::*;
    matches!(
        (from, to),
        (Draft, Estimated)
            | (Estimated, Queued)
            | (Queued, Running)
            | (Running, AwaitingReview)
            | (AwaitingReview, Exporting)
            | (Exporting, Completed)
            | (Exporting, CompletedWithErrors)
            | (Running, Cancelling)
            | (Exporting, Cancelling)
            | (Cancelling, Cancelled)
            | (Running, Interrupted)
            | (Exporting, Interrupted)
            | (Interrupted, Queued)
            | (Running, Failed)
    )
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JobStateError {
    #[error("invalid job transition from {from:?} to {to:?}")]
    InvalidTransition { from: JobState, to: JobState },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_m0_happy_path() {
        let mut job = JobStateMachine::default();
        for next in [
            JobState::Estimated,
            JobState::Queued,
            JobState::Running,
            JobState::AwaitingReview,
            JobState::Exporting,
            JobState::Completed,
        ] {
            job.transition(next).expect("transition should be allowed");
        }
        assert_eq!(job.state(), JobState::Completed);
    }

    #[test]
    fn cannot_export_a_draft_job() {
        let mut job = JobStateMachine::default();
        assert!(job.transition(JobState::Exporting).is_err());
    }
}
