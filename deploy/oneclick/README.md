# ChatX WSS Relay 一键服务器部署

## 适用范围

ChatX「连接 → HTTPS MCP → 服务器一键部署」支持**先预检、再部署**。
沿用 Desktop Commander 原有权限模型及单 Desktop 路由，不需要为运行时 WSS 使用 SSH。
部署阶段临时通过密钥 SSH，把与桌面版本一致的 Relay 源码上传到自有 Linux 服务器，
使用服务器 Cargo 编译、备份旧服务、切换 systemd/Nginx，随后进行健康检查和失败回滚。
不会在服务器运行从外部站点下载的未知脚本。

## 首次准备

- Linux + systemd、python3、tar、curl、OpenSSL、Rust stable/Cargo。
- 自己已有 Nginx HTTPS 443 网站及有效证书。支持宝塔的 Nginx 网站；不会替你购买域名或申请证书。
- root SSH 密钥登录，或具备免交互 sudo -n 的 SSH 用户。程序不收集 SSH 密码。
- 首次连接新服务器，请先人工核对主机公钥指纹并建立 known_hosts；程序强制 StrictHostKeyChecking=yes。
- Desktop ID 会从本机现有 Relay 连接自动读取（没有时需要先有本机 identity）；初次部署新服务器后，在「设置 → 手机及 Relay」填写同一域名并点击「注册此电脑」，安装过程生成的 setup token 会预配至本机。

## 界面操作

1. 在「连接」选择原生 WSS Relay，展开「服务器一键部署」。
2. 填入服务器主机名/IP、SSH 用户、端口、HTTPS 域名、Desktop ID。私钥路径为空时使用默认 SSH 配置。
3. 点击「预检服务器」：只读检查 SSH、Cargo、systemd、HTTPS 站点和 Nginx 当前语法，**不修改服务器**。
4. 预检通过后点击「一键部署」并确认：自动上传、编译、备份、切换、验证。字段变更会取消预检结果。
5. 服务器部署成功后，确保桌面已注册并启动 HTTPS MCP。公网 OAuth Discovery 应为 200，未授权 MCP 为 401。

## 服务器文件及回滚

- /usr/local/bin/chatx-relay-server：新二进制先写临时文件再原子替换。
- /etc/chatx-relay/relay.env：保留原有注册配置和 bootstrap token，仅增加/更改 Desktop ID。
- 宝塔：网站 extension 目录中的 chatx-https-mcp.conf。普通 Nginx：/etc/chatx-relay/chatx-https-mcp.conf。
- 如缺少 Android/Relay 基础路由，额外创建 chatx-relay.conf；普通 Nginx 只在匹配站点插入 include。
- 初次安装时建立 chatx-relay systemd 账号、service 和 registry 目录。永不清空已有 registry.json。新服务器首次生成的 bootstrap token 会通过已验证的 SSH 通道返回并写入本机仅供 Relay 注册使用的文件；不会显示在 UI/日志。若本机原有 setup token，会先备份为 relay-setup-token.previous。
- 备份目录：/var/backups/chatx-relay/<UTC时间-进程号>/；构建目录：/opt/chatx-relay/releases/<UTC时间-进程号>/。

Nginx -t、Relay 健康检查、基本 MCP 网关检查任何一步失败均触发恢复。
若需人工恢复，请先对照 UI 提示的备份目录和站点路径，分别恢复备份的 server、
relay.env、mcp.conf、site 文件，再运行 nginx -t / nginx reload / systemctl restart chatx-relay.service。
**不要直接把某台机器的备份路径套用到其他服务器。**

## WSS v2 性能与兼容

- v1：保留兼容；HTTP 请求 JSON+Base64 最高 5 MiB，响应最高 16 MiB。
- v2：桌面 hello 广告 supportedVersions=[1,2]；新服务器协商 v2，旧服务器仍回退 v1。
- v2 二进制帧：1 字节类型、16 字节请求标识、最长 64 KiB 原始数据。双向分块，不再对每块进行 Base64 编解码。
- v2 单请求上限 64 MiB，响应 128 MiB，并发最多 64。每个流有独立有界队列；溢出只终止对应请求，不阻塞同一 WSS 上的其他工具调用与心跳。无 Content-Length 的超限上传返回 413。
- 桌面 HTTP 请求总超时 600 秒，Relay 等待头/响应空闲不超过 605 秒，Nginx MCP 代理空闲超时 630 秒；超过时会失败，而不是无限占用连接。
- Bridge 解析 MCP JSON-RPC 时仍需缓冲和解析完整 JSON；这里的优化针对 WSS 转发层，并非端到端零拷贝。
- OAuth 凭据仍留在桌面，Relay 不持有签发密钥；服务器却可接触透传的 HTTP 内容，须由可信管理员维护。重注册 Desktop 会更换 Token 并立即关闭旧 MCP WSS 会话，转发前也会复核凭据版本。

## 日志、诊断、更新

- 桌面 state/relay-deploy.log：只含预检、上传、构建、切换、错误等阶段元数据；超过 1 MiB 轮换为 relay-deploy.log.old。不记录 SSH 私钥、OAuth Token 或 MCP 工具参数/输出。
- 失败回滚测试：`node scripts/relay-deploy-rollback-test.mjs` 在临时文件上实际触发并核验旧二进制、环境配置和 Nginx 文件的恢复。它不触碰生产服务，不能替代真实服务器的故障演练。
- Linux：journalctl -u chatx-relay.service。公有 /healthz 新增 mcpGateway 的连接、协议版本、在途请求数。
- 链路自检：GET /healthz 200；GET /.well-known/oauth-authorization-server 200；
  无授权 POST /mcp 401；桌面不在线时 /mcp 503。
- 不会重置 Android Monitor 的 Relay 注册记录，不改 Desktop Commander 的工具权限。
- 当前只支持单目标 Desktop。需要多用户/多设备时另行扩展。
