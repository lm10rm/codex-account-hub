use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartOutcome {
    pub was_running: bool,
    pub processes_closed: usize,
}

#[cfg(windows)]
pub fn restart() -> Result<RestartOutcome, String> {
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    let initial = discover_processes()?;
    let app_id = initial
        .first()
        .map(|process| process.app_id.clone())
        .unwrap_or_else(|| "OpenAI.Codex_2p2nqsd0c76g0!App".to_string());

    if !initial.is_empty() {
        request_graceful_close(&initial);
        thread::sleep(Duration::from_millis(1800));
    }

    let remaining = discover_processes()?;
    for process in &remaining {
        terminate_process(process.pid)?;
    }
    if !remaining.is_empty() {
        thread::sleep(Duration::from_millis(900));
    }

    let mut command = Command::new("explorer.exe");
    command.arg(format!("shell:AppsFolder\\{app_id}"));
    command.creation_flags_no_window();
    command
        .spawn()
        .map_err(|error| format!("账号已切换，但无法重新启动 Codex：{error}"))?;

    Ok(RestartOutcome {
        was_running: !initial.is_empty(),
        processes_closed: initial.len(),
    })
}

#[cfg(not(windows))]
pub fn restart() -> Result<RestartOutcome, String> {
    Err("Codex 桌面端自动重启当前只支持 Windows".to_string())
}

#[cfg(windows)]
#[derive(Debug)]
struct CodexProcess {
    pid: u32,
    app_id: String,
}

#[cfg(windows)]
fn discover_processes() -> Result<Vec<CodexProcess>, String> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err("无法枚举 Windows 进程".to_string());
    }

    let mut result = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let end = entry
            .szExeFile
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(entry.szExeFile.len());
        let executable = String::from_utf16_lossy(&entry.szExeFile[..end]);
        if executable.eq_ignore_ascii_case("ChatGPT.exe")
            && let Some(app_id) = application_user_model_id(entry.th32ProcessID)
            && app_id.starts_with("OpenAI.Codex_")
            && app_id.ends_with("!App")
        {
            result.push(CodexProcess {
                pid: entry.th32ProcessID,
                app_id,
            });
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    Ok(result)
}

#[cfg(windows)]
fn application_user_model_id(pid: u32) -> Option<String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }

    let mut length = 0u32;
    let first = unsafe { GetApplicationUserModelId(process, &mut length, ptr::null_mut()) };
    if first != ERROR_INSUFFICIENT_BUFFER || length == 0 {
        unsafe { CloseHandle(process) };
        return None;
    }
    let mut buffer = vec![0u16; length as usize];
    let second = unsafe { GetApplicationUserModelId(process, &mut length, buffer.as_mut_ptr()) };
    unsafe { CloseHandle(process) };
    if second != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(windows)]
fn request_graceful_close(processes: &[CodexProcess]) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };
    use windows_sys::core::BOOL;

    unsafe extern "system" fn close_window(window: HWND, context: LPARAM) -> BOOL {
        let pids = unsafe { &*(context as *const Vec<u32>) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pids.contains(&pid) {
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        }
        1
    }

    let pids = processes
        .iter()
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    unsafe { EnumWindows(Some(close_window), &pids as *const Vec<u32> as LPARAM) };
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if process.is_null() {
        return Ok(());
    }
    let terminated = unsafe { TerminateProcess(process, 0) };
    unsafe { CloseHandle(process) };
    if terminated == 0 {
        Err(format!("无法结束 Codex 进程 {pid}"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
trait CommandWindowsExt {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl CommandWindowsExt for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x08000000);
        self
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a running Codex desktop app"]
    fn detects_running_codex_desktop_without_touching_it() {
        let processes = discover_processes().unwrap();
        assert!(!processes.is_empty());
        assert!(
            processes
                .iter()
                .all(|process| process.app_id.starts_with("OpenAI.Codex_"))
        );
    }
}
