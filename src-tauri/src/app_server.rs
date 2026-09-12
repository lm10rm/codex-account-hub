use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{Duration, timeout};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    #[serde(default)]
    pub account_id: String,
    pub account: Option<AccountInfo>,
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
    pub rate_limit_reached_type: Option<String>,
    pub reset_credits_available: Option<u64>,
    pub captured_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub account_type: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub window_duration_mins: Option<u64>,
    pub resets_at: Option<u64>,
}

pub async fn query_usage(codex_path: &str, codex_home: &str) -> Result<UsageSnapshot, String> {
    query_usage_owned(codex_path, codex_home, false).await
}

pub async fn query_usage_owned(
    codex_path: &str,
    codex_home: &str,
    isolated: bool,
) -> Result<UsageSnapshot, String> {
    query_for_home(codex_home, async {
        let mut process = AppServerProcess::start(codex_path, codex_home).await?;
        if isolated {
            if let Err(error) = crate::recovery::record_child(
                std::path::Path::new(codex_home),
                process.child.id().ok_or("查询进程已退出")?,
            ) {
                let _ = process.stop().await;
                return Err(error);
            }
        }
        let result = process.query().await;
        process.stop().await?;
        if isolated {
            crate::recovery::clear_child(std::path::Path::new(codex_home))?;
        }
        result
    })
    .await
}

async fn query_for_home(
    codex_home: &str,
    query: impl std::future::Future<Output = Result<UsageSnapshot, String>>,
) -> Result<UsageSnapshot, String> {
    let before = crate::vault::current_account_id(codex_home)?;
    let result = query.await;
    let after = crate::vault::current_account_id(codex_home).ok();
    verify_query_identity(&before, after.as_deref())?;
    let mut snapshot = result?;
    snapshot.account_id = before;
    Ok(snapshot)
}

fn verify_query_identity(before: &str, after: Option<&str>) -> Result<(), String> {
    if Some(before) != after {
        return Err("查询期间账号已变化，已丢弃本次结果，请重新刷新".into());
    }
    Ok(())
}

struct AppServerProcess {
    _job: crate::child_process::ChildJob,
    child: Child,
    stdin: tokio::process::ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl AppServerProcess {
    async fn start(codex_path: &str, codex_home: &str) -> Result<Self, String> {
        let mut child = Command::new(codex_path)
            .args(["app-server", "--stdio"])
            .env("CODEX_HOME", codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .creation_flags_no_window()
            .spawn()
            .map_err(|error| format!("无法启动 Codex App Server：{error}"))?;

        let job = match crate::child_process::ChildJob::attach(child.id().ok_or("查询进程已退出")?)
        {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "无法连接 App Server stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法连接 App Server stdout".to_string())?;

        Ok(Self {
            _job: job,
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }

    async fn query(&mut self) -> Result<UsageSnapshot, String> {
        self.send(json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "clientInfo": {
                    "name": "codex_account_hub",
                    "title": "Codex Account Hub",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {}
            }
        }))
        .await?;
        self.wait_for_result(1).await?;
        self.send(json!({ "method": "initialized", "params": {} }))
            .await?;

        self.send(json!({
            "method": "account/read",
            "id": 2,
            "params": { "refreshToken": false }
        }))
        .await?;
        let account_result = self.wait_for_result(2).await?;

        self.send(json!({
            "method": "account/rateLimits/read",
            "id": 3,
            "params": {}
        }))
        .await?;
        let usage_result = self.wait_for_result(3).await?;

        Ok(to_snapshot(&account_result, &usage_result))
    }

    async fn send(&mut self, message: Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| format!("App Server 写入失败：{error}"))?;
        self.stdin.flush().await.map_err(|error| error.to_string())
    }

    async fn wait_for_result(&mut self, id: u64) -> Result<Value, String> {
        timeout(REQUEST_TIMEOUT, async {
            while let Some(line) = self
                .lines
                .next_line()
                .await
                .map_err(|error| error.to_string())?
            {
                let message: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    let code = error.get("code").and_then(Value::as_i64);
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("未知错误");
                    return Err(format!("App Server 请求失败（{code:?}）：{text}"));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "App Server 响应缺少 result".to_string());
            }
            Err("App Server 在响应前退出".to_string())
        })
        .await
        .map_err(|_| "App Server 请求超时".to_string())?
    }

    async fn stop(&mut self) -> Result<(), String> {
        let _ = self.stdin.shutdown().await;
        match timeout(Duration::from_millis(800), self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            _ => self
                .child
                .kill()
                .await
                .map_err(|error| format!("无法确认查询进程退出：{error}")),
        }
    }
}

