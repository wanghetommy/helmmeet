//! Conversation policy and wake interruption gates, independent of adapters.
use std::collections::VecDeque;

pub struct PersonaField {
    pub key: &'static str,
    pub label: &'static str,
    pub limit: usize,
    pub default: &'static str,
}
pub const PERSONA_FIELDS: [PersonaField; 4] = [
    PersonaField {key:"personality",label:"性格",limit:300,default:"冷静、温和、坦诚，有判断但不强势；尊重不同意见，不迎合、不说教。"},
    PersonaField {key:"expression",label:"表达风格",limit:800,default:"自然口语、短句、少术语；直接回应问题，不机械套用报告结构，不重复自我介绍。\n\n惯用语（按需使用）：我的判断是……；关键在于……；我建议先……。偶尔自然使用，每条回答至多一次；短答和严肃风险讨论优先省略，避免相邻两轮重复。"},
    PersonaField {key:"thinking_models",label:"思维模型",limit:600,default:"按需使用目标澄清、第一性原理、假设与证据检验、5Why、利弊权衡、金字塔表达；不凑步骤，区分事实、推测与建议。"},
    PersonaField {key:"expertise",label:"知识与经验",limit:1200,default:"熟悉会议促进、目标与项目管理、组织协作、经营分析和复盘方法，能结合常见实践提供建议；区分通用经验与本场已确认事实，缺少信息时说明假设。"},
];

pub const GENERAL_RULES: &str = "你是一位帮助讨论形成清晰判断、推进下一步的会议伙伴。先理解当前问题，再直接回应。结合本场信息和必要的通用知识，区分事实、推测与建议。发现关键缺口时，优先澄清最影响判断的问题；出现分歧时，帮助明确判断标准；需要收敛时，整理已确认内容、未决问题和下一步建议。不替参会人宣布决定。";
pub const ANSWER_MODES: [(&str, &str, &str); 6] = [
    (
        "auto",
        "自动",
        "在本次回答中按当前问题选择最合适的方式，不额外发起分类请求。",
    ),
    (
        "clarify",
        "澄清",
        "指出最影响判断的信息缺口，优先澄清一个关键问题。",
    ),
    (
        "analyze",
        "分析",
        "梳理证据、原因、假设与风险，聚焦最关键的1–2点，不把猜测当根因。",
    ),
    (
        "diverge",
        "发散",
        "提供少量有差异的方向或办法，说明适用条件，不急于宣布采纳。",
    ),
    (
        "judge",
        "判断",
        "按明确标准比较选项与取舍，依据充分时提出倾向，关键假设不足时指出缺口。",
    ),
    (
        "converge",
        "收敛",
        "整理已确认事项、未决问题和下一步建议，不补造负责人、期限或共识。",
    ),
];
pub fn answer_mode(mode: &str) -> Option<(&'static str, &'static str)> {
    ANSWER_MODES
        .iter()
        .find(|v| v.0 == mode)
        .map(|v| (v.1, v.2))
}
pub fn meeting_goal(kind: &str) -> &'static str {
    match kind {
        "weekly" | "project" => "对齐目标和进展，识别阻塞与依赖，明确下一步。",
        "review" => "评估方案是否满足目标与约束，明确取舍及验证条件。",
        "decision" => "比较选项与判断标准，帮助参会人形成有依据的决定。",
        "retro" => "依据实际结果识别差距与原因，形成可验证的改进。",
        "brainstorm" => "围绕问题边界探索有差异的想法和待验证方向。",
        _ => "澄清当前讨论目标，形成有依据的判断和下一步建议。",
    }
}

pub fn meeting_strategy(kind: &str) -> &'static str {
    match kind {
        "weekly" | "project" => "优先检查目标、进展、阻塞、依赖和下一步，不把进度汇报当成承诺。",
        "review" => "优先检查问题边界、约束、替代方案、关键取舍和验证条件，不把假设当结论。",
        "decision" => "优先检查选项、判断标准、利弊、决策权限及待确认决定，不代替主持人决策。",
        "retro" => "区分预期与实际、差距、原因证据及改进行动，不把归因猜测当成根因。",
        "brainstorm" => "先澄清探索方向，提供少量有差异的想法，不急于收敛，不把想法当已采纳方案。",
        _ => "直接回答当前问题，必要时澄清目的，不强行套用完整会议分析框架。",
    }
}

