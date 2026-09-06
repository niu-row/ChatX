# ChatX

ChatX 是一个运行在本机的 MCP（Model Context Protocol）服务，用于让 ChatGPT 在你明确授权的范围内访问本地文件、Git 仓库以及可选的 Shell / 进程能力。

ChatX **不会自己调用 OpenAI API 来完成对话**。ChatGPT 仍然是 MCP 客户端；ChatX 只负责把本机能力以 MCP 工具的形式提供给 ChatGPT。

```text
ChatGPT
  ↓
ChatGPT 自定义 MCP 应用 / Plugin
  ↓
OpenAI Secure MCP Tunnel
  ↓
本机 tunnel-client
  ↓
http://127.0.0.1:3210/mcp
  ↓
ChatX
  ├─ 文件系统
  ├─ Git
  └─ Shell / 进程（可选，高权限）
```

## 主要特性

- 本机 MCP 服务默认仅监听 `127.0.0.1:3210`。
- 通过 OpenAI Secure MCP Tunnel 与 ChatGPT 连接，不需要把本机 MCP 端口直接暴露到公网。
- Windows Tauri 桌面应用，支持原生文件夹选择器。
- 支持“安全 / 开发 / 完全开放 / 自定义”权限模式。
- 可动态添加或移除允许访问的本地目录。
- Runtime API Key 可选择使用 Windows DPAPI（`CurrentUser`）加密保存。
- 支持系统托盘；关闭主窗口后可继续保持 Tunnel 和 MCP 后端运行。
- 内置诊断页面。
- 内置真实 MCP 调用日志，可查看最近的工具调用、成功/失败状态和耗时。
- 文件系统、Git、Shell 权限彼此独立，不会因为 ChatGPT 请求某项操作就自动扩大权限。

## Windows 用户：推荐直接安装

普通 Windows 用户建议直接使用已经构建好的安装包：

```text
release\ChatX-Setup-0.2.0.exe
```

安装版已经包含运行桌面程序所需的本地组件。**普通用户不需要另外安装 Node.js 或 Rust。**

安装完成后，从开始菜单或桌面快捷方式启动 **ChatX** 即可。正式安装版使用 Windows GUI 子系统，不会额外弹出 CMD 控制台窗口。

> Node.js、Rust、npm 等要求只针对“从源码开发或重新构建 ChatX”的开发者。

## 第一次使用

第一次启动后，建议按下面顺序配置。

### 1. 获取 Tunnel ID

可以直接在 ChatX 的“使用教程 → 第一次配置”里填写，也可以在“连接”页面填写；两处会实时同步。需要打开管理页面时点击：

```text
获取 Tunnel ID ↗
```

它会打开 OpenAI Platform 的 Tunnel 管理页面。创建或选择一个 Secure MCP Tunnel，并复制类似下面的 ID：

```text
tunnel_...
```

Tunnel ID 可以保存在 ChatX 本地设置中。

### 2. 获取 Runtime API Key

点击：

```text
获取 Runtime Key ↗
```

创建用于 Secure MCP Tunnel 的 Runtime API Key。

如果勾选“安全保存 Runtime Key”，ChatX 会在 Windows 上使用 DPAPI `CurrentUser` 加密保存。明文不会写进 `settings.json`，也不会出现在调用日志中。

### 3. 选择权限模式

推荐日常开发使用：

```text
开发
```

它默认允许：

- 读取文件
- 修改文件
- Git 读取
- 受约束的 Git 写入

默认不会开启：

- Shell
- 高级 Git
- 完整文件系统访问

需要这些高权限能力时再单独开启。

### 4. 选择允许目录

打开：

```text
权限 → 允许目录 → 选择文件夹
```

使用 Windows 原生文件夹选择器添加 ChatGPT 真正需要访问的目录，例如：

```text
E:\Source\AI\my-project
```

建议按项目授权，不建议为了方便长期打开“完整文件系统访问”。

### 5. 连接 Secure Tunnel

回到“连接”页面，填写 Tunnel ID 和 Runtime API Key，然后点击：

```text
连接并启动
```

Tunnel 状态显示为 `running` 后，再打开“诊断”执行一次完整检查。

## 在 ChatGPT 中添加 ChatX

ChatX 是本地 MCP Server，因此 ChatGPT 不能直接访问 `127.0.0.1:3210`。需要通过 Secure MCP Tunnel 创建一个 ChatGPT 自定义 MCP 应用。

截至 2026 年 9 月，OpenAI 的开发者模式和完整 MCP 功能仍可能继续调整，ChatGPT 页面名称也可能变化。以下流程以当前官方界面为参考。

