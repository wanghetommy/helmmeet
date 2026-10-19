//! Serialized native APM owner, created and released on the DSP worker thread.
use serde_json::{json, Value};
use std::{ffi::CStr, marker::PhantomData, ptr::NonNull, rc::Rc, time::Instant};

#[repr(C)]
struct Config {
    abi_version: u32,
    noise_level: i32,
    echo_cancellation: i32,
}
extern "C" {
    fn hm_apm_version() -> *const std::ffi::c_char;
    fn hm_apm_create(config: *const Config, output: *mut *mut std::ffi::c_void) -> i32;
    fn hm_apm_process(
        state: *mut std::ffi::c_void,
        input: *const f32,
        output: *mut f32,
        samples: usize,
    ) -> i32;
    fn hm_apm_reverse(
        state: *mut std::ffi::c_void,
        input: *const f32,
        samples: usize,
        delay_ms: i32,
    ) -> i32;
    fn hm_apm_delay_ms(state: *mut std::ffi::c_void) -> i32;
    fn hm_apm_reset(state: *mut std::ffi::c_void) -> i32;
    fn hm_apm_destroy(state: *mut std::ffi::c_void);
}
pub(crate) struct Processor {
    state: NonNull<std::ffi::c_void>,
    // The C ABI requires serialized ownership; never share/move across workers.
    _single_owner: PhantomData<Rc<()>>,
}
impl Processor {
    pub(crate) fn new(noise_level: i32) -> Result<Self, String> {
        Self::with_echo(noise_level, false)
    }
    pub(crate) fn with_echo(noise_level: i32, echo: bool) -> Result<Self, String> {
        let config = Config {
            abi_version: 1,
            noise_level,
            echo_cancellation: i32::from(echo),
        };
        let mut pointer = std::ptr::null_mut();
        // Create copies this plain configuration and transfers one owned handle.
        let code = unsafe { hm_apm_create(&config, &mut pointer) };
        if code != 0 {
            return Err(format!("APM create failed ({code})"));
        }
        Ok(Self {
            state: NonNull::new(pointer).ok_or("APM returned a null handle")?,
            _single_owner: PhantomData,
        })
    }
    pub(crate) fn process(&mut self, input: &[f32; 160]) -> Result<[f32; 160], String> {
        let mut output = [0.0; 160];
        // Both arrays remain valid for this synchronous call; native code retains neither.
        let code = unsafe {
            hm_apm_process(
                self.state.as_ptr(),
                input.as_ptr(),
                output.as_mut_ptr(),
                160,
            )
        };
        if code != 0 {
            return Err(format!("APM processing failed ({code})"));
        }
        Ok(output)
    }
    pub(crate) fn reverse(&mut self, input: &[f32; 160]) -> Result<(), String> {
        // Reference and capture are serialized on the same owner. Disk streams use
        // a shared host timeline, so no callback scheduling/buffering delay is added.
        let code = unsafe { hm_apm_reverse(self.state.as_ptr(), input.as_ptr(), 160, 0) };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("APM reference failed ({code})"))
        }
    }
    pub(crate) fn delay_ms(&mut self) -> Option<i32> {
        // Exclusive access; upstream aggregates metrics during this synchronous call.
        let value = unsafe { hm_apm_delay_ms(self.state.as_ptr()) };
        (value >= 0).then_some(value)
    }
    pub(crate) fn reset(&mut self) -> Result<(), String> {
        // Exclusive access prevents concurrent processing/destruction.
        let code = unsafe { hm_apm_reset(self.state.as_ptr()) };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("APM reset failed ({code})"))
        }
    }
}
impl Drop for Processor {
    fn drop(&mut self) {
        // Exactly one owner; native destruction is synchronous and does not throw.
        unsafe { hm_apm_destroy(self.state.as_ptr()) };
    }
}

