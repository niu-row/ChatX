# ChatX

ChatX 是一个 Windows / macOS 桌面桥接程序，用来把 ChatGPT 的 Secure MCP Tunnel 连接到本机的 Desktop Commander MCP。

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

ChatX 通过启动器关闭 Desktop Commander 的聊天内文件预览和配置编辑模板：工具列表不包含 UI 关联元数据，资源与资源模板列表为空，旧 UI 资源地址不可读取。26 个 MCP 工具及其返回结果保持不变。适配在启动时生效，不修改锁定的上游 MCPB 内容。

## 0.3.0 运行组件

Windows x64 和 macOS Apple Silicon（M1/M2/M3/M4）构建固定打包：

- OpenAI `tunnel-client`
- Node.js
- Desktop Commander 0.2.48 的官方 GitHub Release MCPB
- MCPB 内随包提供的当前平台 `ripgrep`

Desktop Commander 通过 stdio 启动，不要求最终用户安装 Node、npm、ripgrep 或 Desktop Commander。

构建时 `scripts/prepare-desktop-bundle.mjs` 会：

1. 校验 `runtime-lock.json` 中锁定的 Node 与 tunnel-client 版本/SHA-256。
2. 校验 Desktop Commander 0.2.48 GitHub Release 的 tag、asset 名称、URL、文件大小和 SHA-256。
3. 优先使用 `DESKTOP_COMMANDER_MCPB_PATH` 指定的本地 MCPB；未指定时从锁定的上游 GitHub Release URL 下载。
4. 在解包前校验整个 MCPB 的 SHA-256，再验证包内 `manifest.json`、`package.json` 和 `dist/index.js`。
5. 使用 MCPB 内已经固定的生产 `node_modules`，不在 ChatX 构建阶段重新解析 Desktop Commander 的 npm 传递依赖。
6. 解析 MCPB 内的 `@vscode/ripgrep`，验证 Windows binary 存在并记录 SHA-256。
7. 保留 OpenAI tunnel-client LICENSE/NOTICE 和 Desktop Commander MIT LICENSE。
8. 生成 `runtime-manifest.json`，记录 GitHub Release provenance、bundle manifest hash 和 ripgrep hash。

因此 Desktop Commander 的运行依赖树由锁定的 GitHub Release MCPB 二进制整体确定，而不是由每次构建时的 `npm install` 结果决定。

## 使用

启动 ChatX 后，在“连接”页填写：

- Tunnel ID，例如 `tunnel_...`
- Runtime API Key

Windows 可选择使用 DPAPI（CurrentUser）保存 Runtime Key。macOS 可勾选保存到系统钥匙串，下次连接时 Runtime Key 可留空；明文 Key 不写入 `settings.json`。

点击“连接并启动”后，ChatX 调用：

```text
tunnel-client runtimes connect
  --alias chatx-local
  --tunnel-id <tunnel id>
  --runtime-api-key env:CHATX_TUNNEL_RUNTIME_KEY
  --mcp-command "<bundled node> <ChatX launcher> <isolated home> <bundled Desktop Commander> --no-onboarding"
```

Tunnel runtime 由 OpenAI tunnel-client 管理。ChatX 使用 `runtimes status` 获取结构化状态，并以 `ready` / `healthy` 等字段判断连接是否可用；使用 `runtimes stop` 停止连接。状态命令的真实失败或无法解析的 JSON 会作为 runtime error 显示，不会伪装成普通 stopped 状态。

连接成功后，ChatX 默认开启“断线自动重连”。后台监控不依赖窗口页面轮询：连续检测到 runtime 不再可用后，会重新执行同一 Tunnel 连接，并对连续失败使用 2 / 5 / 10 / 20 / 30 秒退避。Runtime Key 只保留在当前 ChatX 进程内存或既有安全存储中，不写入明文设置。手动“停止连接”、托盘“停止连接”或退出 ChatX 会取消重连意图并清除会话内 Runtime Key。

macOS 提供独立“权限中心”。点击“一次触发全部授权”会集中访问 Desktop、Documents、Downloads，并在存在已保存 Runtime Key 时验证钥匙串访问，从而把可请求的系统权限集中在一个流程里处理。macOS TCC 不允许应用静默替用户授予所有隐私权限，因此系统仍可能按安全类别显示确认；“完全磁盘访问”必须由用户在系统设置中手动开启，权限中心提供直达入口。

## Desktop Commander 本地状态

ChatX bundled Desktop Commander 不使用用户独立安装 Desktop Commander 的配置目录。

ChatX launcher 会把它的 HOME/USERPROFILE 指向 ChatX 自己的数据目录：

```text
<ChatX app local data>/state/desktop-commander-home
```

因此单独安装的 Desktop Commander 与 ChatX bundled instance 不会共用 `config.json`。

ChatX 的“调用记录”页直接读取这个隔离 HOME 下 Desktop Commander 自己维护的 `tool-history.jsonl`，显示最近的工具名、成功/失败、耗时、参数和返回摘要，并支持工具/状态筛选、50/100/200/500/1000 条显示数量、清空，以及当前筛选范围的成功率、平均耗时和 P95 耗时统计。调用参数和结果可能包含本机路径、命令或文件内容，因此详情默认折叠，记录不会作为 ChatX telemetry 上传。

