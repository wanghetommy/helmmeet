//! Lossless reading breaks inside a speech record. Evidence and record boundaries stay intact.
use unicode_segmentation::UnicodeSegmentation;

const MIN: usize = 80;
const TARGET: usize = 120;
const SOFT: usize = 200;
const LIMIT: usize = 240;

fn closing(s: &str) -> bool {
    matches!(
        s,
        "”" | "’" | "」" | "』" | "）" | "》" | "】" | "\"" | "'" | ")" | "]"
    )
}
fn token(s: &str) -> bool {
    s.len() == 1 && (s.as_bytes()[0].is_ascii_alphanumeric() || "._:/?&=%@+-#,".contains(s))
}
fn safe_cut(units: &[(usize, &str)], mut cut: usize) -> usize {
    if cut < units.len() && token(units[cut - 1].1) && token(units[cut].1) {
        let mut start = cut;
        while start > 0 && token(units[start - 1].1) {
            start -= 1;
        }
        if start >= MIN {
            return start;
        }
        // A single unusually long word/URL is kept intact rather than corrupted.
        while cut < units.len() && token(units[cut].1) {
            cut += 1;
        }
    }
    cut
}

/// Concatenating the returned slices always reproduces the original text byte for byte.
/// Counts visible graphemes, so combining marks and joined emoji are never cut apart.
pub fn blocks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let units: Vec<_> = rest.grapheme_indices(true).collect();
        let mut sentence = None;
        let mut clause = None;
        let mut space = None;
        let mut cut = units.len();
        for (i, &(_, g)) in units.iter().enumerate() {
            let count = i + 1;
            if g.contains('\n') || g.contains('\r') {
                cut = count;
                while cut < units.len() && units[cut].1.chars().all(char::is_whitespace) {
                    cut += 1;
                }
                break;
            }
            let numeric = i > 0
                && i + 1 < units.len()
                && units[i - 1].1.chars().all(|c| c.is_ascii_digit())
                && units[i + 1].1.chars().all(|c| c.is_ascii_digit());
            let query = g == "?"
                && i > 0
                && i + 1 < units.len()
                && token(units[i - 1].1)
                && token(units[i + 1].1);
            let end = matches!(g, "。" | "！" | "？" | "；" | "!" | ";")
                || g == "?" && !query
                || g == "."
                    && !numeric
                    && (i + 1 == units.len()
                        || units[i + 1].1.chars().all(char::is_whitespace)
                        || closing(units[i + 1].1));
            if end {
                let mut boundary = count;
                while boundary < units.len()
                    && (closing(units[boundary].1)
                        || matches!(units[boundary].1, "！" | "？" | "!" | "?" | "。" | "."))
                {
                    boundary += 1;
                }
                if count >= MIN {
                    sentence = Some(boundary);
                }
                if count >= TARGET {
                    cut = boundary;
                    break;
                }
            }
            if count >= MIN {
                if matches!(g, "，" | "、" | "：" | "," | ":") && !numeric {
                    clause = Some(count);
                }
                if g.chars().all(char::is_whitespace) {
                    space = Some(count);
                }
            }
            if count >= SOFT {
                if let Some(boundary) = sentence.or(clause).or(space) {
                    cut = boundary;
                    break;
                }
            }
            if count >= LIMIT {
                cut = safe_cut(&units, count);
                break;
            }
        }
        let byte = units.get(cut).map_or(rest.len(), |u| u.0);
        out.push(&rest[..byte]);
        rest = &rest[byte..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lengths(text: &str) -> Vec<usize> {
        blocks(text)
            .iter()
            .map(|s| s.graphemes(true).count())
            .collect()
    }
    #[test]
    fn sentence_breaks_do_not_need_silence_and_short_answers_stay_intact() {
        let sentence = format!("{}。", "文".repeat(59));
        let text = sentence.repeat(6);
        assert_eq!(lengths(&text), [120, 120, 120]);
        for text in ["", "同意。", "先不做", "嗯", "  ", "第一行\n\n第二行"] {
            assert_eq!(blocks(text).concat(), text);
        }
        assert_eq!(blocks("同意。"), ["同意。"]);
    }
    #[test]
    fn long_sentences_use_clauses_and_unpunctuated_text_has_a_fallback() {
        let text = format!(
            "{}，{}，{}",
            "甲".repeat(99),
            "乙".repeat(89),
            "丙".repeat(260)
        );
        assert_eq!(lengths(&text), [190, 240, 20]);
        assert_eq!(blocks(&text).concat(), text);
        assert_eq!(lengths(&"无".repeat(720)), [240, 240, 240]);
    }
    #[test]
    fn quotes_newlines_numbers_and_graphemes_are_preserved() {
        let quoted = format!("“{}。”后续。", "甲".repeat(119));
        assert!(blocks(&quoted)[0].ends_with("。”"));
        let text = format!(
            "{}👩🏽‍💻e\u{301}{}\r\n\r\n金额1,234.56，网址https://example.com/test。",
            "甲".repeat(239),
            "乙".repeat(245)
        );
        assert_eq!(blocks(&text).concat(), text);
        assert!(blocks(&text)
            .iter()
            .all(|s| !s.starts_with('\u{301}') && !s.starts_with('\u{200d}')));
        let number = format!("{}1234567890后续", "甲".repeat(235));
        assert_eq!(lengths(&number)[0], 235);
        assert_eq!(blocks(&number).concat(), number);
        let long_url = format!("https://example.com/{}?key=value", "a".repeat(220));
        assert_eq!(blocks(&long_url), [long_url.as_str()]);
    }
    #[test]
    fn every_input_byte_survives_deterministic_repeated_projection() {
        for n in 0..520 {
            let text = format!(
                "{}\"🙂\"，{}。",
                "字".repeat(n),
                "word 42.5 ".repeat(n % 11)
            );
            let result = blocks(&text);
            assert_eq!(result.concat(), text);
            assert!(result.iter().all(|s| !s.is_empty()));
            assert_eq!(blocks(&text), result);
        }
    }
}
