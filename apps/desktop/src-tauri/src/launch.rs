use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub id: String,
    pub title: Option<String>,
    pub meeting_type: String,
}
#[derive(Default)]
struct Inbox {
    requests: VecDeque<Request>,
    recent: Option<(meeting_core::launch::MeetingDraft, Instant)>,
}
#[derive(Default)]
pub struct LaunchInbox(Mutex<Inbox>);

fn parse(url: &Url) -> Result<Option<meeting_core::launch::MeetingDraft>, &'static str> {
    if url.as_str().len() > 4096
        || url.scheme() != "helmmeet"
        || url.host_str() != Some("open")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !matches!(url.path(), "" | "/")
        || url.fragment().is_some()
    {
        return Err("无法识别的HelmMeet链接");
    }
    let query = url.query().unwrap_or("");
    let bytes = query.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err("链接编码无效");
        }
    }
    if query.is_empty() {
        return Ok(None);
    }
    meeting_core::launch::new_meeting(&url.query_pairs().into_owned().collect::<Vec<_>>()).map(Some)
}
impl LaunchInbox {
    pub fn receive(&self, url: &Url) -> Result<bool, String> {
        let Some(draft) = parse(url)? else {
            return Ok(false);
        };
        let mut inbox = self.0.lock().map_err(|_| "打开请求队列异常")?;
        if inbox
            .recent
            .as_ref()
            .is_some_and(|(last, at)| last == &draft && at.elapsed() < Duration::from_secs(2))
        {
            return Ok(false);
        }
        if inbox.requests.len() >= 8 {
            return Err("待处理打开请求过多，请先处理已有请求".into());
        }
        inbox.requests.push_back(Request {
            id: uuid::Uuid::new_v4().to_string(),
            title: draft.title.clone(),
            meeting_type: draft.meeting_type.clone(),
        });
        inbox.recent = Some((draft, Instant::now()));
        Ok(true)
    }
    pub fn read(&self, acknowledge: Option<&str>) -> Result<Option<Request>, String> {
        let mut inbox = self.0.lock().map_err(|_| "打开请求队列异常")?;
        if acknowledge.is_some_and(|id| inbox.requests.front().is_some_and(|r| r.id == id)) {
            inbox.requests.pop_front();
        }
        Ok(inbox.requests.front().cloned())
    }
}
#[tauri::command]
pub fn launch_request(
    state: tauri::State<'_, LaunchInbox>,
    acknowledge: Option<String>,
) -> Result<Option<Request>, String> {
    state.read(acknowledge.as_deref())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_url_and_decodes_names_once() {
        let draft = parse(
            &Url::parse(
                "helmmeet://open?action=new-meeting&title=%E5%91%A8%E4%BC%9A%20A%26B&type=weekly",
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(draft.title.as_deref(), Some("周会 A&B"));
        assert_eq!(draft.meeting_type, "weekly");
        assert!(parse(&Url::parse("helmmeet://open").unwrap())
            .unwrap()
            .is_none());
        for link in [
            "https://open?action=new-meeting",
            "helmmeet://other?action=new-meeting",
            "helmmeet://user@open?action=new-meeting",
            "helmmeet://open:80?action=new-meeting",
            "helmmeet://open/other?action=new-meeting",
            "helmmeet://open?action=new-meeting#record",
            "helmmeet://open?action=new-meeting&title=%xx",
            "helmmeet://open?action=new-meeting&title=%FF",
            "helmmeet://open?action=new-meeting&title=%0Ahi",
            "helmmeet://open?action=new-meeting&auto_start=true",
            "helmmeet://open?action=new-meeting&action=record",
        ] {
            assert!(parse(&Url::parse(link).unwrap()).is_err(), "{link}");
        }
    }
    #[test]
    fn cold_launch_survives_reads_until_ack_and_warm_links_remain_ordered() {
        let inbox = LaunchInbox::default();
        let first = Url::parse("helmmeet://open?action=new-meeting&title=first").unwrap();
        assert!(inbox.receive(&first).unwrap());
        let a = inbox.read(None).unwrap().unwrap();
        assert_eq!(inbox.read(None).unwrap().unwrap().id, a.id);
        assert!(!inbox.receive(&first).unwrap());
        assert_eq!(inbox.read(Some("wrong")).unwrap().unwrap().id, a.id);
        assert!(inbox
            .receive(&Url::parse("helmmeet://open?action=new-meeting&title=second").unwrap())
            .unwrap());
        let b = inbox.read(Some(&a.id)).unwrap().unwrap();
        assert_eq!(b.title.as_deref(), Some("second"));
        assert_ne!(a.id, b.id);
        assert_eq!(inbox.read(Some(&a.id)).unwrap().unwrap().id, b.id);
        assert!(inbox.read(Some(&b.id)).unwrap().is_none());
        for i in 0..8 {
            inbox
                .receive(
                    &Url::parse(&format!("helmmeet://open?action=new-meeting&title={i}")).unwrap(),
                )
                .unwrap();
        }
        assert!(inbox
            .receive(&Url::parse("helmmeet://open?action=new-meeting&title=overflow").unwrap())
            .is_err());
    }
}
