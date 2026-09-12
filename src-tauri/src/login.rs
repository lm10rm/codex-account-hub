use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Child;
use tokio::time::{Instant, timeout};

pub const CANCELLED: &str = "登录已取消";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub request_id: String,
    pub phase: &'static str,
}

#[derive(Default)]
pub struct LoginState(Mutex<Option<Progress>>);

pub struct Session {
    state: Arc<LoginState>,
    request_id: String,
}

impl LoginState {
    pub fn begin(self: &Arc<Self>, request_id: String) -> Result<Session, String> {
        if request_id.is_empty() || request_id.len() > 80 {
            return Err("登录请求标识无效".into());
        }
        let mut active = self.0.lock().map_err(|_| "登录状态不可用")?;
        if active.is_some() {
            return Err("已有登录进行中".into());
        }
        *active = Some(Progress {
            request_id: request_id.clone(),
            phase: "queued",
        });
        Ok(Session {
            state: self.clone(),
            request_id,
        })
    }

    pub fn cancel(&self, request_id: &str) -> Result<bool, String> {
        let mut active = self.0.lock().map_err(|_| "登录状态不可用")?;
        if let Some(progress) = active.as_mut() {
            if progress.request_id == request_id && progress.phase != "saving" {
                progress.phase = "cancelling";
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Session {
    pub fn phase(&self, phase: &'static str) -> Result<Progress, String> {
        let mut active = self.state.0.lock().map_err(|_| "登录状态不可用")?;
        let progress = active.as_mut().ok_or("登录已经结束")?;
        if progress.phase == "cancelling" {
            return Err(CANCELLED.into());
        }
        progress.phase = phase;
        Ok(progress.clone())
    }

    pub fn check(&self) -> Result<(), String> {
        let active = self.state.0.lock().map_err(|_| "登录状态不可用")?;
        if active
            .as_ref()
            .is_none_or(|progress| progress.phase == "cancelling")
        {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    pub async fn wait<T>(&self, future: impl std::future::Future<Output = T>) -> Result<T, String> {
        let mut future = std::pin::pin!(future);
        loop {
            self.check()?;
            if let Ok(result) = timeout(Duration::from_millis(50), &mut future).await {
                self.check()?;
                return Ok(result);
            }
        }
    }

    pub async fn wait_child(&self, child: &mut Child, deadline: Duration) -> Result<(), String> {
        let end = Instant::now() + deadline;
        loop {
            let stop = self.check().err().or_else(|| {
                (Instant::now() >= end).then(|| "登录等待已超过 10 分钟，请重试".into())
            });
            if let Some(reason) = stop {
                child
                    .kill()
                    .await
                    .map_err(|error| format!("{reason}；无法结束登录进程：{error}"))?;
                child
                    .wait()
                    .await
                    .map_err(|error| format!("无法确认登录进程退出：{error}"))?;
                return Err(reason);
            }
            if let Ok(status) = timeout(Duration::from_millis(50), child.wait()).await {
                let status = status.map_err(|error| format!("等待 Codex 登录失败：{error}"))?;
                self.check()?;
                return if status.success() {
                    Ok(())
                } else {
                    Err("Codex 登录未完成或已取消".into())
                };
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut active) = self.state.0.lock() {
            if active
                .as_ref()
                .is_some_and(|progress| progress.request_id == self.request_id)
            {
                *active = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_scoped_and_cannot_interrupt_commit() {
        let state = Arc::new(LoginState::default());
        let session = state.begin("first".into()).unwrap();
        assert!(!state.cancel("previous").unwrap());
        session.phase("saving").unwrap();
        assert!(!state.cancel("first").unwrap());
        drop(session);
        let next = state.begin("second".into()).unwrap();
        assert!(state.cancel("second").unwrap());
        assert!(next.phase("saving").is_err());
        drop(next);
        assert!(state.begin("third".into()).is_ok());
    }

    #[test]
    fn cancellation_releases_queued_operation() {
        tauri::async_runtime::block_on(async {
            let state = Arc::new(LoginState::default());
            let session = state.begin("queued".into()).unwrap();
            let lock = tokio::sync::RwLock::new(());
            let held = lock.write().await;
            state.cancel("queued").unwrap();
            assert!(session.wait(lock.write()).await.is_err());
            drop(held);
            assert!(lock.try_write().is_ok());
        });
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_and_timeout_reap_only_the_fixture_process() {
        tauri::async_runtime::block_on(async {
            for cancel in [true, false] {
                let state = Arc::new(LoginState::default());
                let session = state.begin("fixture".into()).unwrap();
                let mut command = tokio::process::Command::new("powershell.exe");
                command
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        "Start-Sleep -Seconds 30",
                    ])
                    .creation_flags(0x08000000)
                    .kill_on_drop(true);
                let mut child = command.spawn().unwrap();
                if cancel {
                    state.cancel("fixture").unwrap();
                }
                assert!(
                    session
                        .wait_child(&mut child, Duration::ZERO)
                        .await
                        .is_err()
                );
                assert!(child.try_wait().unwrap().is_some());
            }
        });
    }
}
