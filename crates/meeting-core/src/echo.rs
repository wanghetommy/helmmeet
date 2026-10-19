//! AEC availability is separate from capture and from whether a frame contains silence.
#[derive(Default)]
pub struct Gate {
    route: Option<String>,
    excited: usize,
    silent: usize,
    valid: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Applied,
    Warming,
    Silent,
    Bypass,
}
impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Warming => "warming",
            Self::Silent => "silent",
            Self::Bypass => "bypass",
        }
    }
}
pub struct Decision {
    pub reset: bool,
    pub state: State,
    pub use_echo: bool,
}
impl Gate {
    pub fn observe(
        &mut self,
        route: Option<&str>,
        reference_present: bool,
        reference_energy: f32,
        delay: Option<i32>,
    ) -> Decision {
        let valid = route.is_some() && reference_present;
        let changed = self.route.as_deref() != route || self.valid != valid;
        if changed {
            self.excited = 0;
            self.silent = 0;
        }
        self.route = route.map(str::to_owned);
        self.valid = valid;
        if !valid {
            return Decision {
                reset: changed,
                state: State::Bypass,
                use_echo: false,
            };
        }
        if reference_energy > 1e-8 {
            self.excited += 1;
            self.silent = 0;
        } else {
            self.silent += 1;
        }
        let calibrated = self.excited >= 200 && delay.is_some_and(|d| (0..=500).contains(&d));
        Decision {
            reset: changed,
            state: if reference_energy <= 1e-8 {
                State::Silent
            } else if calibrated {
                State::Applied
            } else {
                State::Warming
            },
            use_echo: calibrated && self.silent < 100,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaps_routes_and_unavailable_delay_never_claim_effective_aec() {
        let mut gate = Gate::default();
        for _ in 0..300 {
            let d = gate.observe(Some("r1"), true, 0.1, None);
            assert!(!d.use_echo);
            assert_eq!(d.state, State::Warming);
        }
        assert!(gate.observe(Some("r1"), true, 0.1, Some(40)).use_echo);
        let d = gate.observe(Some("r1"), false, 0.1, Some(40));
        assert!(d.reset);
        assert_eq!(d.state, State::Bypass);
        assert!(!gate.observe(Some("r1"), true, 0.1, Some(40)).use_echo);
        assert!(gate.observe(Some("r2"), true, 0.1, Some(40)).reset);
        for _ in 0..200 {
            assert!(!gate.observe(Some("r2"), true, 0.1, Some(700)).use_echo);
        }
        assert_eq!(gate.observe(None, true, 0.1, Some(40)).state, State::Bypass);
    }
    #[test]
    fn recorded_silence_is_valid_but_never_reported_as_echo_suppression() {
        let mut gate = Gate::default();
        for _ in 0..300 {
            let d = gate.observe(Some("r"), true, 0.0, Some(0));
            assert_eq!(d.state, State::Silent);
            assert!(!d.use_echo);
        }
    }
}
