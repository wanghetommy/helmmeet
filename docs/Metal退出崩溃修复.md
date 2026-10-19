# Whisper Metal 退出崩溃修复

日期：2026年10月18日。范围：退出 HelmMeet 时 `ggml-metal-device.m:608` 的 `GGML_ASSERT([rsets->data count] == 0)`；保留 Metal 加速，不修改用户模型、会议和系统权限。

## 1 根因

用户堆栈经过 `NSApplication terminate:`、libc `exit`、C++ 全局析构与 `ggml_metal_device_free`。该断言表示全局 Metal 设备清理时仍有登记的资源。原版 ASR 线程无限循环、丢弃 JoinHandle，并在本地缓存 WhisperContext；应用退出没有停止/等待该线程，不能保证模型及推理状态先于全局 Metal 设备释放。

同时检查了锁定的 whisper-rs-sys 0.15.0 / whisper.cpp 1.8.3 源码：buffer_free 已调用 rsets_rm，因此不能直接套用“缺少移除调用”的其他版本补丁。上游[相关问题](https://github.com/ggml-org/llama.cpp/issues/22593)也记录了释放顺序导致同一断言的情况。用户日志中的 [PR 17869](https://github.com/ggml-org/llama.cpp/pull/17869)调整崩溃后的 backtrace 打印，不是此资源存活问题的修复；不启用 GGML_BACKTRACE_LLDB 作为解决方案。

本机锁定的 Tao 0.37.1 在原生 applicationWillTerminate 时发出退出通知，没有 applicationShouldTerminate 拦截；只修复 Tauri ExitRequested 还不能保护原生 Cmd-Q/Dock/AppleEvent Quit。Apple 提供了[退出决策接口](https://developer.apple.com/documentation/appkit/nsapplicationdelegate/applicationshouldterminate(_:))，必须在真正终止之前处理。

## 2 修复

- ASR 与定时调度改为有所有者的 Worker，包含停止令牌、可唤醒等待和 JoinHandle。退出先通知全部 worker，再逐一 join，缓存 WhisperContext 在工作线程内释放。
- ASR 检查任务派发、模型 hash、加载完成、推理开始及结果处理边界；原生解码使用停止回调。取消的推理不标为已处理，不丢弃磁盘音频，后续启动可续转。模型加载的原生调用本身不可中途打断，退出需等它返回并正常释放。
- 退出协调器从 running → draining → ready；重复请求不会重复拆除 worker，只有 join 成功才允许最终退出；异常清理不强制跳过资源释放。
- 使用本进程 NSApplicationDelegate 代理转发原有应用事件，仅接管 applicationShouldTerminate，把原生退出送回统一 Rust 退出流程；未替换框架其他行为。Tauri 菜单退出与原生退出均先检查采集，再处理 ASR。
- 仍在采集时拒绝退出。开始退出后关闭新 IPC、采集启动/电平预览、分析派发入口；与采集锁一起设置退出标记，防止排队中的开始录制越过退出决定。
- 清理在后台协调，主线程继续处理事件；不采用禁用 GPU、关闭断言、直接 _exit 或跳过析构来掩盖错误。没有升级或改写第三方源码。

## 3 可重复验证

普通回归：`npm run verify`、项目工具链的 `cargo test --workspace --locked`、fmt 和严格 clippy。新增测试覆盖休眠 worker 唤醒、资源析构先于 ready、重复退出、一个 worker 异常仍处理其他 worker，以及退出后阻止排队的采集/预览/IPC/分析。

真实本地模型的 opt-in 测试（普通 CI 默认跳过）：

```sh
HELMMEET_TEST_MODEL=/absolute/path/to/ggml-model.bin \
sh scripts/with-toolchain.sh cargo test -p helmmeet-desktop --locked \
  exit_probe::tests::real_metal_worker_releases_cached_context_and_cancelled_work \
  -- --ignored --exact --nocapture
```

打包应用的原生退出回归：

```sh
python3 tests/integration/check_metal_exit.py \
  --model /absolute/path/to/ggml-model.bin
```

脚本运行三份独立临时数据库，生成 8 秒合成 PCM，分别在模型缓存空闲、转写处理和模型加载中触发两次 AppKit 原生 Quit，要求：进程 exit 0；明确经过 worker join；无断言/backtrace；音频块 sealed；空闲时有完整 ASR 检查点，其他两阶段没有未完成的覆盖；无分析任务。子进程清除开发服务配置、远程测试和 Metal 禁用 residency 的环境覆盖，不靠禁用特性通过检查。超时只终止该合成子进程，绝不按应用名停止已有进程。

`--check-asr-exit` 是显式诊断入口，要求 HELMMEET_DATA_ROOT 为不含旧数据库的独立目录，以及 HELMMEET_TEST_MODEL；默认用户启动不执行诊断。无麦克风采集、无系统声音采集、无真实会议读取、无远程模型请求。原生测试与单元测试各有用途，不能用正常 Rust main 返回代替 AppKit 退出验证。

## 4 本次结果

| 检查 | 结果 |
| --- | --- |
| `npm run verify` | 通过：TS 类型、3 项协议、10 项 SQL、文档链接与前端构建 |
| `cargo test --workspace --locked`（项目工具链） | 35 项常规 Rust 测试通过，真实模型 opt-in 用例默认跳过 |
| 显式运行真实模型 opt-in 测试 | 1 项通过，覆盖 idle/busy/loading，真实模型为本机已有 large-v3-turbo；单次停止等待约 9/11/409ms（调试构建，非时延分位数承诺） |
| fmt / 严格 clippy | 最终代码均通过；首次 clippy 发现一个未使用 import，移除后通过 |
| `npm run desktop:build` | Swift/Rust 链接与打包通过，最终 HelmMeet.app 12.45MiB |
| 打包应用，idle 原生重复 Quit | exit 0；明确执行 join；coverage=1，合成音频 sealed，无分析任务；约 2.04s |
| 打包应用，busy 原生重复 Quit | exit 0；明确执行 join；coverage=0，合成音频 sealed，无分析任务；约 2.08s |
| 打包应用，loading 原生重复 Quit | exit 0；明确执行 join；coverage=0，合成音频 sealed，无分析任务；约 1.54s |

上述三个原生退出子进程均无 GGML_ASSERT/backtrace；各时间包含进程启动、达到测试阶段和退出，不是单纯清理耗时。测试脚本清理了自己的临时目录，没有停止已有用户进程或修改用户会议。最终 `codesign --verify --deep --strict`、18 份文档链接与 `git diff --check` 均通过；签名仍是本机 ad-hoc，不代表外部分发公证。

仍需单独验收：真实录制中各种退出入口的交互、不同设备/系统、强制结束进程与断电恢复、长期录音。强制结束进程不保证运行有序清理，音频恢复沿用已保存分段及检查点。
