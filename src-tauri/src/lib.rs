mod app_server;
mod codex_desktop;
mod runtime;
mod vault;

use app_server::UsageSnapshot;
use runtime::RuntimeInfo;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tokio::process::Command;
use tokio::time::timeout;
use vault::SavedAccount;

const MAX_CONCURRENT_USAGE_QUERIES: usize = 3;

struct OperationState {
    gate: tokio::sync::RwLock<()>,
    query_slots: tokio::sync::Semaphore,
}

struct TrayState {
    current_account: tauri::menu::MenuItem<tauri::Wry>,
    tray: tauri::tray::TrayIcon<tauri::Wry>,
    exiting: AtomicBool,
}

impl Default for OperationState {
    fn default() -> Self {
        Self {
            gate: tokio::sync::RwLock::new(()),
            query_slots: tokio::sync::Semaphore::new(MAX_CONCURRENT_USAGE_QUERIES),
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SwitchOutcome {
    account: SavedAccount,
    restart_requested: bool,
    restart_succeeded: bool,
    codex_was_running: bool,
    processes_closed: usize,
    restart_warning: Option<String>,
}

#[tauri::command]
fn discover_runtime() -> Result<RuntimeInfo, String> {
    runtime::discover_runtime()
}

#[tauri::command]
fn update_tray_current_account(app: tauri::AppHandle, label: Option<String>) -> Result<(), String> {
    let label = label
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "未识别当前账号".to_string());
    let menu_label = label.replace('&', "&&");
    let tray_state = app.state::<TrayState>();
    tray_state
        .current_account
        .set_text(format!("当前账号：{menu_label}"))
        .map_err(|error| error.to_string())?;
    tray_state
        .tray
        .set_tooltip(Some(format!("Codex Account Hub · {label}")))
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn query_current_usage(
    operation_state: tauri::State<'_, OperationState>,
) -> Result<UsageSnapshot, String> {
    let _guard = operation_state.gate.read().await;
    let _query_slot = operation_state
        .query_slots
        .acquire()
        .await
        .map_err(|_| "额度查询队列已关闭".to_string())?;
    let runtime = runtime::discover_runtime()?;
    app_server::query_usage(&runtime.codex_path, &runtime.codex_home).await
}

#[tauri::command]
async fn list_saved_accounts(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
) -> Result<Vec<SavedAccount>, String> {
    let _guard = operation_state.gate.read().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let active_id = vault::current_account_id(&runtime.codex_home).ok();
    let mut accounts = vault::list_accounts(&data_dir)?;
    for account in &mut accounts {
        account.is_active = active_id.as_deref() == Some(account.id.as_str());
    }
    Ok(accounts)
}

#[tauri::command]
async fn import_current_account(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    label: Option<String>,
) -> Result<SavedAccount, String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let account = app_server::query_usage(&runtime.codex_path, &runtime.codex_home)
        .await
        .ok()
        .and_then(|snapshot| snapshot.account);
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    vault::import_current(&data_dir, &runtime.codex_home, account, label)
}

#[tauri::command]
async fn query_saved_usage(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    account_id: String,
) -> Result<UsageSnapshot, String> {
    let _guard = operation_state.gate.read().await;
    let _query_slot = operation_state
        .query_slots
        .acquire()
        .await
        .map_err(|_| "额度查询队列已关闭".to_string())?;
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
    operation_state: tauri::State<'_, OperationState>,
    label: Option<String>,
) -> Result<SavedAccount, String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let login_home = create_login_home(&data_dir)?;

    let operation = async {
        run_interactive_login(&runtime.codex_path, &login_home).await?;
        let home_text = login_home
            .to_str()
            .ok_or_else(|| "隔离登录目录不是有效路径".to_string())?;
        let account = app_server::query_usage(&runtime.codex_path, home_text)
            .await
            .ok()
            .and_then(|snapshot| snapshot.account);
        vault::import_current(&data_dir, home_text, account, label)
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
async fn reauthorize_account(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    account_id: String,
) -> Result<SavedAccount, String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    if !vault::list_accounts(&data_dir)?
        .iter()
        .any(|account| account.id == account_id)
    {
        return Err("找不到这个保险库账号".to_string());
    }
    let login_home = create_login_home(&data_dir)?;

    let operation = async {
        run_interactive_login(&runtime.codex_path, &login_home).await?;
        let logged_in_id = vault::isolated_account_id(&login_home)?;
        if logged_in_id != account_id {
            return Err("登录的不是所选账号，已取消覆盖；请用该账号重新登录".to_string());
        }
        let home_text = login_home
            .to_str()
            .ok_or_else(|| "隔离登录目录不是有效路径".to_string())?;
        let account = app_server::query_usage(&runtime.codex_path, home_text)
            .await
            .ok()
            .and_then(|snapshot| snapshot.account);
        vault::import_current(&data_dir, home_text, account, None)
    }
    .await;
    let cleanup = vault::remove_plain_auth(&login_home);
    combine_operation_and_cleanup(operation, cleanup)
}

#[tauri::command]
async fn rename_saved_account(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    account_id: String,
    label: String,
) -> Result<SavedAccount, String> {
    let _guard = operation_state.gate.write().await;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    vault::rename_account(&data_dir, &account_id, &label)
}

#[tauri::command]
async fn delete_saved_account(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    account_id: String,
) -> Result<(), String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let current_id = vault::current_account_id(&runtime.codex_home)?;
    vault::delete_account(&data_dir, &account_id, Some(&current_id))
}

#[tauri::command]
async fn switch_saved_account(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    account_id: String,
    restart_codex: bool,
) -> Result<SwitchOutcome, String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;

