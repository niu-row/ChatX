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

ChatX 不自行实现 Filesystem、Git 或 Shell 工具。文件读写、搜索、编辑和命令执行仍由 Desktop Commander 提供；ChatX 负责桌面 UI、运行组件打包、Tunnel 配置与管理、HTTPS MCP 协议桥接、SSH 反向转发、诊断和日志。

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

## HTTPS MCP · 原生 WSS Relay（不依赖 OpenAI Tunnel / SSH 运行链路）

ChatX 的“连接”页提供 **HTTPS MCP** 与 **Secure MCP Tunnel** 两种互斥模式。选择模式卡片只切换视图，点击启动才停止旧链路并启用新链路；启动与停止控制位于连接页，OAuth 授权由“设置 → 安全与授权”集中管理。与原 Secure MCP Tunnel **运行互斥**（旧模式仍保留）。启动任一链路会先停止另一条的服务并关闭其保存的连接意图；视图切换本身不会断线。升级时如果旧配置同时启用两条，则优先保留 HTTPS MCP，先停掉 Tunnel，自动重连不得将其重新开启。HTTPS 模式不需要 Tunnel ID、Runtime API Key 或 Tunnel 与 ChatGPT 工作空间的组织关联，但 ChatGPT 端仍须允许添加自定义 MCP 应用。

网络结构：

```text
ChatGPT --HTTPS/OAuth--> https://newrow.fun/mcp
  --> 公网服务器 Nginx (443) --> Relay Server (127.0.0.1:8787)
  --> 原生 WSS (/v1/ws/mcp) --> ChatX 本机 127.0.0.1:3000
  --> 独立的 stdio Desktop Commander
```

### 推荐：原生 WSS Relay（无 SSH 运行时依赖）

**新增一键部署向导**：在「连接 → HTTPS MCP」展开「服务器一键部署」，填写域名、SSH 服务器与 Desktop ID，先预检后点击部署。它会自动上传相同版本源码、编译、备份和验证；部署后不再使用 SSH。服务器初次需要已经配置好 Nginx HTTPS 与 Rust，详见 [服务器部署说明](deploy/oneclick/README.md)。WSS v2 用二进制分块代替 Base64，兼容旧 v1，且不会更改 Desktop Commander 权限模型。

新的 HTTPS MCP「传输方式」可选「原生 WSS Relay」。已有 SSH 配置缺少 `transport` 字段时仍按 SSH 加载，**不会因升级自动切断旧链路**。准备好公网网关后，在连接页主动切换 WSS。该模式仍使用原有独立 HTTPS OAuth 主密钥、授权记录和本机 `127.0.0.1:3000` Bridge。

```text
ChatGPT -- HTTPS/OAuth --> newrow.fun:443 /mcp
       -- Nginx --> relay-server:8787 -- (MCP 请求关联/响应分块)
       -- WSS /v1/ws/mcp --> ChatX Desktop -- localhost:3000 --> Desktop Commander
```

部署步骤（**先部署公网网关，再升级/切换桌面端**）：

1. 使用该仓库新版 `relay-server` 编译并更新现有 Relay 服务，**保留原来的 Relay registry 和环境变量**，勿重置 Android Monitor 注册信息。设置 `CHATX_MCP_DESKTOP_ID=d_...` 为目标桌面已注册的 Desktop ID；不设置时公网 MCP 接口保持不可用（503），不能默认为任意在线 Desktop。
2. 检查 Relay 仍只监听服务器回环 `CHATX_RELAY_BIND=127.0.0.1:8787`（如自定义则同步更改 Nginx 代理端口）。将域名 HTTPS server 的旧 `deploy/nginx/chatx-https-mcp.conf` **替换**为 `deploy/nginx/chatx-https-mcp-wss.conf`，切勿同时 include 两套 `/mcp` location；执行 `nginx -t` 通过后 reload。公网只需要 HTTPS 443，新增的 `/v1/ws/mcp` 必须支持 WebSocket Upgrade。
3. 桌面端必须先完成该 HTTPS 域名的 Relay Desktop 注册（注册凭据使用系统密钥保护存储），但运行 WSS MCP **不要求 Android Monitor 处于开启状态**。在 HTTPS MCP 页面选择「原生 WSS Relay」，启动连接即可；不需要 SSH 主机、用户、密钥或远端端口。
4. 等 WSS 连接建立后，检查公网 `GET /.well-known/oauth-authorization-server` 应返回 200 和正确 issuer，未经授权的 `POST /mcp` 应返回 401 且携带 OAuth challenge。断线会返回 503（离线），超时返回 504；不应继续返回 502 的旧 SSH upstream 错误。ChatGPT 端 `https://newrow.fun/mcp` 和现有 OAuth 授权可保持不变。

