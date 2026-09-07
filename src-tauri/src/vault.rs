use crate::app_server::AccountInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAccount {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub imported_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountIndex {
    version: u32,
    accounts: Vec<SavedAccount>,
}

impl Default for AccountIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            accounts: Vec::new(),
        }
    }
}

pub fn list_accounts(data_dir: &Path) -> Result<Vec<SavedAccount>, String> {
    Ok(read_index(data_dir)?.accounts)
}

pub fn prepare_isolated_home(data_dir: &Path, account_id: &str) -> Result<PathBuf, String> {
    let auth = load_saved_auth(data_dir, account_id)?;

    let home = data_dir.join("runtime").join(account_id);
    fs::create_dir_all(&home).map_err(|error| format!("无法创建隔离运行目录：{error}"))?;
    atomic_write(&home.join("auth.json"), &auth)?;
    Ok(home)
}

pub fn activate_account(data_dir: &Path, account_id: &str, codex_home: &str) -> Result<(), String> {
    let target_auth = load_saved_auth(data_dir, account_id)?;
    let auth_path = Path::new(codex_home).join("auth.json");
    let current_auth = fs::read(&auth_path)
        .map_err(|error| format!("无法读取当前认证文件 {}：{error}", auth_path.display()))?;
    validate_auth(&current_auth)?;
    let current_id = auth_identity(&current_auth)?;
    if current_id == account_id {
        return Ok(());
    }

    let encrypted_backup = protect(&current_auth)?;
    let backup_path = data_dir
        .join("switch-backups")
        .join(format!("{}-{current_id}.dpapi", now_seconds()));
    atomic_write(&backup_path, &encrypted_backup)?;

    replace_auth_file(&auth_path, &target_auth)?;
    let activated = fs::read(&auth_path).map_err(|error| format!("无法验证切换结果：{error}"))?;
    if auth_identity(&activated)? == account_id {
        return Ok(());
    }

    replace_auth_file(&auth_path, &current_auth)
        .map_err(|error| format!("切换验证失败，且恢复原账号失败：{error}"))?;
    Err("切换验证失败，已恢复原账号".to_string())
}

pub fn finish_isolated_home(data_dir: &Path, account_id: &str, home: &Path) -> Result<(), String> {
    validate_account_id(account_id)?;
    let auth_path = home.join("auth.json");
    if !auth_path.exists() {
        return Ok(());
    }

    let update_result = (|| {
        let auth =
            fs::read(&auth_path).map_err(|error| format!("无法读取隔离认证状态：{error}"))?;
        validate_auth(&auth)?;
        let encrypted = protect(&auth)?;
        atomic_write(
            &data_dir.join("vault").join(format!("{account_id}.dpapi")),
            &encrypted,
        )
    })();
    let cleanup_result =
        fs::remove_file(&auth_path).map_err(|error| format!("无法清理临时认证文件：{error}"));

    match (update_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(update_error), Err(cleanup_error)) => Err(format!("{update_error}；{cleanup_error}")),
    }
}

pub fn remove_plain_auth(home: &Path) -> Result<(), String> {
    let auth_path = home.join("auth.json");
    if auth_path.exists() {
        fs::remove_file(&auth_path).map_err(|error| format!("无法清理临时认证文件：{error}"))?;
    }
    Ok(())
}

pub fn cleanup_stale_login_auth(data_dir: &Path) -> Result<(), String> {
    let login_root = data_dir.join("login");
    if !login_root.exists() {
        return Ok(());
    }
    for entry in
        fs::read_dir(&login_root).map_err(|error| format!("无法检查隔离登录目录：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取隔离登录目录项：{error}"))?;
        if entry
            .file_type()
            .map_err(|error| format!("无法检查隔离登录目录项：{error}"))?
            .is_dir()
        {
            remove_plain_auth(&entry.path())?;
        }
    }
    Ok(())
}

pub fn import_current(
    data_dir: &Path,
    codex_home: &str,
    account: Option<AccountInfo>,
    label: Option<String>,
) -> Result<SavedAccount, String> {
    let auth_path = Path::new(codex_home).join("auth.json");
    let auth = fs::read(&auth_path)
        .map_err(|error| format!("无法读取当前认证文件 {}：{error}", auth_path.display()))?;
    validate_auth(&auth)?;

    let id = auth_identity(&auth)?;
    let encrypted = protect(&auth)?;
    let vault_dir = data_dir.join("vault");
    fs::create_dir_all(&vault_dir).map_err(|error| format!("无法创建账号保险库：{error}"))?;
    atomic_write(&vault_dir.join(format!("{id}.dpapi")), &encrypted)?;

    let mut index = read_index(data_dir)?;
    let email = account.as_ref().and_then(|value| value.email.clone());
    let plan_type = account.as_ref().and_then(|value| value.plan_type.clone());
    let default_label = email.clone().unwrap_or_else(|| "Codex 账号".to_string());
    let requested_label = label
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let existing_label = index
        .accounts
        .iter()
        .find(|value| value.id == id)
        .map(|value| value.label.clone());
    let clean_label = requested_label.or(existing_label).unwrap_or(default_label);
    if clean_label.chars().count() > 80 {
        return Err("账号名称不能超过 80 个字符".to_string());
    }

    let saved = SavedAccount {
        id: id.clone(),
        label: clean_label,
        email,
        plan_type,
        imported_at: now_seconds(),
    };
    if let Some(existing) = index.accounts.iter_mut().find(|value| value.id == id) {
        *existing = saved.clone();
    } else {
        index.accounts.push(saved.clone());
    }
    write_index(data_dir, &index)?;
    Ok(saved)
}

