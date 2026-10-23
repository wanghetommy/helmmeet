//! External links may seed a preparation form; they never authorize capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingDraft {
    pub title: Option<String>,
    pub meeting_type: String,
}
pub fn new_meeting(fields: &[(String, String)]) -> Result<MeetingDraft, &'static str> {
    let (mut action, mut title, mut kind) = (None, None, None);
    for (key, value) in fields {
        let field = match key.as_str() {
            "action" => &mut action,
            "title" => &mut title,
            "type" => &mut kind,
            _ => return Err("不支持的打开参数"),
        };
        if field.replace(value.as_str()).is_some() {
            return Err("打开参数不能重复");
        }
    }
    if action != Some("new-meeting") {
        return Err("不支持的打开操作");
    }
    let title = title.map(str::trim).map(str::to_owned);
    if title
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.chars().count() > 160)
        || fields
            .iter()
            .any(|(_, v)| v.chars().any(|c| c.is_control() || c == '\u{fffd}'))
    {
        return Err("会议名称无效或超过160字");
    }
    let kind = kind.unwrap_or("general");
    if !matches!(
        kind,
        "general" | "weekly" | "review" | "decision" | "retro" | "brainstorm"
    ) {
        return Err("不支持的会议类型");
    }
    Ok(MeetingDraft {
        title,
        meeting_type: kind.into(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    #[test]
    fn links_seed_only_a_valid_draft() {
        let draft = new_meeting(&fields(&[("action", "new-meeting")])).unwrap();
        assert_eq!(
            draft,
            MeetingDraft {
                title: None,
                meeting_type: "general".into()
            }
        );
        let draft = new_meeting(&fields(&[
            ("action", "new-meeting"),
            ("title", " 周会 "),
            ("type", "weekly"),
        ]))
        .unwrap();
        assert_eq!(draft.title.as_deref(), Some("周会"));
        assert_eq!(draft.meeting_type, "weekly");
        assert!(new_meeting(&fields(&[
            ("action", "new-meeting"),
            ("title", &"会".repeat(160))
        ]))
        .is_ok());
        for extra in [
            [("title", " ")],
            [("title", "a\nb")],
            [("type", "other")],
            [("record", "true")],
            [("action", "new-meeting")],
        ] {
            let mut pairs = fields(&[("action", "new-meeting")]);
            pairs.extend(fields(&extra));
            assert!(new_meeting(&pairs).is_err());
        }
        assert!(new_meeting(&fields(&[("action", "record")])).is_err());
        assert!(new_meeting(&fields(&[
            ("action", "new-meeting"),
            ("title", &"会".repeat(161))
        ]))
        .is_err());
    }
}