安全约束：WSS 握手必须携带**已注册的 Desktop Token**；网关只接受显式 `CHATX_MCP_DESKTOP_ID` 对应的 Desktop，不能跨 Desktop 路由，也不能代理任意 URL、`/healthz` 或 Relay 管理接口。本地 MCP 仍只监听 loopback。WSS v2 请求 64 MiB、响应 128 MiB、并发 64；旧 v1 仍保持 5 MiB/16 MiB。Relay 网关终止 TLS 并能访问透传的 HTTP 请求数据（包含 OAuth 登录/Token 请求），**服务器必须由可信管理员运维**；不要将 Relay 部署到不可信服务商或与不可信租户共享。若已泄露 Desktop Token，应重新注册并撤销旧凭据。

原 SSH 反向转发模式仍可作为过渡回退：

### 在已配置公网域名的服务器部署

该项目自带 Nginx include 参考：`deploy/nginx/chatx-https-mcp.conf`。实际服务器必须先拥有有效 HTTPS 证书、正常的 443 站点，以及支持本机反向代理的 Nginx。将 include 文件放入域名所在 server 的 include 目录，执行 `nginx -t` 通过后再 reload。Nginx 不直接暴露 SSH 转发端口；`18080` 始终只监听远端 `127.0.0.1`，本机 MCP 只监听 `127.0.0.1:3000`。

务必使用**受限专用 SSH 用户**而非长期使用 root。该用户的公钥应仅有 `restrict,port-forwarding,permitlisten="127.0.0.1:18080"` 等受限权限，并拒绝 shell 操作。ChatX 的 SSH 连接使用已有 `known_hosts`、非交互密钥、`StrictHostKeyChecking=yes`、`ExitOnForwardFailure=yes` 与 keepalive；拒绝未知主机密钥并自动重连。Windows 下 HTTPS MCP 的 Node.js Bridge 和 SSH 反向隧道均以 `CREATE_NO_WINDOW` 启动，不会弹出黑色控制台窗口；二者使用 `CREATE_SUSPENDED` 先挂起启动、绑定 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的 Windows Job Object 后再恢复执行，避免子进程归属竞态；ChatX 意外退出时连同 Desktop Commander 子进程终止。macOS 使用独立监护脚本监测 GUI 父进程，父进程消失或被管理的进程先退出时，先向对应进程组发送 `SIGTERM`，1.5 秒后仍会发送 `SIGKILL` 兜底，不会因进程组主进程提前退出而跳过清理。日志仍保存于 ChatX state 目录。

ChatX 桌面端使用 Tauri 官方 single-instance 插件限制 Windows/macOS 同时运行的实例数。再次启动会唤起现有主窗口（即使该窗口此前已被回收），而不是创建第二套 MCP/SSH 后台连接。

打开新 ChatX 安装包，进入“连接”页，输入域名（仅 `https://域名`）、SSH 主机/用户名/端口和本机/远程转发端口，然后点击“启动 HTTPS MCP”。设置保存在 ChatX 原有 `settings.json` 中；下次启动时自动恢复此模式。启动后显示的 OAuth 授权密码由独立的 HTTPS OAuth 主密钥派生，通过 Windows DPAPI 或 macOS Keychain 保存，不会写入明文设置，也不会传递给 Desktop Commander 子进程。升级时会一次性迁移原有密钥以保持短期兼容；之后可点击“重新生成 OAuth 授权密码”，该操作不会改变 Android Monitor 密钥，但会撤销全部 HTTPS OAuth 授权并重启 HTTPS MCP。

