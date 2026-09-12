mod app_server;
mod child_process;
mod codex_desktop;
mod login;
mod recovery;
mod runtime;
mod usage_cache;
mod vault;

use app_server::UsageSnapshot;
use runtime::RuntimeInfo;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tokio::process::Command;
use usage_cache::UsageCache;
use vault::SavedAccount;

const MAX_CONCURRENT_USAGE_QUERIES: usize = 3;

struct RecoveryState(Mutex<recovery::Report>);

#[tauri::command]
fn recovery_status(state: tauri::State<'_, RecoveryState>) -> Result<recovery::Report, String> {
    state
        .0
        .lock()
        .map(|report| report.clone())
        .map_err(|_| "恢复状态不可用".into())
}

#[tauri::command]
async fn retry_recovery(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    state: tauri::State<'_, RecoveryState>,
) -> Result<recovery::Report, String> {
    let _guard = operation_state.gate.write().await;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let home = runtime::codex_home();
    let report = tokio::task::spawn_blocking(move || recovery::run(&data_dir, home))
        .await
        .map_err(|_| "恢复任务异常，请重试恢复".to_string())?;
    *state.0.lock().map_err(|_| "恢复状态不可用")? = report.clone();
    Ok(report)
}

#[tauri::command]
fn cancel_login(
    state: tauri::State<'_, Arc<login::LoginState>>,
    request_id: String,
) -> Result<bool, String> {
    state.cancel(&request_id)
}