pub fn synthetic_probe() -> Result<Value, String> {
    // Static, NUL-terminated version string owned by the native library.
    let version = unsafe { CStr::from_ptr(hm_apm_version()) }
        .to_str()
        .map_err(|_| "Invalid APM version")?;
    let mut processor = Processor::new(2)?;
    let mut times = Vec::with_capacity(1000);
    let mut peak = 0.0f32;
    for frame in 0..1000 {
        let input = std::array::from_fn(|n| {
            let at = (frame * 160 + n) as f32 / 16000.0;
            0.1 * (at * std::f32::consts::TAU * 173.0).sin()
        });
        let at = Instant::now();
        let output = processor.process(&input)?;
        if frame >= 100 {
            times.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        if output.iter().any(|x| !x.is_finite()) {
            return Err("APM returned non-finite PCM".into());
        }
        peak = peak.max(output.iter().map(|x| x.abs()).fold(0.0, f32::max));
    }
    processor.reset()?;
    times.sort_by(f64::total_cmp);
    Ok(
        json!({"engine":version,"synthetic_only":true,"frames":1000,"frame_samples":160,
        "p95_ms":times[times.len()*95/100],"peak":peak,"recording_enabled":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_adapter_runs_and_drops_without_audio_devices() {
        let result = synthetic_probe().unwrap();
        assert_eq!(result["frames"], 1000);
        assert_eq!(result["recording_enabled"], false);
        assert!(result["peak"].as_f64().unwrap() > 0.0);
    }
    #[test]
    fn invalid_samples_leave_instance_reusable() {
        let mut processor = Processor::new(2).unwrap();
        assert!(processor.process(&[f32::NAN; 160]).is_err());
        assert!(processor.process(&[1.1; 160]).is_err());
        processor.reset().unwrap();
        assert!(processor
            .process(&[0.0; 160])
            .unwrap()
            .iter()
            .all(|x| *x == 0.0));
    }
    #[test]
    fn aec_fixed_latency_and_synthetic_echo_doubletalk_metrics() {
        use meeting_core::audio_processing::Frames;
        let count = 24 * 16000;
        let mut seed = 17u32;
        let mut smoothed = 0.0f32;
        let far: Vec<f32> = (0..count)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                smoothed = 0.6 * smoothed + 0.4 * noise;
                if i >= 20 * 16000 {
                    0.0
                } else {
                    smoothed * 0.35
                }
            })
            .collect();
        let near: Vec<f32> = (0..count)
            .map(|i| {
                if i >= 16 * 16000 {
                    0.12 * (i as f32 * 0.073).sin() + 0.08 * (i as f32 * 0.113).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let mic: Vec<f32> = (0..count)
            .map(|i| {
                near[i]
                    + if i >= 640 { far[i - 640] * 0.45 } else { 0.0 }
                    + if i >= 960 { far[i - 960] * 0.12 } else { 0.0 }
            })
            .collect();
        for noise in [0, 1] {
            let mut engine = Processor::with_echo(noise, true).unwrap();
            let mut frames = Frames::with_delay(128 + if noise == 0 { 0 } else { 96 });
            let mut frame_index = 0;
            let mut times = Vec::new();
            let mut process = |input: &[f32; 160]| {
                let render =
                    std::array::from_fn(|i| far.get(frame_index * 160 + i).copied().unwrap_or(0.0));
                frame_index += 1;
                let at = Instant::now();
                engine.reverse(&render)?;
                let output = engine.process(input);
                if frame_index > 100 {
                    times.push(at.elapsed().as_secs_f64() * 1000.0);
                }
                output
            };
            let mut output = frames.push(&mic, &mut process).unwrap();
            output.extend(frames.finish(&mut process).unwrap());
            let energy = |samples: &[f32]| samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            let erle = 10.0
                * (energy(&mic[8 * 16000..14 * 16000])
                    / energy(&output[8 * 16000..14 * 16000]).max(1e-20))
                .log10();
            let gain = output[17 * 16000..19 * 16000]
                .iter()
                .zip(&near[17 * 16000..19 * 16000])
                .map(|(a, b)| *a as f64 * *b as f64)
                .sum::<f64>()
                / energy(&near[17 * 16000..19 * 16000]);
            times.sort_by(f64::total_cmp);
            println!(
                "synthetic AEC NS={noise} reverse+capture P95={:.6}ms P99={:.6}ms",
                times[times.len() * 95 / 100],
                times[times.len() * 99 / 100]
            );
            let delay = engine.delay_ms();
            println!("synthetic AEC NS={noise}: ERLE={erle:.2}dB doubletalk near gain={gain:.4} delay={delay:?}");
            assert!(erle > 10.0);
            let mut baseline_engine = Processor::new(noise).unwrap();
            let mut baseline_frames = Frames::with_delay(if noise == 0 { 0 } else { 96 });
            let mut baseline = baseline_frames
                .push(&mic, |v| baseline_engine.process(v))
                .unwrap();
            baseline.extend(
                baseline_frames
                    .finish(|v| baseline_engine.process(v))
                    .unwrap(),
            );
            let baseline_gain = baseline[17 * 16000..19 * 16000]
                .iter()
                .zip(&near[17 * 16000..19 * 16000])
                .map(|(a, b)| *a as f64 * *b as f64)
                .sum::<f64>()
                / energy(&near[17 * 16000..19 * 16000]);
            println!(
                "synthetic near gain relative to same NS without AEC = {:.4} (NS baseline {:.4})",
                gain / baseline_gain,
                baseline_gain
            );
            assert!(delay.is_some());
            let mut engine = Processor::with_echo(noise, true).unwrap();
            let mut frames = Frames::with_delay(128 + if noise == 0 { 0 } else { 96 });
            let mut pulse = vec![0.0; 16000];
            pulse[8000] = 0.8;
            let mut process = |input: &[f32; 160]| {
                engine.reverse(&[0.0; 160])?;
                engine.process(input)
            };
            let mut output = frames.push(&pulse, &mut process).unwrap();
            output.extend(frames.finish(&mut process).unwrap());
            let peak = output
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
            println!("synthetic AEC NS={noise}: compensated impulse peak={peak}");
            println!("measured residual delay = {} samples", peak as i64 - 8000);
            assert!((0.85..1.2).contains(&(gain / baseline_gain)));
            if noise == 0 {
                assert!((0.85..1.2).contains(&gain));
            }
            assert_eq!(peak, 8000);
        }
    }
    #[test]
    fn aec_handles_multiple_causal_echo_path_delays() {
        use meeting_core::audio_processing::Frames;
        for delay in [320usize, 1280, 2560] {
            let mut seed = 91u32;
            let mut smooth = 0.0f32;
            let far: Vec<f32> = (0..16 * 16000)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    smooth =
                        0.5 * smooth + 0.5 * (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                    smooth * 0.25
                })
                .collect();
            let mic: Vec<f32> = (0..far.len())
                .map(|i| {
                    if i >= delay {
                        far[i - delay] * 0.5
                    } else {
                        0.0
                    }
                })
                .collect();
            let mut engine = Processor::with_echo(0, true).unwrap();
            let mut frames = Frames::with_delay(128);
            let mut cursor = 0;
            let mut process = |v: &[f32; 160]| {
                let render = std::array::from_fn(|i| far.get(cursor + i).copied().unwrap_or(0.0));
                cursor += 160;
                engine.reverse(&render)?;
                engine.process(v)
            };
            let mut output = frames.push(&mic, &mut process).unwrap();
            output.extend(frames.finish(&mut process).unwrap());
            let energy = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            let erle = 10.0
                * (energy(&mic[8 * 16000..]) / energy(&output[8 * 16000..]).max(1e-20)).log10();
            println!("synthetic acoustic path {}ms: ERLE={erle:.2}dB", delay / 16);
            assert!(erle > 10.0);
        }
    }
}
