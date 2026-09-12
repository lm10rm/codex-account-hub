use crate::app_server::AccountInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_VERSION: u32 = 1;
const MAX_SWITCH_BACKUPS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAccount {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub imported_at: u64,
    #[serde(default)]
    pub is_active: bool,
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

pub fn current_account_id(codex_home: &str) -> Result<String, String> {
    let auth_path = Path::new(codex_home).join("auth.json");
    let auth = fs::read(&auth_path)
        .map_err(|error| format!("无法读取当前认证文件 {}：{error}", auth_path.display()))?;
    validate_auth(&auth)?;
    auth_identity(&auth)
}

pub fn isolated_account_id(home: &Path) -> Result<String, String> {
    let auth = fs::read(home.join("auth.json"))
        .map_err(|error| format!("无法读取隔离认证文件：{error}"))?;
    validate_auth(&auth)?;
    auth_identity(&auth)
}

pub fn rename_account(
    data_dir: &Path,
    account_id: &str,
    label: &str,
) -> Result<SavedAccount, String> {
    validate_account_id(account_id)?;
    let label = label.trim();
    if label.is_empty() {
        return Err("账号名称不能为空".to_string());
    }
    if label.chars().count() > 80 {
        return Err("账号名称不能超过 80 个字符".to_string());
    }

    let mut index = read_index(data_dir)?;
    let account = index
        .accounts
        .iter_mut()
        .find(|account| account.id == account_id)
        .ok_or_else(|| "找不到这个保险库账号".to_string())?;
    account.label = label.to_string();
    let updated = account.clone();
    write_index(data_dir, &index)?;
    Ok(updated)
}

pub fn delete_account(
    data_dir: &Path,
    account_id: &str,
    current_account_id: Option<&str>,
) -> Result<(), String> {
    validate_account_id(account_id)?;
    if current_account_id == Some(account_id) {
        return Err("不能删除当前正在使用的账号，请先切换到其他账号".to_string());
    }

    let mut index = read_index(data_dir)?;
    let position = index
        .accounts
        .iter()
        .position(|account| account.id == account_id)
        .ok_or_else(|| "找不到这个保险库账号".to_string())?;
    let runtime_path = data_dir.join("runtime").join(account_id);
    if runtime_path.exists() {
        remove_plain_auth(&runtime_path)?;
        fs::remove_dir_all(&runtime_path)
            .map_err(|error| format!("无法清理账号运行目录：{error}"))?;
    }
    let vault_path = data_dir.join("vault").join(format!("{account_id}.dpapi"));
    let staged_path = data_dir
        .join("vault")
        .join(format!(".{account_id}.deleting"));
    if staged_path.exists() {
        fs::remove_file(&staged_path).map_err(|error| format!("无法清理上次删除残留：{error}"))?;
    }
    if vault_path.exists() {
        fs::rename(&vault_path, &staged_path)
            .map_err(|error| format!("无法暂存待删除账号：{error}"))?;
    }

    index.accounts.remove(position);
    if let Err(error) = write_index(data_dir, &index) {
        if staged_path.exists() {
            let _ = fs::rename(&staged_path, &vault_path);
        }
        return Err(error);
    }
    if staged_path.exists() {
        let _ = fs::remove_file(&staged_path);
    }
    Ok(())
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
    replace_current_auth(data_dir, codex_home, &target_auth, account_id, false)
}

fn read_optional_auth(codex_home: &str) -> Result<Option<Vec<u8>>, String> {
    let auth_path = Path::new(codex_home).join("auth.json");
    match fs::read(auth_path) {
        Ok(auth) => Ok(Some(auth)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("无法读取当前认证文件：{error}")),
    }
}

// A missing/damaged active login must not prevent recovery from the vault.
// Never import the target's old active credentials over its saved authorization.
pub fn save_current_before_switch(
    data_dir: &Path,
    codex_home: &str,
    target_id: &str,
) -> Result<(), String> {
    if let Some(auth) = read_optional_auth(codex_home)? {
        if validate_auth(&auth).is_ok()
            && let Ok(current_id) = auth_identity(&auth)
            && current_id != target_id
        {
            import_current(data_dir, codex_home, None, None)?;
        }
    }
    Ok(())
}

pub fn complete_reauthorization(
    data_dir: &Path,
    codex_home: &str,
    login_home: &Path,
    account_id: &str,
    account: Option<AccountInfo>,
) -> Result<(SavedAccount, bool), String> {
    let auth = fs::read(login_home.join("auth.json"))
        .map_err(|error| format!("无法读取登录认证文件：{error}"))?;
    validate_auth(&auth)?;
    if auth_identity(&auth)? != account_id {
        return Err("登录的不是所选账号，已取消覆盖；请用该账号重新登录".into());
    }
    let is_current = current_account_id(codex_home).ok().as_deref() == Some(account_id);
    if is_current {
        // Update the active file first: on failure, the vault still matches it.
        // A subsequent switch must never overwrite new authorization with the old file.
        replace_current_auth(data_dir, codex_home, &auth, account_id, true)?;
    }
    let saved = import_current(
        data_dir,
        login_home.to_str().ok_or("隔离登录目录不是有效路径")?,
        account,
        None,
    )
    .map_err(|error| {
        if is_current {
            format!("当前登录已更新，但保险库保存失败，请重新导入当前账号：{error}")
        } else {
            error
        }
    })?;
    Ok((saved, is_current))
}

fn replace_current_auth(
    data_dir: &Path,
    codex_home: &str,
    target_auth: &[u8],
    account_id: &str,
    require_same_account: bool,
) -> Result<(), String> {
    replace_current_auth_verified(
        data_dir,
        codex_home,
        target_auth,
        account_id,
        require_same_account,
        |path| {
            let activated = fs::read(path).map_err(|error| format!("无法验证切换结果：{error}"))?;
            validate_auth(&activated)?;
            if auth_identity(&activated)? == account_id {
                Ok(())
            } else {
                Err("切换后账号身份不一致".into())
            }
        },
    )
}

fn replace_current_auth_verified(
    data_dir: &Path,
    codex_home: &str,
    target_auth: &[u8],
    account_id: &str,
    require_same_account: bool,
    verify: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    validate_auth(target_auth)?;
    if auth_identity(target_auth)? != account_id {
        return Err("保存的凭据与目标账号不一致，已取消写入".into());
    }
    let auth_path = Path::new(codex_home).join("auth.json");
    let current_auth = read_optional_auth(codex_home)?;
    if require_same_account
        && current_auth
            .as_deref()
            .and_then(|auth| auth_identity(auth).ok())
            .as_deref()
            != Some(account_id)
    {
        return Err("当前登录已发生变化，已取消写入，请重试".into());
    }
    if current_auth.as_deref() == Some(target_auth) {
        return Ok(());
    }

    if let Some(auth) = &current_auth {
        let current_id = auth_identity(auth).unwrap_or_else(|_| "unrecognized".into());
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let backup_path = data_dir
            .join("switch-backups")
            .join(format!("{}-{nonce}-{current_id}.dpapi", now_seconds()));
        atomic_write(&backup_path, &protect(auth)?)?;
        prune_switch_backups(data_dir, MAX_SWITCH_BACKUPS)?;
    }
    if read_optional_auth(codex_home)? != current_auth {
        return Err("当前登录已发生变化，已取消写入，请重试".into());
    }

    replace_auth_file(&auth_path, target_auth)?;
    let verification = verify(&auth_path);
    if let Err(error) = verification {
        let recovery = match &current_auth {
            Some(auth) => replace_auth_file(&auth_path, auth),
            None => fs::remove_file(&auth_path).map_err(|error| error.to_string()),
        };
        recovery.map_err(|recovery| format!("{error}；恢复原认证失败：{recovery}"))?;
        return Err(format!("{error}；已恢复原认证状态"));
    }
    Ok(())
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
        if auth_identity(&auth)? != account_id {
            return Err("隔离凭据与目标账号不一致，已取消回写".to_string());
        }
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
    let existing = index.accounts.iter().find(|value| value.id == id).cloned();
    let email = account
        .as_ref()
        .and_then(|value| value.email.clone())
        .or_else(|| existing.as_ref().and_then(|value| value.email.clone()));
    let plan_type = account
        .as_ref()
        .and_then(|value| value.plan_type.clone())
        .or_else(|| existing.as_ref().and_then(|value| value.plan_type.clone()));
    let default_label = email.clone().unwrap_or_else(|| "Codex 账号".to_string());
    let requested_label = label
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let existing_label = existing.map(|value| value.label);
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
        is_active: false,
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
    if auth_identity(&auth)? != account_id {
        return Err("保存的凭据与目标账号不一致".into());
    }
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

fn prune_switch_backups(data_dir: &Path, keep: usize) -> Result<(), String> {
    let backup_dir = data_dir.join("switch-backups");
    if !backup_dir.exists() {
        return Ok(());
    }
    let mut backups = fs::read_dir(&backup_dir)
        .map_err(|error| format!("无法读取切换备份目录：{error}"))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "dpapi")
        })
        .collect::<Vec<_>>();
    backups.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for backup in backups.into_iter().skip(keep) {
        fs::remove_file(backup.path()).map_err(|error| format!("无法轮换切换备份：{error}"))?;
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "目标文件没有父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    if temp.exists() {
        fs::remove_file(&temp).map_err(|error| format!("无法清理旧临时文件：{error}"))?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("写入临时文件失败：{error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("写入临时文件失败：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("同步临时文件失败：{error}"))?;
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
            return Err(format!("原子提交账号文件失败：{code}"));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if path.exists() {
            fs::remove_file(path).map_err(|error| format!("无法更新账号文件：{error}"))?;
        }
        fs::rename(&temp, path).map_err(|error| format!("提交账号文件失败：{error}"))
    }
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
    fs::create_dir_all(parent).map_err(|error| format!("无法创建认证目录：{error}"))?;
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

    #[cfg(windows)]
    struct Fixture(PathBuf);

    #[cfg(windows)]
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "codex-hub-recovery-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn home(&self, name: &str, auth: &[u8]) -> PathBuf {
            let home = self.0.join(name);
            fs::create_dir_all(&home).unwrap();
            fs::write(home.join("auth.json"), auth).unwrap();
            home
        }
    }

    #[cfg(windows)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    const OLD: &[u8] = br#"{"tokens":{"access_token":"fixture-old","account_id":"fixture-a"}}"#;
    #[cfg(windows)]
    const NEW: &[u8] = br#"{"tokens":{"access_token":"fixture-renewed","account_id":"fixture-a"}}"#;
    #[cfg(windows)]
    const OTHER: &[u8] = br#"{"tokens":{"access_token":"fixture-other","account_id":"fixture-b"}}"#;

    #[cfg(windows)]
    #[test]
    fn restores_original_state_after_post_write_verification_errors() {
        let fixture = Fixture::new();
        let target_id = auth_identity(NEW).unwrap();
        for (index, failure) in [
            "无法读取认证文件",
            "当前 auth.json 不是有效 JSON",
            "身份不一致",
        ]
        .iter()
        .enumerate()
        {
            let main = fixture.home(&format!("main-{index}"), OLD);
            let error = replace_current_auth_verified(
                &fixture.0,
                main.to_str().unwrap(),
                NEW,
                &target_id,
                false,
                |path| {
                    assert_eq!(fs::read(path).unwrap(), NEW);
                    Err(failure.to_string())
                },
            )
            .unwrap_err();
            assert!(error.contains("已恢复原认证状态"));
            assert_eq!(fs::read(main.join("auth.json")).unwrap(), OLD);
        }
        let missing = fixture.0.join("missing");
        assert!(
            replace_current_auth_verified(
                &fixture.0,
                missing.to_str().unwrap(),
                NEW,
                &target_id,
                false,
                |_| Err("校验失败".into())
            )
            .is_err()
        );
        assert!(!missing.join("auth.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn recovery_failure_keeps_encrypted_backup_and_reports_partial_result() {
        let fixture = Fixture::new();
        let main = fixture.home("main", OLD);
        let target_id = auth_identity(NEW).unwrap();
        let error = replace_current_auth_verified(
            &fixture.0,
            main.to_str().unwrap(),
            NEW,
            &target_id,
            false,
            |_| {
                fs::create_dir(main.join(".auth.json.codex-account-hub.tmp")).unwrap();
                Err("校验读取失败".into())
            },
        )
        .unwrap_err();
        assert!(error.contains("恢复原认证失败"));
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), NEW);
        let backup = fs::read_dir(fixture.0.join("switch-backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(unprotect(&fs::read(backup.path()).unwrap()).unwrap(), OLD);
    }

    #[cfg(windows)]
    #[test]
    fn reauthorizes_current_account_and_preserves_new_credentials_across_switches() {
        let fixture = Fixture::new();
        let main = fixture.home("main", OLD);
        let login = fixture.home("login", NEW);
        let other = fixture.home("other", OTHER);
        let a = import_current(
            &fixture.0,
            main.to_str().unwrap(),
            None,
            Some("工作".into()),
        )
        .unwrap();
        let b = import_current(&fixture.0, other.to_str().unwrap(), None, None).unwrap();
        let (saved, updated) =
            complete_reauthorization(&fixture.0, main.to_str().unwrap(), &login, &a.id, None)
                .unwrap();
        assert!(updated);
        assert_eq!(saved.label, "工作");
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), NEW);
        assert_eq!(load_saved_auth(&fixture.0, &a.id).unwrap(), NEW);
        save_current_before_switch(&fixture.0, main.to_str().unwrap(), &b.id).unwrap();
        activate_account(&fixture.0, &b.id, main.to_str().unwrap()).unwrap();
        save_current_before_switch(&fixture.0, main.to_str().unwrap(), &a.id).unwrap();
        activate_account(&fixture.0, &a.id, main.to_str().unwrap()).unwrap();
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), NEW);
    }

    #[cfg(windows)]
    #[test]
    fn reauthorizing_saved_account_does_not_replace_an_external_login() {
        let fixture = Fixture::new();
        let main = fixture.home("main", OLD);
        let login = fixture.home("login", NEW);
        let a = import_current(&fixture.0, main.to_str().unwrap(), None, None).unwrap();
        fs::write(main.join("auth.json"), OTHER).unwrap();
        let (_, updated) =
            complete_reauthorization(&fixture.0, main.to_str().unwrap(), &login, &a.id, None)
                .unwrap();
        assert!(!updated);
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), OTHER);
        assert_eq!(load_saved_auth(&fixture.0, &a.id).unwrap(), NEW);
    }

    #[cfg(windows)]
    #[test]
    fn wrong_login_and_failed_active_write_do_not_overwrite_vault() {
        let fixture = Fixture::new();
        let main = fixture.home("main", OLD);
        let login = fixture.home("login", OTHER);
        let a = import_current(&fixture.0, main.to_str().unwrap(), None, None).unwrap();
        assert!(
            complete_reauthorization(&fixture.0, main.to_str().unwrap(), &login, &a.id, None)
                .is_err()
        );
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), OLD);
        assert_eq!(load_saved_auth(&fixture.0, &a.id).unwrap(), OLD);
        fs::write(login.join("auth.json"), NEW).unwrap();
        fs::create_dir(main.join(".auth.json.codex-account-hub.tmp")).unwrap();
        assert!(
            complete_reauthorization(&fixture.0, main.to_str().unwrap(), &login, &a.id, None)
                .is_err()
        );
        assert_eq!(fs::read(main.join("auth.json")).unwrap(), OLD);
        assert_eq!(load_saved_auth(&fixture.0, &a.id).unwrap(), OLD);
    }

    #[cfg(windows)]
    #[test]
    fn restores_missing_corrupt_and_same_identity_active_credentials() {
        let fixture = Fixture::new();
        let source = fixture.home("source", NEW);
        let a = import_current(&fixture.0, source.to_str().unwrap(), None, None).unwrap();
        let missing = fixture.0.join("missing-parent");
        save_current_before_switch(&fixture.0, missing.to_str().unwrap(), &a.id).unwrap();
        activate_account(&fixture.0, &a.id, missing.to_str().unwrap()).unwrap();
        assert_eq!(fs::read(missing.join("auth.json")).unwrap(), NEW);
        for (name, auth) in [("corrupt", b"not-json".as_slice()), ("same-account", OLD)] {
            let main = fixture.home(name, auth);
            save_current_before_switch(&fixture.0, main.to_str().unwrap(), &a.id).unwrap();
            activate_account(&fixture.0, &a.id, main.to_str().unwrap()).unwrap();
            assert_eq!(fs::read(main.join("auth.json")).unwrap(), NEW);
            assert_eq!(load_saved_auth(&fixture.0, &a.id).unwrap(), NEW);
        }
        let backups = fs::read_dir(fixture.0.join("switch-backups")).unwrap();
        assert!(
            backups
                .filter_map(Result::ok)
                .any(|entry| unprotect(&fs::read(entry.path()).unwrap()).unwrap() == b"not-json")
        );
    }

    #[cfg(windows)]
    #[test]
    fn refuses_unreadable_main_auth_and_mismatched_vault_credentials() {
        let fixture = Fixture::new();
        let source = fixture.home("source", NEW);
        let a = import_current(&fixture.0, source.to_str().unwrap(), None, None).unwrap();
        let main = fixture.0.join("main");
        fs::create_dir_all(main.join("auth.json")).unwrap();
        assert!(save_current_before_switch(&fixture.0, main.to_str().unwrap(), &a.id).is_err());
        fs::write(
            fixture.0.join("vault").join(format!("{}.dpapi", a.id)),
            protect(OTHER).unwrap(),
        )
        .unwrap();
        assert!(activate_account(&fixture.0, &a.id, source.to_str().unwrap()).is_err());
        assert_eq!(fs::read(source.join("auth.json")).unwrap(), NEW);
    }

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

    #[test]
    fn keeps_only_the_newest_switch_backups() {
        let test_dir = std::env::temp_dir().join(format!(
            "codex-account-hub-backup-test-{}-{}",
            std::process::id(),
            now_seconds()
        ));
        let backup_dir = test_dir.join("switch-backups");
        fs::create_dir_all(&backup_dir).unwrap();
        for number in 0..7 {
            fs::write(
                backup_dir.join(format!("{number:02}-account.dpapi")),
                b"test",
            )
            .unwrap();
        }
        fs::write(backup_dir.join("ignore.txt"), b"test").unwrap();
        prune_switch_backups(&test_dir, 5).unwrap();
        let dpapi_count = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "dpapi")
            })
            .count();
        assert_eq!(dpapi_count, 5);
        assert!(backup_dir.join("ignore.txt").exists());
        fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn renames_and_safely_deletes_saved_accounts() {
        let test_dir = std::env::temp_dir().join(format!(
            "codex-account-hub-account-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let account_id = "aaaaaaaaaaaaaaaaaaaaaaaa";
        let other_id = "bbbbbbbbbbbbbbbbbbbbbbbb";
        let account = SavedAccount {
            id: account_id.to_string(),
            label: "旧名称".to_string(),
            email: None,
            plan_type: None,
            imported_at: now_seconds(),
            is_active: false,
        };
        write_index(
            &test_dir,
            &AccountIndex {
                version: INDEX_VERSION,
                accounts: vec![account],
            },
        )
        .unwrap();
        fs::create_dir_all(test_dir.join("vault")).unwrap();
        fs::write(
            test_dir.join("vault").join(format!("{account_id}.dpapi")),
            b"encrypted-fixture",
        )
        .unwrap();

        assert_eq!(
            rename_account(&test_dir, account_id, "新名称")
                .unwrap()
                .label,
            "新名称"
        );
        assert!(delete_account(&test_dir, account_id, Some(account_id)).is_err());
        assert_eq!(list_accounts(&test_dir).unwrap().len(), 1);
        delete_account(&test_dir, account_id, Some(other_id)).unwrap();
        assert!(list_accounts(&test_dir).unwrap().is_empty());
        assert!(
            !test_dir
                .join("vault")
                .join(format!("{account_id}.dpapi"))
                .exists()
        );
        fs::remove_dir_all(&test_dir).unwrap();
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
