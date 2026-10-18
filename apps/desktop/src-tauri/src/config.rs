use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    pub model_path: String,
    #[serde(default)]
    pub model_sha256: Option<String>,
    #[serde(default = "default_language")]
    pub language: String,
}
fn default_language() -> String {
    "zh".into()
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub config_version: u32,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub credential_ref: Option<String>,
    #[serde(default = "default_context")]
    pub context_tokens: u32,
    #[serde(default = "default_output")]
    pub max_output_tokens: u32,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing)]
    pub api_key: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}
fn default_context() -> u32 {
    32768
}
fn default_output() -> u32 {
    4096
}
fn default_mode() -> String {
    "transcript_only".into()
}
fn default_timeout() -> u64 {
    120
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub asr: AsrConfig,
    pub analysis: ModelConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            asr: AsrConfig {
                model_path: String::new(),
                model_sha256: None,
                language: default_language(),
            },
            analysis: ModelConfig::default(),
        }
    }
}
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            profile_id: String::new(),
            config_version: 0,
            display_name: String::new(),
            credential_ref: None,
            context_tokens: default_context(),
            max_output_tokens: default_output(),
            mode: default_mode(),
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            timeout_seconds: default_timeout(),
        }
    }
}
impl Config {
    pub fn load(root: &Path) -> Self {
        let stored = root.join("settings.json");
        let path = if stored.exists() {
            Some(stored)
        } else if cfg!(test) {
            None // Unit tests must never import personal development credentials.
        } else {
            std::env::var_os("HELMMEET_DEV_CONFIG").map(PathBuf::from)
        };
        path.and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = root.join("settings.pending");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(self).map_err(|_| "配置编码失败")?,
        )
        .map_err(|_| "配置保存失败")?;
        std::fs::rename(path, root.join("settings.json")).map_err(|_| "配置提交失败".into())
    }
}
impl ModelConfig {
    pub fn endpoint(&self) -> Result<(url::Url, bool), String> {
        let mut url = url::Url::parse(self.base_url.trim()).map_err(|_| "服务地址无效")?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("服务地址不能包含凭据、查询或片段".into());
        }
        let local = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"));
        if url.scheme() != "https" && !(local && url.scheme() == "http") {
            return Err("远程服务必须使用 HTTPS；仅回环地址允许 HTTP".into());
        }
        if self.model.trim().is_empty() {
            return Err("请填写模型名称".into());
        }
        let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok((url, local))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_and_secret_boundary() {
        let mut c = ModelConfig {
            base_url: "https://api.example.test/v1/".into(),
            model: "m".into(),
            api_key: "private".into(),
            mode: default_mode(),
            timeout_seconds: 10,
            ..ModelConfig::default()
        };
        assert_eq!(
            c.endpoint().unwrap().0.as_str(),
            "https://api.example.test/v1/chat/completions"
        );
        assert!(!serde_json::to_string(&c).unwrap().contains("private"));
        c.base_url = "http://localhost.evil.test".into();
        assert!(c.endpoint().is_err());
        c.base_url = "http://127.0.0.1:1234/v1".into();
        assert!(c.endpoint().unwrap().1);
        c.base_url = "https://name:secret@example.test".into();
        assert!(c.endpoint().is_err());
    }
}
