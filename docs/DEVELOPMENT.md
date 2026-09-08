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