fn login_phase(
    app: &tauri::AppHandle,
    session: &login::Session,
    phase: &'static str,
) -> Result<(), String> {
    let progress = session.phase(phase)?;
    let _ = app.emit("login-progress", progress);
    Ok(())
}

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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReauthorizationOutcome {
    account: SavedAccount,
    current_auth_updated: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RestartResult {
    succeeded: bool,
    warning: Option<String>,
}

#[tauri::command]
async fn restart_codex(
    operation_state: tauri::State<'_, OperationState>,
) -> Result<RestartResult, String> {
    let _guard = operation_state.gate.write().await;
    let result = perform_restart().await;
    Ok(RestartResult {
        succeeded: result.is_ok(),
        warning: result.err(),
    })
}

async fn perform_restart() -> Result<codex_desktop::RestartOutcome, String> {
    tokio::task::spawn_blocking(codex_desktop::restart)
        .await
        .map_err(|_| "Codex 重启任务异常，请重试启动".to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountList {
    accounts: Vec<SavedAccount>,
    current_account_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageQueryError {
    code: &'static str,
    message: String,
    reauth_required: bool,
}

impl From<String> for UsageQueryError {
    fn from(message: String) -> Self {
        let normalized = message.to_lowercase();
        let (code, reauth_required) = if normalized.contains("查询期间账号已变化") {
            ("account_changed", false)
        } else if [
            "认证文件",
            "认证目录",
            "隔离认证",
            "dpapi",
            "加密账号",
            "凭据与目标",
            "账号索引",
            "auth.json 不是有效 json",
            "无法从认证文件识别账号",
        ]
        .iter()
        .any(|keyword| normalized.contains(keyword))
        {
            ("local_io", false)
        } else if [
            "unauthorized",
            "not logged in",
            "authentication failed",
            "authentication required",
            "invalid_grant",
            "refresh_token_reused",
            "refresh_token_expired",
            "refresh_token_invalidated",
            "token expired",
            "access token",
            "登录失效",
            "未登录",
            "401",
        ]
        .iter()
        .any(|keyword| normalized.contains(keyword))
        {
            ("authentication_required", true)
        } else if normalized.contains("超时") || normalized.contains("timeout") {
            ("timeout", false)
        } else if normalized.contains("未找到 codex") || normalized.contains("无法确定 codex home")
        {
            ("runtime_unavailable", false)
        } else if ["network", "connection", "connect", "dns", "网络", "连接"]
            .iter()
            .any(|keyword| normalized.contains(keyword))
        {
            ("network", false)
        } else if normalized.contains("app server") {
            ("app_server", false)
        } else if ["文件", "目录", "dpapi", "加密账号", "清理隔离"]
            .iter()
            .any(|keyword| normalized.contains(keyword))
        {
            ("local_io", false)
        } else {
            ("unknown", false)
        };
        Self {
            code,
            message,
            reauth_required,
        }
    }
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
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    usage_cache: tauri::State<'_, UsageCache>,
) -> Result<UsageSnapshot, UsageQueryError> {
    let _guard = operation_state.gate.read().await;
    let _query_slot = operation_state
        .query_slots
        .acquire()
        .await
        .map_err(|_| "额度查询队列已关闭".to_string())?;
    let runtime = runtime::discover_runtime()?;
    let snapshot = app_server::query_usage(&runtime.codex_path, &runtime.codex_home).await?;
    if let Ok(data_dir) = app.path().app_data_dir() {
        let _ = usage_cache
            .store(&data_dir, snapshot.account_id.clone(), snapshot.clone())
            .await;
    }
    Ok(snapshot)
}

#[tauri::command]
async fn load_usage_cache(
    usage_cache: tauri::State<'_, UsageCache>,
) -> Result<std::collections::HashMap<String, UsageSnapshot>, String> {
    Ok(usage_cache.all().await)
}

#[tauri::command]
async fn list_saved_accounts(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
) -> Result<AccountList, String> {
    let _guard = operation_state.gate.read().await;
    let codex_home = runtime::codex_home()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let active_id = vault::current_account_id(&codex_home).ok();
    let mut accounts = vault::list_accounts(&data_dir)?;
    for account in &mut accounts {
        account.is_active = active_id.as_deref() == Some(account.id.as_str());
    }
    Ok(AccountList {
        accounts,
        current_account_id: active_id,
    })
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
    usage_cache: tauri::State<'_, UsageCache>,
    account_id: String,
) -> Result<UsageSnapshot, UsageQueryError> {
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
    let query_result = app_server::query_usage_owned(
        &runtime.codex_path,
        isolated_home
            .to_str()
            .ok_or_else(|| "隔离运行目录不是有效路径".to_string())?,
        true,
    )
    .await;
    let cleanup_result = vault::finish_isolated_home(&data_dir, &account_id, &isolated_home);

    let snapshot = match (query_result, cleanup_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(query_error), Ok(())) => Err(query_error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(query_error), Err(cleanup_error)) => Err(format!(
            "{query_error}；同时清理隔离认证状态失败：{cleanup_error}"
        )),
    }?;
    let _ = usage_cache
        .store(&data_dir, account_id, snapshot.clone())
        .await;
    Ok(snapshot)
}

#[tauri::command]
async fn add_account_with_login(
    app: tauri::AppHandle,
    operation_state: tauri::State<'_, OperationState>,
    label: Option<String>,
    login_state: tauri::State<'_, Arc<login::LoginState>>,
    request_id: String,
) -> Result<SavedAccount, String> {
    let session = login_state.begin(request_id)?;
    login_phase(&app, &session, "queued")?;
    let _guard = session.wait(operation_state.gate.write()).await?;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let login_home = create_login_home(&data_dir)?;

    let operation = async {
        login_phase(&app, &session, "waiting")?;
        run_interactive_login(&runtime.codex_path, &login_home, &session).await?;
        let home_text = login_home
            .to_str()
            .ok_or_else(|| "隔离登录目录不是有效路径".to_string())?;
        login_phase(&app, &session, "verifying")?;
        let account = session
            .wait(app_server::query_usage_owned(
                &runtime.codex_path,
                home_text,
                true,
            ))
            .await?
            .ok()
            .and_then(|snapshot| snapshot.account);
        recovery::ensure_idle(&login_home)?;
        login_phase(&app, &session, "saving")?;
        vault::import_current(&data_dir, home_text, account, label)
    }
    .await;
    let cleanup = recovery::cleanup_login(&login_home).await;

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
    login_state: tauri::State<'_, Arc<login::LoginState>>,
    request_id: String,
) -> Result<ReauthorizationOutcome, String> {
    let session = login_state.begin(request_id)?;
    login_phase(&app, &session, "queued")?;
    let _guard = session.wait(operation_state.gate.write()).await?;
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
        login_phase(&app, &session, "waiting")?;
        run_interactive_login(&runtime.codex_path, &login_home, &session).await?;
        let logged_in_id = vault::isolated_account_id(&login_home)?;
        if logged_in_id != account_id {
            return Err("登录的不是所选账号，已取消覆盖；请用该账号重新登录".to_string());
        }
        let home_text = login_home
            .to_str()
            .ok_or_else(|| "隔离登录目录不是有效路径".to_string())?;
        login_phase(&app, &session, "verifying")?;
        let account = session
            .wait(app_server::query_usage_owned(
                &runtime.codex_path,
                home_text,
                true,
            ))
            .await?
            .ok()
            .and_then(|snapshot| snapshot.account);
        recovery::ensure_idle(&login_home)?;
        login_phase(&app, &session, "saving")?;
        let (account, current_auth_updated) = vault::complete_reauthorization(
            &data_dir,
            &runtime.codex_home,
            &login_home,
            &account_id,
            account,
        )?;
        Ok(ReauthorizationOutcome {
            account,
            current_auth_updated,
        })
    }
    .await;
    let cleanup = recovery::cleanup_login(&login_home).await;
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
    usage_cache: tauri::State<'_, UsageCache>,
    account_id: String,
) -> Result<(), String> {
    let _guard = operation_state.gate.write().await;
    let runtime = runtime::discover_runtime()?;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let current_id = vault::current_account_id(&runtime.codex_home)?;
    vault::delete_account(&data_dir, &account_id, Some(&current_id))?;
    let _ = usage_cache.remove(&data_dir, &account_id).await;
    Ok(())
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

    vault::save_current_before_switch(&data_dir, &runtime.codex_home, &account_id)?;
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

    let restart_result = perform_restart().await;
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
    session: &login::Session,
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

    let _job = match child_process::ChildJob::attach(child.id().ok_or("登录进程已退出")?) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill().await;
            return Err(error);
        }
    };

    if let Err(error) = recovery::record_child(login_home, child.id().ok_or("登录进程已退出")?)
    {
        let _ = child.kill().await;
        return Err(error);
    }
    let result = session
        .wait_child(&mut child, Duration::from_secs(600))
        .await;
    if child.try_wait().ok().flatten().is_some() {
        recovery::clear_child(login_home)?;
    }
    result
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

fn start_background_clock(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut clock = tokio::time::interval(Duration::from_secs(30));
        clock.tick().await;
        loop {
            clock.tick().await;
            if app.emit("background-refresh-clock", ()).is_err() {
                break;
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .manage(OperationState::default())
        .manage(Arc::new(login::LoginState::default()))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(RecoveryState(Mutex::new(recovery::run(
                &data_dir,
                runtime::codex_home(),
            ))));
            app.manage(UsageCache::load(&data_dir));
            setup_tray(app)?;
            start_background_clock(app.handle());
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
            recovery_status,
            retry_recovery,
            cancel_login,
            restart_codex,
            update_tray_current_account,
            load_usage_cache,
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

#[cfg(test)]
mod query_error_tests {
    use super::*;

    #[test]
    fn classifies_actionable_usage_errors() {
        let auth = UsageQueryError::from("当前 auth.json 不包含 ChatGPT access token".to_string());
        assert_eq!(auth.code, "authentication_required");
        assert!(auth.reauth_required);

        for message in [
            "无法读取当前认证文件：Access is denied",
            "无法读取隔离认证状态",
            "DPAPI 解密失败",
            "当前 auth.json 不是有效 JSON",
        ] {
            let error = UsageQueryError::from(message.to_string());
            assert_eq!(error.code, "local_io");
            assert!(!error.reauth_required);
        }
        let changed =
            UsageQueryError::from("查询期间账号已变化，已丢弃本次结果，请重新刷新".to_string());
        assert_eq!(changed.code, "account_changed");
        assert!(!changed.reauth_required);
        assert!(
            UsageQueryError::from("App Server 请求失败：refresh_token_reused".to_string())
                .reauth_required
        );

        assert_eq!(
            UsageQueryError::from("App Server 请求超时".to_string()).code,
            "timeout"
        );
        assert_eq!(
            UsageQueryError::from("未找到 codex.exe".to_string()).code,
            "runtime_unavailable"
        );
    }
}
