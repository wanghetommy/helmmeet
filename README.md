# HelmMeet · 本地智能会议助手

面向 macOS 14+ 的个人会议助手。Apple Silicon已有本机运行验证，Intel x86_64独立包配置已接入、实机验收待完成。接入录音、本地 Whisper 转写、DeepSeek 兼容分析、待办、问答、人工修订、回听与导出。

**当前是本地开发测试版。**真实权限、双音源长时间同步、两小时稳定性、中文质量与 P95 延迟仍需验收，详见[运行与验证](docs/运行与验证.md)。不把一次合成内容检查等同于全量 P0 通过。

## 立即运行

本次构建产物：`target/release/bundle/macos/HelmMeet.app`。可在 Finder 打开，或在仓库运行：

```sh
npm run desktop:run
```

当前包已接入浅色品牌蓝、帆船及会前/中/后工作区，见[整体产品方案](docs/会议助手整体产品设计方案.md)。

第一次推荐用该命令启动：它会读取忽略提交的 `config/local.json`，复用已登记的 Whisper 模型和 DeepSeek 配置。应用将密钥原子写入用户应用数据目录的 credentials.json（明文，Unix权限0600）、其余设置也保存在该目录，后续可直接打开 `.app`。

1. 在“设置”核对模型与文本服务；“测试连接与 JSON”只发送固定合成文本，验证结构与原文引用。
2. 新建会议，选择线下麦克风、仅电脑声音或线上双音源。可主动进行三秒电平检查，再点击开始并完成 macOS 授权。线上会议建议戴耳机；系统声音范围包含其他应用。
3. 音频分轨分段保存，本地转写从已保存文件处理。助手麦克风可独立关闭/恢复，会议软件静音不会控制它；暂停后继续不会静默重新开启已关闭的麦克风。
4. 有可用默认模型的新会议直接就绪，自动总结默认关闭；模型设置可逐场切换配置，展示目标服务及发送范围。切换不影响录音和 ASR，旧任务取消，旧结果保留。
5. “会议记录”可切换原稿来源并复制全文。“会议总结”支持独立开关及实时（60 字或 15 秒）/1/3 分钟，等本轮完成后重新计时；“会议纪要”按类型手工生成，行动事项共享；“会议助手”支持本场持续提问和 review。旧会议的明确停用决定保留，新会议无需重复点击允许。
6. 新会议可在录制前选择轻度/中度麦克风降噪（默认关闭）；设置里可改新会议默认值。录制时另存增强轨，回听可选原声/增强，整场完成后可导出增强WAV。系统轨和原声转写保持不变。双音源准备时可另选外放AEC实验版（默认关闭，内置/USB输出）；参考不足明确旁路。当前内置扬声器真人首轮试听确认回声减轻、本人声音完整，完整设备矩阵待验收；[实录结果与限制](docs/音频增强组件_真实外放与双讲验证.md)，两小时按用户要求暂缓。
7. 从“导出 / 管理”导出 Markdown、TXT、CSV 或混音 WAV。

关闭窗口保留菜单栏入口。录音中退出会被阻止并显示提示，需要先结束录音。系统睡眠、设备失效和队列溢出会标记中断，不保证睡眠期间录音。

如需主动检查麦克风，可运行 `npm run desktop:run -- --check-microphone`：使用真实麦克风三秒，仅在本机计算电平，不保存、不转写、不联网。启动故障的原因和修复验证见[麦克风启动故障修复](docs/麦克风启动故障修复.md)。

退出会先停止转写并释放模型；若模型正在加载，需等待原生加载返回。原生退出与 Metal 资源释放的修复、合成音频回归步骤见[Metal 退出崩溃修复](docs/Metal退出崩溃修复.md)。

## 从源码开发

前提：Node.js 22.18+、npm 10+、Python 3.11+、Xcode Command Line Tools。隔离工具安装在 `.tools/`，不修改全局 PATH：

```sh
npm run bootstrap
npm run doctor
.tools/python/bin/python3 -m pip install --require-hashes -r native/audio-processing/build-requirements.txt
python3 native/audio-processing/build.py --download
npm run desktop:dev
```

构建与运行：

```sh
npm run desktop:build
npm run desktop:run
```

本地构建进行 ad-hoc 签名，不代表 Developer ID 签名或 Apple 公证。模型不包含在安装包中。首次下载 Rust/crates/CMake/JS 依赖需要联网，构建后本地录音与 ASR 不依赖网络。

Intel独立包：`npm run desktop:build:intel`，运行用`npm run desktop:run:intel`；产物在`target/x86_64-apple-darwin/release/bundle/macos/HelmMeet.app`。Apple Silicon交叉构建需先安装对应Rust目标；工具/依赖准备、两种架构缓存、CPU后端及实机清单见[Intel构建与验收](docs/Intel_Mac构建与验收.md)。当前不生成Universal通用包。

### 网页预览

在仓库根目录执行 `npm run dev`，浏览器访问 **http://127.0.0.1:1420/**。服务已运行时直接访问，无需重复启动。

不要双击 `apps/desktop/index.html` 或使用 `file://` 打开应用入口；该文件引用 TSX 模块和站点资源，需要 Vite 转换并通过 HTTP 提供，直接打开会出现 `origin null`、CORS 和 `/favicon.svg` 找不到的错误。构建后的 `dist/index.html` 同样通过本地HTTP服务访问，不靠修改浏览器安全设置解决。文档内不依赖构建的静态设计样张与应用入口是两类文件。

