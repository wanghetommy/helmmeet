# HelmMeet 会议对话 Agent

状态：`prompts.rs` 已作为桌面 Rust 模块编译，承载基础总结/纪要/问答的提示策略。持续对话、队列、HTTP 和证据写回由 Rust 主工程执行。Pi 依赖、工具 Agent 和独立 Node 运行时尚未接入。

本目录保留后续受限会议对话运行时的边界，候选实验使用 `@earendil-works/pi-coding-agent` 的 TypeScript SDK。详细边界、候选目录与实验退出条件见[会议类型与对话助手方案](../docs/会议类型与对话助手方案.md)、[ADR-0002](../docs/adr/0002-会议类型与受限对话运行时.md)和[开发计划 T00–T05](../docs/开发计划.md)。

实现时遵循根 [AGENTS.md](../AGENTS.md) 与 [AI 开发指南](../docs/AI开发指南.md)：

- 只做本场快照内的检索与对话编排。数据库、凭据、模型 HTTP、授权和最终证据写回由 Rust 管理。
- SDK 显式提供受控资源、内存会话和只读会议工具；不加载宿主技能/扩展/提示词，不启用 shell、文件、代码执行或任意网络工具。
- Agent 与 UI 均不持有真实 API Key；复用会议绑定模型，禁止自动更换服务。
- 所有工具、循环、重试、上下文和输出有预算、取消及版本栅栏。Agent 失败不得阻塞录音或 Whisper。
- 使用假服务和合成文本验证，不为试接 SDK 自动采集或外发真实会议。

后续代码分为 src/runtime、src/provider、src/tools、src/prompts、src/protocol 和 tests。T00 实验时建立隔离 package.json 与锁文件，采用结论成立后再接入桌面构建及打包；当前未安装依赖。生产版需随应用携带经验证的运行时，不能要求用户自行安装开发工具。
