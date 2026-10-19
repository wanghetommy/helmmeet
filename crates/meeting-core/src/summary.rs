//! Completion-based automatic summary window. Clock is active recording time supplied by host.
use std::collections::BTreeMap;
pub type Revisions = BTreeMap<String, (u64, usize)>;
#[derive(Default, Debug)]
pub struct Window {
    seen: Revisions,
    started: Option<u64>,
    chars: usize,
    in_flight: bool,
    cooldown: Option<u64>,
}
impl Window {
    pub fn observe(&mut self, now: u64, pending: &Revisions) {
        if pending.is_empty() && !self.in_flight {
            self.started = None;
            self.chars = 0;
        }
        if !self.in_flight && !pending.is_empty() {
            self.started.get_or_insert(now);
            for (id, value) in pending {
                if self.seen.get(id) != Some(value) {
                    self.chars += value.1;
                }
            }
        }
        self.seen = pending.clone();
    }
    pub fn can_dispatch(&self, now: u64) -> bool {
        !self.in_flight && self.cooldown.is_none_or(|until| now >= until)
    }
    pub fn due(&self, now: u64, seconds: u64, realtime: bool) -> bool {
        !self.in_flight
            && !self.seen.is_empty()
            && self.cooldown.is_none_or(|until| now >= until)
            && self.started.is_some_and(|start| {
                (realtime && self.chars >= 60) || now.saturating_sub(start) >= seconds * 1000
            })
    }
    pub fn dispatched(&mut self) {
        self.in_flight = true;
    }
    pub fn completed(&mut self, now: u64, pending: &Revisions, failed: bool, seconds: u64) {
        self.in_flight = false;
        self.seen = pending.clone();
        self.chars = 0;
        self.started = if pending.is_empty() { None } else { Some(now) };
        self.cooldown = failed.then_some(now + seconds * 1000);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn text(n: usize) -> Revisions {
        BTreeMap::from([("s".into(), (1, n))])
    }
    #[test]
    fn first_threshold_wins_and_duplicate_events_do_not_count() {
        let mut w = Window::default();
        w.observe(0, &text(59));
        w.observe(14_000, &text(59));
        assert!(!w.due(14_000, 15, true));
        assert!(w.due(15_000, 15, true));
        let mut w = Window::default();
        w.observe(0, &text(20));
        assert!(w.due(15_000, 15, true));
        let mut w = Window::default();
        w.observe(8_000, &text(60));
        assert!(w.due(8_000, 15, true));
    }
    #[test]
    fn backlog_waits_after_full_completion_even_without_new_events() {
        let mut w = Window::default();
        w.observe(7_000, &text(60));
        w.dispatched();
        w.observe(20_000, &text(600));
        assert!(!w.due(24_000, 15, true));
        w.completed(25_000, &text(600), false, 15);
        assert!(!w.due(25_000, 15, true));
        assert!(!w.due(39_999, 15, true));
        assert!(w.due(40_000, 15, true));
    }
    #[test]
    fn no_backlog_waits_for_text_and_failure_cools_down() {
        let mut w = Window::default();
        w.completed(0, &Revisions::new(), false, 60);
        assert!(!w.due(90_000, 60, false));
        w.observe(90_000, &text(60));
        assert!(!w.due(149_999, 60, false));
        assert!(w.due(150_000, 60, false));
        w.dispatched();
        w.completed(160_000, &text(60), true, 60);
        w.observe(160_001, &text(100));
        assert!(!w.due(160_001, 60, true));
    }
}
