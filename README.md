# HelmMeet · 本地智能会议助手

面向 Apple Silicon macOS 14+ 的个人会议助手。已提供可运行的本地 `.app`，接入录音、本地 Whisper 转写、DeepSeek 兼容分析、待办、问答、人工修订、回听与导出。

**当前是本地开发测试版。**真实权限、双音源长时间同步、两小时稳定性、中文质量与 P95 延迟仍需验收，详见[运行与验证](docs/运行与验证.md)。不把一次合成内容检查等同于全量 P0 通过。

## 立即运行

本次构建产物：`target/release/bundle/macos/HelmMeet.app`。可在 Finder 打开，或在仓库运行：

```sh
npm run desktop:run
```

第一次推荐用该命令启动：它会读取忽略提交的 `config/local.json`，复用已登记的 Whisper 模型和 DeepSeek 配置。应用将密钥存入 Keychain、其余设置写入用户应用数据目录，后续可直接打开 `.app`。

1. 在“设置”核对模型与文本服务；“测试已保存的连接”只发送固定测试文本。
2. 新建会议，选择线下麦克风、仅电脑声音或线上双音源。可主动进行三秒电平检查，再点击开始并完成 macOS 授权。线上会议建议戴耳机；系统声音范围包含其他应用。
3. 音频分轨分段保存，本地转写从已保存文件处理。助手麦克风可独立关闭/恢复，会议软件静音不会控制它；暂停后继续不会静默重新开启已关闭的麦克风。
4. 默认只在本机录音转写。“选择模型 / 启用分析”可逐场切换配置，展示目标服务及发送范围。切换不影响录音和 ASR，旧任务取消，旧结果保留。
5. 可手工或定时总结、生成纪要、核对待办、围绕整场/最近 10 分钟/选中片段提问。
6. 从“导出 / 管理”导出 Markdown、TXT、CSV 或混音 WAV。

关闭窗口保留菜单栏入口。录音中退出会被阻止并显示提示，需要先结束录音。系统睡眠、设备失效和队列溢出会标记中断，不保证睡眠期间录音。

## 从源码开发

前提：Node.js 22.18+、npm 10+、Python 3.11+、Xcode Command Line Tools。隔离工具安装在 `.tools/`，不修改全局 PATH：

```sh
npm run bootstrap
npm run doctor
npm run desktop:dev
```

构建与运行：

```sh
npm run desktop:build
npm run desktop:run
```

本地构建进行 ad-hoc 签名，不代表 Developer ID 签名或 Apple 公证。模型不包含在安装包中。首次下载 Rust/crates/CMake/JS 依赖需要联网，构建后本地录音与 ASR 不依赖网络。

`npm run dev` 仅为浏览器 UI 预览，不能直接采集或调用模型。UI 只通过受限 Tauri IPC 调用后端，密钥不返回前端。

## 配置与数据

- `config/local.example.json` 为不含密钥的示例；`config/local.json` 是用户授权的本机开发配置，权限 0600、Git 忽略。
- 已登记用户提供的 `ggml-large-v3-turbo.bin`，无需再次下载；也可通过原生文件对话框选择其他 GGML Whisper 模型。
- 开发文本服务为 DeepSeek / deepseek-flash，已通过固定文本与合成会议分析测试；可在设置保存多套服务/模型/账号，每场会议独立选择和切换。
- 正式运行设置、SQLite 和音频位于 macOS Application Support 的 `com.helmmeet.desktop` 目录；API Key 在 Keychain。应用数据没有全量应用层加密。
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
| [开发计划](docs/开发计划.md) / [可行性评审](docs/可行性评审.md) | 依赖、任务、阶段状态和风险 |
| [AI 指南](docs/AI开发指南.md) / [AGENTS.md](AGENTS.md) | 开发规则 |
| [接口协议](docs/接口协议.md) / [测试验收](docs/测试验收文档.md) | IPC、数据及完整验收步骤 |
| [第三方说明](THIRD_PARTY_NOTICES.md) | 主要依赖来源和许可 |

```text
apps/desktop/src/           React 会议工作区
apps/desktop/src-tauri/src/ Rust 存储、采集调度、推理、分析与受限 IPC
crates/meeting-core/        领域状态、覆盖和证据不变量
native/macos-audio/         Swift AVAudioEngine / ScreenCaptureKit 静态库
native/whisper-bridge/      C ABI 设计参考；实际使用 whisper-rs 封装
packages/contracts/        基础 IPC 类型与运行时校验
migrations/                SQLite v1–v6 迁移
scripts/                   隔离工具、运行、构建和检查
```

不提交真实音频、模型、会议或密钥。源代码沿用仓库 Apache-2.0 许可；本地开发构建未上传或对外发布。
