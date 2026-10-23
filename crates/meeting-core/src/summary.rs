//! Completion-based windows and soft speech boundaries. All times are recording/ASR milliseconds.
use std::collections::BTreeMap;
pub type Revisions = BTreeMap<String, (u64, usize)>;
/// Normalize only explicit equivalents. Task/section names do not establish
/// whether a statement is a decision, a suggestion, or a discussion.
pub fn canonical_kind(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "discussion" | "讨论" => Some("discussion"),
        "decision" | "决定" | "决策" | "决议" => Some("decision"),
        "risk" | "风险" => Some("risk"),
        "question" | "问题" | "疑问" => Some("question"),
        "suggestion" | "建议" => Some("suggestion"),
        "unanswered" | "待澄清" | "未明确" => Some("unanswered"),
        _ => None,
    }
}
#[derive(Default, Debug)]
pub struct Window {
    seen: Revisions,
    started: Option<u64>,
    in_flight: bool,
    cooldown: Option<u64>,
    ready_at: Option<u64>,
}
impl Window {
    pub fn observe(&mut self, now: u64, pending: &Revisions) {
        if !self.in_flight {
            if pending.is_empty() {
                self.ready_at = None;
            } else {
                self.started.get_or_insert(now);
            }
        }
        self.seen = pending.clone();
    }
    pub fn can_dispatch(&self, now: u64) -> bool {
        !self.in_flight && self.cooldown.is_none_or(|until| now >= until)
    }
    pub fn due(&self, now: u64, seconds: u64, realtime: bool) -> bool {
        self.eligible(
            now,
            if realtime {
                "realtime"
            } else if seconds >= 600 {
                "economy"
            } else {
                "three_minutes"
            },
            false,
            false,
        )
    }
    pub fn eligible(&self, now: u64, mode: &str, first: bool, revised: bool) -> bool {
        if !self.can_dispatch(now) || self.seen.is_empty() {
            return false;
        }
        let elapsed = if first {
            now
        } else {
            now.saturating_sub(self.started.unwrap_or(now))
        };
        let chars: usize = self.seen.values().map(|(_, n)| n).sum();
        if revised {
            return elapsed >= 30_000;
        }
        if first {
            return chars >= 500 || (elapsed >= 180_000 && chars >= 200);
        }
        match mode {
            "realtime" => chars >= 100 || (elapsed >= 30_000 && chars >= 20),
            "economy" | "legacy_10" => {
                elapsed >= 300_000 && (chars >= 3000 || (elapsed >= 600_000 && chars >= 1000))
            }
            "legacy_5" => elapsed >= 300_000 && chars >= 200,
            _ => elapsed >= 180_000 && chars >= 200,
        }
    }
    pub fn wait_seconds(mode: &str, first: bool) -> u64 {
        let base: u64 = if first {
            180
        } else {
            match mode {
                "realtime" => 30,
                "economy" | "legacy_10" => 600,
                "legacy_5" => 300,
                _ => 180,
            }
        };
        base.div_ceil(4).clamp(5, 60)
    }
    /// A latched deadline is not extended by more words, short pauses or a noisy VAD.
    pub fn at_boundary(
        &mut self,
        now: u64,
        mode: &str,
        first: bool,
        revised: bool,
        quiet: bool,
    ) -> bool {
        self.at_boundary_on(now, now, mode, first, revised, quiet)
    }
    pub fn clear_boundary(&mut self) {
        self.ready_at = None;
    }
    pub fn at_boundary_on(
        &mut self,
        now: u64,
        boundary_now: u64,
        mode: &str,
        first: bool,
        revised: bool,
        quiet: bool,
    ) -> bool {
        if !self.eligible(now, mode, first, revised) {
            self.ready_at = None;
            return false;
        }
        let ready = *self.ready_at.get_or_insert(boundary_now);
        quiet || boundary_now.saturating_sub(ready) >= Self::wait_seconds(mode, first) * 1000
    }
    pub fn waiting(&self) -> bool {
        self.ready_at.is_some()
    }
    pub fn dispatched(&mut self) {
        self.in_flight = true;
        self.ready_at = None;
    }
    pub fn completed(&mut self, now: u64, pending: &Revisions, failed: bool, seconds: u64) {
        self.in_flight = false;
        self.seen = pending.clone();
        self.started = Some(now);
        self.ready_at = None;
        self.cooldown = failed.then_some(now + seconds * 1000);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kind_aliases_preserve_classification_and_reject_ambiguous_labels() {
        for (alias, expected) in [
            (" Discussion ", "discussion"),
            ("讨论", "discussion"),
            ("决策", "decision"),
            ("风险", "risk"),
            ("问题", "question"),
            ("建议", "suggestion"),
            ("待澄清", "unanswered"),
        ] {
            assert_eq!(canonical_kind(alias), Some(expected));
        }
        for invalid in [
            "stage",
            "minutes",
            "answer",
            "progress",
            "open",
            "task",
            "结论",
            "",
            "discussion/risk",
        ] {
            assert_eq!(canonical_kind(invalid), None);
        }
    }
    fn text(n: usize) -> Revisions {
        BTreeMap::from([("s".into(), (1, n))])
    }
    #[test]
    fn exact_thresholds_and_duplicates() {
        let mut w = Window::default();
        w.observe(0, &text(19));
        assert!(!w.eligible(30_000, "realtime", false, false));
        w.observe(30_000, &text(20));
        assert!(w.eligible(30_000, "realtime", false, false));
        let mut w = Window::default();
        w.observe(0, &text(100));
        assert!(w.eligible(0, "realtime", false, false));
        let mut w = Window::default();
        w.observe(0, &text(199));
        assert!(!w.eligible(180_000, "three_minutes", false, false));
        w.observe(240_000, &text(200));
        assert!(w.eligible(240_000, "three_minutes", false, false));
        for _ in 0..20 {
            w.observe(240_000, &text(200));
        }
        assert!(!w.eligible(0, "realtime", true, false));
    }
    #[test]
    fn waiting_deadline_advances_when_asr_stalls_and_silence_is_unknown() {
        let mut w = Window::default();
        w.completed(0, &Revisions::new(), false, 0);
        w.observe(180_000, &text(200));
        assert!(!w.at_boundary_on(180_000, 1000, "three_minutes", false, false, false));
        w.observe(180_000, &text(201));
        assert!(!w.at_boundary_on(180_000, 45_999, "three_minutes", false, false, false));
        assert!(w.at_boundary_on(180_000, 46_000, "three_minutes", false, false, false));
        w.clear_boundary();
        assert!(!w.at_boundary_on(180_000, 50_000, "three_minutes", false, false, false));
        assert!(w.at_boundary_on(180_000, 50_001, "three_minutes", false, false, true));
        let mut first = Window::default();
        first.observe(180_000, &text(200));
        assert!(first.eligible(180_000, "three_minutes", true, false));
    }
    #[test]
    fn first_and_economy_and_soft_deadlines() {
        let mut w = Window::default();
        w.observe(0, &text(500));
        assert!(!w.at_boundary(0, "realtime", true, false, false));
        assert!(!w.at_boundary(44_999, "realtime", true, false, false));
        assert!(w.at_boundary(45_000, "realtime", true, false, false));
        assert_eq!(Window::wait_seconds("realtime", false), 8);
        assert_eq!(Window::wait_seconds("three_minutes", false), 45);
        assert_eq!(Window::wait_seconds("economy", false), 60);
        let mut w = Window::default();
        w.observe(0, &text(3000));
        assert!(!w.eligible(299_999, "economy", false, false));
        assert!(w.at_boundary(300_000, "economy", false, false, true));
    }
    #[test]
    fn backlog_is_counted_once_and_waits_for_completion() {
        let mut w = Window::default();
        w.observe(0, &text(100));
        w.dispatched();
        w.observe(20_000, &text(600));
        assert!(!w.eligible(20_000, "realtime", false, false));
        w.completed(25_000, &text(600), false, 30);
        assert!(w.eligible(25_000, "realtime", false, false));
        assert!(!w.at_boundary(25_000, "realtime", false, false, false));
        w.observe(32_999, &text(700));
        assert!(!w.at_boundary(32_999, "realtime", false, false, false));
        assert!(w.at_boundary(33_000, "realtime", false, false, false));
        w.dispatched();
        w.completed(40_000, &text(700), true, 30);
        assert!(!w.eligible(69_999, "realtime", false, false));
        assert!(w.eligible(70_000, "realtime", false, false));
    }
}
