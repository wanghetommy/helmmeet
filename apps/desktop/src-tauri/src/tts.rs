//! First version of the Volcengine TTS adapter.
//! Credentials are borrowed from a saved text-model profile and never cross IPC.
use crate::{
    db::{err, Runtime},
    models,
};
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_MODEL: &str = "seed-tts-2.0-standard";
const DEFAULT_SPEAKER: &str = "zh_female_gaolengyujie_uranus_bigtts";
const ENDPOINT: &str = "https://openspeech.bytedance.com/api/v3/plan/tts/unidirectional";

pub fn saved(rt: &Runtime) -> Result<Value, String> {
    let raw: String = rt
        .db()?
        .query_row(
            "SELECT value FROM app_preferences WHERE key='tts_settings'",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    serde_json::from_str(&raw).map_err(err)
}
pub fn settings(rt: &Runtime) -> Result<Value, String> {
    let mut value = saved(rt)?;
    if let (Some(profile), Some(version)) = (
        value["profile_id"].as_str(),
        value["config_version"].as_u64(),
    ) {
        if let Ok(config) = models::load(rt, profile, version as u32) {
            value["profile_name"] = json!(config.display_name);
            value["credential_valid"] =
                json!(models::secret(rt, &config).is_ok_and(|v| v.is_some()));
        } else {
            value["credential_valid"] = json!(false);
        }
    } else {
        value["credential_valid"] = json!(false);
    }
    Ok(value)
}

pub fn validate_settings(rt: &Runtime, input: &Value) -> Result<Value, String> {
    let enabled = input["enabled"].as_bool().ok_or("TTS开关无效")?;
    let model = input["model"].as_str().unwrap_or(DEFAULT_MODEL).trim();
    let speaker = input["speaker"].as_str().unwrap_or(DEFAULT_SPEAKER).trim();
    if model.is_empty() || model.len() > 120 || speaker.is_empty() || speaker.len() > 200 {
        return Err("TTS模型或音色无效".into());
    }
    let profile = input["profile_id"].as_str().filter(|p| !p.is_empty());
    let version = input["config_version"].as_u64().map(|v| v as u32);
    if enabled {
        let profile = profile.ok_or("请选择用于TTS的文本模型凭据")?;
        let version = version.ok_or("文本模型版本无效")?;
        let config = models::load(rt, profile, version)?;
        if models::secret(rt, &config)?.is_none() {
            return Err("所选文本模型没有可用API Key".into());
        }
    }
    let value = json!({"enabled":enabled,"profile_id":profile,"config_version":version,"model":model,"speaker":speaker,"fallback_local":input["fallback_local"].as_bool().unwrap_or(true)});
    Ok(value)
}

fn decode_audio_stream(raw: &str) -> Result<Vec<f32>, String> {
    let mut offset = 0;
    let mut bytes = Vec::new();
    let mut finished = false;
    let decoder = serde_json::Deserializer::from_str(raw).into_iter::<Value>();
    for value in decoder {
        let value = value.map_err(|_| "TTS返回格式无效")?;
        let code = value["code"].as_i64().unwrap_or_default();
        if code != 0 && code != 20_000_000 {
            return Err(value["message"]
                .as_str()
                .map_or_else(|| format!("TTS服务错误码 {code}"), str::to_owned));
        }
        if let Some(encoded) = value["data"]
            .as_str()
            .or_else(|| value["payload"]["data"].as_str())
        {
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| "TTS音频编码无效")?,
            );
        }
        if code == 20_000_000 {
            finished = true;
        }
        offset += 1;
    }
    if offset == 0 || !finished {
        return Err("TTS返回格式无效".into());
    }
    if bytes.is_empty() {
        return Err("TTS未返回音频数据".into());
    }
    if bytes.len() % 2 != 0 {
        return Err("TTS PCM长度无效".into());
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    Ok(pairs
        .iter()
        .map(|p| i16::from_le_bytes(*p) as f32 / 32768.0)
        .collect())
}

