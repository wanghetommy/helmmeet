//! Fixed 10 ms frames with explicit algorithm delay compensation. Never spans a real gap.
pub const FRAME: usize = 160;
pub const NS_DELAY: usize = 96; // 256-point NS overlap - 160 samples, fixed 16 kHz recipe.
pub struct Frames {
    pending: Vec<f32>,
    skip: usize,
    input: usize,
    output: usize,
}
impl Default for Frames {
    fn default() -> Self {
        Self::with_delay(NS_DELAY)
    }
}
impl Frames {
    pub fn with_delay(delay: usize) -> Self {
        assert!(delay <= 320);
        Self {
            pending: Vec::with_capacity(FRAME),
            skip: delay,
            input: 0,
            output: 0,
        }
    }
    pub fn push(
        &mut self,
        input: &[f32],
        mut process: impl FnMut(&[f32; FRAME]) -> Result<[f32; FRAME], String>,
    ) -> Result<Vec<f32>, String> {
        if input.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
            return Err("无效音频样本".into());
        }
        self.input += input.len();
        let mut out = Vec::with_capacity(input.len());
        for &sample in input {
            self.pending.push(sample);
            if self.pending.len() == FRAME {
                let frame: [f32; FRAME] = self.pending.as_slice().try_into().unwrap();
                self.emit(process(&frame)?, &mut out);
                self.pending.clear();
            }
        }
        Ok(out)
    }
    fn emit(&mut self, frame: [f32; FRAME], out: &mut Vec<f32>) {
        let skip = self.skip.min(FRAME);
        self.skip -= skip;
        let take = (self.input - self.output).min(FRAME - skip);
        out.extend_from_slice(&frame[skip..skip + take]);
        self.output += take;
    }
    /// Zero-pad only internally to drain the filter; output always has exactly input length.
    pub fn finish(
        &mut self,
        mut process: impl FnMut(&[f32; FRAME]) -> Result<[f32; FRAME], String>,
    ) -> Result<Vec<f32>, String> {
        let mut out = Vec::new();
        while self.output < self.input {
            let mut frame = [0.0; FRAME];
            frame[..self.pending.len()].copy_from_slice(&self.pending);
            self.pending.clear();
            self.emit(process(&frame)?, &mut out);
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn render(packets: &[&[f32]]) -> Vec<f32> {
        let mut delay = std::collections::VecDeque::from(vec![0.0; NS_DELAY]);
        let mut engine = |frame: &[f32; FRAME]| {
            Ok(std::array::from_fn(|i| {
                delay.push_back(frame[i]);
                delay.pop_front().unwrap()
            }))
        };
        let mut f = Frames::default();
        let mut out = Vec::new();
        for packet in packets {
            out.extend(f.push(packet, &mut engine).unwrap());
        }
        out.extend(f.finish(&mut engine).unwrap());
        out
    }
    #[test]
    fn arbitrary_packets_equal_whole_and_preserve_first_and_last_samples() {
        for size in [1, 95, 159, 160, 161, 64007] {
            let input: Vec<f32> = (0..size).map(|i| (i as f32 * 0.37).sin() * 0.5).collect();
            assert_eq!(render(&[&input]), input);
            assert_eq!(render(&input.chunks(137).collect::<Vec<_>>()), input);
        }
    }
    #[test]
    fn invalid_pcm_does_not_advance_stream() {
        let mut f = Frames::default();
        assert!(f.push(&[f32::NAN], |x| Ok(*x)).is_err());
        assert!(f.finish(|x| Ok(*x)).unwrap().is_empty());
    }
}
