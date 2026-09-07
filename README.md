# Codex Account Hub

Windows 本地优先的 Codex 多账号额度看板与安全切换器。

项目已具备可用的本地 MVP：导入或新增多个账号、并行查看额度，并在确认后安全切换本机 Codex 活动账号。

## 当前能力

- 自动发现 `codex` 可执行文件。
- 连接官方 `codex app-server` JSON-RPC 接口。
- 调用 `account/read` 获取当前账号的脱敏信息。
- 调用 `account/rateLimits/read` 获取额度窗口和重置时间。
- 输出经过脱敏的 JSON，不输出任何 access token、refresh token 或完整账号 ID。
- Tauri 桌面看板展示当前账号的 5 小时/周额度、重置时间与可用 reset 数量。
- 支持将当前 `auth.json` 通过 Windows DPAPI 加密导入本地账号保险库。
- 保险库索引只保存脱敏账号信息，OAuth 凭据只存在于 `.dpapi` 密文文件中。
- 使用独立 Codex Home 查询每个保险库账号的额度，不需要切换当前桌面账号。
- 通过官方 `codex login` 在隔离目录新增账号；密码和登录页面完全由官方流程处理。
- 切换前自动加密保存当前账号，并保存额外的 DPAPI 回退副本。
- 使用 Windows `MoveFileExW` 写穿原子替换活动 `auth.json`，写入后复核账号身份，验证失败自动恢复。
- 支持“切换并重启”：精确识别 Windows 打包版 Codex 进程，先请求正常关闭，再清理残留进程并通过系统应用入口重新启动。
- 启动时清理异常退出可能遗留的隔离登录明文认证文件。

## 启动桌面开发版

```powershell
npm install
npm run desktop:dev
```

首次编译 Tauri 依赖会花几分钟，后续启动会明显更快。

## 构建 Windows 可执行文件

```powershell
npm run desktop:build
```

当前配置生成独立可执行文件：

```text
src-tauri/target/release/codex-account-hub.exe
```

构建同时生成独立可执行文件和当前用户模式的 NSIS 安装包：

```text
src-tauri/target/release/bundle/nsis/Codex Account Hub_0.1.2_x64-setup.exe
```

安装后可从桌面快捷方式或开始菜单直接启动，不会显示 CMD 窗口，也不需要管理员权限。

## 使用方式

1. 首次打开后点击“导入当前账号”。
2. 点击“添加另一个账号”，在打开的 OpenAI 官方页面中自行完成登录。
3. 应用会自动刷新所有已保存账号的额度和重置时间。
4. 点击“切换并重启”可自动关闭并重新打开 Codex；也可选择“仅切换”稍后自行重启。

不要在 Codex 正在执行重要任务时切换账号。已有任务可能继续持有旧的内存会话。

## 运行协议探针

```powershell
npm run probe -- --codex-home "C:\Users\你的用户名\.codex"
```

也可以显式指定 Codex：

```powershell
npm run probe -- --codex "C:\path\to\codex.exe" --codex-home "C:\Users\你的用户名\.codex"
```

## 测试

```powershell
npm test
npm run check
```

## 安全边界

- 探针不会读取或打印 `auth.json` 内容。
- 探针通过 `CODEX_HOME` 让官方 Codex 进程自行加载凭据。
- JSON 输出经过字段白名单处理。
- 任何 stderr 内容只作为诊断信息输出，不写入文件。
- 每次保险库查询结束都会将可能刷新的凭据重新加密，并删除隔离目录中的明文 `auth.json`。
- 切换动作必须由用户在应用内确认；应用不会自动轮换账号或绕过额度限制。

## 尚未完成

- 账号重命名、删除与单账号重新授权。
- 托盘菜单、定时后台刷新和数据缓存。
- 托盘、开机启动、代码签名、依赖审计和故障注入测试。
