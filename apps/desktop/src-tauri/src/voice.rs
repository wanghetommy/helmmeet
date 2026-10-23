//! Meeting-only voice adapter. Capture is owned exclusively by audio.rs.
use crate::{
    assistant,
    db::{err, id, Runtime},
    lifecycle::Worker,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr, CString},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
extern "C" {
    fn hm_kws_create(directory: *const c_char, tokens: *const c_char, silence: f32) -> *mut c_void;
    fn hm_kws_destroy(handle: *mut c_void);
    fn hm_kws_reset(handle: *mut c_void);
    fn hm_kws_accept(handle: *mut c_void, pcm: *const f32, count: i32, kws: i32) -> i32;
    fn hm_voice_pinyin(text: *const c_char, output: *mut c_char, capacity: i32) -> i32;
    fn hm_voice_speak(text: *const c_char) -> i32;
    fn hm_voice_play_pcm(input: *const f32, count: i32) -> i32;
    fn hm_voice_stop();
    fn hm_voice_state() -> i32;
    #[cfg(feature = "audio-processing")]
    fn hm_voice_reference(host: u64, output: *mut f32, count: i32) -> i32;
}
const PRE_ROLL_SAMPLES: usize = 16000 * 2;
const WAKE_RESPONSE_TEXT: &str = "我在";
const WAKE_RESPONSE_WAV: &[u8] =
    include_bytes!("../../../../native/voice-assistant/assets/wake-acknowledgement.wav");
fn wake_pcm() -> Result<&'static [f32], &'static str> {
    static PCM: OnceLock<Result<Vec<f32>, String>> = OnceLock::new();
    PCM.get_or_init(|| crate::audio::read_wav_bytes(WAKE_RESPONSE_WAV))
        .as_deref()
        .map_err(|_| "唤醒提示音不可用")
}
fn acknowledge() -> bool {
    wake_pcm().is_ok_and(|pcm| unsafe { hm_voice_play_pcm(pcm.as_ptr(), pcm.len() as i32) } == 0)
}
fn append_pre_roll(rolling: &mut VecDeque<f32>, pcm: &[f32]) {
    rolling.extend(pcm.iter().copied());
    while rolling.len() > PRE_ROLL_SAMPLES {
        rolling.pop_front();
    }
}
pub struct Frame {
    meeting: String,
    host: u64,
    pcm: Vec<f32>,
    epoch: u64,
}
pub struct Question {
    pub meeting: String,
    pub pcm: Vec<f32>,
    pub utterance: String,
    pub epoch: u64,
    pub wake: String,
    pub capture_epoch: String,
}
pub struct Hub {
    feed: Mutex<Option<SyncSender<Frame>>>,
    accepting: AtomicBool,
    input_gap: AtomicBool,
    pub questions: Mutex<VecDeque<Question>>,
    state: Mutex<Value>,
    epoch: AtomicU64,
    command: AtomicU8,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            feed: Mutex::new(None),
            accepting: AtomicBool::new(false),
            input_gap: AtomicBool::new(false),
            questions: Mutex::new(VecDeque::new()),
            state: Mutex::new(json!({"state":"off","message":"仅会议录制且麦克风开启时提供语音"})),
            epoch: AtomicU64::new(0),
            command: AtomicU8::new(0),
        }
    }
}
pub fn ensure_meeting(rt: &Runtime, mid: &str) -> Result<(), String> {
    rt.ensure_running()?;
    let allowed:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM meetings m JOIN recording_sources s ON s.meeting_id=m.id WHERE m.id=? AND m.state='recording' AND s.source='microphone' AND s.state='active')",[mid],|r|r.get(0)).map_err(err)?;
    if !allowed {
        return Err("语音仅在会议录制且麦克风开启时可用".into());
    }
    Ok(())
}
// Called only after disk write, and never waits for the worker/ASR/model.
pub fn feed(rt: &Runtime, meeting: &str, host: u64, pcm: &[f32]) {
    if !rt.voice.accepting.load(Ordering::Acquire) || pcm.len() > 8192 {
        return;
    }
    if let Ok(g) = rt.voice.feed.try_lock() {
        if let Some(tx) = g.as_ref() {
            if tx
                .try_send(Frame {
                    meeting: meeting.into(),
                    host,
                    pcm: pcm.to_vec(),
                    epoch: rt.voice.epoch.load(Ordering::Acquire),
                })
                .is_err()
            {
                // A slow consumer must not cancel a TTS response or a question
                // already submitted to ASR. Only the unfinished input is lost.
                rt.voice.input_gap.store(true, Ordering::Release);
            }
        }
    }
}
fn state(rt: &Runtime, v: Value) {
    if let Ok(mut s) = rt.voice.state.lock() {
        *s = v
    }
}
pub fn status(rt: &Runtime) -> Result<Value, String> {
    Ok(rt.voice.state.lock().map_err(|_| "语音状态锁异常")?.clone())
}
pub fn command(rt: &Runtime, action: &str) -> Result<Value, String> {
    let code = match action {
        "listen" => 1,
        "submit" => 2,
        "cancel" => 3,
        "stop_speaking" => 4,
        "test_wake" => 5,
        _ => return Err("语音操作无效".into()),
    };
    if code <= 2 || code == 5 {
        let s = status(rt)?;
        if !rt.voice.accepting.load(Ordering::Acquire) {
            return Err("语音模型尚未就绪，请检查语音状态".into());
        }
        let phase = s["state"].as_str().unwrap_or("off");
        if (code == 1 && !["standby", "error", "speaking"].contains(&phase))
            || (code == 2 && phase != "listening")
            || (code == 5 && !["standby", "error"].contains(&phase))
        {
            return Err("当前语音回合仍在进行，请先等待或取消".into());
        }
        ensure_meeting(rt, s["meeting_id"].as_str().ok_or("当前无可用会议麦克风")?)?;
        if !assistant::settings(rt)?["voice_enabled"]
            .as_bool()
            .unwrap_or(false)
        {
            return Err("请先在全局设置启用会议中语音".into());
        }
    }
    rt.voice.command.store(code, Ordering::Release);
    Ok(json!({"accepted":true}))
}
pub fn invalidate(rt: &Runtime) {
    rt.voice.epoch.fetch_add(1, Ordering::AcqRel);
}
pub fn take_question(rt: &Runtime) -> Option<Question> {
    rt.voice.questions.lock().ok()?.pop_front()
}
pub fn valid_question(rt: &Runtime, q: &Question) -> bool {
    rt.voice.epoch.load(Ordering::Acquire) == q.epoch
        && ensure_meeting(rt, &q.meeting).is_ok()
        && crate::product::capture_epoch(rt, &q.meeting)
            .ok()
            .as_deref()
            == Some(q.capture_epoch.as_str())
}
pub fn question_failed(rt: &Runtime, q: &Question, message: &str) {
    if valid_question(rt, q) {
        state(
            rt,
            json!({"state":"error","meeting_id":q.meeting,"message":message}),
        )
    }
}
pub fn recognized(rt: Arc<Runtime>, q: Question, text: String) {
    if !valid_question(&rt, &q) {
        return;
    }
    let text = strip_wake(&text, &q.wake);
    let response_only = !q.wake.is_empty()
        && text.trim_matches(|c: char| !c.is_alphanumeric()) == WAKE_RESPONSE_TEXT;
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 2 || response_only {
        question_failed(&rt, &q, "没有识别到问题，请重新唤醒或输入文字");
        return;
    }
    let result = (|| {
        let c = assistant::open(&rt, Some(q.meeting.clone()), false)?;
        assistant::submit(
            rt.clone(),
            c["id"].as_str().ok_or("会话无效")?,
            &text,
            "voice",
            Some(q.utterance.clone()),
        )
    })();
    match result {
        Ok(v) => state(
            &rt,
            json!({"state":"thinking","meeting_id":q.meeting,"turn_id":v["turn_id"],"epoch":q.epoch,"question":text}),
        ),
        Err(e) => question_failed(&rt, &q, &e),
    }
}
fn strip_wake(text: &str, wake: &str) -> String {
    let t = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let t = if !wake.is_empty() {
        if let Some(rest) = t.strip_prefix(wake) {
            rest
        } else if let Some((at, _)) = t.match_indices(wake).find(|(at, _)| {
            let before = t[..*at].chars().last();
            let after = t[*at + wake.len()..].chars().next();
            before.is_some_and(|c| "。！？!?；;\n".contains(c))
                || after.is_some_and(|c| "，,：:。！？!?".contains(c))
        }) {
            &t[at + wake.len()..]
        } else {
            t
        }
    } else {
        t
    };
    let t = t.trim_start_matches(|c: char| !c.is_alphanumeric());
    // Legacy TTS acknowledgement only. Never strip “我在”: it can be part of
    // the user's question (“我在想…”); echo is handled using playback PCM.
    let t = t
        .strip_prefix("小舵在")
        .unwrap_or(t)
        .trim_start_matches(|c: char| !c.is_alphanumeric());
    t.trim().into()
}

