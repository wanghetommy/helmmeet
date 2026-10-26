//! Meeting-only voice adapter. Capture is owned exclusively by audio.rs.
use crate::{
    assistant,
    db::{err, id, Runtime},
    lifecycle::Worker,
};
use rusqlite::{params, OptionalExtension};
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
fn acknowledgement_pcm(pcm: &[f32]) -> Vec<f32> {
    // Short API voices may start or end on a voiced sample. Keep every cached
    // sample, warming the output route first and draining the last syllable
    // before the player/engine can be closed. Do not rewrite the voice cache.
    let mut output = Vec::with_capacity(3200 + pcm.len() + 5600);
    output.resize(3200, 0.); // 200 ms at 16 kHz
    output.extend_from_slice(pcm);
    output.resize(output.len() + 5600, 0.); // 350 ms tail
    output
}
fn acknowledge(rt: &Runtime, config: &Value) -> bool {
    let pcm = if config["enabled"] == true {
        // No network, synthesis, or file IO on the wake path. Missing new-voice
        // audio stays silent; an old voice must never be substituted.
        rt.voice.ack.pcm(config)
    } else {
        wake_pcm().ok().map(std::sync::Arc::from)
    };
    pcm.is_some_and(|pcm| {
        let output = acknowledgement_pcm(&pcm);
        unsafe { hm_voice_play_pcm(output.as_ptr(), output.len() as i32) == 0 }
    })
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
    pub request_epoch: u64,
    pub wake: String,
    pub capture_epoch: String,
    pub mode: String,
}
pub struct Hub {
    pub(crate) ack: crate::wake_ack::Cache,
    pub(crate) preview: crate::wake_preview::Hub,
    feed: Mutex<Option<SyncSender<Frame>>>,
    accepting: AtomicBool,
    input_gap: AtomicBool,
    pub questions: Mutex<VecDeque<Question>>,
    state: Mutex<Value>,
    speech: Mutex<Value>,
    pub(crate) epoch: AtomicU64,
    pub(crate) speech_epoch: AtomicU64,
    request_epoch: AtomicU64,
    turn_gate: Mutex<()>,
    interruption: Mutex<Value>,
    speech_chars_milli: AtomicU64,
    command: AtomicU8,
    command_mode: Mutex<String>,
    input_level: Mutex<(f32, Instant)>,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            ack: crate::wake_ack::Cache::default(),
            preview: crate::wake_preview::Hub::default(),
            feed: Mutex::new(None),
            accepting: AtomicBool::new(false),
            input_gap: AtomicBool::new(false),
            questions: Mutex::new(VecDeque::new()),
            state: Mutex::new(json!({"state":"off","message":"仅会议录制且麦克风开启时提供语音"})),
            speech: Mutex::new(Value::Null),
            epoch: AtomicU64::new(0),
            speech_epoch: AtomicU64::new(0),
            request_epoch: AtomicU64::new(0),
            turn_gate: Mutex::new(()),
            interruption: Mutex::new(Value::Null),
            speech_chars_milli: AtomicU64::new(4000),
            command: AtomicU8::new(0),
            command_mode: Mutex::new("auto".into()),
            input_level: Mutex::new((0., Instant::now())),
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
fn state(rt: &Runtime, mut v: Value) {
    v["updated_at_ms"] = json!(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64);
    if v["state"] == "off" {
        if let Ok(mut info) = rt.voice.interruption.lock() {
            *info = Value::Null;
        }
    }
    if let Ok(mut s) = rt.voice.state.lock() {
        *s = v
    }
}
pub fn status(rt: &Runtime) -> Result<Value, String> {
    let mut value = rt.voice.state.lock().map_err(|_| "语音状态锁异常")?.clone();
    value["input_level"] = json!(if value["state"] == "listening" {
        rt.voice
            .input_level
            .lock()
            .ok()
            .filter(|v| v.1.elapsed() < Duration::from_millis(500))
            .map(|v| v.0)
            .unwrap_or(0.)
    } else {
        0.
    });
    value["speech"] = rt
        .voice
        .speech
        .lock()
        .map_err(|_| "播报状态锁异常")?
        .clone();
    value["interruption"] = rt
        .voice
        .interruption
        .lock()
        .map_err(|_| "打断状态锁异常")?
        .clone();
    Ok(value)
}
/// UI-only read; the real-time voice loop uses status() without database work.
pub fn ui_status(rt: &Runtime) -> Result<Value, String> {
    let mut v = status(rt)?;
    v["wake_ack"] = rt.voice.ack.status();
    v["wake_preview"] = rt.voice.preview.status();
    let db = rt.db()?;
    let read = |r: &rusqlite::Row<'_>| {
        Ok(
            json!({"id":r.get::<_,String>(0)?,"conversation_id":r.get::<_,String>(1)?,"meeting_id":r.get::<_,Option<String>>(2)?,"state":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?}),
        )
    };
    // Active lookup uses the existing partial index; history uses reverse rowid.
    // Do not sort every historical turn on each presentation poll.
    let active=db.query_row("SELECT t.id,t.conversation_id,c.meeting_id,t.state,t.source FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id WHERE t.epoch=c.epoch AND t.state IN ('pending','running') ORDER BY t.rowid DESC LIMIT 1",[],read).optional().map_err(err)?;
    v["dialogue"]=match active {Some(turn)=>turn,None=>db.query_row("SELECT t.id,t.conversation_id,c.meeting_id,t.state,t.source FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id WHERE t.epoch=c.epoch ORDER BY t.rowid DESC LIMIT 1",[],read).optional().map_err(err)?.unwrap_or(Value::Null)};
    Ok(v)
}
fn observe_input_level(rt: &Runtime, pcm: &[f32]) {
    // Only an aggregate level is exposed; no additional capture or PCM IPC.
    let mean = pcm
        .iter()
        .map(|v| {
            if v.is_finite() {
                v.clamp(-1., 1.).powi(2)
            } else {
                0.
            }
        })
        .sum::<f32>()
        / pcm.len().max(1) as f32;
    if let Ok(mut level) = rt.voice.input_level.try_lock() {
        *level = ((mean.sqrt() * 10.).clamp(0., 1.), Instant::now());
    }
}
pub fn command(rt: &Runtime, action: &str) -> Result<Value, String> {
    command_with_mode(rt, action, "auto")
}
pub fn preview_ack(
    rt: &Runtime,
    tts: Option<Value>,
    expected_id: Option<u64>,
    selection_order: Option<u64>,
) -> Result<Value, String> {
    rt.ensure_running()?;
    let Some(tts) = tts else {
        rt.voice.preview.cancel(expected_id);
        return Ok(json!({"accepted":true}));
    };
    if !["off", "standby", "error"].contains(&status(rt)?["state"].as_str().unwrap_or("off")) {
        return Err("请先结束当前语音提问或播报，再试听音色".into());
    }
    let config = crate::tts::validate_settings(rt, &tts)?;
    if config["enabled"] != true {
        return Err("请先开启TTS API并选择有效凭据后试听".into());
    }
    let id = rt.voice.preview.request(
        config,
        rt.voice.epoch.load(Ordering::Acquire),
        selection_order.ok_or("缺少音色选择顺序")?,
    )?;
    Ok(json!({"accepted":true,"id":id}))
}
pub fn command_with_mode(rt: &Runtime, action: &str, mode: &str) -> Result<Value, String> {
    if action == "refresh_ack" {
        rt.ensure_running()?;
        if crate::tts::saved(rt)?["enabled"] != true {
            return Err("请先启用并保存TTS API设置".into());
        }
        rt.voice.ack.refresh();
        return Ok(json!({"accepted":true}));
    }
    meeting_core::dialogue::answer_mode(mode).ok_or("问答方式无效")?;
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
        if (code == 1 && !meeting_core::dialogue::interruptible(phase))
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
    let mut selected = rt.voice.command_mode.lock().map_err(|_| "问答方式锁异常")?;
    if code != 5 {
        rt.voice.preview.cancel(None);
    }
    *selected = mode.to_owned();
    rt.voice.command.store(code, Ordering::Release);
    Ok(json!({"accepted":true}))
}
pub fn invalidate(rt: &Runtime) {
    rt.voice.epoch.fetch_add(1, Ordering::AcqRel);
    rt.voice.speech_epoch.fetch_add(1, Ordering::AcqRel);
    rt.voice.request_epoch.fetch_add(1, Ordering::AcqRel);
}
pub(crate) fn speech_rate(rt: &Runtime) -> f64 {
    rt.voice.speech_chars_milli.load(Ordering::Acquire) as f64 / 1000.0
}
pub fn take_question(rt: &Runtime) -> Option<Question> {
    rt.voice.questions.lock().ok()?.pop_front()
}
pub(crate) fn request_is_current(rt: &Runtime, epoch: u64) -> bool {
    rt.voice.request_epoch.load(Ordering::Acquire) == epoch
}
pub fn valid_question(rt: &Runtime, q: &Question) -> bool {
    rt.voice.epoch.load(Ordering::Acquire) == q.epoch
        && rt.voice.request_epoch.load(Ordering::Acquire) == q.request_epoch
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
    let Ok(_gate) = rt.voice.turn_gate.lock() else {
        return;
    };
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
        assistant::submit_with_mode(
            rt.clone(),
            c["id"].as_str().ok_or("会话无效")?,
            &text,
            "voice",
            Some(q.utterance.clone()),
            "all",
            false,
            &q.mode,
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
    request_epoch: u64,
    utterance: String,
    had_speech: bool,
    pending_end: bool,
    after_wake: usize,
    empty_since: Option<Instant>,
    manual: bool,
    capture_epoch: String,
    mode: String,
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
const SPEECH_PCM_SAMPLES: usize = 16000 * 45;
#[derive(Default)]
struct SpeechAudio {
    pending: VecDeque<Vec<f32>>,
}
impl SpeechAudio {
    fn start(&mut self, pcm: &[f32], play: impl FnOnce(&[f32]) -> bool) -> bool {
        self.pending.clear();
        if pcm.is_empty() {
            return false;
        }
        let mut chunks = pcm.chunks(SPEECH_PCM_SAMPLES);
        if !play(chunks.next().unwrap()) {
            return false;
        }
        self.pending = chunks.map(<[f32]>::to_vec).collect();
        true
    }
    fn next(&mut self, play: impl FnOnce(&[f32]) -> bool) -> bool {
        self.pending.pop_front().is_some_and(|pcm| play(&pcm))
    }
}
fn play_api_pcm(pcm: &[f32]) -> bool {
    // Swift copies PCM synchronously before the Rust buffer is released.
    unsafe { hm_voice_play_pcm(pcm.as_ptr(), pcm.len() as i32) == 0 }
}
fn speech_status(rt: &Runtime, provider: &str, error: Option<&str>) {
    if let Ok(mut value) = rt.voice.speech.lock() {
        *value = json!({"provider":provider,"error":error});
    }
}
fn play_prepared(
    rt: &Runtime,
    ready: &crate::voice_output::Ready,
    audio: &mut SpeechAudio,
) -> bool {
    use crate::voice_output::Prepared;
    audio.pending.clear();
    let mut error = None;
    match &ready.result {
        Ok(Prepared::Api { pcm, fallback }) => {
            if audio.start(pcm, play_api_pcm) {
                speech_status(rt, "api", None);
                if pcm.len() > 16000 {
                    let observed = (ready.job.text.chars().count() as u64 * 16_000_000
                        / pcm.len() as u64)
                        .clamp(2000, 8000);
                    let old = rt.voice.speech_chars_milli.load(Ordering::Acquire);
                    rt.voice
                        .speech_chars_milli
                        .store((old + observed) / 2, Ordering::Release);
                }
                return true;
            }
            error = Some("TTS音频播放启动失败".to_owned());
            if !fallback {
                speech_status(rt, "none", error.as_deref());
                return false;
            }
        }
        Ok(Prepared::Local { error: reason }) => error.clone_from(reason),
        Err(reason) => {
            speech_status(rt, "none", Some(reason));
            return false;
        }
    }
    let started = speak(&ready.job.text);
    speech_status(rt, if started { "local" } else { "none" }, error.as_deref());
    started
}
fn playback_status(mid: &str, turn: &str, native: i32, continuing: bool) -> Value {
    json!({"state":"speaking","meeting_id":mid,"turn_id":turn,"audio_playing":native==2,"continuing":continuing,"message":if native==2 {"正在回答（语音）"}else{"正在准备语音，文字已可查看"}})
}
fn request_speech(
    rt: &Runtime,
    mid: &str,
    turn: &str,
    text: &str,
    output: &crate::voice_output::SpeechWorker,
    continuing: bool,
) -> bool {
    match output.request(mid, turn, text) {
        Ok(()) => {
            state(
                rt,
                json!({"state":"synthesizing","meeting_id":mid,"turn_id":turn,"continuing":continuing,"message":"正在准备语音，文字已可查看"}),
            );
            true
        }
        Err(error) => {
            speech_status(rt, "none", Some(&error));
            false
        }
    }
}
/// Cancel only this meeting's current dialogue. Capture identity and audio
/// queues are independent; advancing the question epoch fences old ASR work.
pub(crate) fn interrupt_answer(rt: &Runtime, mid: &str) -> Result<(), String> {
    let _gate = rt.voice.turn_gate.lock().map_err(|_| "语音回合锁异常")?;
    rt.voice.request_epoch.fetch_add(1, Ordering::AcqRel);
    rt.voice.speech_epoch.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut q) = rt.voice.questions.lock() {
        q.clear();
    }
    let previous = status(rt)?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    tx.execute("UPDATE assistant_turns SET state='cancelled',error='回答生成已被新问题打断' WHERE state IN ('pending','running') AND conversation_id IN (SELECT id FROM assistant_conversations WHERE meeting_id=?)",[mid]).map_err(err)?;
    tx.execute("UPDATE assistant_proposals SET state='dismissed' WHERE state='pending' AND turn_id IN (SELECT t.id FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id WHERE c.meeting_id=? AND t.state='cancelled')",[mid]).map_err(err)?;
    if let Some(turn) = previous["turn_id"].as_str() {
        tx.execute("UPDATE assistant_turns SET speech_interrupted=1 WHERE id=? AND state='succeeded' AND conversation_id IN (SELECT id FROM assistant_conversations WHERE meeting_id=?)",params![turn,mid]).map_err(err)?;
    }
    tx.commit().map_err(err)?;
    Ok(())
}
fn spoken_answer_chunks(answer: &Value) -> Vec<String> {
    let text = answer["blocks"]
        .as_array()
        .into_iter()
        .flatten()
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
fn begin_playback(
    rt: &Runtime,
    mid: &str,
    turn: &str,
    text: &str,
    last_host: u64,
    playback: &mut Option<String>,
    playback_base: &mut u64,
    playback_host: &mut u64,
) {
    let pid = id();
    *playback_base = rt
        .detail(mid)
        .ok()
        .and_then(|d| d["duration_ms"].as_u64())
        .unwrap_or(0);
    *playback_host = last_host;
    let _=rt.db().and_then(|db|db.execute("INSERT INTO assistant_playback_events(id,meeting_id,turn_id,text,start_host_ns,start_ms) VALUES (?,?,?,?,?,?)",params![pid,mid,turn,text,last_host,*playback_base]).map_err(err));
    *playback = Some(pid);
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
        request_epoch: l.request_epoch,
        wake: if l.manual { String::new() } else { word.into() },
        capture_epoch: l.capture_epoch,
        mode: l.mode,
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
fn inactive_capture_status(
    rt: &Runtime,
    eligible: &str,
    configured: bool,
    paused: bool,
    locked: bool,
) -> Value {
    let mut v = inactive_status(eligible, configured, paused, locked);
    if configured && !paused && !locked && eligible.is_empty() {
        let capture=rt.db().ok().and_then(|db|db.query_row("SELECT m.id,m.state,s.state FROM meetings m LEFT JOIN recording_sources s ON s.meeting_id=m.id AND s.source='microphone' WHERE m.state IN ('recording','paused','interrupted') ORDER BY m.rowid DESC LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).optional().ok().flatten());
        if let Some((id, phase, microphone)) = capture {
            v["meeting_id"] = json!(id);
            if phase != "recording" {
                v["reason"] = json!("meeting_paused");
                v["message"] = json!("听会已暂停，继续会议后恢复监听");
            } else if microphone.as_deref() == Some("disabled") {
                v["reason"] = json!("microphone_disabled");
                v["message"] = json!("麦克风已关闭，恢复后可继续听会");
            }
        }
    }
    v
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    Worker::spawn(move |stop| {
        // Decode the bundled prompt before accepting microphone frames. There
        // is no runtime file lookup or TTS synthesis on the wake path.
        let _ = wake_pcm();
        let _acknowledgements = crate::wake_ack::background(rt.clone());
        let _previews = crate::wake_preview::background(rt.clone());
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
        let mut last_wake = Instant::now() - Duration::from_secs(1);
        let output = crate::voice_output::SpeechWorker::new(rt.clone());
        let mut wake_gate = meeting_core::dialogue::PlaybackWakeGate::default();
        let mut speaking = false;
        let mut speech_failed = false;
        let mut speaking_since = Instant::now();
        let mut speech_queue = VecDeque::<String>::new();
        let mut speech_audio = SpeechAudio::default();
        let mut speaking_turn = String::new();
        let mut test_deadline: Option<Instant> = None;
        let mut playback: Option<String> = None;
        let mut playback_base = 0;
        let mut playback_host = 0;
        let mut preview_playing: Option<(u64, u64)> = None;
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
            let inactive = if mid.is_empty() {
                inactive_capture_status(&rt, &eligible_mid, configured, paused, locked)
            } else {
                Value::Null
            };
            let key = format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                eligible_mid,
                configured,
                paused,
                locked,
                mid,
                prefs["wake_word"],
                prefs["silence_ms"],
                inactive
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
                output.cancel();
                wake_gate.clear();
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speech_audio.pending.clear();
                speaking_turn.clear();
                if let Ok(mut q) = rt.voice.questions.lock() {
                    q.clear()
                }
                if mid.is_empty() {
                    state(&rt, inactive.clone())
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
            if let Some((id, epoch)) = preview_playing {
                if !rt.voice.preview.current(id)
                    || epoch != rt.voice.epoch.load(Ordering::Acquire)
                    || !["off", "standby", "error"].contains(
                        &status(&rt).unwrap_or(Value::Null)["state"]
                            .as_str()
                            .unwrap_or("off"),
                    )
                {
                    stop_speaker();
                    rt.voice.preview.cancel(Some(id));
                    preview_playing = None;
                } else if unsafe { hm_voice_state() } <= 0 {
                    rt.voice.preview.phase(id, "done", None);
                    preview_playing = None;
                }
            }
            if let Some(pid) = playback.as_ref() {
                if mid.is_empty()
                    || (speech_audio.pending.is_empty() && unsafe { hm_voice_state() } <= 0)
                {
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
            let (command, command_mode) = {
                let selected = rt
                    .voice
                    .command_mode
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                (rt.voice.command.swap(0, Ordering::AcqRel), selected.clone())
            };
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
                output.cancel();
                speech_queue.clear();
                speech_audio.pending.clear();
                invalidate(&rt);
                continue;
            }
            if command == 4 {
                output.cancel();
                wake_gate.clear();
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speech_audio.pending.clear();
                if !mid.is_empty() {
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"outcome":"stopped","message":"播报已停止，文字回答已保留"}),
                    )
                }
            }
            if let Some(ready) = output.take_ready() {
                if ready.job.meeting == mid
                    && rt.voice.command.load(Ordering::Acquire) == 0
                    && !mid.is_empty()
                    && status(&rt).is_ok_and(|s| s["state"] == "synthesizing")
                    && ensure_meeting(&rt, &mid).is_ok()
                {
                    let continuing = status(&rt).is_ok_and(|s| s["continuing"] == true);
                    speaking = play_prepared(&rt, &ready, &mut speech_audio);
                    if speaking {
                        begin_playback(
                            &rt,
                            &mid,
                            &ready.job.turn,
                            &ready.job.text,
                            last_host,
                            &mut playback,
                            &mut playback_base,
                            &mut playback_host,
                        );
                        speaking_since = Instant::now();
                        state(
                            &rt,
                            playback_status(
                                &mid,
                                &ready.job.turn,
                                unsafe { hm_voice_state() },
                                continuing,
                            ),
                        );
                    } else {
                        speech_queue.clear();
                        state(
                            &rt,
                            json!({"state":"standby","meeting_id":mid,"outcome":"speech_failed","message":"播报未启动，回答保存在聊天中"}),
                        );
                    }
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
                            speech_failed = false;
                            let text = speech_queue.pop_front();
                            speaking = false;
                            let requested = prefs["speak_answers"].as_bool().unwrap_or(true)
                                && text.as_deref().is_some_and(|text| {
                                    request_speech(&rt, &mid, turn, text, &output, false)
                                });
                            if !requested {
                                speech_queue.clear();
                                state(
                                    &rt,
                                    json!({"state":"standby","meeting_id":mid,"turn_id":turn,"outcome":"answered","message":"回答已保存在聊天中"}),
                                );
                            }
                        } else if ["failed", "cancelled"].contains(&state_name.as_str()) {
                            state(
                                &rt,
                                json!({"state":"standby","meeting_id":mid,"turn_id":turn,"message":"本次问答未完成，可查看聊天记录"}),
                            )
                        }
                    }
                }
            }
            if speaking {
                let native = unsafe { hm_voice_state() };
                let current = status(&rt).unwrap_or(Value::Null);
                if current["state"] == "speaking"
                    && current["audio_playing"].as_bool() != Some(native == 2)
                {
                    state(
                        &rt,
                        playback_status(
                            &mid,
                            &speaking_turn,
                            native,
                            current["continuing"] == true,
                        ),
                    );
                }
                if native < 0 {
                    speech_failed = true;
                    speech_queue.clear();
                    speech_audio.pending.clear();
                    speech_status(&rt, "none", Some("语音播放器未能完成播报"));
                }
            }
            // Allow the full 45-second PCM plus native scheduling/completion
            // latency; the watchdog must not clip each queued buffer's tail.
            if speaking && speaking_since.elapsed() > Duration::from_secs(50) {
                stop_speaker();
                speech_failed = true;
                speech_queue.clear();
                speech_audio.pending.clear();
            }
            if speaking && unsafe { hm_voice_state() } <= 0 {
                if !speech_audio.pending.is_empty() {
                    speaking = speech_audio.next(play_api_pcm);
                    if !speaking {
                        speech_status(&rt, "none", Some("TTS后续音频播放失败"));
                        speech_failed = true;
                    }
                } else if let Some(text) = speech_queue.pop_front() {
                    speaking = false;
                    if !request_speech(&rt, &mid, &speaking_turn, &text, &output, true) {
                        speech_queue.clear();
                        speech_failed = true;
                    }
                } else {
                    speaking = false;
                }
                if speaking {
                    speaking_since = Instant::now();
                    state(
                        &rt,
                        playback_status(&mid, &speaking_turn, unsafe { hm_voice_state() }, true),
                    );
                }
                if !speaking && status(&rt).is_ok_and(|s| s["state"] != "synthesizing") {
                    speech_queue.clear();
                    speech_audio.pending.clear();
                    reference_tail = last_host + 700_000_000;
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"turn_id":speaking_turn,"outcome":if speech_failed {"speech_failed"} else {"spoken"},"message":if speech_failed {"播报未完成，完整文字已保留"} else {"待唤醒"}}),
                    );
                    if let Some(d) = detector.as_mut() {
                        d.reset()
                    }
                }
            }
            if command == 1 && !mid.is_empty() && detector.is_some() {
                // Stop sound before waiting on the dialogue transaction.
                output.cancel();
                stop_speaker();
                if let Err(error) = interrupt_answer(&rt, &mid) {
                    state(
                        &rt,
                        json!({"state":"error","meeting_id":mid,"message":error}),
                    );
                    continue;
                }
                output.cancel();
                wake_gate.clear();
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speech_audio.pending.clear();
                speaking_turn.clear();
                if let Some(pid) = playback.take() {
                    let end = playback_base + last_host.saturating_sub(playback_host) / 1_000_000;
                    let _=rt.db().and_then(|db|db.execute("UPDATE assistant_playback_events SET end_host_ns=?,end_ms=? WHERE id=?",params![last_host,end,pid]).map_err(err));
                }
                listening = Some(Listening {
                    pcm: vec![],
                    epoch: revision,
                    request_epoch: rt.voice.request_epoch.load(Ordering::Acquire),
                    utterance: id(),
                    had_speech: false,
                    pending_end: false,
                    after_wake: 0,
                    empty_since: None,
                    manual: true,
                    capture_epoch: crate::product::capture_epoch(&rt, &mid).unwrap_or_default(),
                    mode: command_mode.clone(),
                });
                rolling.clear();
                if let Some(d) = detector.as_mut() {
                    d.reset();
                }
                state(
                    &rt,
                    json!({"state":"listening","meeting_id":mid,"message":"正在听，请说新的问题，停顿后自动提交"}),
                )
            }
            if command == 2 {
                if let Some(l) = listening.take() {
                    finish(&rt, &mid, prefs["wake_word"].as_str().unwrap_or(""), l)
                }
            }
            // Only this worker owns the player. Preview preparation never opens
            // capture or races a late synthesis against a new spoken question.
            if let Some(ready) = rt.voice.preview.take_ready() {
                let idle = ["off", "standby", "error"].contains(
                    &status(&rt).unwrap_or(Value::Null)["state"]
                        .as_str()
                        .unwrap_or("off"),
                );
                if ready.epoch == rt.voice.epoch.load(Ordering::Acquire) && idle && !locked {
                    let pcm = acknowledgement_pcm(&ready.pcm);
                    let played = unsafe { hm_voice_play_pcm(pcm.as_ptr(), pcm.len() as i32) } == 0;
                    if played && rt.voice.preview.current(ready.id) {
                        preview_playing = Some((ready.id, ready.epoch));
                        rt.voice.preview.phase(ready.id, "playing", None);
                        if !mid.is_empty() {
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
                    } else {
                        if played {
                            stop_speaker();
                        }
                        rt.voice
                            .preview
                            .phase(ready.id, "failed", Some("音色试听未能播放".into()));
                    }
                } else {
                    rt.voice.preview.cancel(Some(ready.id));
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
                    let clean = p.process(input)?;
                    wake_gate.observe(&clean, &reference, present > 0);
                    Ok(clean)
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
            observe_input_level(&rt, &pcm);
            append_pre_roll(&mut rolling, &pcm);
            let st = status(&rt).unwrap_or(Value::Null);
            let standby =
                meeting_core::dialogue::interruptible(st["state"].as_str().unwrap_or("off"));
            let echo_window = unsafe { hm_voice_state() } == 2 || frame.host < reference_tail;
            let can_admit = !echo_window || wake_gate.reference_ready();
            if let Ok(mut info) = rt.voice.interruption.lock() {
                *info = json!({"available":!echo_window || wake_gate.reference_ready(),"reason":if echo_window && !wake_gate.reference_ready(){"播放参考不足，请使用按钮打断"}else{"完整唤醒词可打断当前回答"}});
            }
            // Keep KWS input continuous through reference warmup/gaps. Reference
            // validity gates a completed candidate, never the decoder audio.
            let flags = d.accept(
                &pcm,
                listening.is_none() && standby && last_wake.elapsed() >= Duration::from_millis(600),
            );
            if flags < 0 {
                rt.voice.input_gap.store(true, Ordering::Release);
                continue;
            }
            if listening.is_none() && standby && flags & 1 != 0 {
                d.reset();
                if echo_window && (!can_admit || !wake_gate.near_end()) {
                    continue;
                }
                if test_deadline.take().is_some() {
                    state(
                        &rt,
                        json!({"state":"standby","meeting_id":mid,"message":"已检测到唤醒词，试唤醒成功"}),
                    );
                    continue;
                }
                last_wake = Instant::now();
                rt.voice.preview.cancel(None);
                preview_playing = None;
                let buffer = rolling.iter().copied().collect();
                output.cancel();
                stop_speaker();
                speaking = false;
                speech_queue.clear();
                speech_audio.pending.clear();
                speaking_turn.clear();
                if let Some(pid) = playback.take() {
                    let end = playback_base + last_host.saturating_sub(playback_host) / 1_000_000;
                    let _=rt.db().and_then(|db|db.execute("UPDATE assistant_playback_events SET end_host_ns=?,end_ms=? WHERE id=?",params![last_host,end,pid]).map_err(err));
                }
                if let Err(error) = interrupt_answer(&rt, &mid) {
                    state(
                        &rt,
                        json!({"state":"error","meeting_id":mid,"message":error}),
                    );
                    continue;
                }
                let next = Listening {
                    pcm: buffer,
                    epoch: revision,
                    request_epoch: rt.voice.request_epoch.load(Ordering::Acquire),
                    utterance: id(),
                    had_speech: false,
                    pending_end: false,
                    after_wake: 0,
                    empty_since: None,
                    manual: false,
                    capture_epoch: crate::product::capture_epoch(&rt, &mid).unwrap_or_default(),
                    mode: "auto".into(),
                };
                // Own pre-roll and establish listening before starting output.
                listening = Some(next);
                state(
                    &rt,
                    json!({"state":"listening","meeting_id":mid,"message":"正在听，请说新的问题","wake_word":prefs["wake_word"]}),
                );
                if acknowledge(&rt, &prefs["tts"]) {
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
    fn preview_command_validates_draft_without_saving_settings_or_enabling_capture() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let profile = crate::models::save(
            &rt,
            crate::models::ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "Synthetic preview".into(),
                base_url: "http://127.0.0.1:12345/v1".into(),
                model: "synthetic".into(),
                context_tokens: 32768,
                max_output_tokens: 4096,
                timeout_seconds: 10,
                key_action: "replace".into(),
                api_key: "synthetic-key".into(),
            },
        )
        .unwrap();
        let cfg = json!({"enabled":true,"speaker":"synthetic-voice","model":"seed-tts-2.0-standard","profile_id":profile["profile_id"],"config_version":profile["config_version"],"fallback_local":true});
        let before = crate::tts::saved(&rt).unwrap();
        let epoch = rt.voice.epoch.load(Ordering::Acquire);
        let result = preview_ack(&rt, Some(cfg.clone()), None, Some(1)).unwrap();
        assert!(result["id"].is_u64());
        assert_eq!(crate::tts::saved(&rt).unwrap(), before);
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), epoch);
        assert!(!rt.voice.accepting.load(Ordering::Acquire));
        state(&rt, json!({"state":"thinking"}));
        assert!(preview_ack(&rt, Some(cfg), None, Some(2))
            .unwrap_err()
            .contains("结束当前"));
        preview_ack(&rt, None, result["id"].as_u64(), None).unwrap();
        assert_eq!(rt.voice.preview.status()["state"], "cancelled");
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM meetings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn acknowledgement_keeps_every_voice_sample_and_pads_both_edges_without_changing_cache() {
        for count in [6400, 9623, 12404, 47799, 80000] {
            // Abrupt, non-silent endpoints exercise voices with no API padding.
            let original: Vec<f32> = (0..count)
                .map(|i| if i % 2 == 0 { 0.25 } else { -0.5 })
                .collect();
            let output = acknowledgement_pcm(&original);
            assert_eq!(output.len(), original.len() + 8800);
            assert!(output[..3200].iter().all(|v| *v == 0.));
            assert_eq!(&output[3200..3200 + count], original.as_slice());
            assert!(output[3200 + count..].iter().all(|v| *v == 0.));
            assert_eq!(acknowledgement_pcm(&original), output);
        }
    }
    #[test]
    fn presentation_reports_only_current_dialogue_and_real_fresh_level_without_content() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let c = assistant::open(&rt, None, true).unwrap();
        let cid = c["id"].as_str().unwrap();
        let first = id();
        let later = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'private synthetic question','text','running','now')",params![first,cid]).unwrap();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'another private question','text','succeeded','now')",params![later,cid]).unwrap();
        let ui = ui_status(&rt).unwrap();
        assert_eq!(ui["dialogue"]["id"], first);
        assert!(!ui.to_string().contains("private"));
        rt.db()
            .unwrap()
            .execute(
                "UPDATE assistant_turns SET state='succeeded' WHERE id=?",
                [&first],
            )
            .unwrap();
        assert_eq!(ui_status(&rt).unwrap()["dialogue"]["id"], later);
        rt.db()
            .unwrap()
            .execute(
                "UPDATE assistant_conversations SET epoch=1 WHERE id=?",
                [cid],
            )
            .unwrap();
        assert!(ui_status(&rt).unwrap()["dialogue"].is_null());
        state(&rt, json!({"state":"listening"}));
        observe_input_level(&rt, &[0.05; 160]);
        assert!((status(&rt).unwrap()["input_level"].as_f64().unwrap() - 0.5).abs() < 0.001);
        *rt.voice.input_level.lock().unwrap() = (0.7, Instant::now() - Duration::from_secs(1));
        assert_eq!(status(&rt).unwrap()["input_level"], 0.);
        observe_input_level(&rt, &[f32::NAN, f32::INFINITY]);
        assert_eq!(status(&rt).unwrap()["input_level"], 0.);
        observe_input_level(&rt, &[0.1; 160]);
        state(&rt, json!({"state":"off"}));
        assert_eq!(status(&rt).unwrap()["input_level"], 0.);
        assert!(status(&rt).unwrap()["updated_at_ms"].as_u64().unwrap() > 0);
    }
    #[test]
    fn inactive_capture_reasons_and_local_rendering_do_not_claim_listening_or_playback() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let mid = id();
        rt.db().unwrap().execute("INSERT INTO meetings(id,title,timezone,created_at,state) VALUES (?,'synthetic','Asia/Shanghai','now','recording')",[&mid]).unwrap();
        rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','disabled',0)",[&mid]).unwrap();
        assert_eq!(
            inactive_capture_status(&rt, "", true, false, false)["reason"],
            "microphone_disabled"
        );
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='paused' WHERE id=?", [&mid])
            .unwrap();
        assert_eq!(
            inactive_capture_status(&rt, "", true, false, false)["reason"],
            "meeting_paused"
        );
        assert_eq!(
            inactive_capture_status(&rt, "", false, false, false)["reason"],
            "disabled"
        );
        assert_eq!(
            inactive_capture_status(&rt, "", true, false, true)["reason"],
            "locked"
        );
        assert_eq!(playback_status(&mid, "t", 1, false)["audio_playing"], false);
        assert_eq!(playback_status(&mid, "t", 2, false)["audio_playing"], true);
        assert_eq!(playback_status(&mid, "t", 1, true)["continuing"], true);
    }
    #[test]
    fn interrupt_fences_old_questions_preserves_capture_and_marks_completed_speech() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let mid = rt.create("合成打断", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let other = rt.create("合成另一场", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let c = crate::assistant::open(&rt, Some(mid.clone()), false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let c2 = crate::assistant::open(&rt, Some(other), false).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        for (turn, cid, status) in [
            ("old", &c, "running"),
            ("finished", &c, "succeeded"),
            ("other", &c2, "pending"),
        ] {
            rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,response_json,created_at) VALUES (?,?,0,'synthetic','voice',?,'{\"blocks\":[]}',?)", params![turn,cid,status,crate::db::now()]).unwrap();
        }
        for (pid, status) in [("draft", "pending"), ("accepted", "accepted")] {
            rt.db().unwrap().execute("INSERT INTO assistant_proposals(id,turn_id,payload_json,state) VALUES (?,'old','{}',?)",params![pid,status]).unwrap();
        }
        state(
            &rt,
            json!({"state":"speaking","meeting_id":mid,"turn_id":"finished"}),
        );
        let capture = rt.voice.epoch.load(Ordering::Acquire);
        let question = rt.voice.request_epoch.load(Ordering::Acquire);
        interrupt_answer(&rt, &mid).unwrap();
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), capture);
        assert!(!request_is_current(&rt, question));
        assert!(request_is_current(&rt, question + 1));
        let db = rt.db().unwrap();
        let old: (String, Option<String>) = db
            .query_row(
                "SELECT state,error FROM assistant_turns WHERE id='old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(old.0, "cancelled");
        assert!(old.1.unwrap().contains("被新问题打断"));
        assert_eq!(
            db.query_row(
                "SELECT speech_interrupted FROM assistant_turns WHERE id='finished'",
                [],
                |r| r.get::<_, u8>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM assistant_turns WHERE id='other'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM assistant_proposals WHERE id='draft'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "dismissed"
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM assistant_proposals WHERE id='accepted'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "accepted"
        );
        drop(db);
        assert_eq!(
            crate::assistant::detail(&rt, &c).unwrap()["turns"][1]["speech_interrupted"],
            true
        );
    }
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
                request_epoch: 0,
                wake: "小舵同学".into(),
                capture_epoch: String::new(),
                mode: "auto".into(),
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
                request_epoch: 0,
                utterance: id(),
                had_speech: false,
                pending_end: false,
                after_wake: tail.len(),
                empty_since: None,
                manual: false,
                capture_epoch: String::new(),
                mode: "analyze".into(),
            },
        );
        let question =
            take_question(&rt).expect("pre-buffered short question still reaches local ASR");
        assert_eq!(question.pcm, expected);
        assert_eq!(question.wake, "小舵同学");
        assert_eq!(question.mode, "analyze");
        finish(
            &rt,
            "synthetic",
            "",
            Listening {
                pcm: tail,
                epoch: 0,
                request_epoch: 0,
                utterance: id(),
                had_speech: false,
                pending_end: false,
                after_wake: 80000,
                empty_since: None,
                manual: true,
                capture_epoch: String::new(),
                mode: "auto".into(),
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
            request_epoch: 0,
            utterance: id(),
            had_speech: false,
            pending_end: false,
            after_wake: 0,
            empty_since: None,
            manual: false,
            capture_epoch: String::new(),
            mode: "auto".into(),
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
            request_epoch: 0,
            utterance: id(),
            had_speech: false,
            pending_end: false,
            after_wake: 0,
            empty_since: None,
            manual: false,
            capture_epoch: String::new(),
            mode: "auto".into(),
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
            request_epoch: rt.voice.request_epoch.load(Ordering::Acquire),
            wake: String::new(),
            capture_epoch: String::new(),
            mode: "auto".into(),
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
    fn experience_introduction_is_spoken_with_the_same_short_answer_text() {
        let text = meeting_core::answer_policy::AdviceScope {
            meeting: true,
            ..Default::default()
        }
        .introduce("我建议先小范围测试。", false);
        let answer = json!({"blocks":[{"kind":"suggestion","basis":"experience","text":text,"sources":[]}],"evidence":[]});
        let chunks = spoken_answer_chunks(&answer);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].starts_with("目前我还没找到相关讨论。根据我的知识与经验，"));
        assert!(chunks[0].ends_with("我建议先小范围测试。"));
    }
    #[test]
    fn speech_includes_every_answer_block_and_the_final_confirmation() {
        let texts = [
            "第一项合成结论。".repeat(50),
            "第二项合成建议。".repeat(50),
            "第三项合成问题。".repeat(50),
            "第四项合成提醒。".repeat(50),
            "最后还有一项待确认，请确认是否采用这个方案？".to_owned(),
        ];
        let answer =
            json!({"blocks":texts.iter().map(|text|json!({"text":text})).collect::<Vec<_>>()});
        let chunks = spoken_answer_chunks(&answer);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), texts.join("。"));
        assert!(chunks.last().unwrap().ends_with(texts.last().unwrap()));
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 560));
    }
    #[test]
    fn api_pcm_over_four_seconds_plays_and_long_pcm_is_not_discarded() {
        let pcm = (0..SPEECH_PCM_SAMPLES * 2 + 16000 * 5)
            .map(|i| (i % 100) as f32 / 100.0)
            .collect::<Vec<_>>();
        let mut audio = SpeechAudio::default();
        let mut received = Vec::new();
        assert!(audio.start(&pcm, |part| {
            received.extend_from_slice(part);
            true
        }));
        assert_eq!(received.len(), SPEECH_PCM_SAMPLES);
        while !audio.pending.is_empty() {
            assert!(audio.next(|part| {
                assert!(part.len() <= SPEECH_PCM_SAMPLES);
                received.extend_from_slice(part);
                true
            }));
        }
        assert_eq!(received, pcm);
        assert!(!audio.next(|_| panic!("No stale PCM may replay")));
        assert!(!audio.start(&pcm, |_| false));
        assert!(audio.pending.is_empty());
    }
    #[test]
    fn speech_fallback_diagnostic_survives_voice_state_updates() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        speech_status(&rt, "local", Some("TTS音频播放启动失败"));
        state(&rt, json!({"state":"standby"}));
        let s = status(&rt).unwrap();
        assert_eq!(s["speech"]["provider"], "local");
        assert_eq!(s["speech"]["error"], "TTS音频播放启动失败");
        speech_status(&rt, "api", None);
        assert_eq!(status(&rt).unwrap()["speech"]["provider"], "api");
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
    fn playback_gate_accepts_quiet_public_keyword_with_loud_far_end() {
        let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs/zh_5.wav");
        let mut pcm = crate::audio::read_wav(&path).unwrap();
        for sample in &mut pcm {
            *sample *= 0.4;
        }
        pcm.extend(vec![0.; 32000]);
        let mut d = Detector::new("周望军", 1200).unwrap();
        let mut gate = meeting_core::dialogue::PlaybackWakeGate::default();
        let (mut detected, mut accepted) = (0, 0);
        for (i, block) in pcm.chunks(640).enumerate() {
            for (j, clean) in block.chunks(160).enumerate() {
                let reference: Vec<f32> = (0..clean.len())
                    .map(|n| ((i * 640 + j * 160 + n) as f32 * 0.071).sin() * 0.3)
                    .collect();
                gate.observe(clean, &reference, true);
            }
            let flags = d.accept(block, true);
            if flags & 1 != 0 {
                detected += 1;
                let near = gate.near_end();
                accepted += usize::from(near);
                println!(
                    "public fixture keyword detected at {}ms, admitted={near}",
                    (i + 1) * 40
                );
            }
        }
        assert_eq!(detected, 1);
        assert_eq!(
            accepted, 1,
            "a full independent human keyword was rejected at recognition time"
        );
    }
    #[test]
    fn public_keyword_spoken_by_the_assistant_is_rejected_even_after_echo_coloring() {
        let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs/zh_5.wav");
        let mut reference = crate::audio::read_wav(&path).unwrap();
        reference.extend(vec![0.; 32000]);
        let mut detector = Detector::new("周望军", 1200).unwrap();
        let mut gate = meeting_core::dialogue::PlaybackWakeGate::default();
        let (mut candidates, mut accepted) = (0, 0);
        for start in (0..reference.len()).step_by(160) {
            let end = (start + 160).min(reference.len());
            let echo: Vec<f32> = (start..end)
                .map(|i| {
                    [(33, 0.12), (173, 0.08)]
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
            gate.observe(&echo, &reference[start..end], true);
            if detector.accept(&echo, true) & 1 != 0 {
                candidates += 1;
                accepted += usize::from(gate.near_end());
            }
        }
        assert!(
            candidates > 0,
            "negative fixture must exercise a detected self-keyword"
        );
        assert_eq!(accepted, 0);
    }
    #[test]
    #[cfg(feature = "audio-processing")]
    fn real_apm_and_keyword_decoder_admit_public_human_speech_over_loud_playback() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs",
        );
        let near = crate::audio::read_wav(&root.join("zh_5.wav")).unwrap();
        let far = crate::audio::read_wav(&root.join("zh_0.wav")).unwrap();
        // Five seconds of actual far-end playback allows the echo path to adapt.
        // This offline fixture does not stand in for startup/room acceptance.
        let warm = 80000;
        for volume in [0.3, 0.7] {
            let mut detector = Detector::new("周望军", 1200).unwrap();
            let mut gate = meeting_core::dialogue::PlaybackWakeGate::default();
            let mut apm = crate::processing::Processor::with_echo(1, true).unwrap();
            let (mut accepted, mut detected) = (0, 0);
            let far_at = |i: usize| far[i % far.len()] * volume;
            for start in (0..near.len() + warm + 32000).step_by(160) {
                let reference = std::array::from_fn(|n| far_at(start + n));
                let input = std::array::from_fn(|n| {
                    let at = start + n;
                    let echo = if at >= 320 {
                        far_at(at - 320) * 0.7
                    } else {
                        0.
                    };
                    let human = if at >= warm {
                        near.get(at - warm).copied().unwrap_or(0.) * 0.4
                    } else {
                        0.
                    };
                    (echo + human).clamp(-1., 1.)
                });
                apm.reverse(&reference).unwrap();
                let clean = apm.process(&input).unwrap();
                gate.observe(&clean, &reference, true);
                if detector.accept(&clean, true) & 1 != 0 {
                    detected += 1;
                    accepted += usize::from(gate.near_end());
                }
            }
            assert_eq!(
                detected, 1,
                "actual APM must preserve the public human keyword at volume {volume}"
            );
            assert_eq!(
                accepted, 1,
                "human keyword must pass the playback gate at volume {volume}"
            );
        }
    }
    #[test]
    #[cfg(feature = "audio-processing")]
    fn actual_apm_self_playback_never_admits_its_own_public_keyword() {
        let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tools/voice/models/sherpa-onnx-kws-zipformer-zh-en-3M-2025-12-20/test_wavs/zh_5.wav");
        let pcm = crate::audio::read_wav(&path).unwrap();
        let total = pcm.len() * 3;
        let source = |at: usize| pcm[at % pcm.len()] * 0.7;
        let mut detector = Detector::new("周望军", 1200).unwrap();
        let mut gate = meeting_core::dialogue::PlaybackWakeGate::default();
        let mut apm = crate::processing::Processor::with_echo(1, true).unwrap();
        let mut admitted = 0;
        for start in (0..total).step_by(160) {
            let reference = std::array::from_fn(|n| source(start + n));
            let echo = std::array::from_fn(|n| {
                let at = start + n;
                [(320, 0.6), (1113, 0.2)]
                    .into_iter()
                    .map(|(lag, gain)| {
                        if at >= lag {
                            source(at - lag) * gain
                        } else {
                            0.
                        }
                    })
                    .sum()
            });
            apm.reverse(&reference).unwrap();
            let clean = apm.process(&echo).unwrap();
            gate.observe(&clean, &reference, true);
            if detector.accept(&clean, true) & 1 != 0 {
                admitted += usize::from(gate.near_end());
            }
        }
        assert_eq!(
            admitted, 0,
            "assistant self-keywords must never be admitted after actual APM"
        );
    }
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
