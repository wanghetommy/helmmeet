# 舵舟会议专家 / 小舵

已接入 `@earendil-works/pi-coding-agent@1.3.0` SDK。`src/main.mjs` 是 JSONL stdio sidecar；`src/persona.mjs` 定义人设与5个受控工具。`prompts.rs` 继续服务总结/纪要，聊天不再走旧 Rust QA 生成路径。语音转写和文字均提交同一 `assistant_turns`，SQLite 是唯一业务会话存储。

Pi 显式使用内存资源、设置、凭据接口、SessionManager与自定义 provider；没有编码工具、宿主目录发现、扩展或技能。真实 API Key、HTTP、数据库、版本栅栏和最终引用校验由 Rust 管理。工具只有 get_context、search_evidence、get_transcript、get_methods、propose_action。后者生成待接受的会议准备/笔记草稿，不自行开关录音、确认纪要或执行外部操作。

macOS sidecar同时受 `sandbox-exec` 与 Node permission 约束：禁止网络、子进程、写文件，文件读取只开放打包运行时。无法启动隔离时明确失败，不降级为无沙箱运行。每回合最多8工具、2次检索、6模型调用、一次结构修复和60秒总截止；逐次请求加入全局优先队列，取消时终止 sidecar/HTTP。当前只支持 macOS14+，沙箱接口未来需随平台适配评估。

首次准备（项目内缓存，不改全局工具链）：

```sh
npm ci
node scripts/prepare-agent.mjs --download --target aarch64-apple-darwin
python3 scripts/prepare-voice.py --download
```

Intel把target替换为x86_64-apple-darwin。`desktop:dev`/`desktop:build`使用已校验缓存离线准备运行时；SDK文件改变后重启dev命令。应用自带Node22.22.2与生产依赖，用户运行安装包不需要Node或Pi CLI。Node架构以Rust目标为准，兼容开发者在Apple Silicon上用Intel版npm的情况。

`npm run test:agent`验证真实SDK的假模型工具循环、历史/人设及一次修复；macOS还执行运行时读写/网络/子进程负向测试，须先准备本机Rust目标的运行时。Rust的assistant测试覆盖实际隔离sidecar→本地假HTTP代理、语音/文字历史、去重和取消，不调用真实模型。

完整范围、原生语音和实机待验收项见[方案](../docs/舵舟会议专家_全局Agent与语音交互方案.md)、[开发验证](../docs/小舵全局助手开发验证.md)和[AI开发指南](../docs/AI开发指南.md)。