网页仅为 UI 预览，录音、本地转写和模型分析使用 `npm run desktop:run`，源码联调使用 `npm run desktop:dev`。UI 只通过受限 Tauri IPC 调用后端，密钥不返回前端。

## 配置与数据

- `config/local.example.json` 为不含密钥的示例；`config/local.json` 是用户授权的本机开发配置，权限 0600、Git 忽略。
- 已登记用户提供的 `ggml-large-v3-turbo.bin`，无需再次下载；也可通过原生文件对话框选择其他 GGML Whisper 模型。
- 开发文本服务为 DeepSeek / deepseek-flash，已通过固定文本与合成会议分析测试；可在设置保存多套服务/模型/账号，每场会议独立选择和切换。
- 正式运行设置、SQLite 和音频位于 macOS Application Support 的 `com.helmmeet.desktop` 目录；API Key 在独立 credentials.json（明文，不打包、不提交，前端不回显）。旧Keychain仅一次性读取迁移或主动重试。应用数据没有全量应用层加密。
- 默认配置仅初始化新会议；编辑配置产生新版本，已有会议保持原绑定。切换、停用或旧凭据失效只影响关联会议。录音不发送至模型服务；远程分析仅发送本场授权文字和所需上下文。

## 检查

```sh
npm run verify
npm run test:core
sh scripts/with-toolchain.sh cargo fmt --all -- --check
sh scripts/with-toolchain.sh cargo clippy --workspace --all-targets --locked -- -D warnings
```

真实模型端到端测试使用仓库外的合成 WAV，不采集现场音频：

```sh
HELMMEET_TEST_WAV=/absolute/path/to/synthetic-16k-mono.wav \
HELMMEET_TEST_LLM=1 \
sh scripts/with-toolchain.sh cargo run -p helmmeet-desktop --locked -- --smoke-test
```

该命令会把合成转写发送到开发配置中的服务，测试实际纪要、待办、引用、保存、导出和回听。默认不运行，以免无意调用付费服务。

## 文档与目录

| 文档 | 内容 |
| --- | --- |
| [运行与验证](docs/运行与验证.md) | 当前能力、实测结果、限制与实机待验收 |
| [需求](docs/本地会议助手_需求文档.md) / [技术方案](docs/本地会议助手_技术方案.md) | 产品范围、技术规则 |
| [多模型与线上录制增补方案](docs/多模型配置与线上会议录制方案.md) / [开发验证](docs/增补功能开发验证.md) | 已接入能力、自动化证据和实机待验收项 |
| [会议类型与对话助手方案](docs/会议类型与对话助手方案.md) / [agent 目录](agent/README.md) | 六类模板与基础持续对话已实现；Pi/工具运行时仍待实验 |
| [工作区与自动总结优化方案](docs/会议工作区与自动总结优化方案.md) | 原稿复制、独立三标签、60 字或 15 秒/1/3 分钟串行总结；[实施验证](docs/工作区开发验证.md) |
| [整体产品设计](docs/会议助手整体产品设计方案.md) | 会议列表、会前/中/后、结束确认、无数据状态、全局资产与设置；首版已接入 |
| [知识资料与录制体验优化方案](docs/知识资料与录制体验优化方案.md) | 本机术语/资料、默认分析、紧凑布局；方案阶段，音频连续性缺陷先修 |
| [音频处理组件选型](docs/音频处理组件选型方案.md) / [AP02验证](docs/音频增强组件_AP02验证.md) | APM静态组件与离线实验完成；含依赖App增量约0.65MiB；[降噪首版](docs/音频增强组件_AP03a验证.md)已接入，默认关闭；[外放AEC实验版](docs/音频增强组件_AP03b验证.md)已接入，真人质量与设备/长时待验收 |
| [开发计划](docs/开发计划.md) / [可行性评审](docs/可行性评审.md) | 依赖、任务、阶段状态和风险 |
| [AI 指南](docs/AI开发指南.md) / [AGENTS.md](AGENTS.md) | 开发规则 |
| [接口协议](docs/接口协议.md) / [测试验收](docs/测试验收文档.md) | IPC、数据及完整验收步骤 |
| [第三方说明](THIRD_PARTY_NOTICES.md) | 主要依赖来源和许可 |

```text
apps/desktop/src/           React 会议工作区
apps/desktop/src-tauri/src/ Rust 存储、采集调度、推理、分析与受限 IPC
crates/meeting-core/        领域状态、覆盖和证据不变量
native/macos-audio/         Swift AVAudioEngine / ScreenCaptureKit 静态库
native/audio-processing/    APM静态组件、C ABI及合成对照工具
native/whisper-bridge/      C ABI 设计参考；实际使用 whisper-rs 封装
packages/contracts/        基础 IPC 类型与运行时校验
agent/                     基础提示策略；Pi/工具运行时方案
migrations/                SQLite v1–v10 迁移
scripts/                   隔离工具、运行、构建和检查
```

不提交真实音频、模型、会议或密钥。源代码沿用仓库 Apache-2.0 许可；本地开发构建未上传或对外发布。


### 整体产品工作区

正式界面已按准备、会中、会后三阶段组织，使用浅色品牌蓝与帆船。默认首页为会议列表，全局资料/词库按会议选用版本；模型可用的新会议无需重复允许，自动总结默认关闭。结束录制统一二次确认；会后手动生成/核对纪要，原文和录音按需展开。开发与验收记录见[整体产品改版开发验证](docs/整体产品改版开发验证.md)。

结构化结果、DeepSeek输出模式和跨平台文件凭据的修复与迁移边界见[本次修复记录](docs/结构化输出与凭据文件修复.md)。
