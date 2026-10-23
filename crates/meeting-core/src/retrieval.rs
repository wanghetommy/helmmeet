//! Small local lexical retrieval. No embeddings, network or speaker inference.
use std::collections::{BTreeMap, BTreeSet};
pub fn terms(text: &str) -> Vec<String> {
    let mut words = BTreeSet::new();
    let mut ascii = String::new();
    let mut previous = None;
    for c in text.to_lowercase().chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() {
            ascii.push(c);
            previous = None;
        } else {
            if !ascii.is_empty() {
                words.insert(std::mem::take(&mut ascii));
            }
            if c.is_alphanumeric() {
                if let Some(p) = previous {
                    words.insert(format!("{p}{c}"));
                }
                previous = Some(c);
            } else {
                previous = None;
            }
        }
    }
    words.into_iter().collect()
}
pub fn expand(question: &str, aliases: &[(String, Vec<String>)]) -> String {
    let mut query = question.to_owned();
    for (word, extra) in [
        ("卡点", " 阻碍 阻塞 依赖 风险"),
        ("目标", " 目的 指标 验收"),
        ("决定", " 决策 确定 最终"),
        ("待办", " 行动 任务 负责人"),
        ("上线", " 发布 交付 排期"),
    ] {
        if question.contains(word) {
            query.push_str(extra);
        }
    }
    for (canonical, variants) in aliases {
        if question.contains(canonical)
            || variants
                .iter()
                .any(|a| !a.is_empty() && question.contains(a))
        {
            query.push(' ');
            query.push_str(canonical);
            for alias in variants {
                query.push(' ');
                query.push_str(alias);
            }
        }
    }
    query
}
/// BM25-style ranking over unique lexical terms. Stable ties retain chronology.
pub fn rank(query: &str, documents: &[&str]) -> Vec<usize> {
    let needles = terms(query);
    if needles.is_empty() {
        return vec![];
    }
    let docs = documents.iter().map(|s| terms(s)).collect::<Vec<_>>();
    let mean =
        (docs.iter().map(Vec::len).sum::<usize>() as f64 / docs.len().max(1) as f64).max(1.0);
    let mut frequencies = BTreeMap::new();
    for needle in &needles {
        frequencies.insert(
            needle,
            docs.iter()
                .filter(|d| d.binary_search(needle).is_ok())
                .count(),
        );
    }
    let mut scores = docs
        .iter()
        .enumerate()
        .filter_map(|(index, doc)| {
            let mut score = 0.0;
            for needle in &needles {
                if doc.binary_search(needle).is_ok() {
                    let df = frequencies[needle] as f64;
                    let idf = (1.0 + (docs.len() as f64 - df + 0.5) / (df + 0.5)).ln();
                    score += idf * 2.2 / (1.0 + 1.2 * (0.25 + 0.75 * doc.len() as f64 / mean));
                }
            }
            (score > 0.0).then_some((index, score))
        })
        .collect::<Vec<_>>();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scores.into_iter().map(|(i, _)| i).collect()
}
pub fn whole_review(question: &str) -> bool {
    let global = [
        "全部",
        "所有",
        "有没有遗漏",
        "是否遗漏",
        "哪些目标",
        "哪些卡点",
        "有哪些卡点",
    ]
    .iter()
    .any(|s| question.contains(s));
    let topic = [
        "检查", "审阅", "复盘", "review", "目标", "决定", "行动", "风险", "卡点", "遗漏",
    ]
    .iter()
    .any(|s| question.contains(s));
    global && topic
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_specific_terms_find_evidence_without_method_noise() {
        let query = expand("王贺的上线卡点", &[("王鹤".into(), vec!["王贺".into()])]);
        let docs = [
            "会议管理方法：先澄清目标",
            "王鹤：发布依赖还没解决，不能上线",
            "我们讨论了目标",
        ];
        assert_eq!(rank(&query, &docs).first(), Some(&1));
        assert!(rank("不存在词汇", &docs).is_empty());
        assert!(whole_review("检查整场全部决定和行动"));
        assert!(!whole_review("王鹤刚才说的上线时间是什么"));
        assert!(terms("API v2 目标").contains(&"api".into()));
    }
}
