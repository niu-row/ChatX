# ChatX Monitor Android

ChatX Monitor 是 ChatX Desktop 的只读手机监控客户端。

## 功能

- 扫描桌面端本地生成的配对二维码，或粘贴配对 JSON。
- 使用 Android Keystore + AES-GCM 保存 Monitor Token 和 TLS 指纹。
- 对 ChatX 自签名 TLS 证书执行 SHA-256 certificate pinning。
- 动态维护 LAN、Tailscale、Public IPv6 与 ChatX Relay endpoint；Relay 在线时会同步 Desktop 最新地址。
- 支持 LAN 优先、Relay 优先、自动稳定路径、手动首选四种连接策略，并在切换前先验证新路径。
- 连接页并行执行轻量 WSS 握手测试，不再逐条等待完整 Snapshot。
- 前台服务使用常驻 WSS 监控 ChatX，并在 endpoint 或策略变化时在线重评估路径。
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
