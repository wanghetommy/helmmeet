//! Meeting presentation follows capture state and actual persisted capabilities.
#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Preparation,
    Live,
    After,
}
pub fn phase(state: &str) -> Phase {
    match state {
        "ready" => Phase::Preparation,
        "recording" | "paused" | "interrupted" => Phase::Live,
        _ => Phase::After,
    }
}
pub fn can_analyze(
    state: &str,
    confirmed: usize,
    model_ready: bool,
    globally_paused: bool,
) -> bool {
    state != "ready" && state != "deleting" && confirmed > 0 && model_ready && !globally_paused
}
/// A dialog must never stop a different epoch after pause/resume or a new capture.
pub fn validate_end(
    state: &str,
    version: u64,
    epoch: &str,
    expected_version: u64,
    expected_epoch: &str,
) -> Result<(), &'static str> {
    if !["recording", "paused", "interrupted"].contains(&state)
        || version != expected_version
        || epoch != expected_epoch
    {
        return Err("录制会话或会议版本已变化，请关闭弹窗后重新确认");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_results_or_empty_paid_requests_before_capture() {
        assert_eq!(phase("ready"), Phase::Preparation);
        assert!(!can_analyze("ready", 4, true, false));
        assert!(!can_analyze("recording", 0, true, false));
        assert!(can_analyze("finalizing", 1, true, false));
        assert!(!can_analyze("completed", 1, true, true));
    }
    #[test]
    fn stale_confirmation_cannot_stop_resumed_or_changed_meeting() {
        assert!(validate_end("paused", 2, "epoch-a", 2, "epoch-a").is_ok());
        assert!(validate_end("recording", 2, "epoch-b", 2, "epoch-a").is_err());
        assert!(validate_end("recording", 3, "epoch-a", 2, "epoch-a").is_err());
    }
}
