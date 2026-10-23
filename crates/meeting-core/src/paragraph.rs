//! Group stable ASR evidence into readable speech paragraphs. No speaker guesses.
#[derive(Clone, Debug)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub text: String,
    pub speaker: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Pause {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug)]
pub struct Group {
    pub first: usize,
    pub end: usize,
    pub reason: &'static str,
    pub open: bool,
}
fn sentence(text: &str) -> bool {
    text.trim_end()
        .ends_with(['。', '！', '？', '.', '!', '?', '；', ';'])
}
fn meaningful(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}
fn filler(text: &str) -> bool {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !"，。！？、,.!?；;".contains(*c))
        .collect();
    matches!(
        compact.as_str(),
        "嗯" | "嗯嗯" | "啊" | "哦" | "噢" | "好" | "那嗯" | "对" | "是"
    )
}
pub fn project(
    segments: &[Segment],
    pauses: &[Pause],
    hard: &[u64],
    legacy: bool,
    active: bool,
) -> Vec<Group> {
    let mut sorted = pauses.to_vec();
    sorted.sort_unstable_by_key(|p| p.start);
    let pauses = sorted.as_slice();
    let mut out = vec![];
    if segments.is_empty() {
        return out;
    }
    let mut first = 0;
    let mut chars = meaningful(&segments[0].text);
    for i in 1..segments.len() {
        let prev = &segments[i - 1];
        let next = &segments[i];
        let elapsed = prev.end.saturating_sub(segments[first].start);
        let gap = next.start.saturating_sub(prev.end);
        let pause = pauses[pauses.partition_point(|p| p.start < prev.start)..]
            .iter()
            .take_while(|p| p.start <= next.start)
            .filter(|p| prev.end <= p.end + 750)
            .map(|p| p.end.saturating_sub(p.start))
            .max()
            .unwrap_or(if legacy { gap } else { 0 });
        let speaker =
            prev.speaker.is_some() && next.speaker.is_some() && prev.speaker != next.speaker;
        let short_filler = chars <= 8
            && filler(
                &segments[first..i]
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>(),
            );
        let reason = if short_filler {
            None
        } else if speaker {
            Some("speaker")
        } else if hard.iter().any(|t| *t > prev.start && *t <= next.start) {
            Some("interruption")
        } else if pause >= 3000 {
            Some("pause")
        } else if pause >= 1500 && sentence(&prev.text) && (chars >= 50 || elapsed >= 15000) {
            Some("soft_pause")
        } else if elapsed >= 60000
            || chars >= 400
            || ((elapsed >= 45000 || chars >= 250) && sentence(&prev.text))
        {
            Some("continuation")
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(Group {
                first,
                end: i,
                reason,
                open: false,
            });
            first = i;
            chars = 0;
        }
        chars += meaningful(&next.text);
    }
    let last = segments.last().unwrap();
    let tail = pauses
        .iter()
        .filter(|p| p.start >= last.start && last.end <= p.end + 750)
        .map(|p| p.end.saturating_sub(p.start))
        .max()
        .unwrap_or(0);
    let close = tail >= 3000
        || tail >= 1500
            && sentence(&last.text)
            && (chars >= 50 || last.end.saturating_sub(segments[first].start) >= 15000);
    let last_text = segments[first..]
        .iter()
        .map(|s| s.text.as_str())
        .collect::<String>();
    let close = close && !(chars <= 8 && filler(&last_text));
    out.push(Group {
        first,
        end: segments.len(),
        reason: if close {
            "pause"
        } else if active {
            "growing"
        } else {
            "ended"
        },
        open: active && !close,
    });
    if out.len() > 1 {
        let last_group = out.last().unwrap();
        let tail_text = segments[last_group.first..last_group.end]
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>();
        if meaningful(&tail_text) <= 8 && filler(&tail_text) {
            let tail_end = last_group.end;
            out.pop();
            if let Some(previous) = out.last_mut() {
                previous.end = tail_end;
                previous.open = false;
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    fn s(a: u64, b: u64, t: &str) -> Segment {
        Segment {
            start: a,
            end: b,
            text: t.into(),
            speaker: None,
        }
    }
    #[test]
    fn windows_and_punctuation_do_not_force_paragraphs() {
        let rows = (0..12)
            .map(|n| s(n * 2000, n * 2000 + 1700, "这是连续发言。"))
            .collect::<Vec<_>>();
        assert_eq!(project(&rows, &[], &[], false, true).len(), 1);
        let mut rows = rows;
        rows.push(s(26000, 28000, "继续说。"));
        assert_eq!(
            project(
                &rows,
                &[Pause {
                    start: 23700,
                    end: 26000
                }],
                &[],
                false,
                true
            )
            .len(),
            2
        );
    }
    #[test]
    fn short_thoughts_continue_until_clear_pause_and_unknown_is_not_silence() {
        let rows = vec![s(0, 1000, "我认为。"), s(3000, 4000, "还要考虑。")];
        assert_eq!(
            project(
                &rows,
                &[Pause {
                    start: 1000,
                    end: 3000
                }],
                &[],
                false,
                true
            )
            .len(),
            1
        );
        let rows = vec![s(0, 1000, "同意。"), s(4500, 5500, "下一项。")];
        assert_eq!(
            project(
                &rows,
                &[Pause {
                    start: 1000,
                    end: 4500
                }],
                &[],
                false,
                true
            )
            .len(),
            2
        );
        assert_eq!(project(&rows, &[], &[], false, true).len(), 1);
        assert_eq!(project(&rows, &[], &[], true, false).len(), 2);
        assert_eq!(project(&rows, &[], &[2500], false, true).len(), 2);
    }
    #[test]
    fn long_turns_and_confirmed_speaker_changes_are_boundaries() {
        let mut rows = vec![s(0, 61000, "长发言"), s(61000, 63000, "续段")];
        assert_eq!(
            project(&rows, &[], &[], false, true)[0].reason,
            "continuation"
        );
        rows[0].end = 1000;
        rows[0].speaker = Some("1".into());
        rows[1].speaker = Some("2".into());
        assert_eq!(project(&rows, &[], &[], false, true)[0].reason, "speaker");
    }

    #[test]
    fn filler_fragments_join_the_natural_paragraph() {
        let rows = vec![
            s(0, 1000, "嗯"),
            s(4500, 5500, "我们先看本周的投放数据和转化情况。"),
            s(9000, 10000, "嗯"),
        ];
        let groups = project(
            &rows,
            &[
                Pause {
                    start: 1000,
                    end: 4500,
                },
                Pause {
                    start: 5500,
                    end: 9000,
                },
            ],
            &[],
            true,
            false,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].first, 0);
        assert_eq!(groups[0].end, 3);
    }
}
