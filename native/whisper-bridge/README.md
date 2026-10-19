# Whisper 适配

实际运行版使用 whisper-rs 0.16.0 / whisper-rs-sys 0.15.0 的 C API 封装，附带 whisper.cpp 1.8.3 源码，通过 Cargo.lock 固定版本与校验值。Apple Silicon构建Metal静态后端，Intel x86_64构建CPU后端（实机质量/性能待验收）；正式转写及合成诊断共用架构参数。推理调度在 apps/desktop/src-tauri/src/asr.rs，不依赖终端输出。

本目录头文件保留为独立桥接 ABI 设计参考，尚未实现这些 hm_whisper_* 符号，不参与应用链接。当前实例由单个 ASR 工作线程持有，从已封口 WAV 混音、追加有限上下文、识别并保存片段和覆盖区间。内存不足或模型错误不阻止音频保存。

本机模型配置不提交、不打进安装包。已经使用现有模型完成真实 Metal 合成音频检查，性能、中文质量与长会议边界仍按验收文档验证。详见 docs/运行与验证.md。