    let current_account = app_server::query_usage(&runtime.codex_path, &runtime.codex_home)
        .await
        .ok()
        .and_then(|snapshot| snapshot.account);
    vault::import_current(&data_dir, &runtime.codex_home, current_account, None)?;
    vault::activate_account(&data_dir, &account_id, &runtime.codex_home)?;
    let account = vault::list_accounts(&data_dir)?
        .into_iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| "切换成功，但账号索引中找不到目标账号".to_string())?;

    if !restart_codex {
        return Ok(SwitchOutcome {
            account,
            restart_requested: false,
            restart_succeeded: false,
            codex_was_running: false,
            processes_closed: 0,
            restart_warning: None,
        });
    }

    let restart_result = tokio::task::spawn_blocking(codex_desktop::restart)
        .await
        .map_err(|error| format!("账号已切换，但 Codex 重启任务异常：{error}"))?;
    match restart_result {
        Ok(restart) => Ok(SwitchOutcome {
            account,
            restart_requested: true,
            restart_succeeded: true,
            codex_was_running: restart.was_running,
            processes_closed: restart.processes_closed,
            restart_warning: None,
        }),
        Err(warning) => Ok(SwitchOutcome {
            account,
            restart_requested: true,
            restart_succeeded: false,
            codex_was_running: false,
            processes_closed: 0,
            restart_warning: Some(warning),
        }),
    }
}

fn create_login_home(data_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let login_home = data_dir
        .join("login")
        .join(format!("{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&login_home)
        .map_err(|error| format!("无法创建隔离登录目录：{error}"))?;
    Ok(login_home)
}

fn combine_operation_and_cleanup<T>(
    operation: Result<T, String>,
    cleanup: Result<(), String>,
) -> Result<T, String> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(cleanup_error)) => {
            Err(format!("{operation_error}；{cleanup_error}"))
        }
    }
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

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open_item = MenuItem::with_id(
        app,
        "tray-open",
        "打开 Codex Account Hub",
        true,
        None::<&str>,
    )?;
    let current_account = MenuItem::with_id(
        app,
        "tray-current-account",
        "当前账号：正在读取…",
        false,
        None::<&str>,
    )?;
    let refresh_item = MenuItem::with_id(app, "tray-refresh", "刷新全部账号", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "tray-quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &current_account,
            &refresh_item,
            &separator,
            &quit_item,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("main-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?,
        )
        .tooltip("Codex Account Hub")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-open" => show_main_window(app),
            "tray-refresh" => {
                let _ = app.emit("tray-refresh-requested", ());
            }
            "tray-quit" => {
                app.state::<TrayState>()
                    .exiting
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayState {
        current_account,
        tray,
        exiting: AtomicBool::new(false),
    });
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .manage(OperationState::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            vault::cleanup_stale_login_auth(&data_dir).map_err(std::io::Error::other)?;
            setup_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if !window.state::<TrayState>().exiting.load(Ordering::SeqCst) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            discover_runtime,
            update_tray_current_account,
            query_current_usage,
            list_saved_accounts,
            import_current_account,
            query_saved_usage,
            add_account_with_login,
            reauthorize_account,
            rename_saved_account,
            delete_saved_account,
            switch_saved_account
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Account Hub");
}
