# Windows 11 x64 适配方案

版本：v1.0

日期：2026年10月21日

状态：方案设计，尚未开始 Windows 实现。

## 1. 目标与边界

首期只支持 Windows 11 x64，Rust 目标为 `x86_64-pc-windows-msvc`。目标是让 HelmMeet 在 Windows 11 上完成会议录音、本地 Whisper 转写、结构化分析、会议助手问答、历史聊天检索、滚动摘要和导出。

首期不支持 Windows 10、Windows ARM64、CUDA/DirectML 加速、复杂语音音色和多架构安装包。Windows 10 与 ARM64 在首期稳定后另行评估。

macOS 继续支持 `aarch64-apple-darwin` 和 `x86_64-apple-darwin`。Windows 适配不得改变现有 macOS 原生实现、构建命令、安装包内容和运行行为。

## 2. 隔离原则

平台代码只在对应 target 编译：

- macOS 使用 `#[cfg(target_os = "macos")]`，保留 Swift、CoreAudio、ScreenCaptureKit、AppKit 生命周期和 Metal 路径；
- Windows 使用 `#[cfg(target_os = "windows")]`，新增 WASAPI、MMDevice、WinRT/SAPI 和 Windows 进程控制实现；
- 共享代码只扩展稳定接口，不在共享路径中加入 macOS 或 Windows 特有调用；
- Windows 原生代码放入独立目录，例如 `native/windows-audio`，不修改 `native/macos-audio` 的实现语义；
- `build.rs`、Cargo 依赖、资源准备和打包脚本按 target 分支；
- 数据库迁移、历史知识库、会议领域规则和 IPC DTO 保持跨平台，新增字段必须继续向后兼容。

macOS 构建仍使用：

```bash
npm run desktop:build:arm64
npm run desktop:build:intel
```

Windows 新增独立命令：

```bash
npm run desktop:build:win11-x64
```

## 3. 当前工程差异

可以直接复用的部分：

- `crates/meeting-core` 中的录音时间、分段、证据、预算、检索和总结规则；
- SQLite、迁移、会议状态、模型配置和凭据文件业务逻辑；
- React/Tauri UI、ContextPack、历史聊天索引和滚动摘要；
- Whisper 调度、模型请求、结构化输出校验和导出流程；
- Agent 的工具协议、人格提示和历史上下文协议。

必须增加 Windows 实现的部分：

- `apps/desktop/src-tauri/build.rs` 当前只接受 Apple target，并直接链接 Apple Framework；
- `apps/desktop/src-tauri/src/audio.rs` 当前通过 macOS 原生 C ABI 采集音频；
- `native/macos-audio` 当前使用 AVAudioEngine、ScreenCaptureKit、CoreAudio 和 Swift TTS；
- `native/audio-processing` 当前构建配置只声明 Darwin 和 Apple 架构；
- `native/voice-assistant` 当前静态库构建只接受 Apple target；
- `apps/desktop/src-tauri/src/assistant.rs` 当前使用 `/usr/bin/sandbox-exec` 隔离 Agent；
- URL Scheme、Info.plist、ad-hoc 签名和 macOS 应用数据目录需要替换为 Windows 机制。

## 4. 原生音频方案

Windows 使用 WASAPI 作为采集底层：

| 需求 | Windows 11 实现 | 共享给 Rust 的结果 |
| --- | --- | --- |
| 麦克风 | WASAPI capture client | 16 kHz 单声道 Float PCM |
| 电脑声音 | WASAPI loopback capture | 16 kHz 单声道 Float PCM |
| 设备枚举 | MMDevice API | 受控设备 DTO，不返回句柄 |
| 默认设备变化 | IMMNotificationClient | 路由版本和可见错误 |
| 设备时钟 | WASAPI device position/QPC | 单调 host 时间 |
| 输出路由 | MMDevice 属性 | AEC 路由快照 |
| 停止与恢复 | IAudioClient 生命周期 | 与现有录制状态机一致 |

Rust 侧保留现有 `CaptureHardware` 抽象，新增 `WindowsHardware`。采集回调仍然只复制有限 PCM 后送入现有有界队列，不能让音频采集等待 ASR、总结或问答。

系统声音采集使用 loopback，不读取其他应用文件或窗口内容。受保护内容、独占模式、设备拔出、睡眠恢复和采样率变化必须返回明确的不可用或中断状态，不能伪造静音或成功录音。

APM/AEC 继续使用现有窄 C ABI。首期先编译 Windows x64 静态库，使用 MSVC/clang-cl；不改变原声轨、ASR 原始来源和引用时间范围。

## 5. 语音助手与本地模型

### 5.1 Whisper

首期使用 Windows x64 CPU 后端，继续从用户选择的 GGML 模型加载，不把模型打进安装包。Whisper bridge 的 C/C++ 依赖固定 commit、版本和 hash；Metal 代码只在 macOS 构建。