### ChatGPT 套餐要求

当前 OpenAI 文档说明：

- **Business、Enterprise、Edu**：支持开发者模式和完整 MCP，包括经过授权的写入 / 修改操作。
- **Pro**：可以在开发者模式连接自定义 MCP，但目前主要限于读取 / fetch 类能力，不提供完整 MCP 写入能力。
- 完整 MCP 和开发者模式当前主要在 **ChatGPT 网页版**使用。

如果是 Business / Enterprise / Edu 工作空间，还可能需要管理员或所有者先启用开发者模式以及相应访问权限。

官方说明：

- [ChatGPT 开发者模式和 MCP 应用](https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt-beta)
- [OpenAI Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)

### 添加应用

大致流程如下：

1. 在 ChatGPT 工作空间中开启 **Developer mode / 开发者模式**。
2. 打开 ChatGPT 的 **Apps / Plugins** 管理页面。
3. 创建一个新的自定义 MCP 应用。
4. 应用名称建议填写：

```text
chatx
```

5. Connection / 连接方式选择：

```text
Tunnel
```

6. 选择已经创建的 Secure MCP Tunnel，或填写对应的 `tunnel_...`。
7. 扫描 / 刷新 MCP Tools。
8. 确认 ChatX 暴露出来的工具列表。
9. 保存、创建或按工作空间要求发布应用。

如果 Tunnel 没有出现在 ChatGPT 中，请重点检查：

- Tunnel 是否创建在正确的 OpenAI Organization / Workspace 下。
- Runtime API Key 是否有对应 Tunnel 的使用权限。
- 工作空间管理员是否已经启用开发者模式。
- Enterprise / Edu 的 RBAC 是否允许当前用户访问该应用。

## 在聊天中调用

先确保：

```text
ChatX 正在运行
Secure Tunnel = running
ChatGPT 中已经启用 ChatX 应用
```

如果当前 ChatGPT 界面支持通过 `@` 调用应用，可以直接写：

```text
@chatx 列出 E:\Source\AI\my-project 的目录结构
```

也可以先在聊天界面中选择 ChatX 应用，再直接描述任务。

示例：

```text
@chatx 读取 package.json，解释这些 npm scripts 的用途
```

```text
@chatx 修改这个项目的 README，并运行测试
```

```text
@chatx 查看当前 git diff，总结这次修改
```

```text
@chatx 在项目目录运行 npm test
```

其中：

- 读取文件需要“文件读取”权限。
- 修改文件需要“文件写入”权限。
- Git 提交等操作需要 Git 写入权限。
- 任意 Git 参数需要额外开启“高级 Git”。
- 执行 npm、python、PowerShell 等命令需要 Shell 权限。

权限不足时 ChatX 会直接拒绝调用，不会自动打开更高权限。

## MCP 调用日志

桌面版左侧包含：

```text
调用日志
```

这里展示真实 MCP 工具调用，而不是 UI 模拟数据。

可以查看最近：

```text
20 / 50 / 100 / 200 / 500
```

条调用。

每条日志包含：

```text
时间 | MCP 工具 | 成功/失败 | 耗时
```

例如：

```text
19:20:13  fs_read      成功   8 ms
19:20:15  fs_edit      成功   4 ms
19:20:18  run_command  成功   612 ms
19:20:22  git_diff     成功   31 ms
```

为了避免泄漏敏感数据，调用日志**不会记录**：

- 工具参数全文
- 文件内容
- Runtime API Key
- Tunnel 凭据
- Shell 输出正文

调用日志目前保存在后端进程内存中，最多保存最近 500 条；ChatX 后端重启后会清空。

调用日志 API 还会返回每次调用的响应字节数，以及按工具聚合的调用数、错误数、平均耗时、P50、P95、最大耗时和平均响应大小。

## 桌面版运行方式

### 最小化

点击 Windows 最小化按钮后，ChatX 会正常进入任务栏。

### 关闭窗口

点击右上角关闭按钮不会立即退出程序，而是隐藏到系统托盘。

此时：

- Secure Tunnel 继续运行。
- 本地 MCP 服务继续运行。
- ChatGPT 仍可以继续调用 ChatX。

### 恢复窗口

左键点击托盘中的 ChatX 图标，或者右键选择：

```text
打开 ChatX
```

### 真正退出

右键托盘图标，选择：

```text
退出 ChatX
```

如果本次本地后端是由 ChatX 桌面程序启动的，退出时会同时清理它管理的 Tunnel 和后端进程。

