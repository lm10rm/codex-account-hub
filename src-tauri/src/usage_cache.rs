use crate::app_server::UsageSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    version: u32,
    snapshots: HashMap<String, UsageSnapshot>,
}

pub struct UsageCache {
    snapshots: tokio::sync::Mutex<HashMap<String, UsageSnapshot>>,
}

impl UsageCache {
    pub fn load(data_dir: &Path) -> Self {
        Self {
            snapshots: tokio::sync::Mutex::new(load_snapshots(data_dir)),
        }
    }

    pub async fn all(&self) -> HashMap<String, UsageSnapshot> {
        self.snapshots.lock().await.clone()
    }

    pub async fn store(
        &self,
        data_dir: &Path,
        account_id: String,
        snapshot: UsageSnapshot,
    ) -> Result<(), String> {
        let mut snapshots = self.snapshots.lock().await;
        if snapshots
            .get(&account_id)
            .is_some_and(|current| current.captured_at > snapshot.captured_at)
        {
            return Ok(());
        }
        snapshots.insert(account_id, snapshot);
        write_cache(data_dir, &snapshots)
    }

    pub async fn remove(&self, data_dir: &Path, account_id: &str) -> Result<(), String> {
        let mut snapshots = self.snapshots.lock().await;
        snapshots.remove(account_id);
        write_cache(data_dir, &snapshots)
    }
}

fn load_snapshots(data_dir: &Path) -> HashMap<String, UsageSnapshot> {
    read_cache(data_dir).unwrap_or_default()
}

fn read_cache(data_dir: &Path) -> Result<HashMap<String, UsageSnapshot>, String> {
    let path = cache_path(data_dir);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let bytes = fs::read(&path).map_err(|error| format!("无法读取额度缓存：{error}"))?;
    let cache: CacheFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("额度缓存损坏：{error}"))?;
    if cache.version != CACHE_VERSION {
        return Err(format!("不支持的额度缓存版本：{}", cache.version));
    }
    Ok(cache.snapshots)
}

fn write_cache(data_dir: &Path, snapshots: &HashMap<String, UsageSnapshot>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&CacheFile {
        version: CACHE_VERSION,
        snapshots: snapshots.clone(),
    })
    .map_err(|error| error.to_string())?;
    atomic_write(&cache_path(data_dir), &bytes)
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("usage-cache.json")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "额度缓存没有父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建缓存目录：{error}"))?;
    let temp = parent.join(".usage-cache.json.tmp");
    if temp.exists() {
        fs::remove_file(&temp).map_err(|error| format!("无法清理旧缓存临时文件：{error}"))?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("无法创建缓存临时文件：{error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("无法写入缓存临时文件：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("无法同步缓存临时文件：{error}"))?;
    drop(file);

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            let _ = fs::remove_file(&temp);
            return Err(format!("原子提交额度缓存失败：{code}"));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if path.exists() {
            fs::remove_file(path).map_err(|error| format!("无法更新额度缓存：{error}"))?;
        }
        fs::rename(&temp, path).map_err(|error| format!("无法提交额度缓存：{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::{AccountInfo, LimitWindow};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-account-hub-cache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn snapshot(captured_at: u64) -> UsageSnapshot {
        UsageSnapshot {
            account: Some(AccountInfo {
                account_type: Some("chatgpt".to_string()),
                email: Some("a***@example.com".to_string()),
                plan_type: Some("plus".to_string()),
            }),
            primary: Some(LimitWindow {
                used_percent: Some(25.0),
                remaining_percent: Some(75.0),
                window_duration_mins: Some(300),
                resets_at: Some(1234),
            }),
            secondary: None,
            rate_limit_reached_type: None,
            reset_credits_available: Some(2),
            captured_at,
        }
    }

    #[test]
    fn round_trips_non_sensitive_usage_snapshots() {
        let dir = test_dir("roundtrip");
        let mut snapshots = HashMap::new();
        snapshots.insert("aaaaaaaaaaaaaaaaaaaaaaaa".to_string(), snapshot(42));
        write_cache(&dir, &snapshots).unwrap();
        let loaded = read_cache(&dir).unwrap();
        assert_eq!(loaded["aaaaaaaaaaaaaaaaaaaaaaaa"].captured_at, 42);
        assert_eq!(
            loaded["aaaaaaaaaaaaaaaaaaaaaaaa"]
                .account
                .as_ref()
                .and_then(|account| account.email.as_deref()),
            Some("a***@example.com")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignores_corrupt_cache_at_startup() {
        let dir = test_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(cache_path(&dir), b"not-json").unwrap();
        assert!(load_snapshots(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn does_not_replace_a_newer_snapshot_with_an_older_one() {
        tauri::async_runtime::block_on(async {
            let dir = test_dir("ordering");
            let cache = UsageCache::load(&dir);
            let account_id = "aaaaaaaaaaaaaaaaaaaaaaaa".to_string();
            cache
                .store(&dir, account_id.clone(), snapshot(100))
                .await
                .unwrap();
            cache
                .store(&dir, account_id.clone(), snapshot(50))
                .await
                .unwrap();
            assert_eq!(cache.all().await[&account_id].captured_at, 100);
            fs::remove_dir_all(dir).unwrap();
        });
    }
}