fn to_snapshot(account_result: &Value, usage_result: &Value) -> UsageSnapshot {
    let account = account_result.get("account").map(|value| AccountInfo {
        account_type: string(value, &["type"]),
        email: string(value, &["email"]).map(|email| redact_email(&email)),
        plan_type: string(value, &["planType", "plan_type"]),
    });

    let limits = usage_result
        .get("rateLimits")
        .or_else(|| usage_result.get("rate_limits"));
    UsageSnapshot {
        account_id: String::new(),
        account,
        primary: limits
            .and_then(|value| value.get("primary"))
            .and_then(parse_window),
        secondary: limits
            .and_then(|value| value.get("secondary"))
            .and_then(parse_window),
        rate_limit_reached_type: limits
            .and_then(|value| string(value, &["rateLimitReachedType", "rate_limit_reached_type"])),
        reset_credits_available: usage_result
            .get("rateLimitResetCredits")
            .or_else(|| usage_result.get("rate_limit_reset_credits"))
            .and_then(|value| number_u64(value, &["availableCount", "available_count"])),
        captured_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

fn parse_window(value: &Value) -> Option<LimitWindow> {
    if value.is_null() {
        return None;
    }
    let used = number_f64(value, &["usedPercent", "used_percent"]);
    Some(LimitWindow {
        used_percent: used,
        remaining_percent: used.map(|number| (100.0 - number).clamp(0.0, 100.0)),
        window_duration_mins: number_u64(value, &["windowDurationMins", "window_duration_mins"]),
        resets_at: number_u64(value, &["resetsAt", "resets_at"]),
    })
}

fn string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn number_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_f64))
}

fn number_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_u64))
}

fn redact_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_string();
    };
    let first = local.chars().next().unwrap_or('*');
    format!("{first}***@{domain}")
}

trait CommandWindowsExt {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl CommandWindowsExt for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            self.creation_flags(0x08000000);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_usage_when_login_changes_or_disappears_during_query() {
        assert!(verify_query_identity("account-a", Some("account-a")).is_ok());
        assert!(verify_query_identity("account-a", Some("account-b")).is_err());
        assert!(verify_query_identity("account-a", None).is_err());
    }

    #[test]
    fn binds_results_to_auth_identity_and_rejects_external_switch() {
        tauri::async_runtime::block_on(async {
            let home = std::env::temp_dir().join(format!(
                "codex-hub-query-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let a = br#"{"tokens":{"access_token":"fixture-a","account_id":"account-a"}}"#;
            let a_refreshed =
                br#"{"tokens":{"access_token":"fixture-renewed","account_id":"account-a"}}"#;
            let b = br#"{"tokens":{"access_token":"fixture-b","account_id":"account-b"}}"#;
            std::fs::write(home.join("auth.json"), a).unwrap();
            let expected = crate::vault::current_account_id(home.to_str().unwrap()).unwrap();
            let snapshot = query_for_home(home.to_str().unwrap(), async {
                std::fs::write(home.join("auth.json"), a_refreshed).unwrap();
                Ok(to_snapshot(&json!({}), &json!({})))
            })
            .await
            .unwrap();
            assert_eq!(snapshot.account_id, expected);
            let result = query_for_home(home.to_str().unwrap(), async {
                std::fs::write(home.join("auth.json"), b).unwrap();
                Ok(to_snapshot(&json!({}), &json!({})))
            })
            .await;
            assert!(result.unwrap_err().contains("查询期间账号已变化"));
            assert_eq!(home.parent(), Some(std::env::temp_dir().as_path()));
            std::fs::remove_dir_all(home).unwrap();
        });
    }

    #[test]
    fn parses_camel_case_usage() {
        let account = json!({
            "account": { "type": "chatgpt", "email": "alice@example.com", "planType": "plus" }
        });
        let usage = json!({
            "rateLimits": {
                "primary": { "usedPercent": 63.0, "windowDurationMins": 300, "resetsAt": 1000 },
                "secondary": null,
                "rateLimitReachedType": null
            },
            "rateLimitResetCredits": { "availableCount": 3 }
        });
        let snapshot = to_snapshot(&account, &usage);
        assert_eq!(
            snapshot.account.unwrap().email.as_deref(),
            Some("a***@example.com")
        );
        assert_eq!(snapshot.primary.unwrap().remaining_percent, Some(37.0));
        assert_eq!(snapshot.reset_credits_available, Some(3));
    }
}