pub(crate) fn model_dir() -> PathBuf {
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()?
                .parent()
                .map(|p| p.join("Resources/voice-models"))
        })
        .filter(|p| p.join("tokens.txt").exists())
    {
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/bundle")
}
pub fn keyword(word: &str) -> Result<String, String> {
    // Limit the first shipping tokenizer to Chinese. Unsupported words fail at save
    // time, instead of accepting a setting that can never wake the model.
    if !word.chars().all(|c| ('\u{3400}'..='\u{9fff}').contains(&c)) {
        return Err("本版唤醒词请使用2–12个中文汉字".into());
    }
    let c = CString::new(word).map_err(err)?;
    let mut out = [0i8; 512];
    if unsafe { hm_voice_pinyin(c.as_ptr(), out.as_mut_ptr(), 512) } != 0 {
        return Err("唤醒词读音转换失败".into());
    }
    let latin = unsafe { CStr::from_ptr(out.as_ptr()) }
        .to_str()
        .map_err(err)?;
    let vocab = std::fs::read_to_string(model_dir().join("tokens.txt"))
        .map_err(|_| "唤醒模型资源未准备")?;
    let tokens = vocab
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect::<Vec<_>>();
    let mut result = vec![];
    for syllable in latin.split_whitespace() {
        // ICU transliteration may use combining tone marks; normalize through native
        // precomposed mapping above before checking the model vocabulary.
        let initial = [
            "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x",
            "r", "z", "c", "s", "y", "w",
        ]
        .into_iter()
        .find(|i| syllable.starts_with(i));
        let parts = if let Some(i) = initial {
            vec![i, &syllable[i.len()..]]
        } else {
            vec![syllable]
        };
        for part in parts {
            if !part.is_empty() {
                if !tokens.contains(&part) {
                    return Err(format!("唤醒词包含暂不支持的读音：{syllable}，请换一个词"));
                }
                result.push(part)
            }
        }
    }
    if result.len() < 2 {
        return Err("唤醒词读音无效".into());
    }
    Ok(format!("{} @{word}", result.join(" ")))
}
struct Detector(*mut c_void);
impl Detector {
    fn new(word: &str, silence: u64) -> Result<Self, String> {
        let d = CString::new(model_dir().to_string_lossy().as_bytes()).map_err(err)?;
        let k = CString::new(keyword(word)?).map_err(err)?;
        let p = unsafe { hm_kws_create(d.as_ptr(), k.as_ptr(), silence as f32 / 1000.) };
        if p.is_null() {
            return Err("离线唤醒/VAD模型加载失败".into());
        }
        Ok(Self(p))
    }
    fn reset(&mut self) {
        unsafe { hm_kws_reset(self.0) }
    }
    fn accept(&mut self, pcm: &[f32], kws: bool) -> i32 {
        unsafe { hm_kws_accept(self.0, pcm.as_ptr(), pcm.len() as i32, i32::from(kws)) }
    }
}
impl Drop for Detector {
    fn drop(&mut self) {
        unsafe { hm_kws_destroy(self.0) }
    }
}
struct Listening {
    pcm: Vec<f32>,
    epoch: u64,
    utterance: String,
    had_speech: bool,
    pending_end: bool,
    after_wake: usize,
    empty_since: Option<Instant>,
    manual: bool,
    capture_epoch: String,
}
impl Listening {
    fn empty_timeout(&mut self, acknowledgement_active: bool, now: Instant) -> bool {
        // Listening already owns the pre-roll and all prompt-overlapping audio.
        // Only the empty-question deadline starts after actual playback ends.
        if acknowledgement_active {
            self.empty_since = None;
            return false;
        }
        let since = *self.empty_since.get_or_insert(now);
        !self.had_speech && now.saturating_duration_since(since) >= Duration::from_secs(5)
    }
    fn endpoint(&mut self, flags: i32, acknowledgement_active: bool) -> bool {
        if flags & 2 != 0 {
            self.had_speech = true;
            self.pending_end = false;
        }
        if self.had_speech && flags & 4 != 0 {
            self.pending_end = true;
        }
        // Keep an endpoint that arrives during the acknowledgement: dropping it
        // would leave a short question waiting until the 30-second limit.
        self.pending_end && !acknowledgement_active
    }
}
fn speak(text: &str) -> bool {
    CString::new(text).is_ok_and(|s| unsafe { hm_voice_speak(s.as_ptr()) } == 0)
}
fn spoken_answer_chunks(answer: &Value) -> Vec<String> {
    let text = answer["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("。");
    let chars = text.chars().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + 560).min(chars.len());
        let end = if hard_end < chars.len() {
            chars[start..hard_end]
                .iter()
                .rposition(|c| "。！？.!?；;\n".contains(*c))
                .map_or(hard_end, |offset| start + offset + 1)
        } else {
            hard_end
        };
        if end == start {
            break;
        }
        chunks.push(chars[start..end].iter().collect::<String>());
        start = end;
    }
    chunks
}
#[allow(clippy::too_many_arguments)]
fn start_speech_chunk(
    rt: &Runtime,
    mid: &str,
    turn: &str,
    text: &str,
    last_host: u64,
    playback: &mut Option<String>,
    playback_base: &mut u64,
    playback_host: &mut u64,
) -> bool {
    if !speak(text) {
        return false;
    }
    let pid = id();
    *playback_base = rt
        .detail(mid)
        .ok()
        .and_then(|d| d["duration_ms"].as_u64())
        .unwrap_or(0);
    *playback_host = last_host;
    let _ = rt.db().and_then(|db| {
        db.execute(
            "INSERT INTO assistant_playback_events(id,meeting_id,turn_id,text,start_host_ns,start_ms) VALUES (?,?,?,?,?,?)",
            params![pid, mid, turn, text, last_host, *playback_base],
        )
        .map_err(err)
    });
    *playback = Some(pid);
    true
}
fn stop_speaker() {
    unsafe { hm_voice_stop() }
}
fn finish(rt: &Runtime, mid: &str, word: &str, l: Listening) {
    if l.epoch != rt.voice.epoch.load(Ordering::Acquire) {
        return;
    }
    // A very short question may already be inside the pre-roll by the time
    // KWS fires. After the bounded empty-input window, let local ASR inspect it
    // rather than discarding it solely because VAD saw no post-wake speech.
    let has_pre_roll = !l.manual && l.pcm.len() > l.after_wake;
    if (!l.had_speech && !has_pre_roll) || l.after_wake < 3200 {
        state(
            rt,
            json!({"state":"standby","meeting_id":mid,"message":"未听到问题，请重新唤醒"}),
        );
        return;
    }
    let q = Question {
        meeting: mid.into(),
        pcm: l.pcm,
        utterance: l.utterance,
        epoch: l.epoch,
        wake: if l.manual { String::new() } else { word.into() },
        capture_epoch: l.capture_epoch,
    };
    if let Ok(mut queue) = rt.voice.questions.lock() {
        if queue.is_empty() {
            queue.push_back(q);
            state(
                rt,
                json!({"state":"transcribing","meeting_id":mid,"message":"正在识别问题"}),
            )
        }
    }
}
fn inactive_status(eligible_mid: &str, configured: bool, paused: bool, locked: bool) -> Value {
    let can_enable = !eligible_mid.is_empty() && !configured && !paused && !locked;
    json!({"state":"off", "meeting_id": if eligible_mid.is_empty() {Value::Null} else {json!(eligible_mid)},
        "can_enable":can_enable,
        "reason": if locked {"locked"} else if paused {"globally_paused"} else if !configured {"disabled"} else {"no_microphone"},
        "message":if locked {"锁屏期间语音已停止"} else if paused {"全局分析已暂停，语音未启用"} else if !configured {"语音唤醒未开启"} else {"仅会议录制且麦克风开启时提供语音"}})
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    Worker::spawn(move |stop| {
        // Decode the bundled prompt before accepting microphone frames. There
        // is no runtime file lookup or TTS synthesis on the wake path.
        let _ = wake_pcm();
        let (tx, rx) = mpsc::sync_channel(24);
        if let Ok(mut slot) = rt.voice.feed.lock() {
            *slot = Some(tx)
        }
        let mut detector: Option<Detector> = None;
        let mut identity = String::new();
        let mut active = String::new();
        let mut listening: Option<Listening> = None;
        let mut rolling = VecDeque::<f32>::new();
        let mut last_host = 0u64;
        let mut last_frame = Instant::now();
        let mut revision = rt.voice.epoch.load(Ordering::Acquire);
        #[cfg(feature = "audio-processing")]
        let mut apm = crate::processing::Processor::with_echo(1, true).ok();
        #[cfg(feature = "audio-processing")]
        let mut dsp_frames = meeting_core::audio_processing::Frames::with_delay(0);
        #[cfg(feature = "audio-processing")]
        let mut dsp_input = 0usize;
        let mut reference_tail = 0u64;
        let mut speaking = false;
        let mut speaking_since = Instant::now();
        let mut speech_queue = VecDeque::<String>::new();
        let mut speaking_turn = String::new();
        let mut test_deadline: Option<Instant> = None;
        let mut playback: Option<String> = None;
        let mut playback_base = 0;
        let mut playback_host = 0;
        while !stop.is_stopped() {
            let prefs = match assistant::settings(&rt) {
                Ok(v) => v,
                Err(_) => {
                    stop.wait(Duration::from_millis(200));
                    continue;
                }
            };
            let configured = prefs["voice_enabled"].as_bool().unwrap_or(false);
            let paused = crate::product::globally_paused(&rt).unwrap_or(true);
            let locked = unsafe { hm_voice_state() } == -2;
            let enabled = configured && !paused && !locked;
            // Keep eligible capture separate from activation: a disabled wake switch
            // must remain visible during recording instead of silently hiding the UI.
            let eligible_mid = rt.db().ok().and_then(|db|db.query_row("SELECT m.id FROM meetings m JOIN recording_sources s ON s.meeting_id=m.id WHERE m.state='recording' AND s.source='microphone' AND s.state='active' LIMIT 1",[],|r|r.get::<_,String>(0)).ok()).unwrap_or_default();
            let mid = if enabled {
                eligible_mid.clone()
            } else {
                String::new()
            };
            let key = format!(
                "{}:{}:{}:{}:{}:{}:{}",
                eligible_mid,
                configured,
                paused,
                locked,
                mid,
                prefs["wake_word"],
                prefs["silence_ms"]
            );
            let epoch = rt.voice.epoch.load(Ordering::Acquire);
            if key != identity || epoch != revision {
                rt.voice.accepting.store(false, Ordering::Release);
                identity = key;
                active = mid.clone();
                invalidate(&rt);
                revision = rt.voice.epoch.load(Ordering::Acquire);
                listening = None;
                test_deadline = None;
                rolling.clear();
                #[cfg(feature = "audio-processing")]
                {
                    dsp_frames = meeting_core::audio_processing::Frames::with_delay(0);
                    dsp_input = 0;
                    if let Some(p) = apm.as_mut() {
                        let _ = p.reset();
                    }
                }
                detector = None;
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speaking_turn.clear();
                if let Ok(mut q) = rt.voice.questions.lock() {
                    q.clear()
                }
                if mid.is_empty() {
                    state(
                        &rt,
                        inactive_status(&eligible_mid, configured, paused, locked),
                    )
                } else {
                    match Detector::new(
                        prefs["wake_word"].as_str().unwrap_or("小舵同学"),
                        prefs["silence_ms"].as_u64().unwrap_or(1200),
                    ) {
                        Ok(d) => {
                            detector = Some(d);
                            rt.voice.accepting.store(true, Ordering::Release);
                            state(
                                &rt,
                                json!({"state":"standby","meeting_id":mid,"message":"待唤醒","wake_word":prefs["wake_word"]}),
                            )
                        }
                        Err(e) => state(&rt, json!({"state":"error","meeting_id":mid,"message":e})),
                    }
                }
            }
            if let Some(pid) = playback.as_ref() {
                if mid.is_empty() || unsafe { hm_voice_state() } <= 0 {
                    let end = playback_base + (last_host.saturating_sub(playback_host)) / 1_000_000;
                    let _=rt.db().and_then(|db|db.execute("UPDATE assistant_playback_events SET end_host_ns=?,end_ms=? WHERE id=?",params![last_host,end,pid]).map_err(err));
                    playback = None;
                }
            }
            if rt.voice.input_gap.swap(false, Ordering::AcqRel) {
                // Catch up without replaying discontinuous input to KWS/VAD.
                while rx.try_recv().is_ok() {}
                let lost_question = listening.take().is_some();
                rolling.clear();
                if let Some(d) = detector.as_mut() {
                    d.reset();
                }
                #[cfg(feature = "audio-processing")]
                {
                    dsp_frames = meeting_core::audio_processing::Frames::with_delay(0);
                    dsp_input = 0;
                    if let Some(p) = apm.as_mut() {
                        let _ = p.reset();
                    }
                }
                if lost_question {
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"message":"语音输入短暂中断，请等回应播完后重新唤醒"}),
                    );
                }
            }
            let command = rt.voice.command.swap(0, Ordering::AcqRel);
            if command == 5 && !mid.is_empty() && detector.is_some() {
                if let Some(d) = detector.as_mut() {
                    d.reset();
                }
                rolling.clear();
                test_deadline = Some(Instant::now() + Duration::from_secs(10));
                state(
                    &rt,
                    json!({"state":"testing_wake","meeting_id":mid,"message":format!("请说“{}”，本次只检查唤醒，不提交问题",prefs["wake_word"].as_str().unwrap_or("小舵同学"))}),
                );
            }
            if test_deadline.is_some_and(|end| Instant::now() >= end) {
                test_deadline = None;
                state(
                    &rt,
                    json!({"state":"standby","meeting_id":mid,"message":"本次未检测到唤醒词，可重试或点击说话"}),
                );
            }
            if command == 3 {
                speech_queue.clear();
                invalidate(&rt);
                continue;
            }
            if command == 4 {
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                if !mid.is_empty() {
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"message":"播报已停止，文字回答已保留"}),
                    )
                }
            }
            let st = status(&rt).unwrap_or(Value::Null);
            if !mid.is_empty() && st["state"] == "thinking" {
                if let Some(turn) = st["turn_id"].as_str() {
                    if let Ok((state_name, answer)) = rt.db().and_then(|db| {
                        db.query_row(
                            "SELECT state,response_json FROM assistant_turns WHERE id=?",
                            [turn],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                        )
                        .map_err(err)
                    }) {
                        if state_name == "succeeded" {
                            let answer = answer
                                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                                .unwrap_or(Value::Null);
                            speech_queue = spoken_answer_chunks(&answer).into_iter().collect();
                            speaking_turn = turn.to_owned();
                            let text = speech_queue.pop_front();
                            speaking = prefs["speak_answers"].as_bool().unwrap_or(true)
                                && text.as_deref().is_some_and(|text| {
                                    start_speech_chunk(
                                        &rt,
                                        &mid,
                                        turn,
                                        text,
                                        last_host,
                                        &mut playback,
                                        &mut playback_base,
                                        &mut playback_host,
                                    )
                                });
                            if !speaking {
                                speech_queue.clear();
                            }
                            if speaking {
                                speaking_since = Instant::now();
                            }
                            state(
                                &rt,
                                json!({"state":if speaking{"speaking"}else{"standby"},"meeting_id":mid,"turn_id":turn,"message":if speaking{"正在播报 · 可点击打断"}else{"回答已保存在聊天中"}}),
                            );
                        } else if ["failed", "cancelled"].contains(&state_name.as_str()) {
                            state(
                                &rt,
                                json!({"state":"standby","meeting_id":mid,"turn_id":turn,"message":"本次问答未完成，可查看聊天记录"}),
                            )
                        }
                    }
                }
            }
            if speaking && speaking_since.elapsed() > Duration::from_secs(45) {
                stop_speaker();
            }
            if speaking && unsafe { hm_voice_state() } <= 0 {
                if let Some(text) = speech_queue.pop_front() {
                    speaking = start_speech_chunk(
                        &rt,
                        &mid,
                        &speaking_turn,
                        &text,
                        last_host,
                        &mut playback,
                        &mut playback_base,
                        &mut playback_host,
                    );
                    if speaking {
                        speaking_since = Instant::now();
                        state(
                            &rt,
                            json!({"state":"speaking","meeting_id":mid,"turn_id":speaking_turn,"message":"正在继续播报 · 可点击打断"}),
                        );
                    }
                }
                if !speaking {
                    speech_queue.clear();
                    reference_tail = last_host + 700_000_000;
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"message":"待唤醒"}),
                    );
                    if let Some(d) = detector.as_mut() {
                        d.reset()
                    }
                }
            }
            if command == 1 && !mid.is_empty() && detector.is_some() {
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speaking_turn.clear();
                listening = Some(Listening {
                    pcm: vec![],
                    epoch: revision,
                    utterance: id(),
                    had_speech: false,
                    pending_end: false,
                    after_wake: 0,
                    empty_since: None,
                    manual: true,
                    capture_epoch: crate::product::capture_epoch(&rt, &mid).unwrap_or_default(),
                });
                rolling.clear();
                if let Some(d) = detector.as_mut() {
                    d.reset();
                }
                state(
                    &rt,
                    json!({"state":"listening","meeting_id":mid,"message":"请说话，停顿后自动提交"}),
                )
            }
            if command == 2 {
                if let Some(l) = listening.take() {
                    finish(&rt, &mid, prefs["wake_word"].as_str().unwrap_or(""), l)
                }
            }
            let frame = match rx.recv_timeout(Duration::from_millis(80)) {
                Ok(f) => f,
                Err(_) => {
                    if !active.is_empty()
                        && last_frame.elapsed() > Duration::from_secs(2)
                        && listening.is_some()
                    {
                        rt.voice.input_gap.store(true, Ordering::Release);
                    }
                    continue;
                }
            };
            if frame.meeting != active || frame.epoch != revision || active.is_empty() {
                continue;
            }
            last_frame = Instant::now();
            last_host = frame.host;
            let Some(d) = detector.as_mut() else { continue };
            let pcm = frame.pcm;
            // Our app is excluded from ScreenCaptureKit audio. Use only the controlled TTS
            // PCM (bundled wake prompt or TTS) here, never add a second system reference.
            #[cfg(feature = "audio-processing")]
            let pcm = if let Some(p) = apm.as_mut() {
                // Reanchor each hardware packet while retaining partial DSP
                // frames, so sample-clock drift cannot accumulate in TTS reference.
                let mut dsp_host = frame.host.saturating_sub((dsp_input % 160) as u64 * 62500);
                dsp_input += pcm.len();
                match dsp_frames.push(&pcm, |input| {
                    let mut reference = [0.; 160];
                    let present =
                        unsafe { hm_voice_reference(dsp_host, reference.as_mut_ptr(), 160) };
                    if present > 0 {
                        reference_tail = dsp_host + 700_000_000;
                    }
                    p.reverse(&reference)?;
                    dsp_host += 10_000_000;
                    p.process(input)
                }) {
                    Ok(v) => v,
                    Err(_) => {
                        rt.voice.input_gap.store(true, Ordering::Release);
                        continue;
                    }
                }
            } else {
                pcm
            };
            if pcm.is_empty() {
                continue;
            }
            if speaking {
                continue;
            } // Explicit half-duplex while answer is spoken; button interrupt remains available.
            if listening.is_none()
                && (frame.host < reference_tail || unsafe { hm_voice_state() } > 0)
            {
                continue;
            }
            append_pre_roll(&mut rolling, &pcm);
            let st = status(&rt).unwrap_or(Value::Null);
            let standby = matches!(
                st["state"].as_str(),
                Some("standby" | "error" | "testing_wake")
            );
            let flags = d.accept(&pcm, listening.is_none() && standby);
            if flags < 0 {
                rt.voice.input_gap.store(true, Ordering::Release);
                continue;
            }
            if listening.is_none() && standby && flags & 1 != 0 {
                d.reset();
                if test_deadline.take().is_some() {
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"message":"已检测到唤醒词，试唤醒成功"}),
                    );
                    continue;
                }
                let buffer = rolling.iter().copied().collect();
                let next = Listening {
                    pcm: buffer,
                    epoch: revision,
                    utterance: id(),
                    had_speech: false,
                    pending_end: false,
                    after_wake: 0,
                    empty_since: None,
                    manual: false,
                    capture_epoch: crate::product::capture_epoch(&rt, &mid).unwrap_or_default(),
                };
                // Own pre-roll and establish listening before starting output.
                listening = Some(next);
                state(
                    &rt,
                    json!({"state":"listening","meeting_id":mid,"message":"我在，请说","wake_word":prefs["wake_word"]}),
                );
                if acknowledge() {
                    let pid = id();
                    playback_base = rt
                        .detail(&mid)
                        .ok()
                        .and_then(|d| d["duration_ms"].as_u64())
                        .unwrap_or(0);
                    playback_host = last_host;
                    let _=rt.db().and_then(|db|db.execute("INSERT INTO assistant_playback_events(id,meeting_id,text,start_host_ns,start_ms) VALUES (?,?,?,?,?)",params![pid,mid,WAKE_RESPONSE_TEXT,last_host,playback_base]).map_err(err));
                    playback = Some(pid);
                }
                continue;
            }
            if let Some(l) = listening.as_mut() {
                l.pcm.extend_from_slice(&pcm);
                l.after_wake += pcm.len();
                let acknowledgement_active = unsafe { hm_voice_state() } > 0;
                let speech_end = l.endpoint(flags, acknowledgement_active);
                let timeout = l.empty_timeout(acknowledgement_active, Instant::now());
                if l.after_wake >= 400000 {
                    state(
                        &rt,
                        json!({"state":"listening","meeting_id":mid,"message":format!("还可说 {} 秒，随后自动提交",30-l.after_wake/16000)}),
                    );
                }
                let limit = l.after_wake >= 480000;
                if speech_end || timeout || limit {
                    stop_speaker();
                    if let Some(l) = listening.take() {
                        finish(&rt, &mid, prefs["wake_word"].as_str().unwrap_or(""), l)
                    }
                }
            }
        }
        rt.voice.accepting.store(false, Ordering::Release);
        invalidate(&rt);
        stop_speaker();
        if let Ok(mut tx) = rt.voice.feed.lock() {
            *tx = None
        }
        if let Ok(mut q) = rt.voice.questions.lock() {
            q.clear()
        }
        state(&rt, json!({"state":"off","message":"已停止"}));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_wake_audio_matches_manifest_and_is_not_generated_speech() {
        use sha2::{Digest, Sha256};
        let manifest: Value = serde_json::from_str(include_str!(
            "../../../../native/voice-assistant/assets/wake-acknowledgement.json"
        ))
        .unwrap();
        assert_eq!(manifest["text"], WAKE_RESPONSE_TEXT);
        assert_eq!(
            manifest["asset_sha256"],
            format!("{:x}", Sha256::digest(WAKE_RESPONSE_WAV))
        );
        let pcm = wake_pcm().unwrap();
        assert_eq!(pcm.len(), 16384);
        assert_eq!(pcm.len() as u64, manifest["sample_count"].as_u64().unwrap());
        assert!(pcm.iter().all(|x| x.is_finite() && x.abs() <= 1.));
        assert!(pcm.iter().any(|x| x.abs() > 0.01));
    }
    #[test]
    fn wake_or_acknowledgement_only_never_creates_a_model_conversation() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let mid = rt.create("合成唤醒空问题", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&mid])
            .unwrap();
        rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','active',0)",[&mid]).unwrap();
        for text in ["小舵同学", "小舵同学，我在。"] {
            let q = Question {
                meeting: mid.clone(),
                pcm: vec![],
                utterance: id(),
                epoch: 0,
                wake: "小舵同学".into(),
                capture_epoch: String::new(),
            };
            recognized(rt.clone(), q, text.into());
            assert_eq!(
                status(&rt).unwrap()["message"],
                "没有识别到问题，请重新唤醒或输入文字"
            );
        }
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM assistant_conversations", [], |r| {
                    r.get::<_, usize>(0)
                })
                .unwrap(),
            0
        );
    }
    #[test]
    fn pre_roll_handoff_keeps_last_two_seconds_and_does_not_drop_a_short_question() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let input: Vec<f32> = (0..48000).map(|i| i as f32 / 100000.).collect();
        let mut rolling = VecDeque::new();
        for packet in input.chunks(683) {
            append_pre_roll(&mut rolling, packet);
        }
        let mut pcm: Vec<f32> = rolling.iter().copied().collect();
        assert_eq!(pcm, input[16000..]);
        let tail = vec![0.; 80000]; // bounded empty-input window after activation
        pcm.extend_from_slice(&tail);
        let expected = pcm.clone();
        finish(
            &rt,
            "synthetic",
            "小舵同学",
            Listening {
                pcm,
                epoch: 0,
                utterance: id(),
                had_speech: false,
                pending_end: false,
                after_wake: tail.len(),
                empty_since: None,
                manual: false,
                capture_epoch: String::new(),
            },
        );
        let question =
            take_question(&rt).expect("pre-buffered short question still reaches local ASR");
        assert_eq!(question.pcm, expected);
        assert_eq!(question.wake, "小舵同学");
        finish(
            &rt,
            "synthetic",
            "",
            Listening {
                pcm: tail,
                epoch: 0,
                utterance: id(),
                had_speech: false,
                pending_end: false,
                after_wake: 80000,
                empty_since: None,
                manual: true,
                capture_epoch: String::new(),
            },
        );
        assert!(take_question(&rt).is_none());
        rolling.clear();
        append_pre_roll(&mut rolling, &[0.25; 160]);
        assert_eq!(rolling.len(), 160); // reset never borrows a previous meeting's pre-roll
    }
    #[test]
    fn short_question_endpoint_waits_for_acknowledgement_without_being_lost() {
        let mut l = Listening {
            pcm: vec![],
            epoch: 0,
            utterance: id(),
            had_speech: false,
            pending_end: false,
            after_wake: 0,
            empty_since: None,
            manual: false,
            capture_epoch: String::new(),
        };
        assert!(!l.endpoint(2, true));
        assert!(!l.endpoint(4, true));
        assert!(!l.endpoint(0, true));
        assert!(l.endpoint(0, false));
        // A resumed question supersedes the old endpoint until it ends again.
        assert!(!l.endpoint(2, false));
        assert!(l.endpoint(4, false));
    }
    #[test]
    fn empty_question_gets_five_full_seconds_after_prompt_not_after_wake() {
        let mut l = Listening {
            pcm: vec![],
            epoch: 0,
            utterance: id(),
            had_speech: false,
            pending_end: false,
            after_wake: 0,
            empty_since: None,
            manual: false,
            capture_epoch: String::new(),
        };
        let wake = Instant::now();
        // Even an unusually long prompt must not consume the question window.
        assert!(!l.empty_timeout(true, wake));
        assert!(!l.empty_timeout(true, wake + Duration::from_secs(6)));
        let ended = wake + Duration::from_secs(7);
        assert!(!l.empty_timeout(false, ended));
        assert!(!l.empty_timeout(false, ended + Duration::from_millis(4999)));
        assert!(l.empty_timeout(false, ended + Duration::from_secs(5)));
        // Speech during playback is retained; its endpoint, not empty timeout, wins.
        l.had_speech = true;
        assert!(!l.empty_timeout(false, ended + Duration::from_secs(6)));
        // Button capture / failed prompt has no playback to wait for.
        l.had_speech = false;
        l.empty_since = None;
        assert!(!l.empty_timeout(false, wake));
        assert!(l.empty_timeout(false, wake + Duration::from_secs(5)));
    }
    #[test]
    fn congested_input_does_not_invalidate_acknowledgement_or_submitted_question() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        *rt.voice.feed.lock().unwrap() = Some(tx);
        rt.voice.accepting.store(true, Ordering::Release);
        let epoch = rt.voice.epoch.load(Ordering::Acquire);
        state(&rt, json!({"state":"speaking","turn_id":"synthetic-turn"}));
        rt.voice.questions.lock().unwrap().push_back(Question {
            meeting: "synthetic".into(),
            pcm: vec![],
            utterance: id(),
            epoch,
            wake: String::new(),
            capture_epoch: String::new(),
        });
        feed(&rt, "synthetic", 0, &[0.; 160]);
        feed(&rt, "synthetic", 10_000_000, &[0.; 160]);
        assert!(rt.voice.input_gap.load(Ordering::Acquire));
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), epoch);
        assert_eq!(status(&rt).unwrap()["state"], "speaking");
        assert_eq!(take_question(&rt).unwrap().epoch, epoch);
        assert_eq!(rx.try_recv().unwrap().pcm.len(), 160);
    }
    #[test]
    fn disabled_wake_is_visible_and_only_offer_enable_on_eligible_capture() {
        let s = inactive_status("synthetic-meeting", false, false, false);
        assert_eq!(s["state"], "off");
        assert_eq!(s["reason"], "disabled");
        assert_eq!(s["meeting_id"], "synthetic-meeting");
        assert_eq!(s["can_enable"], true);
        for s in [
            inactive_status("", false, false, false),
            inactive_status("m", false, true, false),
            inactive_status("m", false, false, true),
            inactive_status("m", true, false, false),
        ] {
            assert_eq!(s["can_enable"], false);
        }
        assert_eq!(
            inactive_status("m", true, true, false)["reason"],
            "globally_paused"
        );
    }
    #[test]
    fn wake_prefix_only() {
        assert_eq!(
            strip_wake("小舵同学，目标是什么？", "小舵同学"),
            "目标是什么？"
        );
        assert_eq!(
            strip_wake("目标包括小舵同学的测试", "小舵同学"),
            "目标包括小舵同学的测试"
        );
        assert_eq!(
            strip_wake("先前的会议发言。小舵同学，帮我核对目标。", "小舵同学"),
            "帮我核对目标。"
        );
        assert_eq!(
            strip_wake("小舵同学，我在想目标是否明确？", "小舵同学"),
            "我在想目标是否明确？"
        );
        assert_eq!(strip_wake("小舵同学", "小舵同学"), "");
    }
    #[test]
    fn speech_never_cuts_a_claim_halfway_or_calls_another_model() {
        let sentence = "先核对目标和验收条件。";
        let answer = json!({"blocks":[{"text":format!("{sentence}{}","待核对".repeat(220))}]});
        let chunks = spoken_answer_chunks(&answer);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.first().unwrap(), sentence);
        assert_eq!(
            chunks.iter().map(String::len).sum::<usize>(),
            answer["blocks"][0]["text"].as_str().unwrap().len()
        );
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 560));
    }
    #[test]
    fn chinese_default_tokens() {
        assert_eq!(
            keyword("小舵同学").unwrap(),
            "x iǎo d uò t óng x ué @小舵同学"
        );
        assert!(keyword("abc").is_err());
    }
}