ChatX 同时设置 Desktop Commander 官方支持的硬关闭开关：

```text
DESKTOP_COMMANDER_DISABLE_TELEMETRY=1
```

所以 ChatX bundled instance 不发送 Desktop Commander telemetry。

`CHATX_TUNNEL_RUNTIME_KEY` 只用于 tunnel-client 身份验证。ChatX 的 Desktop Commander launcher 会在加载 MCP server 之前从进程环境中删除该变量，避免 Desktop Commander 以及它启动的 Shell/子进程继承 Runtime API Key。

Desktop Commander 仍然是高权限本地自动化工具。它可以读取和修改文件，并执行终端命令。其 `allowedDirectories` 和 command blocklist 属于防误操作 guardrail，不是 OS sandbox；终端命令能够以当前登录用户身份启动其他程序。如需强隔离，应使用 VM、dev container 或独立机器。

## 从 0.2.1 升级

0.3.0 会读取旧版 `settings.json` 中的：

```text
connection.tunnelId
```

并迁移到新的精简设置格式。旧版 `runtime-key.dpapi` 使用的 DPAPI CurrentUser + Base64 格式保持兼容，因此已保存 Runtime Key 可以继续使用。

## Tunnel 网络与公网 Relay

桌面端“设置”页提供 Tunnel `Direct / System / Manual` 三种代理模式。`Direct` 会显式清除 ChatX 启动 `tunnel-client` 时继承到的 HTTP/SOCKS 代理变量；`System` 会在点击“应用网络设置”时读取当前 macOS / Windows 显式系统代理并固定为本次运行路由；`Manual` 支持 `http://`、`https://`、`socks5://`、`socks5h://` 的 `host:port` 地址。当前不保存代理账号密码，也不解析 PAC 自动代理脚本。

手机公网 Monitor Relay 由 ChatX 的统一 `relay` 设置管理。macOS 会生成并维护 `~/Library/LaunchAgents/com.chatx.relay.plist`，使用 SSH reverse forwarding 把配置的公网端口映射到本机 Monitor HTTPS 端口。旧的 `state/monitor-relay.json` 仅用于一次迁移，不再作为独立配置源。Relay 状态会持续显示 LaunchAgent 进程、SSH 端口、公网 Monitor 端口和最近错误。

Tunnel 健康状态同时参考本地 runtime `ready/health` 和 control-plane poll 日志。连续 poll 异常依次进入 `suspect` / `down`，恢复时通过 `poller recovered; polling operational` 回到 `healthy`，避免仅凭本地 `/readyz` 将上游断线误判成正常。

## 开发

要求：

- Windows x64 或 macOS Apple Silicon（darwin-arm64）
- Node 构建输入必须匹配对应平台 runtime lock
- 已安装/可定位锁定版本的 OpenAI tunnel-client
- Rust/Tauri 构建环境
- 可访问锁定的 Desktop Commander GitHub Release，或准备好对应 MCPB 本地文件

安装 ChatX 开发依赖：

```powershell
npm ci
```

### ChatX 隔离 HOME 与本机工具链

ChatX bundled Desktop Commander 会把 `HOME` / `USERPROFILE` 指向自己的隔离目录。通过 ChatX MCP 执行开发命令时，不能假设 `~/.cargo`、`~/.gradle` 或 `~/Library/Android` 属于真实登录用户；否则会误报 Rust、Java、Android SDK 或 Gradle 不存在。

仓库统一使用 `scripts/dev-toolchain.mjs` 恢复 OS 账户真实主目录（Node `os.userInfo().homedir`），并从真实 HOME 定位 Cargo/Rustup、Android Studio JDK、Android SDK 和 Gradle 缓存。先运行：

```bash
npm run dev:doctor
```

完整本机验证使用：

```bash
npm run test:local-full
```

也可以分别运行 `npm run test:rust:check`、`npm run test:rust` 和 `npm run test:android:gradle`。Android 验证会执行 `lintDebug` + unit tests，并优先复用真实用户 `~/.gradle` 中已解包的 Gradle；如果只有 `wrapper/dists/.../manual/gradle-*-bin.zip`，会解包到 `~/.gradle/chatx-toolchains/` 后直接运行，避免因为隔离 HOME 或重复网络下载导致误判。`test:local-full` 最后还会执行 `git diff --check`。

如需显式覆盖真实主目录，可设置 `CHATX_REAL_HOME`；通常不需要手工配置。

### Tunnel Proxy 与手机公网 Relay

桌面“设置”页提供 Tunnel 网络模式：`Direct`、`System`、`Manual`。`Direct` 会显式移除 ChatX 启动 `tunnel-client` 时继承的 HTTP/SOCKS 代理环境变量；`System` 在用户点击“应用网络设置”时读取当前 OS 显式代理并固定为本次运行配置；`Manual` 支持无认证的 `http://`、`https://`、`socks5://`、`socks5h://` `host:port`。macOS PAC 自动代理当前不支持。

