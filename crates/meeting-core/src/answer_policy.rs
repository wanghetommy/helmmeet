//! Provenance and scope rules for general experience in meeting answers.
#[derive(Clone, Copy, Default)]
pub struct AdviceScope {
    pub meeting: bool,
    pub complete: bool,
    pub meeting_only: bool,
    pub validation_failed: bool,
}
impl AdviceScope {
    pub fn introduction(self, mixed: bool) -> &'static str {
        match (self.meeting, self.complete, mixed) {
            (false, _, _) => "根据我的知识与经验，",
            (true, true, false) => "前面我们还没讨论这一点。根据我的知识与经验，",
            (true, false, false) => "目前我还没找到相关讨论。根据我的知识与经验，",
            (true, true, true) => "这部分前面还没讨论。根据我的知识与经验，",
            (true, false, true) => "这部分还没找到相关讨论。根据我的知识与经验，",
        }
    }
    pub fn introduce(self, text: &str, mixed: bool) -> String {
        let mut body = text.trim();
        // Idempotent normalization also downgrades an overconfident full-scope
        // opening when the actual context was only partially read.
        for intro in [
            "前面我们还没讨论这一点。根据我的知识与经验，",
            "目前我还没找到相关讨论。根据我的知识与经验，",
            "这部分前面还没讨论。根据我的知识与经验，",
            "这部分还没找到相关讨论。根据我的知识与经验，",
            "根据我的知识与经验，",
        ] {
            if let Some(rest) = body.strip_prefix(intro) {
                body = rest.trim();
                break;
            }
        }
        format!("{}{body}", self.introduction(mixed))
    }
}
/// Recognize explicit plain-language restrictions without another model call.
/// The persona also follows equivalent semantic requests, including languages
/// not covered by this conservative literal guard.
pub fn meeting_only(question: &str) -> bool {
    let question: String = question.chars().filter(|c| !c.is_whitespace()).collect();
    for clause in question.split(['。', '，', ',', '；', ';', '！', '？', '\n']) {
        for refusal in [
            "不要凭经验",
            "不要根据经验",
            "不要提供经验",
            "不要补充经验",
            "不要经验建议",
            "不要补充常识",
            "不补充常识",
            "不要通用建议",
            "不需要经验建议",
            "不需要通用建议",
        ] {
            if clause.contains(refusal) {
                return true;
            }
        }
        for only in [
            "只依据",
            "仅依据",
            "只根据",
            "只能根据",
            "只能依据",
            "仅根据",
            "只基于",
            "仅基于",
            "只看",
            "只用",
            "仅使用",
            "仅限",
            "只按",
        ] {
            for (at, _) in clause.match_indices(only) {
                let before = &clause[..at];
                if ["不要", "不必", "不用", "无需", "别", "并非", "不是", "不能"]
                    .iter()
                    .any(|s| before.ends_with(s))
                {
                    continue;
                }
                let after: String = clause[at + only.len()..].chars().take(24).collect();
                if ["会议", "原文", "记录", "逐字稿", "转写"]
                    .iter()
                    .any(|s| after.contains(s))
                {
                    return true;
                }
            }
        }
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_meeting_only_requests_block_experience_but_negated_restrictions_do_not() {
        for q in [
            "请只依据本次会议回答",
            "仅根据会议原文，不要补充常识",
            "只用刚才的转写分析",
            "不要提供经验建议",
            "仅限本次会议内容",
            "不要补充经验",
            "只能依据本次会议的记录回答",
        ] {
            assert!(meeting_only(q), "{q}");
        }
        for q in [
            "按你的经验怎么做？",
            "不要只看会议，也结合经验",
            "不必仅根据原文，可以提出建议",
            "这次会议可以怎么组织？",
        ] {
            assert!(!meeting_only(q), "{q}");
        }
    }
    #[test]
    fn introductions_match_real_coverage_and_do_not_repeat_or_deny_existing_facts() {
        let partial = AdviceScope {
            meeting: true,
            complete: false,
            meeting_only: false,
            validation_failed: false,
        };
        let text = partial.introduce("我建议先做小范围测试。", false);
        assert!(text.starts_with("目前我还没找到"));
        assert_eq!(partial.introduce(&text, false), text);
        let full = AdviceScope {
            complete: true,
            ..partial
        };
        assert!(full
            .introduce("我建议先测试。", false)
            .starts_with("前面我们还没讨论"));
        assert!(partial
            .introduce(&full.introduce("我建议先测试。", false), false)
            .starts_with("目前我还没找到"));
        assert!(partial
            .introduce("我建议先测试。", true)
            .starts_with("这部分"));
        assert_eq!(
            AdviceScope::default().introduce("我建议先测试。", false),
            "根据我的知识与经验，我建议先测试。"
        );
    }
}
