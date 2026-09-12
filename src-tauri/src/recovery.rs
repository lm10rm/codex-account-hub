use crate::vault;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const JOURNAL: &str = "switch-transaction.dpapi";

#[derive(Serialize, Deserialize)]
struct Transaction {
    version: u32,
    codex_home: String,
    target_id: String,
    target_hash: String,
    before: Option<Vec<u8>>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub messages: Vec<String>,
    pub needs_attention: bool,
    pub restart_suggested: bool,
}

// Never follow a junction/symlink while cleaning credentials or restoring a login.
pub fn checked_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("恢复路径必须是绝对路径".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let linked = metadata.file_type().is_symlink();
                if linked {
                    return Err("恢复路径包含链接或重解析点，已停止自动处理".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查恢复路径：{error}")),
        }
    }
    Ok(())
}

pub fn begin(
    data_dir: &Path,
    home: &str,
    target: &[u8],
    target_id: &str,
    before: Option<Vec<u8>>,
) -> Result<(), String> {
    checked_path(data_dir)?;
    checked_path(&Path::new(home).join("auth.json"))?;
    ensure_no_pending(data_dir)?;
    let path = data_dir.join(JOURNAL);
    let bytes = serde_json::to_vec(&Transaction {
        version: 1,
        codex_home: home.into(),
        target_id: target_id.into(),
        target_hash: format!("{:x}", Sha256::digest(target)),
        before,
    })
    .map_err(|_| "无法生成恢复记录")?;
    vault::atomic_write(&path, &vault::protect(&bytes)?)
}

pub fn ensure_no_pending(data_dir: &Path) -> Result<(), String> {
    let path = data_dir.join(JOURNAL);
    checked_path(&path)?;
    if path.exists() {
        return Err("有未完成的认证恢复记录，请先点击“检查并恢复”或“重试恢复”".into());
    }
    Ok(())
}

pub fn finish(data_dir: &Path) -> Result<(), String> {
    remove_file(&data_dir.join(JOURNAL))
}

fn remove_file(path: &Path) -> Result<(), String> {
    checked_path(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法清理恢复文件：{error}")),
    }
}

pub fn record_child(home: &Path, pid: u32) -> Result<(), String> {
    checked_path(home)?;
    let record = crate::child_process::Record {
        version: 1,
        process: crate::child_process::capture(pid)?,
        owner: crate::child_process::capture(std::process::id())?,
    };
    vault::atomic_write(
        &home.join("hub-child.json"),
        &serde_json::to_vec(&record).map_err(|_| "无法记录进程")?,
    )
}

pub fn clear_child(home: &Path) -> Result<(), String> {
    remove_file(&home.join("hub-child.json"))
}