fn validate_auth(bytes: &[u8]) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "当前 auth.json 不是有效 JSON".to_string())?;
    let has_access_token = value
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|token| !token.is_empty());
    if !has_access_token {
        return Err("当前 auth.json 不包含 ChatGPT access token".to_string());
    }
    Ok(())
}

fn auth_identity(bytes: &[u8]) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let stable_value = value
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("tokens")
                .and_then(|tokens| tokens.get("id_token"))
                .and_then(serde_json::Value::as_str)
        })
        .ok_or_else(|| "无法从认证文件识别账号".to_string())?;
    let digest = Sha256::digest(stable_value.as_bytes());
    Ok(format!("{:x}", digest)[..24].to_string())
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.len() == 24 && account_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("账号 ID 格式无效".to_string())
    }
}

fn load_saved_auth(data_dir: &Path, account_id: &str) -> Result<Vec<u8>, String> {
    validate_account_id(account_id)?;
    let index = read_index(data_dir)?;
    if !index
        .accounts
        .iter()
        .any(|account| account.id == account_id)
    {
        return Err("找不到这个保险库账号".to_string());
    }
    let encrypted_path = data_dir.join("vault").join(format!("{account_id}.dpapi"));
    let encrypted = fs::read(&encrypted_path)
        .map_err(|error| format!("无法读取加密账号 {}：{error}", encrypted_path.display()))?;
    let auth = unprotect(&encrypted)?;
    validate_auth(&auth)?;
    Ok(auth)
}

fn read_index(data_dir: &Path) -> Result<AccountIndex, String> {
    let path = index_path(data_dir);
    if !path.exists() {
        return Ok(AccountIndex::default());
    }
    let bytes = fs::read(&path).map_err(|error| format!("无法读取账号索引：{error}"))?;
    let index: AccountIndex =
        serde_json::from_slice(&bytes).map_err(|error| format!("账号索引损坏：{error}"))?;
    if index.version != INDEX_VERSION {
        return Err(format!("不支持的账号索引版本：{}", index.version));
    }
    Ok(index)
}

fn write_index(data_dir: &Path, index: &AccountIndex) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
    let bytes = serde_json::to_vec_pretty(index).map_err(|error| error.to_string())?;
    atomic_write(&index_path(data_dir), &bytes)
}

fn index_path(data_dir: &Path) -> PathBuf {
    data_dir.join("accounts.json")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "目标文件没有父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temp, bytes).map_err(|error| format!("写入临时文件失败：{error}"))?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| format!("无法更新账号文件：{error}"))?;
    }
    fs::rename(&temp, path).map_err(|error| format!("提交账号文件失败：{error}"))
}

#[cfg(windows)]
fn replace_auth_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let parent = path
        .parent()
        .ok_or_else(|| "认证文件没有父目录".to_string())?;
    let temp = parent.join(".auth.json.codex-account-hub.tmp");
    if temp.exists() {
        fs::remove_file(&temp).map_err(|error| format!("无法清理旧切换临时文件：{error}"))?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("无法创建切换临时文件：{error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("无法写入切换临时文件：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("无法同步切换临时文件：{error}"))?;
    drop(file);

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
        return Err(format!("原子替换认证文件失败：{code}"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_auth_file(_path: &Path, _bytes: &[u8]) -> Result<(), String> {
    Err("账号切换当前只支持 Windows".to_string())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(windows)]
fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext
            .len()
            .try_into()
            .map_err(|_| "认证文件过大".to_string())?,
        pbData: plaintext.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("Windows DPAPI 加密失败：{}", unsafe {
            GetLastError()
        }));
    }
    let encrypted =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(encrypted)
}

#[cfg(windows)]
fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: ciphertext
            .len()
            .try_into()
            .map_err(|_| "加密账号文件过大".to_string())?,
        pbData: ciphertext.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("Windows DPAPI 解密失败：{}", unsafe {
            GetLastError()
        }));
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn protect(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Err("账号保险库当前只支持 Windows DPAPI".to_string())
}

#[cfg(not(windows))]
fn unprotect(_ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    Err("账号保险库当前只支持 Windows DPAPI".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_auth_without_access_token() {
        assert!(validate_auth(br#"{"tokens":{}}"#).is_err());
    }

    #[test]
    fn identity_is_stable_for_same_account() {
        let a = br#"{"tokens":{"access_token":"one","account_id":"account-1"}}"#;
        let b = br#"{"tokens":{"access_token":"two","account_id":"account-1"}}"#;
        assert_eq!(auth_identity(a).unwrap(), auth_identity(b).unwrap());
    }

    #[test]
    fn rejects_account_ids_that_could_escape_the_vault() {
        assert!(validate_account_id("../auth").is_err());
        assert!(validate_account_id("aaaaaaaaaaaaaaaaaaaaaaaa").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn atomically_replaces_auth_file() {
        let test_dir = std::env::temp_dir().join(format!(
            "codex-account-hub-replace-test-{}-{}",
            std::process::id(),
            now_seconds()
        ));
        fs::create_dir_all(&test_dir).unwrap();
        let auth_path = test_dir.join("auth.json");
        fs::write(&auth_path, b"old-auth").unwrap();
        replace_auth_file(&auth_path, b"new-auth").unwrap();
        assert_eq!(fs::read(&auth_path).unwrap(), b"new-auth");
        fs::remove_dir_all(&test_dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_output_does_not_contain_plaintext() {
        let secret = b"fixture-refresh-token-never-real";
        let encrypted = protect(secret).unwrap();
        assert!(
            !encrypted
                .windows(secret.len())
                .any(|window| window == secret)
        );
        assert_eq!(unprotect(&encrypted).unwrap(), secret);
    }
}
