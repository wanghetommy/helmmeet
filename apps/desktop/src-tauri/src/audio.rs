use crate::db::{err, id, now, Runtime};
use meeting_core::audio_clock::{resample_frame, AudioClock};
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

extern "C" {
    fn hm_audio_start(
        mask: i32,
        device: u32,
        callback: extern "C" fn(*const f32, i32, i32, u64),
    ) -> i32;
    fn hm_audio_stop();
    fn hm_audio_host_ns() -> u64;
    fn hm_audio_set_microphone_enabled(enabled: i32) -> i32;
    fn hm_audio_stop_source(source: i32);
    fn hm_audio_input_device() -> u32;
    fn hm_audio_devices() -> *mut std::ffi::c_char;
    fn hm_audio_free(pointer: *mut std::ffi::c_char);
}
struct Frame {
    source: i32,
    samples: Vec<f32>,
    host: u64,
    fault_code: i32,
}
enum Input {
    Frame(Frame),
    Microphone {
        enabled: bool,
        expected_version: u64,
        reply: SyncSender<Result<(), String>>,
    },
}
static BUS: Mutex<Option<SyncSender<Input>>> = Mutex::new(None);
static NATIVE_GATE: Mutex<()> = Mutex::new(());
static DROPPED: AtomicU64 = AtomicU64::new(0);
extern "C" fn receive(data: *const f32, source: i32, count: i32, host: u64) {
    // Swift owns this buffer only until the callback returns. Copy before enqueue.
    let samples = if !data.is_null() && count > 0 && source >= 0 {
        unsafe { std::slice::from_raw_parts(data, count as usize) }.to_vec()
    } else {
        vec![]
    };
    if let Ok(bus) = BUS.try_lock() {
        if let Some(sender) = bus.as_ref() {
            if sender
                .try_send(Input::Frame(Frame {
                    source,
                    samples,
                    host,
                    fault_code: if source < 0 { count } else { 0 },
                }))
                .is_err()
            {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}
trait CaptureHardware: Send + Sync {
    fn stop_all(&self);
    fn stop_source(&self, source: i32);
    fn microphone(&self, enabled: bool) -> Result<(), String>;
    fn host_ns(&self) -> u64;
}
struct MacHardware;
impl CaptureHardware for MacHardware {
    fn stop_all(&self) {
        let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { hm_audio_stop() };
    }
    fn stop_source(&self, source: i32) {
        let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { hm_audio_stop_source(source) };
    }
    fn microphone(&self, enabled: bool) -> Result<(), String> {
        let _native = NATIVE_GATE.lock().map_err(|_| "原生采集锁异常")?;
        if unsafe { hm_audio_set_microphone_enabled(i32::from(enabled)) } == 0 {
            Ok(())
        } else {
            Err("麦克风无法恢复，请检查权限和原设备；电脑声音继续录制".into())
        }
    }
    fn host_ns(&self) -> u64 {
        unsafe { hm_audio_host_ns() }
    }
}
pub struct CaptureSession {
    pub meeting: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    input: SyncSender<Input>,
    stopping: Arc<AtomicBool>,
    hardware: Arc<dyn CaptureHardware>,
}
impl CaptureSession {
    pub fn stop(mut self) {
        // Native stop drains its callback queue before Rust releases the sender.
        self.stopping.store(true, Ordering::Release);
        self.hardware.stop_all();
        if let Ok(mut bus) = BUS.lock() {
            *bus = None;
        }
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct WavFile {
    file: File,
    pub samples: u32,
}
impl WavFile {
    pub fn create(path: &Path) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(err)?;
        file.write_all(&[0; 44]).map_err(err)?;
        let mut value = Self { file, samples: 0 };
        value.sync()?;
        Ok(value)
    }
    pub fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        self.file.seek(SeekFrom::End(0)).map_err(err)?;
        self.file.write_all(&bytes).map_err(err)?;
        self.samples += samples.len() as u32;
        Ok(())
    }
    pub fn sync(&mut self) -> Result<(), String> {
        write_header(&mut self.file, self.samples)?;
        self.file.sync_all().map_err(err)
    }
}
fn write_header(file: &mut File, samples: u32) -> Result<(), String> {
    let bytes = samples * 2;
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend((bytes + 36).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend(16u32.to_le_bytes());
    h.extend(1u16.to_le_bytes());
    h.extend(1u16.to_le_bytes());
    h.extend(16000u32.to_le_bytes());
    h.extend(32000u32.to_le_bytes());
    h.extend(2u16.to_le_bytes());
    h.extend(16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend(bytes.to_le_bytes());
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    file.write_all(&h).map_err(err)?;
    file.seek(SeekFrom::End(0)).map_err(err)?;
    Ok(())
}
pub fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let raw = std::fs::read(path).map_err(err)?;
    if raw.len() < 12 || &raw[0..4] != b"RIFF" || &raw[8..12] != b"WAVE" {
        return Err("无效 WAV 文件".into());
    }
    let mut position = 12;
    let mut valid = false;
    let mut output = None;
    while position + 8 <= raw.len() {
        let tag = &raw[position..position + 4];
        let size = u32::from_le_bytes(raw[position + 4..position + 8].try_into().unwrap()) as usize;
        let start = position + 8;
        let end = start.saturating_add(size).min(raw.len());
        if tag == b"fmt " && end - start >= 16 {
            valid = u16::from_le_bytes(raw[start..start + 2].try_into().unwrap()) == 1
                && u16::from_le_bytes(raw[start + 2..start + 4].try_into().unwrap()) == 1
                && u32::from_le_bytes(raw[start + 4..start + 8].try_into().unwrap()) == 16000
                && u16::from_le_bytes(raw[start + 14..start + 16].try_into().unwrap()) == 16;
        }
        if tag == b"data" {
            output = Some(&raw[start..end]);
            break;
        }
        position = start.saturating_add(size).saturating_add(size % 2);
    }
    if !valid {
        return Err("仅支持 16kHz 单声道 16bit PCM WAV".into());
    }
    Ok(output
        .ok_or("WAV 缺少音频数据")?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}
pub fn repair(path: &Path) -> Result<u64, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(err)?;
    let len = f.metadata().map_err(err)?.len();
    if len < 44 {
        return Err("WAV 文件头不完整".into());
    }
    let samples = (len - 44) / 2;
    f.set_len(44 + samples * 2).map_err(err)?;
    write_header(&mut f, samples as u32)?;
    f.sync_all().map_err(err)?;
    Ok(samples)
}
struct Track {
    name: &'static str,
    position: u64,
    chunk: Option<(String, u64, WavFile)>,
    clock: AudioClock,
    status: &'static str,
    gap: Option<(String, u64, String)>,
}
impl Track {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            position: 0,
            chunk: None,
            clock: AudioClock::default(),
            status: "active",
            gap: None,
        }
    }
    fn begin_gap(
        &mut self,
        rt: &Runtime,
        meeting: &str,
        at: u64,
        reason: &str,
        state: &'static str,
    ) -> Result<(), String> {
        self.seal(rt, meeting)?;
        self.status = state;
        self.clock.reset();
        self.gap = Some((id(), at, reason.into()));
        rt.db()?.execute("UPDATE recording_sources SET state=?,gap_start_ms=?,reason=? WHERE meeting_id=? AND source=?",params![state,at,reason,meeting,self.name]).map_err(err)?;
        Ok(())
    }
    fn extend_gap(&self, rt: &Runtime, meeting: &str, end: u64) -> Result<(), String> {
        if let Some((gap, start, reason)) = &self.gap {
            if end > *start {
                rt.db()?.execute("INSERT INTO coverage_intervals VALUES (?,?,?,?,?,'missing',?,NULL) ON CONFLICT(id) DO UPDATE SET end_ms=excluded.end_ms",params![gap,meeting,format!("capture_{}",self.name),start,end,reason]).map_err(err)?;
            }
        }
        Ok(())
    }
    fn close_gap(&mut self, rt: &Runtime, meeting: &str, end: u64) -> Result<(), String> {
        self.extend_gap(rt, meeting, end)?;
        self.gap = None;
        self.clock.reset();
        self.status = "active";
        rt.db()?.execute("UPDATE recording_sources SET state='active',gap_start_ms=NULL,reason=NULL WHERE meeting_id=? AND source=?",params![meeting,self.name]).map_err(err)?;
        Ok(())
    }
    fn frame(
        &mut self,
        rt: &Runtime,
        meeting: &str,
        base: u64,
        target: u64,
        samples: &[f32],
    ) -> Result<i64, String> {
        let plan = self
            .clock
            .align(target, samples.len())
            .map_err(|_| "音源时钟发生跳变")?;
        if plan.start_sample > self.position + 1600 || plan.gap.is_some() {
            self.seal(rt, meeting)?;
            let start = base + self.position / 16;
            let end = base + plan.start_sample / 16;
            if end > start {
                rt.db()?.execute("INSERT INTO coverage_intervals VALUES (?,?,?,?,?,'missing','输入未返回数据',NULL)",params![id(),meeting,format!("capture_{}",self.name),start,end]).map_err(err)?;
            }
        }
        // Files contain only actual audio. Gaps exist as timeline metadata, never archived zeros.
        if self.chunk.is_none() {
            self.position = plan.start_sample;
        }
        let aligned = resample_frame(samples, plan.output_samples);
        self.append(rt, meeting, base, &aligned)?;
        Ok(plan.correction_samples)
    }
    fn append(
        &mut self,
        rt: &Runtime,
        meeting: &str,
        base: u64,
        mut samples: &[f32],
    ) -> Result<(), String> {
        while !samples.is_empty() {
            if self.chunk.is_none() {
                let start = base + self.position / 16;
                let chunk = id();
                let rel = format!("meetings/{meeting}/audio/{}-{start}-{chunk}.wav", self.name);
                rt.db()?.execute("INSERT INTO audio_chunks(id,meeting_id,source,relative_path,start_ms,status) VALUES (?,?,?,?,?,'writing')",params![chunk,meeting,self.name,rel,start]).map_err(err)?;
                let wav = WavFile::create(&rt.root.join(rel))?;
                self.chunk = Some((chunk, start, wav));
            }
            let (chunk, _, wav) = self.chunk.as_mut().unwrap();
            let n = (64000 - wav.samples as usize).min(samples.len());
            wav.write(&samples[..n])?;
            self.position += n as u64;
            samples = &samples[n..];
            if wav.samples >= 64000 {
                self.seal(rt, meeting)?;
            } else {
                wav.sync()?;
                rt.db()?
                    .execute(
                        "UPDATE audio_chunks SET sample_count=? WHERE id=?",
                        params![wav.samples, chunk.as_str()],
                    )
                    .map_err(err)?;
            }
        }
        Ok(())
    }
    fn seal(&mut self, rt: &Runtime, meeting: &str) -> Result<(), String> {
        if let Some((chunk, start, mut wav)) = self.chunk.take() {
            wav.sync()?;
            let end = start + u64::from(wav.samples) / 16;
            if end > start {
                let db = rt.db()?;
                db.execute(
                    "UPDATE audio_chunks SET status='sealed',sample_count=?,end_ms=? WHERE id=?",
                    params![wav.samples, end, chunk],
                )
                .map_err(err)?;
                db.execute(
                    "INSERT INTO coverage_intervals VALUES (?,?,?, ?,?,'covered',NULL,NULL)",
                    params![id(), meeting, format!("capture_{}", self.name), start, end],
                )
                .map_err(err)?;
            }
        }
        Ok(())
    }
}
pub fn available_bytes(path: &Path) -> Result<u64, String> {
    let name =
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "磁盘路径无效")?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs initializes the provided structure only on success.
    unsafe {
        if libc::statvfs(name.as_ptr(), info.as_mut_ptr()) != 0 {
            return Err("无法查询磁盘空间".into());
        }
        let info = info.assume_init();
        Ok(u64::from(info.f_bavail).saturating_mul(info.f_frsize))
    }
}
pub fn start(
    rt: Arc<Runtime>,
    meeting: &str,
    mask: i32,
    device: u32,
    confirm_microphone: bool,
) -> Result<(), String> {
    if !(1..=3).contains(&mask) {
        return Err("请选择至少一个音源".into());
    }
    rt.check_id(meeting)?;
    if available_bytes(&rt.root)? < 200_000_000 {
        return Err("磁盘剩余空间不足 200MB，请先释放空间".into());
    }
    let mut guard = rt.capture.lock().map_err(|_| "采集锁异常")?;
    if guard
        .as_ref()
        .is_some_and(|s| s.meeting == meeting && s.worker.as_ref().is_some_and(|w| w.is_finished()))
    {
        if let Some(old) = guard.take() {
            old.stop();
        }
    }
    if guard.is_some() {
        return Err("已有会议正在录制".into());
    }
    let (state, base): (String, u64) = rt
        .db()?
        .query_row(
            "SELECT state,recording_duration_ms FROM meetings WHERE id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    if !matches!(state.as_str(), "ready" | "paused" | "interrupted") {
        return Err("当前会议不能开始录制".into());
    }
    let previous: Option<(u32, String)> = rt
        .db()?
        .query_row(
            "SELECT device,state FROM recording_sources WHERE meeting_id=? AND source='microphone'",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(err)?;
    let device = if state != "ready" {
        meeting_core::capture::resume_input(
            mask,
            device,
            previous.as_ref().map_or(0, |p| p.0),
            previous.as_ref().is_some_and(|p| p.1 == "disabled"),
            confirm_microphone,
        )
        .map_err(str::to_string)?
    } else {
        device
    };
    let active:u32=rt.db()?.query_row("SELECT count(*) FROM meetings WHERE state IN ('recording','paused','interrupted') AND id!=?",[meeting],|r|r.get(0)).map_err(err)?;
    if active > 0 {
        return Err("请先结束另一场会议".into());
    }
    std::fs::create_dir_all(rt.meeting_dir(meeting).join("audio")).map_err(err)?;
    let (tx, rx) = sync_channel::<Input>(256);
    let input = tx.clone();
    *BUS.lock().map_err(|_| "音频通道锁异常")? = Some(tx);
    DROPPED.store(0, Ordering::Relaxed);
    let code = {
        let _native = NATIVE_GATE.lock().map_err(|_| "原生采集锁异常")?;
        unsafe { hm_audio_start(mask, device, receive) }
    };
    if code != 0 {
        {
            let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
            unsafe { hm_audio_stop() };
        }
        *BUS.lock().map_err(|_| "音频通道异常")? = None;
        return Err(match code {
            1 => "麦克风权限未授权，请在系统设置中开启",
            3 => "系统声音采集失败，请检查屏幕录制权限",
            _ => "音源不可用，所有已启动采集已回滚",
        }
        .into());
    }
    let first = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Input::Frame(frame)) => frame,
        Ok(_) => unreachable!(),
        Err(_) => {
            {
                let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
                unsafe { hm_audio_stop() };
            }
            *BUS.lock().map_err(|_| "音频通道异常")? = None;
            return Err("音源启动后未收到音频".into());
        }
    };
    if first.source < 0 {
        {
            let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
            unsafe { hm_audio_stop() };
        }
        *BUS.lock().map_err(|_| "音频通道异常")? = None;
        return Err("音源初始化失败".into());
    }
    let origin = first.host;
    let persist = (|| -> Result<(), String> {
        let mut db = rt.db()?;
        let tx = db.transaction().map_err(err)?;
        tx.execute("UPDATE meetings SET state='recording',wall_start=coalesce(wall_start,?),last_error=NULL,source_mask=?,version=version+1 WHERE id=?",params![now(),mask,meeting]).map_err(err)?;
        tx.execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES (?,?,?,?,'recording')",params![id(),meeting,base,now()]).map_err(err)?;
        tx.execute(
            "DELETE FROM recording_sources WHERE meeting_id=?",
            [meeting],
        )
        .map_err(err)?;
        for (bit, name) in [(1, "microphone"), (2, "system")] {
            if mask & bit != 0 {
                tx.execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,?,'active',?)",params![meeting,name,if bit==1 {unsafe {hm_audio_input_device()}}else{0}]).map_err(err)?;
            }
        }
        tx.commit().map_err(err)?;
        Ok(())
    })();
    if let Err(error) = persist {
        {
            let _native = NATIVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
            unsafe {
                hm_audio_stop();
            }
        }
        *BUS.lock().map_err(|_| "音频通道异常")? = None;
        return Err(error);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let stopping = Arc::new(AtomicBool::new(false));
    let thread_stopping = stopping.clone();
    let owner = rt.clone();
    let mid = meeting.to_string();
    let hardware: Arc<dyn CaptureHardware> = Arc::new(MacHardware);
    let worker = Writer {
        owner,
        mid,
        mask,
        base,
        origin,
        first,
        rx,
        thread_stop,
        thread_stopping,
        hardware: hardware.clone(),
    }
    .spawn();
    *guard = Some(CaptureSession {
        meeting: meeting.into(),
        stop,
        worker: Some(worker),
        input,
        stopping,
        hardware,
    });
    Ok(())
}
struct Writer {
    owner: Arc<Runtime>,
    mid: String,
    mask: i32,
    base: u64,
    origin: u64,
    first: Frame,
    rx: Receiver<Input>,
    thread_stop: Arc<AtomicBool>,
    thread_stopping: Arc<AtomicBool>,
    hardware: Arc<dyn CaptureHardware>,
}
impl Writer {
    fn spawn(self) -> thread::JoinHandle<()> {
        thread::spawn(move || self.run())
    }
    fn run(self) {
        let Self {
            owner,
            mid,
            mask,
            base,
            origin,
            first,
            rx,
            thread_stop,
            thread_stopping,
            hardware,
        } = self;
        let mut tracks = HashMap::new();
        if mask & 1 != 0 {
            tracks.insert(0, Track::new("microphone"));
        }
        if mask & 2 != 0 {
            tracks.insert(1, Track::new("system"));
        }
        let mut pending = Some(Input::Frame(first));
        let mut maximum = 0u64;
        let mut failed = false;
        let mut disk_check = std::time::Instant::now();
        loop {
            let item = pending
                .take()
                .or_else(|| rx.recv_timeout(Duration::from_millis(100)).ok());
            if let Some(Input::Microphone {
                enabled,
                expected_version,
                reply,
            }) = item
            {
                let mut native_changed = false;
                let result = (|| -> Result<(), String> {
                    let track = tracks
                        .get_mut(&0)
                        .ok_or("本场没有启用麦克风，请暂停后选择音源")?;
                    let mut db = owner.db()?;
                    let tx = db.transaction().map_err(err)?;
                    let (version, state): (u64, String) = tx
                        .query_row(
                            "SELECT version,state FROM meetings WHERE id=?",
                            [&mid],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map_err(err)?;
                    if version != expected_version || state != "recording" {
                        return Err("会议版本已变化，请刷新后重试麦克风控制".into());
                    }
                    if enabled == (track.status == "active") {
                        return Ok(());
                    }
                    // Native stop removes the input tap and drains its conversion queue.
                    hardware.microphone(enabled)?;
                    native_changed = true;
                    tx.execute("UPDATE meetings SET version=version+1 WHERE id=?", [&mid])
                        .map_err(err)?;
                    tx.commit().map_err(err)?;
                    drop(db);
                    let at = base + hardware.host_ns().saturating_sub(origin) / 1_000_000;
                    if enabled {
                        track.close_gap(&owner, &mid, at)?;
                        track.position = at.saturating_sub(base) * 16;
                    } else {
                        track.begin_gap(
                            &owner,
                            &mid,
                            base + track.position / 16,
                            "用户主动关闭麦克风",
                            "disabled",
                        )?;
                    }
                    Ok(())
                })();
                if native_changed && result.is_err() {
                    owner.error(&mid, "音源控制后持久化失败，录制已中断以核对状态");
                    failed = true;
                }
                let _ = reply.send(result);
                if failed {
                    break;
                }
            } else if let Some(Input::Frame(frame)) = item {
                if frame.source < 0 {
                    if thread_stopping.load(Ordering::Acquire) {
                        continue;
                    }
                    if frame.source == -1 {
                        owner.error(&mid, "系统睡眠或全局采集中断");
                        failed = true;
                        break;
                    }
                    let source = if frame.source == -2 { 0 } else { 1 };
                    if let Some(track) = tracks.get_mut(&source) {
                        if track.status == "active" {
                            hardware.stop_source(source);
                            let reason = if frame.fault_code == 5 {
                                "音源缓冲溢出"
                            } else if frame.fault_code == 6 {
                                "音源格式转换失败"
                            } else {
                                "音源或设备失效"
                            };
                            if track
                                .begin_gap(
                                    &owner,
                                    &mid,
                                    base + track.position / 16,
                                    reason,
                                    "failed",
                                )
                                .is_err()
                            {
                                failed = true;
                                break;
                            }
                            owner.error(&mid,"部分录制：一条音源失效，另一条已选音源继续。请检查设备；恢复系统声音需暂停后继续。");
                        }
                    }
                } else if let Some(track) = tracks.get_mut(&frame.source) {
                    if track.status == "active" && !frame.samples.is_empty() {
                        let target = frame.host.saturating_sub(origin) / 62500;
                        match track.frame(&owner, &mid, base, target, &frame.samples) {
                            Ok(correction) => {
                                maximum = maximum.max(track.position / 16);
                                let rms = (frame.samples.iter().map(|v| v * v).sum::<f32>()
                                    / frame.samples.len() as f32)
                                    .sqrt();
                                if let Ok(mut state) = owner.transient.lock() {
                                    state[&mid]["levels"][track.name] = json!(rms);
                                    state[&mid]["clock_correction_samples"][track.name] =
                                        json!(correction);
                                }
                            }
                            Err(e) if e == "音源时钟发生跳变" => {
                                hardware.stop_source(frame.source);
                                if track
                                    .begin_gap(
                                        &owner,
                                        &mid,
                                        base + track.position / 16,
                                        &e,
                                        "failed",
                                    )
                                    .is_err()
                                {
                                    failed = true;
                                    break;
                                }
                                owner.error(&mid,"部分录制：一路音源时钟异常，其他已选音源继续。请暂停后检查路由。");
                            }
                            Err(e) => {
                                owner.error(&mid, &e);
                                failed = true;
                                break;
                            }
                        }
                    }
                }
                if tracks.values().all(|t| t.status != "active")
                    && tracks.values().any(|t| t.status == "failed")
                {
                    owner.error(&mid, "所有可用音源已失效，录音已中断");
                    failed = true;
                    break;
                }
            } else if thread_stop.load(Ordering::Acquire) {
                break;
            }
            // A deliberately disabled mic must not freeze the system track or effective clock.
            if tracks.values().all(|t| t.status != "active") {
                maximum = maximum.max(hardware.host_ns().saturating_sub(origin) / 1_000_000);
            }
            if disk_check.elapsed() > Duration::from_secs(1) {
                disk_check = std::time::Instant::now();
                for track in tracks.values() {
                    if let Err(e) = track.extend_gap(&owner, &mid, base + maximum) {
                        owner.error(&mid, &e);
                        failed = true;
                    }
                }
                if let Ok(db) = owner.db() {
                    let _ = db.execute(
                        "UPDATE meetings SET recording_duration_ms=? WHERE id=?",
                        params![base + maximum, mid],
                    );
                }
                if let Ok(bytes) = available_bytes(&owner.root) {
                    if bytes < 200_000_000 {
                        owner.error(&mid, "磁盘空间不足，录音已安全停止");
                        failed = true;
                    } else if bytes < 1_000_000_000 {
                        owner.error(&mid, "磁盘空间低于 1GB，请尽快释放空间");
                    }
                }
                if failed {
                    break;
                }
            }
            if DROPPED.load(Ordering::Relaxed) > 0 {
                owner.error(&mid, "音频队列溢出，录音已中断以避免无提示缺段");
                failed = true;
                break;
            }
        }
        if failed {
            hardware.stop_all();
            if let Ok(mut bus) = BUS.lock() {
                *bus = None;
            }
        }
        for track in tracks.values_mut() {
            if let Err(e) = track
                .seal(&owner, &mid)
                .and_then(|_| track.extend_gap(&owner, &mid, base + maximum))
            {
                owner.error(&mid, &e);
                failed = true;
            }
        }
        if let Ok(db) = owner.db() {
            let _ = db.execute(
                "UPDATE meetings SET recording_duration_ms=? WHERE id=?",
                params![base + maximum, mid],
            );
            let _=db.execute("UPDATE recording_epochs SET recording_end_ms=?,wall_end=? WHERE meeting_id=? AND wall_end IS NULL",params![base+maximum,now(),mid]);
            let _=db.execute("UPDATE recording_sources SET state=CASE WHEN state='active' THEN 'stopped' ELSE state END WHERE meeting_id=?",[&mid]);
            if failed {
                let _ = db.execute(
                    "UPDATE meetings SET state='interrupted',version=version+1 WHERE id=?",
                    [&mid],
                );
            }
        };
    }
}
pub fn set_microphone_enabled(
    rt: &Runtime,
    meeting: &str,
    expected_version: u64,
    enabled: bool,
) -> Result<(), String> {
    rt.check_id(meeting)?;
    let guard = rt.capture.lock().map_err(|_| "采集锁异常")?;
    let session = guard
        .as_ref()
        .filter(|s| s.meeting == meeting && s.worker.as_ref().is_some_and(|w| !w.is_finished()))
        .ok_or("本场没有活动录制")?;
    let (reply, result) = sync_channel(1);
    session
        .input
        .send(Input::Microphone {
            enabled,
            expected_version,
            reply,
        })
        .map_err(|_| "采集线程已停止")?;
    result
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "麦克风控制超时，请核对实际采集状态")?
}
pub fn preview(rt: &Runtime, mask: i32, device: u32) -> Result<serde_json::Value, String> {
    if !(1..=3).contains(&mask) {
        return Err("请选择音源".into());
    }
    let guard = rt.capture.lock().map_err(|_| "采集锁异常")?;
    if guard.is_some() {
        return Err("录制中请查看会议内的实时电平".into());
    }
    let (tx, rx) = sync_channel(256);
    *BUS.lock().map_err(|_| "音频通道锁异常")? = Some(tx);
    let code = {
        let _native = NATIVE_GATE.lock().map_err(|_| "原生采集锁异常")?;
        unsafe { hm_audio_start(mask, device, receive) }
    };
    let result = if code != 0 {
        Err("音源预览无法启动，请检查麦克风与系统音频权限".into())
    } else {
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let mut levels = serde_json::json!({});
        let mut error = None;
        while std::time::Instant::now() < until {
            if let Ok(Input::Frame(f)) = rx.recv_timeout(Duration::from_millis(100)) {
                if f.source < 0 {
                    error = Some("音源预览中断，请检查设备".to_string());
                    break;
                }
                let name = if f.source == 0 {
                    "microphone"
                } else {
                    "system"
                };
                let rms = (f.samples.iter().map(|v| v * v).sum::<f32>()
                    / f.samples.len().max(1) as f32)
                    .sqrt();
                levels[name] = json!(levels[name].as_f64().unwrap_or(0.0).max(rms as f64));
            }
        }
        error.map_or_else(|| Ok(json!({"levels":levels,"seconds":3})), Err)
    };
    MacHardware.stop_all();
    *BUS.lock().map_err(|_| "音频通道锁异常")? = None;
    result
}
pub fn control(rt: &Runtime, meeting: &str, action: &str) -> Result<(), String> {
    rt.check_id(meeting)?;
    let mut guard = rt.capture.lock().map_err(|_| "采集锁异常")?;
    if let Some(session) = guard.as_ref() {
        if session.meeting != meeting {
            return Err("另一场会议正在录制".into());
        }
    }
    if let Some(session) = guard.take() {
        session.stop();
    }
    let db = rt.db()?;
    let state: String = db
        .query_row("SELECT state FROM meetings WHERE id=?", [meeting], |r| {
            r.get(0)
        })
        .map_err(err)?;
    let next = match action {
        "pause" if state == "recording" => "paused",
        "stop" if matches!(state.as_str(), "recording" | "paused" | "interrupted") => "finalizing",
        _ => return Err("当前状态不支持此操作".into()),
    };
    db.execute(
        "UPDATE meetings SET state=?,version=version+1 WHERE id=?",
        params![next, meeting],
    )
    .map_err(err)?;
    Ok(())
}
pub fn devices() -> Result<serde_json::Value, String> {
    // The Swift allocation is freed by the matching Swift/Darwin function.
    unsafe {
        let ptr = hm_audio_devices();
        if ptr.is_null() {
            return Err("无法枚举音频输入设备".into());
        }
        let text = std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned();
        hm_audio_free(ptr);
        serde_json::from_str(&text).map_err(|_| "设备列表格式异常".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wav_roundtrip_and_repair() {
        let path = std::env::temp_dir().join(format!("{}.wav", id()));
        let mut wav = WavFile::create(&path).unwrap();
        wav.write(&[0.0, 0.5, -0.5]).unwrap();
        drop(wav);
        assert_eq!(repair(&path).unwrap(), 3);
        let samples = read_wav(&path).unwrap();
        assert_eq!(samples.len(), 3);
        assert!((samples[1] - 0.5).abs() < 0.001);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::models::tests::MemoryVault;
    #[derive(Default)]
    struct FakeHardware {
        clock: AtomicU64,
        mic: Mutex<Vec<bool>>,
        stopped: Mutex<Vec<i32>>,
    }
    impl CaptureHardware for FakeHardware {
        fn stop_all(&self) {}
        fn stop_source(&self, source: i32) {
            self.stopped.lock().unwrap().push(source);
        }
        fn microphone(&self, enabled: bool) -> Result<(), String> {
            self.mic.lock().unwrap().push(enabled);
            Ok(())
        }
        fn host_ns(&self) -> u64 {
            self.clock.load(Ordering::Relaxed)
        }
    }
    fn frame(source: i32, host: u64, n: usize, value: f32) -> Input {
        Input::Frame(Frame {
            source,
            host,
            samples: vec![value; n],
            fault_code: 0,
        })
    }
    fn writer() -> (
        std::path::PathBuf,
        Arc<Runtime>,
        String,
        Arc<FakeHardware>,
        SyncSender<Input>,
    ) {
        let root = std::env::temp_dir().join(format!("helmmeet-writer-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        let m = rt.create("合成双轨", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::create_dir_all(rt.meeting_dir(&m).join("audio")).unwrap();
        {
            let db = rt.db().unwrap();
            db.execute(
                "UPDATE meetings SET state='recording',source_mask=3 WHERE id=?",
                [&m],
            )
            .unwrap();
            for name in ["microphone", "system"] {
                db.execute(
                    "INSERT INTO recording_sources(meeting_id,source,state) VALUES (?,?,'active')",
                    params![m, name],
                )
                .unwrap();
            }
        }
        let hardware = Arc::new(FakeHardware::default());
        let (tx, rx) = sync_channel(256);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let worker = Writer {
            owner: rt.clone(),
            mid: m.clone(),
            mask: 3,
            base: 0,
            origin: 0,
            first: Frame {
                source: 0,
                host: 0,
                samples: vec![0.1; 1600],
                fault_code: 0,
            },
            rx,
            thread_stop: stop.clone(),
            thread_stopping: stopping.clone(),
            hardware: hardware.clone(),
        }
        .spawn();
        *rt.capture.lock().unwrap() = Some(CaptureSession {
            meeting: m.clone(),
            stop,
            worker: Some(worker),
            input: tx.clone(),
            stopping,
            hardware: hardware.clone(),
        });
        (root, rt, m, hardware, tx)
    }
    fn wait(mut f: impl FnMut() -> bool) {
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while !f() {
            assert!(std::time::Instant::now() < until, "writer did not settle");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn finish(rt: &Runtime) {
        rt.capture.lock().unwrap().take().unwrap().stop();
    }
    #[test]
    fn microphone_mute_discards_frames_keeps_system_and_preserves_timeline() {
        let (root, rt, m, hardware, input) = writer();
        assert!(set_microphone_enabled(&rt, &m, 999, false).is_err());
        assert!(hardware.mic.lock().unwrap().is_empty());
        let version = rt.detail(&m).unwrap()["version"].as_u64().unwrap();
        set_microphone_enabled(&rt, &m, version, false).unwrap();
        input.send(frame(0, 1_000_000_000, 16000, 0.9)).unwrap(); // An errant post-mute callback must not be archived.
        input.send(frame(1, 0, 64000, 0.2)).unwrap();
        wait(|| {
            rt.db().unwrap().query_row("SELECT count(*) FROM audio_chunks WHERE meeting_id=? AND source='system' AND status='sealed'",[&m],|r|r.get::<_,u32>(0)).unwrap()==1
        });
        hardware.clock.store(4_000_000_000, Ordering::Relaxed);
        let v = rt.detail(&m).unwrap()["version"].as_u64().unwrap();
        set_microphone_enabled(&rt, &m, v, true).unwrap();
        input.send(frame(0, 4_000_000_000, 1600, 0.3)).unwrap();
        finish(&rt);
        let db = rt.db().unwrap();
        let samples:u64=db.query_row("SELECT sum(sample_count) FROM audio_chunks WHERE meeting_id=? AND source='microphone'",[&m],|r|r.get(0)).unwrap();
        assert_eq!(samples, 3200);
        let gap:(u64,u64,String)=db.query_row("SELECT start_ms,end_ms,reason FROM coverage_intervals WHERE meeting_id=? AND kind='capture_microphone' AND state='missing'",[&m],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!((gap.0, gap.1), (100, 4000));
        assert!(gap.2.contains("主动关闭"));
        drop(db);
        let absent = crate::asr::mix_source(&rt, &m, 1000, 2000, Some("microphone")).unwrap();
        assert!(absent.iter().all(|v| *v == 0.0));
        let resumed = crate::asr::mix_source(&rt, &m, 4000, 4100, Some("microphone")).unwrap();
        assert!(resumed.iter().all(|v| (*v - 0.3).abs() < 0.001));
        assert_eq!(*hardware.mic.lock().unwrap(), vec![false, true]);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn failed_source_does_not_stop_the_other_track() {
        let (root, rt, m, hardware, input) = writer();
        input
            .send(Input::Frame(Frame {
                source: -2,
                host: 0,
                samples: vec![],
                fault_code: 7,
            }))
            .unwrap();
        input.send(frame(1, 0, 64000, 0.2)).unwrap();
        wait(|| {
            rt.db().unwrap().query_row("SELECT count(*) FROM audio_chunks WHERE meeting_id=? AND source='system' AND status='sealed'",[&m],|r|r.get::<_,u32>(0)).unwrap()==1
        });
        assert_eq!(rt.detail(&m).unwrap()["state"], "recording");
        assert_eq!(*hardware.stopped.lock().unwrap(), vec![0]);
        finish(&rt);
        let d = rt.detail(&m).unwrap();
        assert!(d["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source"] == "microphone" && s["state"] == "failed"));
        assert!(d["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "capture_microphone"
                && s["state"] == "missing"
                && s["end_ms"] == 4000));
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn two_failed_sources_interrupt_the_meeting() {
        let (root, rt, m, _, input) = writer();
        for source in [-2, -3] {
            input
                .send(Input::Frame(Frame {
                    source,
                    host: 0,
                    samples: vec![],
                    fault_code: 7,
                }))
                .unwrap();
        }
        wait(|| rt.detail(&m).unwrap()["state"] == "interrupted");
        finish(&rt);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod model_capture_tests {
    use super::*;
    // The real writer/IPC are exercised above with injected hardware. A model switch
    // must be independent of both recording state and selected audio source rows.
    #[test]
    fn model_switch_leaves_recording_state_and_source_selection_unchanged() {
        let root = std::env::temp_dir().join(format!("helmmeet-switch-audio-{}", id()));
        let rt = Runtime::open_with_vault(
            root.clone(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let profile = |name: &str| {
            crate::models::save(&rt,serde_json::from_value(json!({"profile_id":null,"expected_version":null,"display_name":name,"base_url":"https://example.test","model":name,"context_tokens":32768,"max_output_tokens":4096,"timeout_seconds":120,"key_action":"clear"})).unwrap()).unwrap()
        };
        let a = profile("A");
        let b = profile("B");
        let m = rt.create("切换时录音独立", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        {
            let db = rt.db().unwrap();
            db.execute(
                "UPDATE meetings SET state='recording',source_mask=3 WHERE id=?",
                [&m],
            )
            .unwrap();
            for source in ["microphone", "system"] {
                db.execute(
                    "INSERT INTO recording_sources(meeting_id,source,state) VALUES (?,?,'active')",
                    params![m, source],
                )
                .unwrap();
            }
        }
        for p in [a, b] {
            let v = rt.detail(&m).unwrap()["version"].as_u64().unwrap();
            crate::models::authorize(
                &rt,
                &m,
                v,
                false,
                0,
                Some((p["profile_id"].as_str().unwrap(), 1)),
            )
            .unwrap();
        }
        let d = rt.detail(&m).unwrap();
        assert_eq!(d["state"], "recording");
        assert_eq!(d["source_mask"], 3);
        assert!(d["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["state"] == "active"));
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
