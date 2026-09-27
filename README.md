# NyaRemoteControl 客户端（nya-client）

装在**你正在使用的电脑**上，用来连接被控端（nya-server）。

## 连接

双击 `nya-client.exe`，按提示选择已保存的被控端，或者输入地址（例如 `100.64.0.2` 或 `100.64.0.2:47100`）。

也可以直接用命令行：

```powershell
.\nya-client.exe connect 100.64.0.2 --name 家里台式机
.\nya-client.exe connect 家里台式机 --mode game --fullscreen
```

**第一次连接需要配对码**：在被控端运行 `nya-server pair` 查看。配对成功后客户端会保存被控端的证书指纹，之后连接不再需要配对码；如果证书指纹变了，客户端会拒绝连接，以防止中间人攻击。

网络断开后客户端会自动重连，最多持续 2 分钟。

## 快捷键（Ctrl + Alt + Shift + …）

| 键 | 功能 |
|---|---|
| Q | 捕获 / 释放键盘。捕获时 Win、Alt+Tab 等组合键发给远程电脑 |
| S | 显示 / 隐藏统计面板：帧率、码率、端到端延迟、编码/解码耗时 |
| M | 切换办公模式 / 游戏模式 |
| R | 相对鼠标模式（适合 FPS 游戏，鼠标会被锁在窗口内） |
| F | 全屏 / 窗口 |
| D | 发送 Ctrl+Alt+Del（需要被控端以服务模式运行） |
| 1–9 | 切换被控端的第 N 个显示器 |
| X | 断开连接 |

## 两种模式

- **办公模式**（默认）：本机显卡能硬件解码时，自动使用 HEVC 4:4:4，文字清晰；画面静止后会再补发几帧来提高清晰度。画面不变时几乎不占带宽。
- **游戏模式**：4:2:0，高帧率（跟随显示器刷新率），码率固定，延迟最低，允许画面撕裂。

## 配置 `%APPDATA%\NyaRemoteControl\client\client.toml`

```toml
[defaults]
mode = "office"        # office | game
fullscreen = false
display = 0            # 被控端显示器编号，0 = 主显示器
bitrate_kbps = 0       # 0 = 由被控端决定
max_fps = 0            # 0 = 本机显示器刷新率
encoder = "auto"       # auto | nvenc | qsv | amf | software
codec = "auto"         # auto | h264 | hevc | av1
chroma = "auto"        # auto | 420 | 444
audio = true
clipboard = true
hw_decode = true

[[hosts]]              # 连接成功后自动保存
name = "家里台式机"
address = "100.64.0.2"
fingerprint = "…"
```

## 诊断

```powershell
.\nya-client.exe diag
```

会列出本机显卡、各显卡支持的硬件解码格式以及音频输出状态。日志保存在 `%APPDATA%\NyaRemoteControl\client\logs\`。

## 构建

```powershell
cargo build --release
.\scripts\package.ps1      # 生成 dist\nya-client
```