### 5.2 唤醒词与 VAD

将 `native/voice-assistant` 的静态 wrapper 和 sherpa-onnx 依赖编译为 Windows x64 MSVC 版本。模型文件继续离线准备、校验 hash 后打包。中文唤醒词的规范化不能依赖 macOS ICU 专用调用，需要改为跨平台 Unicode/拼音实现，并保持保存时校验非法词形。

### 5.3 TTS

将 Swift TTS 替换为 WinRT `SpeechSynthesizer` 或 SAPI。输出统一转换为现有播放协议所需的 PCM；播放状态、打断、AEC 参考和聊天文字状态继续由共享 Rust 状态机管理。

## 6. Agent 隔离与凭据

Windows 没有 `sandbox-exec`，不能简单删除隔离。首期使用 Windows 受限子进程方案：

- Rust broker 继续是唯一模型网络出口；
- Agent 子进程使用 Job Object 限制生命周期、句柄和资源；
- 使用 Restricted Token 或 AppContainer 限制文件和网络能力；
- Node `--permission` 继续作为第二层应用内限制；
- Agent 只能读取打包运行时和被宿主写入的 stdin，不能直接访问会议目录、凭据或任意网络。

凭据继续由后端读写 `%LOCALAPPDATA%\HelmMeet\credentials.json`。Windows 使用当前用户 ACL 限制文件访问，替代 Unix `0600`；前端仍不能读取已保存密钥。不要因为换成 Windows 就把 API Key 放入前端配置或日志。

## 7. 构建、安装与协议

构建环境固定为 Windows 11 x64：

- Rust MSVC toolchain；
- Visual Studio Build Tools、Windows SDK、CMake/Ninja；
- Node 运行时和 Agent 依赖按 `win32-x64` 锁定 hash；
- voice/APM/Whisper 原生库按 `x86_64-pc-windows-msvc` 独立缓存；
- 构建脚本提供 PowerShell 入口，同时保留 Node 跨平台逻辑。

应用分发使用 NSIS 或 MSI。需要完成：

- WebView2 Runtime 检查与安装提示；
- Windows 应用图标、开始菜单和卸载入口；
- `helmmeet://` 注册表协议；
- 单实例启动和二次 URL 参数交接；
- Authenticode 签名和安装包完整性校验。

Windows 适配不应复用 macOS ad-hoc 签名脚本，也不应改写 macOS `.app` 产物。

## 8. 实施阶段

### W0：编译隔离

增加 Windows target、`cfg` 分支、target-specific Cargo 依赖和 Windows 构建入口。完成后应能编译不启用音频的 Windows 桌面骨架，macOS ARM64/Intel 构建保持通过。

### W1：录音主链路

实现 WASAPI 麦克风、loopback、设备枚举、时间戳、停止/恢复和错误映射。完成录音持久化后再接 ASR，禁止用模拟数据替代真实设备结果。

### W2：本地模型与语音能力

接入 Windows x64 Whisper CPU、KWS/VAD、TTS 和 APM/AEC。先完成文字问答，再加入点击说话和唤醒词，避免语音问题阻塞主链路。

### W3：安全与发布

完成 Agent 受限进程、凭据 ACL、协议注册、WebView2 检查、NSIS/MSI 和 Authenticode 签名。

### W4：Windows 11 实机验收

完成单源、双源、设备切换、睡眠恢复、外放、真人双讲、长会、模型退出、安装升级和卸载验收。每次 Windows 改动都执行 macOS 回归构建。

## 9. 验收门槛

工程回归：

- Windows x64 `cargo check --workspace --tests --locked`；
- Windows Rust tests、fmt、clippy；
- `npm run verify`；
- macOS ARM64 和 Intel 构建及签名校验；
- 迁移、历史索引和滚动摘要跨平台数据回归。

实机回归：

- 内置麦克风、USB 麦克风、蓝牙耳机；
- 麦克风、电脑声音、双音源；
- Teams、Zoom、浏览器播放声音；
- 默认设备切换、拔插、睡眠恢复、权限拒绝；
- 外放单讲、真人双讲、AEC 参考覆盖；
- 本地 Whisper CPU 速度、长会议磁盘和退出释放；
- 安装、升级、卸载、协议唤起和凭据 ACL。

未通过真实设备验收的适配器必须明确返回不可用，不能将交叉编译成功、合成 PCM 或单元测试通过写成 Windows 录音能力已通过。

## 10. 交付顺序

首个可用里程碑是“Windows 11 x64 文字问答版”：能够安装、保存会议、录入文字、调用本地或受控模型、使用历史上下文并导出结果。第二个里程碑加入麦克风和系统声音；第三个里程碑加入 KWS、TTS、AEC 和完整实机验收。

Windows 10、Windows ARM64、GPU 加速和更丰富的语音交互都在 Windows 11 x64 稳定后单独立项。macOS 两个现有 target 始终作为不可回归的发布门槛。
