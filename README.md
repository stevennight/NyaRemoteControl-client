# NyaRemoteControl 客户端（nya-client）

装在**你正在使用的电脑**上，用来连接被控端（nya-server）。

## 连接

双击 `nya-client.exe` 打开主界面（需要 Windows 自带的 WebView2 运行库；精简版系统缺少时会提示下载地址）：

- **设备**：顶部输入框直接输入地址连接（例如 `100.64.0.2` 或 `100.64.0.2:47100`）；已保存的设备以卡片显示，点卡片或“连接”即可。“⋯”菜单里可以改名、复制地址、删除。
- **连接设置**：被控端显示器（虚拟显示器 / 隐私屏）、画面模式、码率、声音、剪贴板等，改完点“保存”。
- **关于与诊断**：本机硬件解码情况、运行诊断、打开日志目录。

连接后，把鼠标移到窗口最上方（或按 Ctrl+Alt+Shift+T）会出现工具条：被控端名称、显示器（切换屏幕、虚拟显示器、一键隐私屏）、办公/游戏、键盘捕获、全屏、“⋯”（统计、相对鼠标、麦克风、USB、发送文件、Ctrl+Alt+Del、网络变差时的策略）、断开。断开后回到主界面。

也可以直接用命令行：

```powershell
.\nya-client.exe connect 100.64.0.2 --name 家里台式机
.\nya-client.exe connect 家里台式机 --mode game --fullscreen
```

**第一次连接需要配对码**：在被控端运行 `nya-server pair` 查看。配对成功后客户端会保存被控端的证书指纹，之后连接不再需要配对码；如果证书指纹变了，客户端会拒绝连接，以防止中间人攻击。

网络断开后客户端会自动重连，最多持续 2 分钟。

## 被控端显示器：虚拟显示器 / 隐私屏

在“连接设置 → 被控端显示器”里设置，连接后也可以在工具条的“显示设置”里随时修改。三项可以自由组合：

| 设置 | 说明 |
|---|---|
| 虚拟显示器：不用 / 1–4 个 | 在被控端新建虚拟显示器，第一个设为主显示器；有多个时在工具条“显示器”里切换查看 |
| 被控端物理显示器：保持显示 / 关闭（黑屏） | 保持显示 = 虚拟屏和物理屏同时存在（扩展屏）；关闭 = 只保留虚拟屏，被控端屏幕黑屏 |
| 屏蔽被控端本地键盘鼠标 | 被控端旁边的人无法操作 |

工具条“显示设置”里的**隐私屏**一键设为：1 个虚拟显示器 + 物理显示器黑屏 + 屏蔽本地键鼠。“恢复被控端原样”则撤销全部。

- 虚拟显示器的分辨率可以**跟随窗口**（调整窗口大小或全屏后自动跟随，画面 1:1 最清晰）、**跟随本机屏幕**或**固定**，还可以沿用本机的缩放比例（如 150%）。不会改动被控端物理显示器的分辨率。
- 第一次用某个不常见的分辨率时，被控端的虚拟显示器会重启一下（约 2 秒），之后切换是即时的。
- 断开后被控端自动恢复原来的显示器布局（为了网络闪断时不来回切换，会等 15 秒）。
- 需要被控端在管理界面的“可选组件”里安装**虚拟显示器**，并以服务模式运行；不满足时会提示原因并自动改用物理显示器。
- 隐私屏无法屏蔽被控端本地按下的 Ctrl+Alt+Del（Windows 的安全设计），但本地屏幕仍然是黑的。

被控端显示器开启了 HDR 时，画面会在被控端转换为 SDR 再传输（不再发白、过曝），统计面板里会注明。

## 文件和剪贴板

- **发送文件到被控端**：把文件拖进窗口，或点工具条上的“发送文件…”。文件保存在被控端当前用户的 `下载\NyaRemoteControl`，并放入被控端剪贴板，可以直接粘贴。
- **从被控端取文件**：在被控端复制文件（Ctrl+C），右下角会提示“被控端复制了文件”，点“下载到本机”。文件保存在本机的 `下载\NyaRemoteControl`，并放入本机剪贴板。
- **剪贴板文字和图片**双向自动同步（截图后直接粘贴即可）。
- 暂不支持文件夹。

## 快捷键（Ctrl + Alt + Shift + …）

| 键 | 功能 |
|---|---|
| T | 显示 / 隐藏工具条 |
| Q | 捕获 / 释放键盘。捕获时 Win+D、Win+E 等组合键发给远程电脑（Alt+Tab 在云电脑里无法捕获） |
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
vd_count = 0          # 虚拟显示器数量（0–4）
physical_off = false   # 有虚拟显示器时关闭被控端物理显示器
block_input = false    # 屏蔽被控端本地键盘鼠标
vd_size = "window"     # 虚拟显示器分辨率：window（跟随窗口）| screen（跟随本机屏幕）| fixed
vd_width = 1920        # vd_size = "fixed" 时使用
vd_height = 1080
vd_scale = true        # 虚拟显示器使用本机的缩放比例

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
cargo build --release      # 会自动用 npm 构建界面（../common/web），需要安装 Node.js
.\scripts\package.ps1      # 生成 dist\nya-client
```

主界面是 `../common/web` 里的 Svelte 页面，编译时嵌入 exe。改界面时可以在那里运行 `npm run dev`，在浏览器里用示例数据预览；或者设置环境变量 `NYA_WEB_DEV=http://localhost:5173` 再运行客户端，直接加载开发服务器。

## 仓库布局

本项目由三个仓库组成，需要克隆到同一个父目录下（server / client 通过 `../common` 引用公共库）：

```powershell
gh repo clone stevennight/NyaRemoteControl-common common
gh repo clone stevennight/NyaRemoteControl-server server
gh repo clone stevennight/NyaRemoteControl-client client
```