“设置 → 安全与授权”中的 OAuth 授权管理支持 1 小时、1 天、7 天（默认）、30 天、自定义小时数（最长 10 年）以及**不限时间**。该设置仅作用于新完成的 OAuth 授权，不会延长旧授权。有限期授权使用绝对截止时间，令牌刷新不会重置授权有效期；不限时间的授权没有总截止时间，但每次刷新均轮换 Refresh Token 并把最新令牌 ID 哈希持久化；在 30 秒幂等重试窗口内提交同一刷新令牌，会返回相同的后继令牌，避免网络响应丢失导致误撤销；重试窗口外的旧令牌重放、或在后继令牌再次轮换后重放，会撤销该授权。每个 Access Token 仍为最多 1 小时。每项授权会保存本机授权记录，点击“撤销所选授权”或“撤销全部授权”可立即禁止其后续 Access/Refresh 请求。“撤销全部授权”同时会使此前已批准但未交换令牌的 OAuth 授权码、以及尚未确认的授权表单失效。授权记录仅保存随机授权 ID、创建/到期时间、客户端哈希及当前 Refresh Token 的标识哈希，不保存 Bearer Token 明文。选择不限时间将长期保留本机文件读写及命令执行权限，应仅授予可信客户端。

由旧版本发出的无授权 ID 令牌获得一次固定的 7 天迁移期；旧 Refresh Token 首次续期会转换为新的一次性令牌；网络丢包后的 30 秒内可幂等重试，超过窗口或后继令牌继续轮换后再使用旧令牌会被视为重放。迁移期后需重新 OAuth 授权才能继续连接；可通过授权管理中的“旧版迁移授权”提前撤销。升级后如需“不限时间”，先在 ChatX 保存该选项，再于 ChatGPT 重新执行 OAuth。

在 ChatGPT 支持的自定义 MCP/应用配置里使用 **`https://newrow.fun/mcp`**，选择 OAuth。授权页面需输入 ChatX 显示的密码。桥接端提供 OAuth discovery、动态客户端注册、authorization-code + PKCE (S256)、短期 Bearer Token 和 refresh token。登录页面仅允许 ChatGPT/OpenAI 的 HTTPS 回调域名，未经授权的 `/mcp` 请求返回 401。工具仍有读写文件和本机命令执行权限；请将授权密码视为敏感凭据，且只为可信的 ChatGPT 客户端授权。

HTTPS 连接状态会在后台定期验证公网 OAuth 元数据（HTTP 200）与未经授权的 MCP 访问（HTTP 401），而不只是检查 SSH 进程是否存在。公网 OAuth 授权入口采用每 IP 限流和较高的全局安全上限，避免少量分布式无效请求耗尽低额度并阻断正常授权；在 WSS v2 下 Node.js 与 Nginx 的 MCP 请求上限均为 64 MiB，WSS 响应上限 128 MiB（v1 仍采用旧限制）。“设置 → 连接策略”分别提供 Tunnel 与 HTTPS 的断线自动重连开关。HTTPS 关闭自动重连后仍可手动启动一次 Bridge / SSH；启动 ChatX 时会恢复上次已选择的连接模式。设置页按连接策略、安全与授权、网络与代理、手机与 Relay 及 macOS 系统选项分组；手机配对继续保留在“手机连接”，运行日志归到“诊断”。首页“停止连接”会优先持久化两种传输的独立手动停止标记，断开旧 Tunnel 与 HTTPS MCP，再事务化保存停止意图；即使主设置文件保存失败，下次启动也会由停止标记阻止自动重连，并向用户提示保存故障。再次手动连接成功后才清除对应标记。HTTPS MCP 与 Tunnel 的手动连接启停共用生命周期操作锁，避免停止与启动互相覆盖。停用连接不代表撤销 OAuth 授权。本机健康检查使用每次启动随机生成的实例标识，避免端口被旧进程占用时误报就绪。

调用记录及实时状态分别读取两套 Desktop Commander 工作目录，并按时间合并、标注来源；历史记录读取限制为每个传输最近 2 MiB 的 JSONL 数据，统计只涵盖这个窗口。状态汇总优先展示仍存活、快照有效的进行中调用，避免另一条较新的空闲快照覆盖活跃调用；Android Monitor 保留旧 `tunnel`/`mcp` 字段，另提供只读 `connections.activeMode`、`connections.tunnel`、`connections.httpsMcp` 以表示唯一选中的链路，并按来源标注工具调用；新版 Android 的首页和通知使用此数据，旧版桌面缺少新字段时自动兼容。手机仍经 Monitor Direct HTTPS / Relay 获取状态，不直接连接高权限 MCP/OAuth 接口。

