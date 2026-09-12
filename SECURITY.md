# 安全策略

Codex Account Hub 管理可用于登录 Codex 的 OAuth 凭据。请把应用数据目录和安装包视为安全敏感内容。

## 安全模型

- 账号密码始终由官方 Codex 登录流程处理，本应用不会接收或保存密码。
- 完整认证载荷使用 Windows DPAPI 加密，并绑定到当前 Windows 用户。
- 非敏感索引只保存别名、套餐和不可逆的账号标识哈希。
- 额度缓存只保存脱敏账号信息、额度比例、重置时间和采集时间，不包含 OAuth token。
- 自动刷新设置只在 WebView 本地存储中保存一个刷新间隔数字，不包含账号信息。
- 额度读取由本机官方 Codex App Server 完成。
- 前端不提供 token 查看、复制、导出或上传能力。
- 所有认证相关 Tauri commands 使用显式参数，不提供任意文件读取或任意命令执行入口。
- 登录、导入、删除和切换等写操作由全局读写锁独占执行；额度查询可受控并发，但不能与这些写操作重叠。
- 切换前生成加密回退备份，写入后验证身份，失败时恢复原认证。
- 认证事务记录也使用 DPAPI 加密；启动时恢复未完成更新，保留检测到的外部有效登录。

## 临时明文

为了让官方 Codex App Server 查询不同账号，应用会在受控的独立 Codex Home 中短暂创建 auth.json。正常完成后：

1. 可能刷新的认证会重新使用 DPAPI 加密。
2. 临时 auth.json 会被删除。
3. 隔离登录和查询流程遗留的明文认证会在下次启动时检查，确认相关进程结束后再处理。

无法确认子进程退出、加密写回失败或路径包含链接时，会保留文件并提示重试，因此异常情况下临时明文可能继续存在。恢复覆盖本应用的认证事务及已知临时文件；外部进程并发写入、进程启动与 PID 记录之间的中断窗口、真实断电行为仍需进一步加固。不要把应用数据目录同步到云盘或提交到 Git。

## 不会上传的内容

应用自身不包含遥测或自建云端服务，不会主动上传：

- access token 或 refresh token
- auth.json
- DPAPI 密文保险库
- 完整账号 ID
- Codex 会话和项目历史

官方 Codex 进程仍会按照 OpenAI 服务本身的正常工作方式连接网络。

## 本地文件

以下内容永远不应进入 Git：

```text
accounts.json
usage-cache.json
auth.json
*.dpapi
vault/
runtime/
login/
switch-backups/
```

仓库的 `.gitignore` 应持续覆盖这些路径。发布前还应扫描全部可达 Git 历史，而不只是当前工作区。

## 发布包

当前安装包尚未代码签名，因此 Windows 可能显示未知发布者或 SmartScreen 提示。只应从项目官方 GitHub Releases 页面下载安装包，并核对发布说明。

## 报告漏洞

请不要在公开 Issue 中提交 token、auth.json、完整邮箱、完整账号 ID 或应用数据目录压缩包。

优先使用仓库的 GitHub Security Advisory 私下报告漏洞：

<https://github.com/lm10rm/codex-account-hub/security/advisories/new>

报告中请只提供复现步骤、受影响版本和经过脱敏的日志。若必须提供敏感样本，请先等待维护者给出安全传输方式。

## 支持范围

安全修复优先应用到最新发布版本。旧版本不保证持续获得补丁，发现问题后请先升级到最新 Release 再复现。