如果 3210 端口上的 ChatX 后端在桌面程序启动之前就已经存在，桌面程序会复用该后端，不会把这个外部进程当作自己的子进程关闭。

## 权限模式

ChatX 提供四种权限模式。

### 安全

适合只读检查：

- 文件读取：开
- 文件写入：关
- Git 读取：开
- Git 写入：关
- 高级 Git：关
- Shell：关
- 完整文件系统：关

### 开发

推荐用于普通本地代码开发：

- 文件读取：开
- 文件写入：开
- Git 读取：开
- Git 写入：开
- 高级 Git：关
- Shell：关
- 完整文件系统：关

### 完全开放

高风险模式：

- 文件读写：开
- Git 读写：开
- 高级 Git：开
- Shell：开
- 完整文件系统：开

### 自定义

手动修改任何权限开关后会进入“自定义”模式。

## 文件系统工具

ChatX 当前提供：

- `fs_list`：列出目录，支持递归深度、默认目录排除、结果上限、`offset` 分页以及可选元数据；元数据读取采用 32 路有界并发。
- `fs_stat`：读取文件、目录、符号链接元数据。
- `fs_read`：UTF-8 / Base64 读取，支持字节范围和行范围；普通读取默认最多返回 256 KiB，并通过 `next_offset` 续读。
- `fs_read_many`：在一次 MCP 调用中有限并发读取最多 100 个文件，并分别返回成功或失败结果。
- `fs_write`：创建或覆盖文件。
- `fs_append`：追加文件内容。
- `fs_edit`：精确文本替换，可检查替换次数。
- `fs_mkdir`：创建目录。
- `fs_delete`：删除文件或目录。
- `fs_move`：移动 / 重命名，支持跨卷回退。
- `fs_copy`：复制文件或目录。
- `fs_search`：流式递归文字或正则搜索；字面量和兼容正则均优先使用 ripgrep，不可用或语法不兼容时自动回退到有限并发 JavaScript 实现。
- `fs_project_snapshot`：一次返回受限的项目树、关键配置文件和可选 Git 摘要；关键文件采用有界并发读取，非 Git 目录不会启动多条无效 Git 命令。

允许目录会在每次文件系统 / Git 工具调用时动态生效，不需要重启 MCP Server。权限开关还会动态更新 MCP 工具列表：被禁用的工具不会继续占用模型上下文，客户端会收到工具列表变更通知。

## Git 工具

### 常规 Git

- `git_status`
- `git_diff`：默认限制补丁响应大小，支持 `offset` / `max_chars` 分页。
- `git_diff_summary`：只返回逐文件增删行统计，避免为概览传输完整补丁。
- `git_log`
- `git_inspect`：并行返回状态、最近提交和 diff 统计。
- `git_stage`
- `git_unstage`
- `git_create_branch`
- `git_commit`

`git_commit` 只提交已经 staged 的变更，并且该受约束工具会关闭 Git hooks 和 GPG signing，避免提交过程通过 hook 间接执行任意外部程序。

### 高级 Git

```text
git_run
```

它可以传入任意 Git 参数，因此除了 Git 相关权限外，还需要单独开启：

```text
高级 Git
```

建议默认保持关闭。

## Shell 和后台进程

开启 Shell 后可使用：

- `run_command`
- `run_process`：直接传递 executable 和参数数组，不启动命令 Shell；前台输出支持 `output_offset` / `max_output_chars` 流式分页。
- `process_output`
- `process_list`
- `process_stdin`
- `process_terminate`

`run_command` 可以调用 PowerShell、cmd、Bash、sh 或平台默认 Shell；简单程序调用可优先使用启动开销更低、无需 Shell 字符串解析的 `run_process`。`run_command` 支持：

- 前台命令
- 超时
- stdout / stderr 捕获
- 后台进程
- 后台进程 ID 管理

### 重要：允许目录不是 Shell 沙箱

`CHATGPTX_ROOTS` 和桌面版“允许目录”只约束 ChatX 自己的文件系统 / Git 路径策略。

它**不能限制任意 Shell 命令**。

一旦开启 `run_command`，命令理论上可以访问当前 Windows / macOS / Linux 用户账号本身能够访问的任何资源。

因此建议：

- 日常保持 Shell 关闭。
- 只在确实需要运行本机命令时开启。
- 不要以 Administrator / root 身份运行 ChatX，除非你明确需要系统级权限。

## Secure MCP Tunnel

ChatGPT 不能直接连接本机 `localhost` MCP 地址。

