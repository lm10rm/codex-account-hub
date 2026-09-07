mod app_server;
mod runtime;
mod vault;

use app_server::UsageSnapshot;
use runtime::RuntimeInfo;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Manager;
use tokio::process::Command;
use tokio::time::timeout;
use vault::SavedAccount;

#[tauri::command]
fn discover_runtime() -> Result<RuntimeInfo, String> {
    runtime::discover_runtime()
}

#[tauri::command]
async fn query_current_usage() -> Result<UsageSnapshot, String> {
    let runtime = runtime::discover_runtime()?;
    app_server::query_usage(&runtime.codex_path, &runtime.codex_home).await
}

#[tauri::command]
fn list_saved_accounts(app: tauri::AppHandle) -> Result<Vec<SavedAccount>, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    vault::list_accounts(&data_dir)
}

#[tauri::command]
async fn import_current_account(
    app: tauri::AppHandle,
    label: Option<String>,
) -> Result<SavedAccount, String> {
    let runtime = runtime::discover_runtime()?;
    let snapshot = app_server::query_usage(&runtime.codex_path, &runtime.codex_home).await?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    vault::import_current(&data_dir, &runtime.codex_home, snapshot.account, label)
}

#[tauri::command]
async fn query_saved_usage(
    app: tauri::AppHandle,
    account_id: String,
) -> Result<UsageSnapshot, String> {
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let isolated_home = vault::prepare_isolated_home(&data_dir, &account_id)?;
    let query_result = app_server::query_usage(
        &runtime.codex_path,
        isolated_home
            .to_str()
            .ok_or_else(|| "隔离运行目录不是有效路径".to_string())?,
    )
    .await;
    let cleanup_result = vault::finish_isolated_home(&data_dir, &account_id, &isolated_home);

    match (query_result, cleanup_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(query_error), Ok(())) => Err(query_error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(query_error), Err(cleanup_error)) => Err(format!(
            "{query_error}；同时清理隔离认证状态失败：{cleanup_error}"
        )),
    }
}

#[tauri::command]
async fn add_account_with_login(
    app: tauri::AppHandle,
    label: Option<String>,
) -> Result<SavedAccount, String> {
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let login_home = data_dir
        .join("login")
        .join(format!("{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&login_home)
        .map_err(|error| format!("无法创建隔离登录目录：{error}"))?;

    let operation = async {
        run_interactive_login(&runtime.codex_path, &login_home).await?;
        let home_text = login_home
            .to_str()
            .ok_or_else(|| "隔离登录目录不是有效路径".to_string())?;
        let snapshot = app_server::query_usage(&runtime.codex_path, home_text).await?;
        vault::import_current(&data_dir, home_text, snapshot.account, label)
    }
    .await;
    let cleanup = vault::remove_plain_auth(&login_home);

    match (operation, cleanup) {
        (Ok(account), Ok(())) => Ok(account),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(cleanup_error)) => {
            Err(format!("{operation_error}；{cleanup_error}"))
        }
    }
}

#[tauri::command]
async fn switch_saved_account(
    app: tauri::AppHandle,
    account_id: String,
) -> Result<SavedAccount, String> {
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;

    let current = app_server::query_usage(&runtime.codex_path, &runtime.codex_home).await?;
    vault::import_current(&data_dir, &runtime.codex_home, current.account, None)?;
    vault::activate_account(&data_dir, &account_id, &runtime.codex_home)?;
    vault::list_accounts(&data_dir)?
        .into_iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| "切换成功，但账号索引中找不到目标账号".to_string())
}

async fn run_interactive_login(
    codex_path: &str,
    login_home: &std::path::Path,
) -> Result<(), String> {
    let mut command = Command::new(codex_path);
    command
        .arg("login")
        .env("CODEX_HOME", login_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.creation_flags_no_window();
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 Codex 官方登录：{error}"))?;

    let status = match timeout(Duration::from_secs(600), child.wait()).await {
        Ok(result) => result.map_err(|error| format!("等待 Codex 登录失败：{error}"))?,
        Err(_) => {
            let _ = child.kill().await;
            return Err("登录等待已超过 10 分钟，请重试".to_string());
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err("Codex 登录未完成或已取消".to_string())
    }
}

trait LoginCommandWindowsExt {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl LoginCommandWindowsExt for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            self.creation_flags(0x08000000);
        }
        self
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            vault::cleanup_stale_login_auth(&data_dir).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            discover_runtime,
            query_current_usage,
            list_saved_accounts,
            import_current_account,
            query_saved_usage,
            add_account_with_login,
            switch_saved_account
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Account Hub");
}
