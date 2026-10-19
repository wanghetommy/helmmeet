//! Portable, per-user plaintext credential configuration. Never exposed through IPC.
use crate::{db::Runtime, models::CredentialStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    api_keys: BTreeMap<String, String>,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            api_keys: BTreeMap::new(),
        }
    }
}
pub struct FileStore {
    root: PathBuf,
    gate: Mutex<()>,
}
impl FileStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            gate: Mutex::new(()),
        }
    }
    fn regular(path: &Path) -> Result<(), String> {
        match std::fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                Err("凭据配置必须是普通文件，不能使用链接或目录".into())
            }
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("无法检查凭据配置文件".into()),
        }
    }
    fn open_lock(&self) -> Result<File, String> {
        std::fs::create_dir_all(&self.root).map_err(|_| "无法创建应用配置目录")?;
        let path = self.root.join("credentials.lock");
        Self::regular(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let f = options.open(path).map_err(|_| "无法打开凭据配置锁")?;
        f.lock().map_err(|_| "无法锁定凭据配置")?;
        Ok(f)
    }
    fn read(&self) -> Result<Document, String> {
        let path = self.root.join("credentials.json");
        Self::regular(&path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = match options.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Document::default()),
            Err(_) => return Err("无法读取本地凭据配置，请检查文件权限".into()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "无法设置凭据配置权限")?;
        }
        let mut bytes = vec![];
        Read::by_ref(&mut f)
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取凭据配置")?;
        if bytes.len() > 1_048_576 {
            return Err("凭据配置超过大小限制".into());
        }
        let data: Document = serde_json::from_slice(&bytes)
            .map_err(|_| "凭据配置格式无效，请修复credentials.json；未覆盖原文件")?;
        if data.version != 1 {
            return Err("凭据配置版本不受支持，请使用匹配的应用".into());
        }
        Ok(data)
    }
    fn write(&self, data: &Document) -> Result<(), String> {
        let path = self.root.join("credentials.json");
        Self::regular(&path)?;
        let bytes = serde_json::to_vec_pretty(data).map_err(|_| "凭据配置编码失败")?;
        if bytes.len() > 1_048_576 {
            return Err("凭据配置超过大小限制".into());
        }
        let mut tmp = tempfile::Builder::new()
            .prefix(".credentials-")
            .suffix(".tmp")
            .tempfile_in(&self.root)
            .map_err(|_| "无法创建凭据临时配置")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "无法设置凭据配置权限")?;
        }
        tmp.write_all(&bytes)
            .and_then(|_| tmp.as_file().sync_all())
            .map_err(|_| "凭据配置写入失败，原文件保留")?;
        tmp.persist(path)
            .map_err(|_| "凭据配置原子替换失败，原文件保留")?;
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "凭据配置目录同步失败")?;
        Ok(())
    }
}
impl CredentialStore for FileStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        let _gate = self.gate.lock().map_err(|_| "凭据锁异常")?;
        let _file = self.open_lock()?;
        Ok(self.read()?.api_keys.get(account).cloned())
    }
    fn put(&self, account: &str, secret: &str) -> Result<(), String> {
        if account.is_empty()
            || account.len() > 8192
            || secret.trim().is_empty()
            || secret.len() > 8192
        {
            return Err("凭据字段无效".into());
        }
        let _gate = self.gate.lock().map_err(|_| "凭据锁异常")?;
        let _file = self.open_lock()?;
        let mut data = self.read()?;
        data.api_keys.insert(account.into(), secret.into());
        self.write(&data)
    }
    fn remove(&self, account: &str) -> Result<(), String> {
        let _gate = self.gate.lock().map_err(|_| "凭据锁异常")?;
        let _file = self.open_lock()?;
        let mut data = self.read()?;
        if data.api_keys.remove(account).is_some() {
            self.write(&data)?;
        }
        Ok(())
    }
}
#[cfg(target_os = "macos")]
struct LegacyKeychain;
#[cfg(target_os = "macos")]
impl CredentialStore for LegacyKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match security_framework::passwords::get_generic_password("com.helmmeet.models", account) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "旧凭据编码无效".into()),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(_) => Err("旧钥匙串不可读".into()),
        }
    }
    fn put(&self, _: &str, _: &str) -> Result<(), String> {
        Err("旧凭据存储只用于读取迁移".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("旧凭据存储只用于读取迁移".into())
    }
}
pub fn migrate(rt: &Runtime, force: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        migrate_from(rt, &LegacyKeychain, force)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (rt, force);
        Ok(())
    }
}
#[cfg(any(target_os = "macos", test))]
fn migrate_from(rt: &Runtime, old: &dyn CredentialStore, force: bool) -> Result<(), String> {
    let _gate = rt.model_gate.lock().map_err(|_| "模型目录锁异常")?;
    let done:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM app_preferences WHERE key='credential_file_migration_attempted')",[],|r|r.get(0)).map_err(crate::db::err)?;
    if done && !force {
        return Ok(());
    }
    let mut refs = {
        let db = rt.db()?;
        let mut q=db.prepare("SELECT DISTINCT c.credential_ref FROM model_configs c JOIN model_profiles p ON p.id=c.id WHERE p.archived=0 AND c.credential_ref IS NOT NULL AND NOT EXISTS(SELECT 1 FROM revoked_credentials r WHERE r.credential_ref=c.credential_ref)").map_err(crate::db::err)?;
        let entries = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(crate::db::err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::db::err)?;
        entries
    };
    let legacy = rt.config.lock().map_err(|_| "配置锁异常")?.analysis.clone();
    if !legacy.base_url.is_empty() && legacy.api_key.is_empty() {
        refs.push(legacy.base_url);
    }
    let mut failed = 0;
    for reference in refs {
        if rt.vault.get(&reference)?.is_some() {
            continue;
        }
        match old.get(&reference) {
            Ok(Some(secret)) => {
                rt.vault.put(&reference, &secret)?;
                if rt.vault.get(&reference)?.as_deref() != Some(&secret) {
                    return Err("旧凭据转存校验失败，原条目保留".into());
                }
            }
            _ => failed += 1,
        }
    }
    rt.db()?.execute("INSERT OR REPLACE INTO app_preferences VALUES ('credential_file_migration_attempted','1')",[]).map_err(crate::db::err)?;
    if failed > 0 {
        return Err(format!("{failed} 项旧凭据无法转存，请在文本模型设置重新填写密钥，或主动重试迁移；录音转写仍可使用"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::id,
        models::{self, tests::MemoryVault, ProfileInput},
    };
    use std::sync::Arc;
    #[test]
    fn atomic_file_roundtrip_permissions_restart_and_concurrent_stores() {
        let root = std::env::temp_dir().join(format!("helmmeet-credentials-{}", id()));
        let store = FileStore::new(root.clone());
        assert_eq!(store.get("missing").unwrap(), None);
        store.put("a", "synthetic-key-a").unwrap();
        let handles = (0..12)
            .map(|i| {
                let root = root.clone();
                std::thread::spawn(move || {
                    FileStore::new(root)
                        .put(&format!("p{i}"), "synthetic-key")
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for h in handles {
            h.join().unwrap();
        }
        let next = FileStore::new(root.clone());
        assert_eq!(next.get("a").unwrap().as_deref(), Some("synthetic-key-a"));
        for i in 0..12 {
            assert!(next.get(&format!("p{i}")).unwrap().is_some());
        }
        next.remove("a").unwrap();
        assert!(store.get("a").unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("credentials.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::write(root.join("credentials.json"), b"broken secret fixture").unwrap();
        assert!(store.put("b", "new-key").is_err());
        assert_eq!(
            std::fs::read(root.join("credentials.json")).unwrap(),
            b"broken secret fixture"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_target_without_changing_external_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("credentials.json")).unwrap();
        let store = FileStore::new(root.path().into());
        assert!(store.put("a", "synthetic-key").is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"keep");
    }
    #[test]
    fn migration_keeps_reference_and_is_one_time_without_deleting_old_copy() {
        let root = tempfile::tempdir().unwrap();
        let old = Arc::new(MemoryVault::default());
        let rt = Runtime::open_with_vault(root.path().into(), old.clone()).unwrap();
        let p = models::save(
            &rt,
            ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "合成迁移".into(),
                base_url: "https://example.test".into(),
                model: "fixture".into(),
                context_tokens: 32768,
                max_output_tokens: 4096,
                timeout_seconds: 10,
                key_action: "replace".into(),
                api_key: "synthetic-migration-key".into(),
            },
        )
        .unwrap();
        let p = p["profile_id"].as_str().unwrap().to_string();
        let c = models::load(&rt, &p, 1).unwrap();
        drop(rt);
        let file = Arc::new(FileStore::new(root.path().into()));
        let rt = Runtime::open_with_vault(root.path().into(), file.clone()).unwrap();
        migrate_from(&rt, &*old, false).unwrap();
        assert_eq!(
            models::secret(&rt, &c).unwrap().as_deref(),
            Some("synthetic-migration-key")
        );
        assert_eq!(
            old.get(c.credential_ref.as_ref().unwrap())
                .unwrap()
                .as_deref(),
            Some("synthetic-migration-key")
        );
        file.remove(c.credential_ref.as_ref().unwrap()).unwrap();
        migrate_from(&rt, &*old, false).unwrap();
        assert!(models::secret(&rt, &c).is_err());
        migrate_from(&rt, &*old, true).unwrap();
        assert!(models::secret(&rt, &c).is_ok());
    }
}