pub fn interruptible(phase: &str) -> bool {
    matches!(
        phase,
        "standby"
            | "error"
            | "testing_wake"
            | "transcribing"
            | "thinking"
            | "synthesizing"
            | "speaking"
    )
}

/// Validate a recognized keyword against processed microphone and actual playback
/// reference. AEC is allowed to remove arbitrarily loud far-end echo; admission
/// must never require the remaining human voice to dominate the raw microphone.
#[derive(Default)]
pub struct PlaybackWakeGate {
    frames: VecDeque<(usize, bool)>,
    clean: VecDeque<f32>,
    reference: VecDeque<f32>,
}
impl PlaybackWakeGate {
    pub fn clear(&mut self) {
        self.frames.clear();
        self.clean.clear();
        self.reference.clear();
    }
    pub fn observe(&mut self, clean: &[f32], reference: &[f32], present: bool) {
        self.frames.push_back((clean.len(), present));
        self.clean.extend(clean.iter().copied());
        self.reference.extend(reference.iter().copied());
        while self.clean.len() > 3200 {
            self.clean.pop_front();
        }
        // Keep an additional 100ms for delayed echo paths, separate from the
        // 200ms of microphone evidence evaluated when KWS reports a keyword.
        while self.reference.len() > 4800 {
            self.reference.pop_front();
        }
        while self.frames.len() > 20 {
            self.frames.pop_front();
        }
    }
    pub fn reference_ready(&self) -> bool {
        let samples: usize = self.frames.iter().map(|v| v.0).sum();
        let referenced: usize = self.frames.iter().filter(|v| v.1).map(|v| v.0).sum();
        samples >= 1600 && referenced * 5 >= samples * 4
    }
    pub fn near_end(&self) -> bool {
        if !self.reference_ready() {
            return false;
        }
        let reference: Vec<f64> = self.reference.iter().map(|v| f64::from(*v)).collect();
        if reference.iter().any(|v| !v.is_finite()) {
            return false;
        }
        let max_lag = 1600.min(reference.len().saturating_sub(800));
        let n = self
            .clean
            .len()
            .min(reference.len().saturating_sub(max_lag));
        if n < 800 {
            return false;
        }
        let mut residual: Vec<f64> = self
            .clean
            .iter()
            .skip(self.clean.len() - n)
            .map(|v| f64::from(*v))
            .collect();
        let original = residual.iter().map(|v| v * v).sum::<f64>();
        // KWS has already recognized a full word. This is a numerical silence
        // floor, not a fixed audibility/VAD threshold after AEC suppression.
        if !original.is_finite() || original / (n as f64) < 0.00000001 {
            return false;
        }
        // Match a bounded set of echo paths rather than comparing clean/raw
        // loudness. Single or multiple delayed replicas must remain ineligible.
        // This bounded search runs only for a completed KWS candidate, never
        // on every frame. Exact sample offsets also reject broadband echoes.
        for _ in 0..4 {
            let score = |lag: usize| {
                let begin = reference.len() - lag - n;
                let (dot, power) = residual
                    .iter()
                    .zip(&reference[begin..begin + n])
                    .fold((0., 0.), |(d, p), (a, b)| (d + a * b, p + b * b));
                (
                    if power > 1e-12 { dot * dot / power } else { 0. },
                    dot,
                    power,
                )
            };
            let mut best = (0., 0usize, 0., 0.);
            for lag in 0..=max_lag {
                let (value, dot, power) = score(lag);
                if value > best.0 {
                    best = (value, lag, dot, power);
                }
            }
            if best.0 <= original * 0.02 {
                break;
            }
            let begin = reference.len() - best.1 - n;
            let gain = best.2 / best.3;
            for (value, echo) in residual.iter_mut().zip(&reference[begin..begin + n]) {
                *value -= gain * echo;
            }
            if residual.iter().map(|v| v * v).sum::<f64>() <= original * 0.2 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_answer_phases_can_be_interrupted_but_listening_cannot_retrigger() {
        for phase in ["thinking", "synthesizing", "speaking", "transcribing"] {
            assert!(interruptible(phase));
        }
        for phase in ["listening", "off"] {
            assert!(!interruptible(phase));
        }
    }
    #[test]
    fn self_playback_and_missing_reference_cannot_trigger_barge_in() {
        let mut gate = PlaybackWakeGate::default();
        for i in 0..20 {
            let reference: Vec<f32> = (0..160)
                .map(|n| ((i * 160 + n) as f32 * 0.17).sin() * 0.1)
                .collect();
            let residual: Vec<f32> = reference.iter().map(|x| x * 0.1).collect();
            gate.observe(&residual, &reference, true);
        }
        assert!(gate.reference_ready());
        assert!(!gate.near_end());
        gate.clear();
        for _ in 0..20 {
            gate.observe(&[0.1; 160], &[0.; 160], false);
        }
        assert!(!gate.near_end());
    }
    #[test]
    fn delayed_self_playback_is_not_near_end_speech_even_without_aec_attenuation() {
        let mut gate = PlaybackWakeGate::default();
        let reference: Vec<f32> = (0..3200)
            .map(|n| ((n as f32 * 0.19).sin() + (n as f32 * 0.079).sin()) * 0.04)
            .collect();
        let mut delayed = vec![0.; 480];
        delayed.extend_from_slice(&reference[..2720]);
        for (a, b) in delayed.chunks(160).zip(reference.chunks(160)) {
            gate.observe(a, b, true);
        }
        assert!(gate.reference_ready());
        assert!(!gate.near_end());
    }
    #[test]
    fn several_attenuated_echo_paths_cannot_masquerade_as_a_quiet_human() {
        let mut random = 17u32;
        let reference: Vec<f32> = (0..6400)
            .map(|_| {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                ((random >> 8) as f32 / 16777216. - 0.5) * 0.6
            })
            .collect();
        let mut gate = PlaybackWakeGate::default();
        for start in (0..6400).step_by(160) {
            let clean: Vec<f32> = (start..start + 160)
                .map(|i| {
                    [(33, 0.07), (173, 0.05), (418, 0.04)]
                        .into_iter()
                        .map(|(delay, gain)| {
                            if i >= delay {
                                reference[i - delay] * gain
                            } else {
                                0.
                            }
                        })
                        .sum()
                })
                .collect();
            gate.observe(&clean, &reference[start..start + 160], true);
        }
        assert!(!gate.near_end());
        for _ in 0..10 {
            gate.observe(&[0.1; 160], &[0.; 160], false);
        }
        assert!(!gate.reference_ready());
        assert!(!gate.near_end());
        gate.clear();
        assert!(!gate.reference_ready());
    }
    #[test]
    fn independent_near_end_speech_remains_eligible_during_playback() {
        let mut gate = PlaybackWakeGate::default();
        for i in 0..20 {
            let reference: Vec<f32> = (0..160)
                .map(|n| ((i * 160 + n) as f32 * 0.17).sin() * 0.1)
                .collect();
            let clean: Vec<f32> = (0..160)
                .map(|n| ((i * 160 + n) as f32 * 0.29).sin() * 0.01)
                .collect();
            let raw: Vec<f32> = clean
                .iter()
                .zip(&reference)
                .map(|(a, b)| a + b * 0.9)
                .collect();
            assert!(
                clean.iter().map(|v| v * v).sum::<f32>()
                    < raw.iter().map(|v| v * v).sum::<f32>() * 0.2
            );
            gate.observe(&clean, &reference, true);
        }
        assert!(gate.near_end());
    }
}