#[cfg(test)]
mod detector_regressions {
    use super::*;
    #[test]
    fn official_fixture_detects_keyword_and_speech_end_without_capture() {
        let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs/zh_5.wav");
        let mut pcm = crate::audio::read_wav(&path).expect("prepare-voice fixture");
        pcm.extend(vec![0.; 32000]);
        let mut d = Detector::new("周望军", 1200).unwrap();
        let (mut wakes, mut endpoints) = (0, 0);
        for block in pcm.chunks(640) {
            let flags = d.accept(block, true);
            assert!(flags >= 0);
            wakes += u32::from(flags & 1 != 0);
            endpoints += u32::from(flags & 4 != 0);
        }
        assert_eq!(wakes, 1);
        assert!(endpoints >= 1);
        d.reset();
        for _ in 0..100 {
            assert_eq!(d.accept(&[0.; 640], true) & 1, 0);
        }
    }
    #[test]
    #[ignore = "explicit HELMMEET_TEST_MODEL and HELMMEET_SYNTHETIC_VOICE_WAV; synthetic TTS only, no capture/playback"]
    fn synthetic_system_tts_default_wake_vad_and_whisper() {
        let model = std::env::var("HELMMEET_TEST_MODEL").expect("local model path");
        let wav = std::env::var("HELMMEET_SYNTHETIC_VOICE_WAV").expect("synthetic fixture only");
        let mut pcm = crate::audio::read_wav(std::path::Path::new(&wav)).unwrap();
        let seconds = pcm.len() as f64 / 16000.;
        pcm.extend(vec![0.; 32000]);
        let mut detector = Detector::new("小舵同学", 1200).unwrap();
        let (mut wakes, mut endpoints) = (0, 0);
        #[cfg(feature = "audio-processing")]
        let mut apm = crate::processing::Processor::with_echo(1, true).unwrap();
        #[cfg(feature = "audio-processing")]
        let mut frames = meeting_core::audio_processing::Frames::with_delay(0);
        for block in pcm.chunks(683) {
            #[cfg(feature = "audio-processing")]
            let enhanced = frames
                .push(block, |f| {
                    apm.reverse(&[0.; 160])?;
                    apm.process(f)
                })
                .unwrap();
            #[cfg(feature = "audio-processing")]
            let block = enhanced.as_slice();
            if block.is_empty() {
                continue;
            }
            let flags = detector.accept(block, true);
            assert!(flags >= 0);
            wakes += u32::from(flags & 1 != 0);
            endpoints += u32::from(flags & 4 != 0);
        }
        assert_eq!(wakes, 1);
        assert!(endpoints >= 1);
        let context =
            whisper_rs::WhisperContext::new_with_params(&model, crate::asr::context_parameters())
                .unwrap();
        let text = crate::asr::decode(&context, &pcm, "zh")
            .unwrap()
            .into_iter()
            .map(|(_, _, t)| t)
            .collect::<Vec<_>>()
            .join("");
        let question = strip_wake(&text, "小舵同学");
        assert!(
            question.contains("目标"),
            "Synthetic recognition lacked expected question: {question}"
        );
        println!("synthetic-only voice pipeline: KWS={wakes}, VAD endpoints={endpoints}, speech_seconds={seconds:.2}, ASR question preserved");
        drop(context);
    }
    #[test]
    fn voice_cannot_activate_before_recording_or_without_microphone() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            tmp.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let mid = rt.create("合成语音门禁", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(ensure_meeting(&rt, &mid).is_err());
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&mid])
            .unwrap();
        assert!(ensure_meeting(&rt, &mid).is_err());
        rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','active',0)",[&mid]).unwrap();
        assert!(ensure_meeting(&rt, &mid).is_ok());
        for status in ["paused", "interrupted", "completed"] {
            rt.db()
                .unwrap()
                .execute(
                    "UPDATE meetings SET state=? WHERE id=?",
                    params![status, mid],
                )
                .unwrap();
            assert!(ensure_meeting(&rt, &mid).is_err());
        }
    }
}

/// Keep the original transcript intact. A segment overlapping our own playback
/// is ambiguous (the user may also be speaking); it cannot establish consensus.
pub fn playback_overlap(rt: &Runtime, mid: &str, start: u64, end: u64) -> Result<bool, String> {
    rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM assistant_playback_events WHERE meeting_id=? AND start_ms<? AND coalesce(end_ms,9223372036854775807)>?)",params![mid,end,start],|r|r.get(0)).map_err(err)
}