pub fn ensure_idle(home: &Path) -> Result<(), String> {
    checked_path(home)?;
    if child_is_running(home)? {
        return Err("遗留查询进程仍在运行，请先重试恢复".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChildRecord {
    Verified(crate::child_process::Record),
    Legacy(u32),
}

fn read_child(home: &Path) -> Result<Option<ChildRecord>, String> {
    let path = home.join("hub-child.json");
    checked_path(&path)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取遗留进程状态：{error}")),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "遗留进程记录损坏，请检查应用数据目录后重试".into())
}

fn child_is_running(home: &Path) -> Result<bool, String> {
    match read_child(home)? {
        None => Ok(false),
        Some(ChildRecord::Legacy(pid)) => crate::child_process::legacy_running(pid),
        Some(ChildRecord::Verified(record)) if record.version == 1 => {
            crate::child_process::running(&record.process)
        }
        Some(_) => Err("子进程记录版本不受支持".into()),
    }
}

// Called only during startup or while the exclusive operation lock is held.
fn stop_stale_child(home: &Path) -> Result<(), String> {
    match read_child(home)? {
        None => Ok(()),
        Some(ChildRecord::Verified(record)) => crate::child_process::stop(&record),
        Some(ChildRecord::Legacy(pid)) => {
            if crate::child_process::legacy_running(pid)? {
                Err(format!(
                    "旧版记录只有 PID {pid}，无法安全核验进程身份。请关闭旧登录流程后重试恢复；新版已增加自动回收。"
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn recover_transaction(data_dir: &Path, home: &str, report: &mut Report) -> Result<(), String> {
    let path = data_dir.join(JOURNAL);
    checked_path(&path)?;
    if !path.exists() {
        return Ok(());
    }
    let encrypted = fs::read(&path).map_err(|error| format!("无法读取认证恢复记录：{error}"))?;
    let journal: Transaction = serde_json::from_slice(&vault::unprotect(&encrypted)?)
        .map_err(|_| "认证恢复记录损坏，请保留应用数据目录并检查备份")?;
    if journal.version != 1 || journal.codex_home != home {
        return Err("恢复记录版本或 Codex Home 不匹配，请恢复原 CODEX_HOME 设置后重试".into());
    }
    checked_path(&Path::new(home).join("auth.json"))?;
    let current = vault::read_optional_auth(home)?;
    let current_id = current
        .as_deref()
        .filter(|auth| vault::validate_auth(auth).is_ok())
        .and_then(|auth| vault::auth_identity(auth).ok());
    if current == journal.before {
        report
            .messages
            .push("上次认证更新未提交或已回滚，已保留原登录。".into());
    } else if current
        .as_deref()
        .is_some_and(|auth| format!("{:x}", Sha256::digest(auth)) == journal.target_hash)
        || current_id.as_deref() == Some(&journal.target_id)
    {
        vault::import_current(data_dir, home, None, None)?;
        report
            .messages
            .push("上次认证更新已生效，已确认并结束恢复。".into());
        report.restart_suggested = true;
    } else if current_id.is_some() {
        report
            .messages
            .push("检测到其他有效登录，已保留外部登录变更并结束旧恢复记录。".into());
    } else if let Some(before) = journal
        .before
        .as_deref()
        .filter(|auth| vault::validate_auth(auth).is_ok() && vault::auth_identity(auth).is_ok())
    {
        if vault::read_optional_auth(home)? != current {
            return Err("恢复期间登录发生变化，请重试恢复".into());
        }
        vault::replace_auth_file(&Path::new(home).join("auth.json"), before)?;
        if vault::read_optional_auth(home)?.as_deref() != Some(before) {
            return Err("恢复后认证校验失败，请保留备份并重试".into());
        }
        report
            .messages
            .push("已从加密恢复记录还原上次有效登录。".into());
        report.restart_suggested = true;
    } else {
        report
            .messages
            .push("上次操作没有可用的原登录，请从已保存账号切换恢复，或重新登录后导入。".into());
        report.needs_attention = true;
    }
    finish(data_dir)
}

fn cleanup_home(home: &Path, data_dir: &Path, runtime: bool) -> Result<usize, String> {
    checked_path(home)?;
    if child_is_running(home)? {
        return Err("遗留的 Codex 登录/查询进程仍在运行，请结束该进程后点击“重试恢复”".into());
    }
    for name in [
        "auth.json",
        ".auth.json.tmp",
        ".auth.json.codex-account-hub.tmp",
    ] {
        checked_path(&home.join(name))?;
    }
    let mut count = 0;
    if home.join("auth.json").exists() {
        if runtime {
            let id = home
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("运行目录名称无效")?;
            // Preserve a token refreshed just before the crash before deleting its plaintext.
            vault::finish_isolated_home(data_dir, id, home)?;
        } else {
            remove_file(&home.join("auth.json"))?;
        }
        count += 1;
    }
    for name in [".auth.json.tmp", ".auth.json.codex-account-hub.tmp"] {
        if home.join(name).exists() {
            remove_file(&home.join(name))?;
            count += 1;
        }
    }
    clear_child(home)?;
    Ok(count)
}

pub async fn cleanup_login(home: &Path) -> Result<(), String> {
    for _ in 0..60 {
        if !child_is_running(home)? {
            return cleanup_home(home, home, false).map(|_| ());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err("登录进程尚未退出，临时认证已保留，请结束该进程后重试恢复".into())
}

pub fn run(data_dir: &Path, home: Result<String, String>) -> Report {
    let mut report = Report::default();
    let mut errors = vec![];
    if let Err(error) = checked_path(data_dir) {
        return Report {
            messages: vec![error],
            needs_attention: true,
            restart_suggested: false,
        };
    }
    let mut cleaned = 0;
    for (name, runtime) in [("login", false), ("runtime", true)] {
        let root = data_dir.join(name);
        if let Err(error) = checked_path(&root) {
            errors.push(error);
            continue;
        }
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                errors.push(format!("无法检查临时认证目录：{error}"));
                continue;
            }
        };
        for entry in entries {
            let result = entry.map_err(|error| error.to_string()).and_then(|entry| {
                checked_path(&entry.path())?;
                if entry.path().is_dir() {
                    stop_stale_child(&entry.path())?;
                    cleanup_home(&entry.path(), data_dir, runtime)
                } else {
                    Ok(0)
                }
            });
            match result {
                Ok(count) => cleaned += count,
                Err(error) => errors.push(error),
            }
        }
    }
    // Settle old queries before the journal imports the authoritative active credentials.
    match home {
        Ok(home) => {
            if let Err(error) = recover_transaction(data_dir, &home, &mut report) {
                errors.push(error);
            }
            if let Err(error) =
                remove_file(&Path::new(&home).join(".auth.json.codex-account-hub.tmp"))
            {
                errors.push(error);
            }
        }
        Err(error) => errors.push(error),
    }
    if cleaned > 0 {
        report
            .messages
            .push(format!("已清理 {cleaned} 份遗留临时认证文件。"));
    }
    report.needs_attention |= !errors.is_empty();
    report.messages.extend(errors);
    report
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const OLD: &[u8] = br#"{"tokens":{"access_token":"fixture-old","account_id":"fixture-a"}}"#;
    const TARGET: &[u8] = br#"{"tokens":{"access_token":"fixture-new","account_id":"fixture-b"}}"#;
    const OTHER: &[u8] =
        br#"{"tokens":{"access_token":"fixture-external","account_id":"fixture-c"}}"#;
    struct Fixture {
        root: std::path::PathBuf,
        home: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "codex-hub-startup-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let home = root.join("main");
            fs::create_dir_all(&home).unwrap();
            fs::write(home.join("auth.json"), OLD).unwrap();
            Self {
                root,
                home: home.to_string_lossy().into_owned(),
            }
        }
        fn journal(&self) {
            begin(
                &self.root,
                &self.home,
                TARGET,
                &vault::auth_identity(TARGET).unwrap(),
                Some(OLD.to_vec()),
            )
            .unwrap();
        }
        fn report(&self) -> Report {
            run(&self.root, Ok(self.home.clone()))
        }
        fn active(&self) -> Vec<u8> {
            fs::read(Path::new(&self.home).join("auth.json")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn journal_is_encrypted_and_uncommitted_transaction_keeps_original_login() {
        let fixture = Fixture::new();
        fixture.journal();
        let encrypted = fs::read(fixture.root.join(JOURNAL)).unwrap();
        assert!(!encrypted.windows(OLD.len()).any(|part| part == OLD));
        assert!(!fixture.report().needs_attention);
        assert_eq!(fixture.active(), OLD);
        assert!(!fixture.root.join(JOURNAL).exists());
        assert!(fixture.report().messages.is_empty());
    }

    #[test]
    fn completed_transaction_preserves_target_and_synchronizes_vault() {
        let fixture = Fixture::new();
        fixture.journal();
        fs::write(Path::new(&fixture.home).join("auth.json"), TARGET).unwrap();
        let report = fixture.report();
        assert!(!report.needs_attention);
        assert!(report.restart_suggested);
        assert_eq!(fixture.active(), TARGET);
        let id = vault::auth_identity(TARGET).unwrap();
        let encrypted = fs::read(fixture.root.join("vault").join(format!("{id}.dpapi"))).unwrap();
        assert_eq!(vault::unprotect(&encrypted).unwrap(), TARGET);
    }

    #[test]
    fn corruption_or_missing_auth_restores_backup_but_external_login_is_preserved() {
        for current in [Some(b"broken".as_slice()), None, Some(OTHER)] {
            let fixture = Fixture::new();
            fixture.journal();
            let path = Path::new(&fixture.home).join("auth.json");
            if let Some(auth) = current {
                fs::write(path, auth).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
            assert!(!fixture.report().needs_attention);
            assert_eq!(
                fixture.active(),
                if current == Some(OTHER) { OTHER } else { OLD }
            );
        }
    }

    #[test]
    fn damaged_journal_and_changed_home_are_reported_without_overwriting_auth() {
        let fixture = Fixture::new();
        fixture.journal();
        let other_home = fixture.root.join("other");
        fs::create_dir(&other_home).unwrap();
        fs::write(other_home.join("auth.json"), OTHER).unwrap();
        assert!(run(&fixture.root, Ok(other_home.to_string_lossy().into_owned())).needs_attention);
        assert_eq!(fixture.active(), OLD);
        assert!(fixture.root.join(JOURNAL).exists());
        fs::write(fixture.root.join(JOURNAL), b"damaged").unwrap();
        assert!(fixture.report().needs_attention);
        assert_eq!(fixture.active(), OLD);
        assert!(fixture.root.join(JOURNAL).exists());
    }

    #[test]
    fn startup_cleans_login_and_runtime_plaintext_preserving_refreshed_credentials() {
        let fixture = Fixture::new();
        let saved = vault::import_current(&fixture.root, &fixture.home, None, None).unwrap();
        let isolated = vault::prepare_isolated_home(&fixture.root, &saved.id).unwrap();
        let refreshed =
            br#"{"tokens":{"access_token":"fixture-renewed","account_id":"fixture-a"}}"#;
        fs::write(isolated.join("auth.json"), refreshed).unwrap();
        fs::write(isolated.join(".auth.json.tmp"), OLD).unwrap();
        let login = fixture.root.join("login").join("fixture");
        fs::create_dir_all(&login).unwrap();
        fs::write(login.join("auth.json"), TARGET).unwrap();
        let report = fixture.report();
        assert!(!report.needs_attention);
        assert!(!isolated.join("auth.json").exists());
        assert!(!isolated.join(".auth.json.tmp").exists());
        assert!(!login.join("auth.json").exists());
        let encrypted = fs::read(
            fixture
                .root
                .join("vault")
                .join(format!("{}.dpapi", saved.id)),
        )
        .unwrap();
        assert_eq!(vault::unprotect(&encrypted).unwrap(), refreshed);
        assert_eq!(fixture.active(), OLD);
    }

    #[test]
    fn running_child_blocks_cleanup_and_new_query_until_retry() {
        let fixture = Fixture::new();
        let saved = vault::import_current(&fixture.root, &fixture.home, None, None).unwrap();
        let isolated = vault::prepare_isolated_home(&fixture.root, &saved.id).unwrap();
        record_child(&isolated, std::process::id()).unwrap();
        assert!(fixture.report().needs_attention);
        assert!(isolated.join("auth.json").exists());
        assert!(vault::prepare_isolated_home(&fixture.root, &saved.id).is_err());
        clear_child(&isolated).unwrap();
        assert!(!fixture.report().needs_attention);
        assert!(!isolated.join("auth.json").exists());
    }

    #[test]
    fn restore_failure_retains_journal_for_successful_retry() {
        let fixture = Fixture::new();
        fixture.journal();
        let main = Path::new(&fixture.home);
        fs::write(main.join("auth.json"), b"damaged").unwrap();
        let blocked = main.join(".auth.json.codex-account-hub.tmp");
        fs::create_dir(&blocked).unwrap();
        assert!(fixture.report().needs_attention);
        assert!(fixture.root.join(JOURNAL).exists());
        fs::remove_dir(blocked).unwrap();
        assert!(!fixture.report().needs_attention);
        assert_eq!(fixture.active(), OLD);
    }

    #[test]
    fn retry_recovery_stops_verified_child_and_cleans_legacy_dead_marker() {
        tauri::async_runtime::block_on(async {
            let fixture = Fixture::new();
            let home = fixture.root.join("login").join("abandoned");
            fs::create_dir_all(&home).unwrap();
            fs::write(home.join("auth.json"), TARGET).unwrap();
            let mut child = tokio::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .creation_flags(0x08000000)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            // Legacy live PID must not be terminated without reliable identity metadata.
            fs::write(
                home.join("hub-child.json"),
                serde_json::to_vec(&pid).unwrap(),
            )
            .unwrap();
            assert!(fixture.report().needs_attention);
            assert!(child.try_wait().unwrap().is_none());
            record_child(&home, pid).unwrap();
            assert!(!fixture.report().needs_attention);
            assert!(child.try_wait().unwrap().is_some());
            assert!(!home.join("auth.json").exists());
            assert!(!home.join("hub-child.json").exists());
            fs::write(
                home.join("hub-child.json"),
                serde_json::to_vec(&pid).unwrap(),
            )
            .unwrap();
            assert!(!fixture.report().needs_attention);
            assert!(!home.join("hub-child.json").exists());
            assert_eq!(fixture.active(), OLD);
        });
    }

    #[test]
    fn cleanup_login_removes_fixture_auth_after_cancelled_process() {
        tauri::async_runtime::block_on(async {
            let fixture = Fixture::new();
            let home = fixture.root.join("login").join("fixture");
            fs::create_dir_all(&home).unwrap();
            fs::write(home.join("auth.json"), TARGET).unwrap();
            let state = std::sync::Arc::new(crate::login::LoginState::default());
            let session = state.begin("fixture".into()).unwrap();
            let mut child = tokio::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .creation_flags(0x08000000)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            record_child(&home, child.id().unwrap()).unwrap();
            state.cancel("fixture").unwrap();
            assert!(
                session
                    .wait_child(&mut child, std::time::Duration::from_secs(600))
                    .await
                    .is_err()
            );
            cleanup_login(&home).await.unwrap();
            assert!(!home.join("auth.json").exists());
            assert!(!home.join("hub-child.json").exists());
            assert_eq!(fixture.active(), OLD);
        });
    }
}
