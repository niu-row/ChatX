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

## 运行组件

Windows 安装包固定打包：

- OpenAI `tunnel-client`
- Node.js
- `@wonderwhy-er/desktop-commander` 0.2.48

Desktop Commander 通过 stdio 启动，不要求最终用户安装 Node、npm 或 Desktop Commander。

构建时 `scripts/prepare-desktop-bundle.mjs` 会：

1. 校验 `runtime-lock.json` 中锁定的 Node 与 tunnel-client 版本/SHA-256。
2. 安装固定版本 Desktop Commander 到 `src-tauri/resources/desktop-commander`。
3. 禁用 Desktop Commander npm postinstall 脚本，避免构建阶段安装追踪；其当前 `@vscode/ripgrep` 依赖本身随平台包提供 binary。
4. 保留 OpenAI tunnel-client LICENSE/NOTICE 和 Desktop Commander MIT LICENSE。
5. 生成 `runtime-manifest.json`。

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
  --mcp-command "<bundled node> <bundled desktop commander> --no-onboarding"
```

Tunnel runtime 由 OpenAI tunnel-client 管理。ChatX 使用 `runtimes status` 获取状态，使用 `runtimes stop` 停止连接。

## Desktop Commander 权限模型

Desktop Commander 是高权限本地自动化工具。它可以读取和修改文件，并执行终端命令。

其 `allowedDirectories` 和 command blocklist 属于防误操作 guardrail，不是 OS sandbox。终端命令能够以当前 Windows 用户身份启动其他程序，因此如果需要强隔离，应使用 Docker、VM、dev container 或独立机器。

ChatX 不再额外制造一套与终端能力重叠的 Filesystem/Git/Shell 权限开关。

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

发布到 `release/`：

```powershell
npm run desktop:release
```

静态检查：

```powershell
npm test
```

准备完资源后可额外检查安装资源：

```powershell
npm run test:installer
```

## 目录

```text
desktop/
  index.html
  app.css
  app.js

src-tauri/
  src/main.rs        # Tunnel/runtime lifecycle + Tauri commands
  tauri.conf.json
  resources/         # build-time generated, not source of truth

scripts/
  prepare-desktop-bundle.mjs
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

如果选择记住密钥，ChatX 在 Windows 上通过 DPAPI CurrentUser 加密保存；Tunnel ID 可以保存在普通 JSON 设置中。

关闭主窗口只会把 ChatX 隐藏到托盘。选择“退出 ChatX”或“停止连接”会停止 `chatx-local` Tunnel runtime。

## 上游项目

- OpenAI tunnel-client: `openai/tunnel-client`
- Desktop Commander: `wonderwhy-er/DesktopCommanderMCP`

Desktop Commander 使用 MIT License。ChatX 安装资源中保留其 LICENSE。