**排障**：优先检查连接页 WSS 状态、ChatX state 下的 `https-mcp-bridge.log`、`relay-deploy.log` 以及服务器 `journalctl -u chatx-relay.service`。SSH 兼容模式再查 `https-mcp-ssh.log`。如果公网 `/mcp` 返回 502，通常是 ChatX/SSH 尚未连接到服务器的 `18080`；如果返回 401，证明 OAuth 保护已生效；如果连接状态仍然显示“启动中”，排查 SSH 免密登录或服务器端转发端口占用。保留原 Secure MCP Tunnel 作为回退方式。

本机验证命令：`npm test`（OAuth 模拟 MCP）和 `npm run test:runtime`（真实 bundled Desktop Commander，含 HTTPS Bridge）。完整验证使用 `npm run test:local-full`。Windows 测试安装包可通过 `npm run desktop:installer` 构建；正式发布仍要求代码签名。

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

手机公网 Monitor Relay 由 ChatX 的统一 `relay` 设置管理。Desktop 通过 HTTPS 注册并维持 WSS 会话，按设备上传端到端加密 Snapshot、路由受限控制与配对流量；Relay 只保存 token hash 和密文，不持有 Runtime Key。旧的 `state/monitor-relay.json` 仅用于一次迁移，不再作为独立配置源。Relay 健康状态独立于 Tunnel，并显示 Desktop 注册、WSS 会话和最近错误。

Tunnel 健康状态同时参考本地 runtime `ready/health` 和 control-plane poll 日志。连续 poll 异常依次进入 `suspect` / `down`，恢复时通过 `poller recovered; polling operational` 回到 `healthy`，避免仅凭本地 `/readyz` 将上游断线误判成正常。

### Monitor 兼容矩阵

当前 Desktop / Android Monitor 使用 pairing schema 3 和 wire protocol v1。Desktop 在 Relay Hello 中上报自身能力，Relay 仅向手机发布端到端实际可用的能力；Android 只有看到 `control.refresh_snapshot` / `control.reconnect_tunnel` 后才发送对应控制消息。设备远程撤销使用 `revoke.sync`：Relay 必须等 Desktop 本地撤销确认后才删除 Relay registry 并向手机返回成功。缺少 capability 的旧 Desktop / Relay 会被视为 legacy：立即刷新回退到 HTTPS Snapshot，Tunnel 重连或同步撤销会明确报告不支持，而不是依赖超时猜测。

当前仓库版本组合为 Desktop 0.4.11、Android Monitor 0.3.6；两者版本号独立演进，兼容性以 pairing schema、wire protocol 和 capability 为准。

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

仓库统一使用 `scripts/toolchain-env.mjs` 恢复 OS 账户真实主目录（Node `os.userInfo().homedir`）；`dev-toolchain.mjs`、Tauri dev/build、installer/release 都复用这一环境，并从真实 HOME 定位 Cargo/Rustup、Android Studio JDK、Android SDK 和 Gradle 缓存。先运行：

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

手机公网 Relay 的唯一配置源是 `settings.json` 中的 `relay`。旧 `state/monitor-relay.json` 仅作为一次性迁移输入。当前 Relay 使用 HTTPS/WSS：Desktop 注册后上传每设备 E2EE Snapshot，并通过长连接处理配对、受限控制和同步撤销；Relay 与 Tunnel 生命周期互相独立。Android release 只接受 HTTPS/WSS Relay URL，避免与系统 cleartext policy 冲突。

静态检查：

```powershell
npm test
```

准备 bundled runtime 并执行真实 Desktop Commander stdio MCP 验证：

```powershell
npm run desktop:verify
```

`desktop:verify` 会先验证/解包锁定的 Desktop Commander MCPB，然后实际启动 bundled Desktop Commander，执行 MCP `initialize/tools/list`、Runtime Key 子进程隔离检查，以及文件写入、读取和 ripgrep 搜索 smoke test。所有联网运行资源下载都按 runtime lock 的 SHA-256（有大小时同时按大小）复核，并缓存在真实用户 `~/.chatx-cache/runtime/`；命中缓存仍重新校验，损坏缓存会自动丢弃。它完全在本机运行，不依赖 GitHub Actions。

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
