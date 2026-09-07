use serde::Serialize;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub codex_path: String,
    pub codex_home: String,
    pub codex_version: Option<String>,
}

pub fn discover_runtime() -> Result<RuntimeInfo, String> {
    let codex_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|value| PathBuf::from(value).join(".codex")))
        .ok_or_else(|| "无法确定 Codex Home：缺少 USERPROFILE".to_string())?;

    let codex_path = discover_codex_path()?;
    let codex_version = Command::new(&codex_path)
        .arg("--version")
        .creation_flags_no_window()
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());

    Ok(RuntimeInfo {
        codex_path: codex_path.to_string_lossy().into_owned(),
        codex_home: codex_home.to_string_lossy().into_owned(),
        codex_version,
    })
}

fn discover_codex_path() -> Result<PathBuf, String> {
    if let Some(path) = find_on_path() {
        return Ok(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let bin_root = PathBuf::from(local_app_data)
            .join("OpenAI")
            .join("Codex")
            .join("bin");
        if let Some(path) = newest_bundled_codex(&bin_root) {
            return Ok(path);
        }
    }

    Err("未找到 codex.exe，请确认已安装 Codex 桌面端或 CLI".to_string())
}

fn find_on_path() -> Option<PathBuf> {
    let output = Command::new("where.exe")
        .arg("codex")
        .creation_flags_no_window()
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn newest_bundled_codex(root: &Path) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
    entries.reverse();
    entries
        .into_iter()
        .map(|entry| entry.path().join("codex.exe"))
        .find(|path| path.is_file())
}

trait CommandWindowsExt {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl CommandWindowsExt for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x08000000);
        }
        self
    }
}
