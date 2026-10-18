#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingState {
    Ready,
    Recording,
    Paused,
    Interrupted,
    Finalizing,
    Completed,
    NeedsAttention,
    Deleting,
}

#[derive(Debug, Clone, Copy)]
pub enum MeetingAction {
    Start,
    Pause,
    Resume,
    Interrupt,
    Stop,
    FinalizeSucceeded,
    FinalizeFailed,
    Retry,
    Delete,
}

#[derive(Debug, PartialEq, Eq)]
pub struct InvalidTransition;

impl MeetingState {
    /// The service persists this transition only after the corresponding IO succeeds.
    pub fn transition(self, action: MeetingAction) -> Result<Self, InvalidTransition> {
        use MeetingAction as A;
        use MeetingState as S;
        match (self, action) {
            (S::Ready, A::Start) => Ok(S::Recording),
            (S::Recording, A::Pause) => Ok(S::Paused),
            (S::Recording, A::Interrupt) => Ok(S::Interrupted),
            (S::Paused | S::Interrupted, A::Resume) => Ok(S::Recording),
            (S::Recording | S::Paused | S::Interrupted, A::Stop) => Ok(S::Finalizing),
            (S::Finalizing, A::FinalizeSucceeded) => Ok(S::Completed),
            (S::Finalizing, A::FinalizeFailed) => Ok(S::NeedsAttention),
            (S::NeedsAttention, A::Retry) => Ok(S::Finalizing),
            // Active recording must be stopped and flushed before deletion.
            (S::Ready | S::Completed | S::NeedsAttention, A::Delete) => Ok(S::Deleting),
            _ => Err(InvalidTransition),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_resume_and_finalize() {
        use MeetingAction::*;
        let mut state = MeetingState::Ready;
        for action in [Start, Pause, Resume, Stop, FinalizeSucceeded] {
            state = state.transition(action).unwrap();
        }
        assert_eq!(state, MeetingState::Completed);
    }
    #[test]
    fn active_recording_cannot_be_deleted_or_completed() {
        assert!(MeetingState::Recording
            .transition(MeetingAction::Delete)
            .is_err());
        assert!(MeetingState::Recording
            .transition(MeetingAction::FinalizeSucceeded)
            .is_err());
        assert!(MeetingState::Deleting
            .transition(MeetingAction::Start)
            .is_err());
    }
    #[test]
    fn interruption_requires_explicit_resume_and_failure_can_retry() {
        let interrupted = MeetingState::Recording
            .transition(MeetingAction::Interrupt)
            .unwrap();
        assert_eq!(interrupted, MeetingState::Interrupted);
        assert!(interrupted.transition(MeetingAction::Start).is_err());
        let finalizing = interrupted.transition(MeetingAction::Stop).unwrap();
        let failed = finalizing
            .transition(MeetingAction::FinalizeFailed)
            .unwrap();
        assert_eq!(
            failed.transition(MeetingAction::Retry),
            Ok(MeetingState::Finalizing)
        );
    }
}