Tunnel 健康不再只看本地 `/healthz` / `readyz`，还会从当前 runtime 的 control-plane 日志持续读取 poll 成功、失败与恢复事件。状态分为 `healthy / suspect / down`，并独立记录连续 Control Plane 失败次数与实际 proxy source。

手机公网 Relay 的唯一配置源是 `settings.json` 中的 `relay`。旧 `state/monitor-relay.json` 仅作为一次性迁移输入。macOS 下 ChatX 会管理 `~/Library/LaunchAgents/com.chatx.relay.plist` 与 state 目录中的 `relay-run.sh`，使用严格 SSH host-key 校验、5 秒 keepalive，并在重连前仅清理由 `sshd` 占用的同一远端反向转发端口，降低 stale reverse-forward 导致的重连循环。

静态检查：

```powershell
npm test
```

准备 bundled runtime 并执行真实 Desktop Commander stdio MCP 验证：

```powershell
npm run desktop:verify
```

`desktop:verify` 会先验证/解包锁定的 Desktop Commander MCPB，然后实际启动 bundled Desktop Commander，执行 MCP `initialize/tools/list`、Runtime Key 子进程隔离检查，以及文件写入、读取和 ripgrep 搜索 smoke test。它完全在本机运行，不依赖 GitHub Actions。

如果已经下载官方 MCPB，或构建环境不应重复访问网络，可以指定本地文件；文件仍必须与 `runtime-lock.json` 中的大小和 SHA-256 完全匹配：

```powershell
$env:DESKTOP_COMMANDER_MCPB_PATH = "C:\path\to\desktop-commander-0.2.48.mcpb"
npm run desktop:verify
```

准备运行资源并启动开发版：

```powershell
npm run desktop:dev
```

构建桌面程序：

```powershell
npm run desktop:build
```

构建本机安装包（Windows 为 NSIS；macOS arm64 为 app + DMG）：

```bash
npm run desktop:installer
```

在 M1 Mac 上，产物位于 `src-tauri/target/release/bundle/macos/` 和 `src-tauri/target/release/bundle/dmg/`。当前 macOS 本地包尚未配置 Developer ID notarization，仅用于本机开发/测试。macOS 本地构建固定使用 ad-hoc `signingIdentity = "-"`，确保 codesign identifier 与 `CFBundleIdentifier` 都是 `com.chatgptx.local`；这是 macOS Local Network/NECP 正确匹配“本地网络”授权所必需的。Windows 本地安装包仍允许 unsigned build。

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

如果选择记住密钥，ChatX 在 Windows 上通过 DPAPI CurrentUser 加密保存；macOS 使用系统钥匙串保存。Tunnel ID 可以保存在普通 JSON 设置中。Desktop Commander launcher 会删除继承到 MCP 进程的 `CHATX_TUNNEL_RUNTIME_KEY`，避免其继续传播到工具启动的子进程。

Desktop Commander 的构建输入使用 `runtime-lock.json` 中锁定的 GitHub Release MCPB。ChatX 在解包前验证整个 release asset 的大小和 SHA-256，因此包内 `dist`、生产依赖和 ripgrep 都由同一个已锁定 artifact 决定。

关闭主窗口只会把 ChatX 隐藏到托盘。选择“退出 ChatX”或“停止连接”会停止 `chatx-local` Tunnel runtime。

## 上游项目

- OpenAI tunnel-client: `openai/tunnel-client`
- Desktop Commander: `wonderwhy-er/DesktopCommanderMCP`

Desktop Commander 使用 MIT License。ChatX 安装资源中保留其 LICENSE。


## macOS Apple Silicon 快速开始

在 M1/M2/M3/M4 Mac 上安装 Node.js 24 和 Rust/Tauri 构建环境后：

```bash
npm ci
npm test
npm run desktop:verify
npm run desktop:dev
```

`desktop:verify` 会下载并校验锁定的 Node.js arm64、OpenAI tunnel-client arm64 和 Desktop Commander MCPB，然后执行真实 stdio MCP smoke test。需要生成本机 DMG 时运行：

```bash
npm run desktop:installer
```

macOS 版本可勾选“使用 macOS 钥匙串保存 Runtime Key”。保存后，下次连接可留空；点击“清除已保存密钥”可移除。系统提示钥匙串访问时，请允许 ChatX 读取。

macOS 15+ 的“本地网络”权限会结合应用代码身份匹配。ChatX 的本地 app/DMG 构建通过 `src-tauri/tauri.macos.conf.json` 固定使用 ad-hoc signing identity `-`，使签名 identifier 与 Bundle ID `com.chatgptx.local` 保持一致；`src-tauri/Info.plist` 同时提供 `NSLocalNetworkUsageDescription`。正式发布时应改用稳定的 Apple Development / Developer ID 身份并完成 notarization，不应继续使用 ad-hoc 签名。

macOS 启动时会从系统账户信息恢复真实用户主目录，避免从 Desktop Commander 等隔离环境启动时继承错误的 `HOME`，导致数据目录重复嵌套或弹出“找不到钥匙串”。此修复不重置系统钥匙串，也不修改其搜索列表。
