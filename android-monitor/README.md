# ChatX Monitor Android

ChatX Monitor 是以只读监控为主的 ChatX Desktop 手机客户端，并提供两项受限的端到端加密控制：立即刷新状态和在 Desktop 保持连接意图时触发 Tunnel 重连。

## 功能

- 扫描桌面端本地生成的配对二维码，或粘贴配对 JSON。
- 使用 Android Keystore + AES-GCM 保存 Monitor Token 和 TLS 指纹。
- 对 ChatX 自签名 TLS 证书执行 SHA-256 certificate pinning。
- 动态维护 LAN、Tailscale、Public IPv6 与 ChatX Relay endpoint；Relay 在线时会同步 Desktop 最新地址。
- 支持 LAN 优先、Relay 优先、自动稳定路径、手动首选四种连接策略，并在切换前先验证新路径。
- 连接页并行执行轻量 HTTPS Snapshot 请求，直接验证当前监控路径。
- 前台服务通过短连接 HTTPS 获取加密 Snapshot，刷新间隔可选 10 / 15 / 30 / 60 秒；快照过期与离线确认阈值也可配置，稳态监控不依赖常驻 WSS。
- “立即刷新”通过短生命周期的 E2EE 控制连接直接向 Desktop 请求新 Snapshot，不再等待 Relay 定时缓存更新。
- Tunnel 异常时可从手机发起受限的 E2EE 重连请求；Desktop 明确停止连接时手机不能覆盖连接意图，Runtime Key 始终只保留在 Desktop。
- 控制连接使用单次总超时并跨候选路径复用同一 requestId；Desktop 对相同密文请求返回幂等缓存结果。
- 通过 `hello_ack.capabilities` 协商控制能力；旧端缺少 capability 时立即刷新自动回退 HTTPS Snapshot，远程重连明确提示版本不支持。
- 区分 Host Offline、Tunnel Down、MCP GAP、MCP STALLED。
- 支持 1 / 2 / 3 / 5 分钟中断告警与恢复通知。

## 构建要求

- JDK 17
- Android SDK 37
- Android Gradle Plugin 9.4.0
- Gradle 9.6.0
- Android 17 targetSdk 37
## 本地构建

在 Android Studio 中打开 `android-monitor/`，安装 SDK 37 后同步项目。

命令行环境准备好 JDK 17 和 Android SDK 后：

```sh
./gradlew test
./gradlew assembleDebug
```

生成的 debug APK 位于：

```text
app/build/outputs/apk/debug/app-debug.apk
```

## 权限

Android 17 target 37 使用 `ACCESS_LOCAL_NETWORK` 访问 LAN。
实时监控使用 `connectedDevice` Foreground Service。
Android 13+ 需要通知权限才能可靠显示中断告警。
Google Code Scanner 负责二维码扫描，因此应用本身不申请相机权限。
