//! Own only the CLI processes launched by the hub, never the user's desktop application.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    created: u64,
    image: String,
}

#[derive(Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub process: Identity,
    pub owner: Identity,
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::*;

    // Store the value rather than a raw pointer so the owning handle may cross async awaits.
    pub struct Handle(isize);
    impl Handle {
        fn raw(&self) -> HANDLE {
            self.0 as HANDLE
        }
        fn open(pid: u32, access: u32) -> Result<Option<Self>, String> {
            let raw = unsafe { OpenProcess(access, 0, pid) };
            if raw.is_null() {
                let error = unsafe { GetLastError() };
                if error == ERROR_INVALID_PARAMETER {
                    return Ok(None);
                }
                return Err(format!(
                    "无法检查子进程 PID {pid}（Windows {error}），请重试恢复"
                ));
            }
            Ok(Some(Self(raw as isize)))
        }
        fn running(&self) -> Result<bool, String> {
            match unsafe { WaitForSingleObject(self.raw(), 0) } {
                WAIT_OBJECT_0 => Ok(false),
                WAIT_TIMEOUT => Ok(true),
                _ => Err("无法确认子进程退出状态，请重试恢复".into()),
            }
        }
        fn identity(&self, pid: u32) -> Result<Identity, String> {
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            if unsafe {
                GetProcessTimes(self.raw(), &mut created, &mut exit, &mut kernel, &mut user)
            } == 0
            {
                return Err(format!("无法读取子进程 PID {pid} 的创建时间"));
            }
            let mut image = vec![0u16; 32768];
            let mut length = image.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    self.raw(),
                    PROCESS_NAME_WIN32,
                    image.as_mut_ptr(),
                    &mut length,
                )
            } == 0
            {
                return Err(format!("无法核验子进程 PID {pid} 的程序路径"));
            }
            Ok(Identity {
                pid,
                created: (u64::from(created.dwHighDateTime) << 32)
                    | u64::from(created.dwLowDateTime),
                image: String::from_utf16_lossy(&image[..length as usize]),
            })
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.raw()) };
        }
    }

    pub fn capture(pid: u32) -> Result<Identity, String> {
        Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?
            .ok_or("子进程已退出")?
            .identity(pid)
    }

    pub fn running(identity: &Identity) -> Result<bool, String> {
        let Some(handle) = Handle::open(
            identity.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE,
        )?
        else {
            return Ok(false);
        };
        Ok(handle.running()? && handle.identity(identity.pid)? == *identity)
    }

    pub fn legacy_running(pid: u32) -> Result<bool, String> {
        match Handle::open(pid, SYNCHRONIZATION_SYNCHRONIZE)? {
            Some(handle) => handle.running(),
            None => Ok(false),
        }
    }

    pub fn stop(record: &Record) -> Result<(), String> {
        if record.version != 1 {
            return Err("子进程记录版本不受支持".into());
        }
        if !running(&record.process)? {
            return Ok(());
        }
        if record.process.pid == std::process::id() {
            return Err("恢复记录指向管理器自身，已拒绝结束进程".into());
        }
        let current_owner = capture(std::process::id())?;
        if record.owner != current_owner && running(&record.owner)? {
            return Err("子进程仍由另一正在运行的管理器使用，请退出另一实例后重试恢复".into());
        }
        let Some(handle) = Handle::open(
            record.process.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE | PROCESS_TERMINATE,
        )?
        else {
            return Ok(());
        };
        // Check through the same handle used to terminate, avoiding PID reuse between checks.
        if !handle.running()? || handle.identity(record.process.pid)? != record.process {
            return Ok(());
        }
        if unsafe { TerminateProcess(handle.raw(), 1) } == 0 && handle.running()? {
            return Err(format!(
                "无法结束遗留子进程 PID {}，请重试恢复",
                record.process.pid
            ));
        }
        if unsafe { WaitForSingleObject(handle.raw(), 3000) } != WAIT_OBJECT_0 {
            return Err(format!(
                "遗留子进程 PID {} 尚未退出，请重试恢复",
                record.process.pid
            ));
        }
        Ok(())
    }

    /// The OS closes this non-inheritable handle even when the hub crashes or exits abruptly.
    pub struct ChildJob {
        _handle: Handle,
    }
    impl ChildJob {
        pub fn attach(pid: u32) -> Result<Self, String> {
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err("无法创建子进程回收任务".into());
            }
            let handle = Handle(raw as isize);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // Own the direct CLI only: a browser opened by `codex login` must outlive it.
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
            if unsafe {
                SetInformationJobObject(
                    handle.raw(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err("无法设置子进程退出回收策略".into());
            }
            let process =
                Handle::open(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)?.ok_or("子进程已退出")?;
            if unsafe { AssignProcessToJobObject(handle.raw(), process.raw()) } == 0 {
                return Err(format!(
                    "无法托管子进程（Windows {}），已取消本次操作",
                    unsafe { GetLastError() }
                ));
            }
            Ok(Self { _handle: handle })
        }
    }
}

#[cfg(windows)]
pub use win::*;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn sleeper() -> tokio::process::Child {
        tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[test]
    fn verified_recovery_rejects_reused_pid_and_live_other_owner() {
        tauri::async_runtime::block_on(async {
            let mut child = sleeper();
            let mut other_owner = sleeper();
            let mut record = Record {
                version: 1,
                process: capture(child.id().unwrap()).unwrap(),
                owner: capture(std::process::id()).unwrap(),
            };
            record.process.created += 1;
            stop(&record).unwrap();
            assert!(child.try_wait().unwrap().is_none());
            record.process = capture(child.id().unwrap()).unwrap();
            record.process.image.push_str(".different");
            stop(&record).unwrap();
            assert!(child.try_wait().unwrap().is_none());
            record.process = capture(child.id().unwrap()).unwrap();
            record.owner = capture(other_owner.id().unwrap()).unwrap();
            assert!(stop(&record).is_err());
            assert!(child.try_wait().unwrap().is_none());
            other_owner.kill().await.unwrap();
            stop(&record).unwrap();
            assert!(child.try_wait().unwrap().is_some());
        });
    }

    #[test]
    fn job_owner_helper() {
        let Some(path) = std::env::var_os("HUB_JOB_TEST_READY") else {
            return;
        };
        tauri::async_runtime::block_on(async {
            let mut child = sleeper();
            let _job = ChildJob::attach(child.id().unwrap()).unwrap();
            std::fs::write(path, child.id().unwrap().to_string()).unwrap();
            let _ = child.wait().await;
        });
    }

    #[test]
    fn abrupt_owner_exit_reaps_job_child_without_touching_unrelated_process() {
        tauri::async_runtime::block_on(async {
            let root = std::env::temp_dir().join(format!(
                "codex-hub-job-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let ready = root.join("ready.txt");
            let mut unrelated = sleeper();
            let mut owner = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "child_process::tests::job_owner_helper",
                    "--nocapture",
                ])
                .env("HUB_JOB_TEST_READY", &ready)
                .creation_flags(0x08000000)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            for _ in 0..100 {
                if ready.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let pid: u32 = std::fs::read_to_string(&ready)
                .expect("fixture helper did not start")
                .parse()
                .unwrap();
            assert!(legacy_running(pid).unwrap());
            owner.kill().await.unwrap();
            for _ in 0..60 {
                if !legacy_running(pid).unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(!legacy_running(pid).unwrap());
            assert!(unrelated.try_wait().unwrap().is_none());
            unrelated.kill().await.unwrap();
            assert_eq!(root.parent(), Some(std::env::temp_dir().as_path()));
            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
