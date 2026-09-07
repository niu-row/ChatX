# ChatX

ChatX 是一个 Windows 桌面桥接程序，用来把 ChatGPT 的 Secure MCP Tunnel 连接到本机的 Desktop Commander MCP。

当前架构：

```text
ChatGPT
   ↓
Custom MCP Connector
   ↓
OpenAI Secure MCP Tunnel
   ↓
ChatX Desktop
   ↓
tunnel-client
   ↓ stdio
Desktop Commander
   ↓
本机文件 / 搜索 / 编辑 / 进程 / Shell
```

ChatX 不再实现自己的 Filesystem、Git、Shell 或 MCP HTTP Server。文件读写、搜索、编辑和命令执行由 Desktop Commander 提供；ChatX 只负责桌面 UI、运行组件打包、Tunnel 配置、启动/停止、诊断和日志。

## 0.3.0 运行组件

Windows 安装包固定打包：

- OpenAI `tunnel-client`
- Node.js
- `@wonderwhy-er/desktop-commander` 0.2.48
- Desktop Commander 所需的 Windows `ripgrep`

Desktop Commander 通过 stdio 启动，不要求最终用户安装 Node、npm、ripgrep 或 Desktop Commander。

构建时 `scripts/prepare-desktop-bundle.mjs` 会：

1. 校验 `runtime-lock.json` 中锁定的 Node 与 tunnel-client 版本/SHA-256。
2. 安装固定版本 Desktop Commander 到 `src-tauri/resources/desktop-commander`。
3. 保持普通 npm lifecycle scripts 关闭，然后单独执行 `npm rebuild @vscode/ripgrep`，保证 `start_search` 可用。
4. 验证并记录 bundled `rg.exe` 的 SHA-256。
5. 保留 OpenAI tunnel-client LICENSE/NOTICE 和 Desktop Commander MIT LICENSE。
6. 生成 `runtime-manifest.json`。

Desktop Commander 顶层版本固定为 0.2.48；构建生成的独立 `package-lock.json` 哈希会记录在 manifest 中，用于确认某个安装包实际包含的依赖树。

## 使用

启动 ChatX 后，在“连接”页填写：

- Tunnel ID，例如 `tunnel_...`
- Runtime API Key

可选择使用 Windows DPAPI（CurrentUser）保存 Runtime Key。明文 Key 不写入 `settings.json`。

点击“连接并启动”后，ChatX 调用：

```text
tunnel-client runtimes connect
  --alias chatx-local
  --tunnel-id <tunnel id>
  --runtime-api-key env:CHATX_TUNNEL_RUNTIME_KEY
  --mcp-command "<bundled node> <ChatX launcher> <isolated home> <bundled Desktop Commander> --no-onboarding"
```

Tunnel runtime 由 OpenAI tunnel-client 管理。ChatX 使用 `runtimes status` 获取结构化状态，并以 `ready` / `healthy` 等字段判断连接是否可用；使用 `runtimes stop` 停止连接。状态命令的真实失败或无法解析的 JSON 会作为 runtime error 显示，不会伪装成普通 stopped 状态。

## Desktop Commander 本地状态

ChatX bundled Desktop Commander 不使用用户独立安装 Desktop Commander 的配置目录。

ChatX launcher 会把它的 HOME/USERPROFILE 指向 ChatX 自己的数据目录：

```text
<ChatX app local data>/state/desktop-commander-home
```

因此单独安装的 Desktop Commander 与 ChatX bundled instance 不会共用 `config.json`。

ChatX 同时设置 Desktop Commander 官方支持的硬关闭开关：

```text
DESKTOP_COMMANDER_DISABLE_TELEMETRY=1
```

所以 ChatX bundled instance 不发送 Desktop Commander telemetry。

`CHATX_TUNNEL_RUNTIME_KEY` 只用于 tunnel-client 身份验证。ChatX 的 Desktop Commander launcher 会在加载 MCP server 之前从进程环境中删除该变量，避免 Desktop Commander 以及它启动的 Shell/子进程继承 Runtime API Key。

Desktop Commander 仍然是高权限本地自动化工具。它可以读取和修改文件，并执行终端命令。其 `allowedDirectories` 和 command blocklist 属于防误操作 guardrail，不是 OS sandbox；终端命令能够以当前 Windows 用户身份启动其他程序。如需强隔离，应使用 VM、dev container 或独立机器。

## 从 0.2.1 升级

0.3.0 会读取旧版 `settings.json` 中的：

```text
connection.tunnelId
```

并迁移到新的精简设置格式。旧版 `runtime-key.dpapi` 使用的 DPAPI CurrentUser + Base64 格式保持兼容，因此已保存 Runtime Key 可以继续使用。

## 开发

要求：

- Windows x64
- Node 版本必须匹配 `runtime-lock.json`
- 已安装/可定位锁定版本的 OpenAI tunnel-client
- Rust/Tauri 构建环境

安装依赖：

```powershell
npm ci
```

静态检查：

```powershell
npm test
```

准备 bundled runtime 并执行真实 Desktop Commander stdio MCP 验证：

```powershell
npm run desktop:verify
```

`desktop:verify` 会检查安装资源，并实际启动 bundled Desktop Commander，执行 MCP `initialize/tools/list`、Runtime Key 子进程隔离检查，以及文件写入、读取和 ripgrep 搜索 smoke test。它完全在本机运行，不依赖 GitHub Actions。

准备运行资源并启动开发版：

```powershell
npm run desktop:dev
```

构建桌面程序：

```powershell
npm run desktop:build
```

构建 NSIS 安装包：

```powershell
npm run desktop:installer
```

`desktop:installer` 可用于本地开发安装包；未配置证书时允许生成 unsigned installer，Windows 可能显示 Unknown Publisher / SmartScreen 提示。

正式发布到 `release/` 必须配置 Authenticode 证书和时间戳服务：

```powershell
$env:CHATX_WINDOWS_CERT_THUMBPRINT = "<certificate thumbprint>"
$env:CHATX_WINDOWS_TIMESTAMP_URL = "<RFC3161 timestamp URL>"
npm run desktop:release
```

`desktop:release` 会验证应用 EXE 与 NSIS installer 的 Authenticode 状态，验证失败或未配置证书时拒绝发布，不提供 unsigned release 路径。

## 目录

```text
desktop/
  index.html
  app.css
  app.js

src-tauri/
  src/main.rs
  tauri.conf.json
  resources/         # build-time generated, not source of truth

scripts/
  prepare-desktop-bundle.mjs
  desktop-commander-launcher.mjs
  bridge-smoke-test.mjs
  build-installer.mjs
  desktop-static-test.mjs
  installer-runtime-test.mjs
  version-consistency-test.mjs

runtime-lock.json
```

## 安全与密钥

Runtime API Key 只通过环境变量引用传给 tunnel-client：

```text
--runtime-api-key env:CHATX_TUNNEL_RUNTIME_KEY
```

如果选择记住密钥，ChatX 在 Windows 上通过 DPAPI CurrentUser 加密保存；Tunnel ID 可以保存在普通 JSON 设置中。Desktop Commander launcher 会删除继承到 MCP 进程的 `CHATX_TUNNEL_RUNTIME_KEY`，避免其继续传播到工具启动的子进程。

关闭主窗口只会把 ChatX 隐藏到托盘。选择“退出 ChatX”或“停止连接”会停止 `chatx-local` Tunnel runtime。

## 上游项目

- OpenAI tunnel-client: `openai/tunnel-client`
- Desktop Commander: `wonderwhy-er/DesktopCommanderMCP`

Desktop Commander 使用 MIT License。ChatX 安装资源中保留其 LICENSE。
