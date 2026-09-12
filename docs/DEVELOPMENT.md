# 开发与构建

本文面向希望从源码运行、测试或发布 Codex Account Hub 的开发者。

## 技术栈

- Tauri 2
- React 19 + TypeScript
- Rust 2024 edition
- Vite
- Windows DPAPI 与 Win32 API

## 环境要求

- Windows 10 或 Windows 11 x64
- Node.js 22 或更高版本
- Rust stable 与 Cargo
- 已安装并可正常运行的 Codex 桌面端或 Codex CLI

## 安装依赖

```powershell
npm install
```

## 启动开发版

```powershell
npm run desktop:dev
```

首次运行需要编译 Rust 和 Tauri 依赖，耗时会明显长于后续启动。

仅启动 Vite 页面无法使用 Tauri IPC，也不会读取真实账号数据：

```powershell
npm run dev
```

## 测试

```powershell
npm test
npm run check
cargo test --manifest-path src-tauri/Cargo.toml
```

Rust 测试中有一项真实 Codex 进程检测测试默认忽略，避免测试过程关闭或操作用户正在运行的 Codex。

账号恢复测试使用独立临时目录和假凭据，覆盖当前账号重新授权、授权后切换往返、外部换号、错误账号拒绝覆盖、主认证缺失或损坏恢复，以及缓存身份校验。

0.7 的故障测试还覆盖事务中断、加密写回失败后的重试、遗留进程保护、登录取消与超时。子进程测试仅启动隐藏的测试 PowerShell 等待进程，不执行真实 Codex 登录或重启。界面回归覆盖取消后解锁、保存阶段禁止取消、启动恢复提示和重启重试不重复切换。

0.7.2 额外启动测试父进程并强制终止，验证 Job Object 能回收直接子进程且不影响无关进程；覆盖 PID 创建时间/路径不匹配、其他管理器仍存活、结束已核验残留进程，以及旧版 PID 记录兼容。

另有生产前端的浏览器回归测试，使用虚拟 Tauri IPC 和假数据，不启动真实 Tauri 或读取用户认证：

```powershell
npm run test:ui
```

运行浏览器回归需要额外可用的 Playwright 模块，默认使用本机 Microsoft Edge。可设置 `PLAYWRIGHT_MODULE` 为已安装 Playwright 的模块目录，设置 `TEST_BROWSER_CHANNEL` 选择其他已安装浏览器渠道。测试构建仅保存在内存中，不覆盖正式 `dist/`，运行结束自动关闭浏览器和本机测试服务器。

## 构建 Windows 安装包

```powershell
npm run desktop:build
```

构建结果：

```text
src-tauri/target/release/codex-account-hub.exe
src-tauri/target/release/bundle/nsis/Codex Account Hub_<version>_x64-setup.exe
```

NSIS 使用当前用户安装模式，不需要管理员权限。

## App Server 协议探针

探针用于检查本机 Codex App Server 返回的账号和额度结构。输出经过字段白名单处理，不会打印 token 或完整账号 ID。

```powershell
npm run probe -- --codex-home "C:\Users\你的用户名\.codex"
```

也可以显式指定 Codex 可执行文件：

```powershell
npm run probe -- --codex "C:\path\to\codex.exe" --codex-home "C:\Users\你的用户名\.codex"
```

## 主要目录

```text
src/
  App.tsx                 React 界面与 Tauri IPC 调用
  styles.css              深浅主题与组件样式
  types.ts                前后端共享数据类型
  core/                   Node 协议探针核心

src-tauri/src/
  app_server.rs           Codex App Server JSON-RPC
  runtime.rs              Codex 与 Codex Home 发现
  vault.rs                DPAPI 账号保险库与安全切换
  login.rs                登录生命周期与取消
  recovery.rs             加密事务及临时认证恢复
  child_process.rs        Windows 子进程托管和身份核验
  codex_desktop.rs        Codex 桌面进程关闭与重启
  lib.rs                  Tauri commands 与操作互斥
```

## 发布检查

1. 更新 package、Cargo 与 Tauri 配置中的版本号。
2. 运行全部测试和生产构建。
3. 确认源码与 Git 历史中不存在真实邮箱、JWT、OAuth token、auth.json 或 DPAPI 密文。
4. 构建 NSIS 安装包。
5. 创建对应 Git tag 和 GitHub Release。
6. 上传安装包，并注明尚未代码签名时可能出现的 SmartScreen 提示。

架构和安全边界分别见 [ARCHITECTURE.md](ARCHITECTURE.md) 与 [../SECURITY.md](../SECURITY.md)。
