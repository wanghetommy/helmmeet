//! Map converted PCM onto a common 16 kHz host clock without hiding real gaps.
#[derive(Default)]
pub struct AudioClock {
    cursor: Option<u64>,
    last_host: Option<u64>,
}
#[derive(Debug)]
pub struct Alignment {
    pub start_sample: u64,
    pub output_samples: usize,
    pub gap: Option<(u64, u64)>,
    pub correction_samples: i64,
}
impl AudioClock {
    pub fn reset(&mut self) {
        self.cursor = None;
        self.last_host = None;
    }
    pub fn align(&mut self, host_sample: u64, samples: usize) -> Result<Alignment, &'static str> {
        if samples == 0 {
            return Err("empty frame");
        }
        if self.last_host.is_some_and(|p| host_sample < p) {
            return Err("audio host clock moved backwards");
        }
        let cursor = self.cursor.unwrap_or(host_sample);
        let error = host_sample as i128 - cursor as i128;
        if error < -1600 {
            return Err("audio clock discontinuity");
        }
        let gap = (error > 1600).then_some((cursor, host_sample));
        let start = if gap.is_some() { host_sample } else { cursor };
        // At most 0.2% per frame. Small errors converge smoothly; genuine holes stay holes.
        let bound = (samples / 500).max(1) as i128;
        let correction = if gap.is_some() {
            0
        } else {
            (error / 8).clamp(-bound, bound)
        };
        let output = (samples as i128 + correction).max(1) as usize;
        self.cursor = Some(start + output as u64);
        self.last_host = Some(host_sample);
        Ok(Alignment {
            start_sample: start,
            output_samples: output,
            gap,
            correction_samples: correction as i64,
        })
    }
}
pub fn resample_frame(input: &[f32], length: usize) -> Vec<f32> {
    if input.is_empty() || length == 0 {
        return vec![];
    }
    if length == input.len() {
        return input.to_vec();
    }
    (0..length)
        .map(|i| {
            let at = i as f64 * (input.len() - 1) as f64 / (length - 1).max(1) as f64;
            let lo = at.floor() as usize;
            let hi = (lo + 1).min(input.len() - 1);
            input[lo] + (input[hi] - input[lo]) * (at - lo as f64) as f32
        })
        .collect()
}
/// Fixed source gain, independent of packet availability. Individual track exports use one source.
pub fn mix_gain(source_count: usize) -> f32 {
    1.0 / source_count.max(1) as f32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_hour_clock_drift_is_bounded_for_both_directions() {
        for ppm in [-500.0, 500.0] {
            let mut c = AudioClock::default();
            let mut tail = 0;
            for i in 0..72000u64 {
                let host = (i as f64 * 1600.0 * (1.0 + ppm / 1_000_000.0)).round() as u64;
                let a = c.align(host, 1600).unwrap();
                assert!(a.gap.is_none());
                assert!(a.start_sample.abs_diff(host) < 64);
                tail = a.start_sample + a.output_samples as u64;
            }
            let expected = (72000.0 * 1600.0 * (1.0 + ppm / 1_000_000.0)) as u64;
            assert!(tail.abs_diff(expected) < 64);
        }
    }
    #[test]
    fn preserves_holes_and_rejects_clock_reset() {
        let mut c = AudioClock::default();
        c.align(0, 1600).unwrap();
        let a = c.align(8000, 1600).unwrap();
        assert_eq!(a.gap, Some((1600, 8000)));
        assert_eq!(a.start_sample, 8000);
        assert!(c.align(0, 1600).is_err());
        c.reset();
        assert_eq!(c.align(20000, 1600).unwrap().start_sample, 20000);
    }
    #[test]
    fn conversion_preserves_length_endpoints_and_finite_samples() {
        let x = resample_frame(&[0.0, 0.5, 1.0], 5);
        assert_eq!(x, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(resample_frame(&[0.2], 1), vec![0.2]);
    }
}