pub fn synthesize_cancellable(
    rt: &Runtime,
    text: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<f32>, String> {
    check()?;
    let config = saved(rt)?;
    synthesize_snapshot(rt, &config, text, check)
}

pub fn synthesize_snapshot(
    rt: &Runtime,
    config: &Value,
    text: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<f32>, String> {
    check()?;
    if config["enabled"] != true {
        return Err("TTS API未启用".into());
    }
    let profile = config["profile_id"].as_str().ok_or("TTS凭据未配置")?;
    let version = config["config_version"]
        .as_u64()
        .ok_or("TTS文本模型版本无效")? as u32;
    let model = models::load(rt, profile, version)?;
    let key = models::secret(rt, &model)?.ok_or("TTS凭据不可用")?;
    synthesize_config_cancellable(config, &key, text, check)
}

#[cfg(test)]
fn synthesize_config(config: &Value, key: &str, text: &str) -> Result<Vec<f32>, String> {
    synthesize_config_cancellable(config, key, text, &|| Ok(()))
}

fn synthesize_config_cancellable(
    config: &Value,
    key: &str,
    text: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<f32>, String> {
    synthesize_at(ENDPOINT, config, key, text, check)
}

fn synthesize_at(
    endpoint: &str,
    config: &Value,
    key: &str,
    text: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<f32>, String> {
    let body = json!({"req_params":{"text":text,"model":config["model"].as_str().unwrap_or(DEFAULT_MODEL),"speaker":config["speaker"].as_str().unwrap_or(DEFAULT_SPEAKER),"audio_params":{"format":"pcm","sample_rate":16000}}});
    let response = tauri::async_runtime::block_on(async {
        let exchange = async {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| "TTS HTTP初始化失败".to_string())?
                .post(endpoint)
                .header("X-Api-Key", key)
                .header("X-Api-Resource-Id", "seed-tts-2.0")
                .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
                .json(&body)
                .send()
                .await
                .map_err(|_| "TTS服务连接失败".to_string())?
                .error_for_status()
                .map_err(|e| format!("TTS HTTP错误 {}", e.status().map_or(0, |v| v.as_u16())))?
                .bytes()
                .await
                .map_err(|_| "TTS响应读取失败".to_string())
        };
        let mut exchange = Box::pin(exchange);
        loop {
            check()?;
            if let Ok(result) =
                tokio::time::timeout(Duration::from_millis(50), exchange.as_mut()).await
            {
                return result;
            }
        }
    })?;
    check()?;
    let raw = String::from_utf8(response.to_vec()).map_err(|_| "TTS返回不是JSON")?;
    decode_audio_stream(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_http_sends_a_complete_sentence_and_keeps_terminal_audio() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            let (start, length) = loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while request.len() < start + length {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&request[start..start + length]).unwrap();
            assert_eq!(body["req_params"]["text"], "我在。");
            assert_eq!(body["req_params"]["speaker"], "synthetic-voice");
            assert_eq!(body["req_params"]["audio_params"]["sample_rate"], 16000);
            let first = base64::engine::general_purpose::STANDARD.encode(8192_i16.to_le_bytes());
            let last = base64::engine::general_purpose::STANDARD.encode(16384_i16.to_le_bytes());
            let response = format!(
                "{{\"code\":0,\"data\":\"{first}\"}}\n{{\"code\":20000000,\"data\":\"{last}\"}}"
            );
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        });
        let pcm = synthesize_at(
            &endpoint,
            &json!({"speaker":"synthetic-voice"}),
            "synthetic-key",
            crate::wake_ack::TEXT,
            &|| Ok(()),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(pcm, vec![0.25, 0.5]);
    }

    #[test]
    fn cancellation_drops_an_inflight_http_exchange_without_waiting_for_tts_timeout() {
        use std::{
            io::Read,
            net::TcpListener,
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
            time::Instant,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let (done, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 4096];
            assert!(stream.read(&mut bytes).unwrap() > 0);
            flag.store(true, Ordering::Release);
            received.recv_timeout(Duration::from_secs(2)).unwrap();
            // No response is sent: cancellation must work while HTTP is pending.
        });
        let began = Instant::now();
        let error = synthesize_at(
            &endpoint,
            &json!({}),
            "synthetic-key",
            "合成测试",
            &|| {
                if cancelled.load(Ordering::Acquire) {
                    Err("synthetic-cancelled".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error, "synthetic-cancelled");
        assert!(began.elapsed() < Duration::from_secs(1));
        done.send(()).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn decodes_all_json_stream_audio_frames() {
        let bytes = [0_i16, 16384_i16]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let raw = format!(
            "{{\"code\":0,\"data\":\"{}\"}}{{\"code\":20000000,\"usage\":{{\"text_words\":2}}}}",
            encoded
        );
        let pcm = decode_audio_stream(&raw).unwrap();
        assert_eq!(pcm.len(), 2);
        assert_eq!(pcm[1], 0.5);
    }

    #[test]
    fn exposes_tts_service_error_message() {
        let error = decode_audio_stream(r#"{"code":55000000,"message":"resource ID mismatch"}"#)
            .unwrap_err();
        assert_eq!(error, "resource ID mismatch");
    }

    fn saved_real_profile() -> (Value, String) {
        // Read-only diagnostic: never open Runtime against the user's database,
        // since startup recovery mutates meeting state. Credentials stay in memory.
        let root = std::path::PathBuf::from(
            std::env::var_os("HELMMEET_TTS_CHECK_DATA_ROOT").expect("explicit TTS data root"),
        );
        let db = rusqlite::Connection::open_with_flags(
            root.join("app.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let raw: String = db
            .query_row(
                "SELECT value FROM app_preferences WHERE key='tts_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let settings: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(settings["enabled"], true);
        let reference: String = db
            .query_row(
                "SELECT credential_ref FROM model_configs WHERE id=? AND version=?",
                rusqlite::params![
                    settings["profile_id"].as_str(),
                    settings["config_version"].as_u64()
                ],
                |r| r.get(0),
            )
            .unwrap();
        let credentials: Value =
            serde_json::from_slice(&std::fs::read(root.join("credentials.json")).unwrap()).unwrap();
        let key = credentials["api_keys"][&reference]
            .as_str()
            .expect("saved credential")
            .to_owned();
        (settings, key)
    }

    #[test]
    #[ignore = "explicit saved credentials; two fixed punctuated wake voices only, no playback/capture"]
    fn real_saved_punctuated_wake_voices_without_playback() {
        let (mut settings, key) = saved_real_profile();
        let root = std::path::PathBuf::from(
            std::env::var_os("HELMMEET_WAKE_AUDIT_OUTPUT")
                .expect("explicit temporary audio output"),
        );
        std::fs::create_dir_all(&root).unwrap();
        for speaker in [
            "zh_female_xiaohe_uranus_bigtts",
            "zh_female_kefunvsheng_uranus_bigtts",
        ] {
            settings["speaker"] = json!(speaker);
            let pcm = synthesize_config(&settings, &key, crate::wake_ack::TEXT).unwrap();
            let bytes: Vec<u8> = pcm
                .iter()
                .flat_map(|v| ((v * 32768.).round().clamp(-32768., 32767.) as i16).to_le_bytes())
                .collect();
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(format!("{speaker}.pcm")))
                .unwrap();
            file.write_all(&bytes).unwrap();
            println!(
                "Punctuated wake {speaker}: {} samples, {:.3}s",
                pcm.len(),
                pcm.len() as f64 / 16000.
            );
        }
    }

    #[test]
    #[ignore = "explicit saved TTS credentials; paid fixed 我在 synthesis only, no playback or capture"]
    fn real_saved_wake_response_http_without_playback() {
        let (settings, key) = saved_real_profile();
        let pcm = synthesize_config(&settings, &key, crate::wake_ack::TEXT).unwrap();
        assert!((1600..=80000).contains(&pcm.len()));
        assert!(pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.));
        assert!(pcm.iter().any(|v| v.abs() > 0.001));
        println!(
            "Fixed wake response: {} samples, {:.3}s; no playback/capture or user-data writes",
            pcm.len(),
            pcm.len() as f64 / 16000.
        );
    }

    #[test]
    #[ignore = "explicit HELMMEET_TTS_CHECK_DATA_ROOT; real paid TTS, synthetic text only, no playback"]
    fn real_saved_profile_http_pcm_without_playback() {
        let (settings, key) = saved_real_profile();
        let pcm = synthesize_config(
            &settings,
            &key,
            "你好，这是会议助手的语音合成测试。我们正在检查较长回答的音频是否完整返回。",
        )
        .unwrap();
        assert!(
            pcm.len() > 16000 * 4,
            "Must cover the old four-second C ABI rejection"
        );
        assert!(pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(
            pcm.iter().any(|v| v.abs() > 0.001),
            "Non-silent audio required"
        );
        println!(
            "TTS production HTTP adapter: {} samples, {:.2}s; no playback/capture",
            pcm.len(),
            pcm.len() as f64 / 16000.0
        );
    }
}