OpenAI Secure MCP Tunnel 通过本机主动向 OpenAI 建立出站连接，将 ChatGPT 的 MCP 请求安全转发到：

```text
http://127.0.0.1:3210/mcp
```

ChatX 的桌面版会管理官方 `tunnel-client`，并通过 tunnel-client 自己的健康检查接口确认 Tunnel 真正进入 ready 状态。

它还会在 MCP discovery / probe 请求中显式加入：

```text
Content-Type: application/json
```

避免新版本 MCP Server 因请求缺少 JSON media type 而返回 HTTP 415。

官方资料：

- [OpenAI tunnel-client](https://github.com/openai/tunnel-client)
- [OpenAI Secure MCP Tunnel 文档](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)

## 本地接口

默认 MCP 地址：

```text
http://127.0.0.1:3210/mcp
```

健康检查：

```text
http://127.0.0.1:3210/healthz
http://127.0.0.1:3210/readyz
```

浏览器管理控制台（主要作为开发 / 回退入口）：

```text
http://127.0.0.1:3210/
```

桌面版正常使用时不需要手动打开这个网页。

## 配置文件与兼容命名

项目已经从 **ChatGPTX** 更名为 **ChatX**，但为了避免破坏已有用户配置，当前仍保留以下内部兼容名称：

```text
.chatgptx
CHATGPTX_*
com.chatgptx.local
```

因此看到这些名称是正常的，不代表桌面产品名仍然是 ChatGPTX。

Tunnel ID 等本地设置默认保存在：

```text
.chatgptx/settings.json
```

Runtime API Key 如果选择持久化，会通过 Windows DPAPI 单独加密保存，不会以明文形式写入该 JSON 文件。

## 环境变量

ChatX 直接读取环境变量，不会自动加载 `.env` 文件。`.env.example` 只作为配置参考。

| 环境变量 | 默认值 | 作用 |
| --- | --- | --- |
| `CHATGPTX_HOST` | `127.0.0.1` | HTTP 监听地址 |
| `CHATGPTX_PORT` | `3210` | HTTP 端口 |
| `CHATGPTX_ROOTS` | 当前工作目录 | 新设置文件的默认允许目录 |
| `CHATGPTX_SETTINGS_DIR` | `<cwd>/.chatgptx` | 设置、DPAPI 数据、Tunnel health 文件等本地数据目录 |
| `CHATGPTX_FULL_ACCESS` | `false` | 是否绕过允许目录检查 |
| `CHATGPTX_ENABLE_SHELL` | `true` | 环境变量层面的 Shell 默认值 |
| `CHATGPTX_AUTH_TOKEN` | 空 | 非 Tunnel 私有 HTTP 部署时可选的 Bearer Token |
| `CHATGPTX_MAX_FILE_BYTES` | `10485760` | 单个文件读取 / 编辑大小上限 |
| `CHATGPTX_MAX_COMMAND_OUTPUT_CHARS` | `200000` | 前台命令 stdout / stderr 缓冲上限 |
| `CHATGPTX_MAX_PROCESS_BUFFER_CHARS` | `1000000` | 后台进程输出缓冲上限 |
| `CHATGPTX_DEFAULT_COMMAND_TIMEOUT_MS` | `120000` | 默认前台命令超时时间 |
| `CHATGPTX_MAX_SEARCH_FILES` | `10000` | 文件搜索最大扫描数量 |
| `CHATGPTX_RG_PATH` | `rg` | 可选 ripgrep 可执行文件路径；不可用时自动使用 JavaScript 搜索 |

当 `CHATGPTX_HOST` 为 loopback 地址时，服务会启用 MCP SDK 的 localhost Host / Origin 校验。

标准 Secure MCP Tunnel 用法应保持：

```text
CHATGPTX_HOST=127.0.0.1
```

## 可选 Bearer Token

如果不是通过标准 Secure MCP Tunnel，而是在受控私有网络里直接访问 MCP HTTP 服务，可以配置：

```text
Authorization: Bearer <CHATGPTX_AUTH_TOKEN>
```

PowerShell 示例：

```powershell
$env:CHATGPTX_AUTH_TOKEN = "replace-with-a-long-random-secret"
```

标准的 `localhost + Secure MCP Tunnel` 部署不需要为了形式上“多一层认证”而额外打开公网监听。

## 从源码开发

如果你要修改 ChatX 本身，而不是只安装使用，则需要开发环境。

### 基本要求

- Node.js 20 或更高版本。
- Windows、macOS 或 Linux 可运行 Node MCP 后端。
- Git 工具需要系统已安装 Git。
- 构建 Tauri Windows 桌面版需要 Rust 1.77.2 或更高版本以及正常的 Tauri Windows 构建依赖。

当前开发环境验证使用过 Node.js 24。

### 安装依赖

```bash
npm install
```

### 开发 MCP 后端

```bash
npm run dev
```

生产式本地启动：

```bash
npm run build
npm start
```

### 启动 Tauri 开发版

```bash
npm run desktop:dev
```

### 构建桌面程序

准备内置后端资源并构建：

```bash
npm run desktop:build
```

### 构建 NSIS 安装包

```bash
npm run desktop:installer
```

项目当前发布的安装包放在：

```text
release\ChatX-Setup-0.2.0.exe
```

## MCP stdio / Inspector

不经过 ChatGPT，直接测试 stdio MCP：

```bash
npm run dev:stdio
```

也可以用 MCP Inspector：

```bash
npx @modelcontextprotocol/inspector npx tsx src/index.ts --stdio
```

stdio 模式下不要把普通日志写到 stdout，因为 stdout 是 MCP 协议通道。ChatX 的启动日志会写到 stderr。

## 测试

完整测试：

```bash
npm test
```

也可以分别执行：

```bash
npm run check
npm run build
npm run test:settings
npm run test:smoke
npm run test:desktop
npm run benchmark
```

`npm run benchmark` 会建立启动、连接、工具列表、500 文件目录元数据、文字/正则搜索和批量读取的性能基线，并在超过宽松回归阈值时失败。

当前测试覆盖的主要内容包括：

- TypeScript 类型检查。
- settings v1 → v2 迁移。
- 权限预设。
- 动态允许目录。
- Windows DPAPI 凭据存储。
- MCP 29 个工具的 smoke test。
- 文件系统读写。
- Shell / 后台进程。
- 受约束 Git 写入。
- 默认关闭的高级 `git_run`。
- 本地 Dashboard / 诊断接口。
- 非 JSON `/mcp` POST 的 HTTP 415 防护。
- MCP 调用日志接口。
- Tauri 桌面静态检查、托盘行为、原生文件夹选择以及安装资源配置。

## 安全建议

ChatX 的设计目标就是提供真实的本地操作能力，因此某些权限具有很高影响。

建议长期遵循以下原则：

- 使用普通用户账号运行，不要默认使用 Administrator / root。
- 使用 Secure MCP Tunnel 时保持 MCP HTTP 服务只监听 loopback。
- 优先使用明确的“允许目录”，而不是完整文件系统访问。
- 日常使用“开发”模式，不要默认“完全开放”。
- Shell、高级 Git、完整文件系统仅在需要时临时开启。
- 对 ChatGPT 提示的写入 / 删除 / 执行操作进行确认。
- 不要把不可信 MCP 客户端连接到 ChatX。
- 不要把“允许目录”误认为任意 Shell 命令的安全沙箱。

## 常见问题

### ChatGPT 无法访问某个文件或目录

检查：

```text
权限 → 允许目录
```

目标路径必须位于允许目录中，除非明确开启了“完整文件系统访问”。

### 可以读文件，但不能修改

检查当前权限模式。如果使用“安全”模式，请切换到“开发”，或者在高级权限中开启“修改文件”。

### 无法运行 npm / python / PowerShell

这些操作需要 Shell 权限。Shell 默认建议保持关闭。

### Tunnel 一直连不上

依次检查：

1. Tunnel ID 是否正确。
2. Runtime API Key 是否正确且有权限使用该 Tunnel。
3. ChatX 本地 MCP 是否通过 `/healthz`。
4. ChatX“诊断”中的 tunnel-client / readyz 是否正常。
5. ChatGPT 工作空间是否启用了开发者模式。

### 修改了 MCP 工具，但 ChatGPT 里没有出现

自定义 MCP 应用的工具列表不一定自动更新。根据 ChatGPT 工作空间类型，可能需要在应用管理页面执行 **Refresh / 刷新操作**，或者重新创建 / 发布应用。

### 关闭 ChatX 后为什么进程还在

桌面版右上角关闭按钮默认只是隐藏到托盘。需要真正退出时，请使用托盘菜单中的：

```text
退出 ChatX
```

---

ChatX 当前仍处于快速迭代阶段。涉及 ChatGPT 开发者模式、MCP 应用和 Secure MCP Tunnel 的产品界面与套餐权限，请以 OpenAI 最新官方文档为准。
