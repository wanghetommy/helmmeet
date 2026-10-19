# 独立音频处理组件（AP02/AP03a）

固定 WebRTC Audio Processing 2.1 + Abseil 20240722.1，目标为 macOS 14+ arm64。窄 C ABI 提供实例创建、16kHz 单声道 160 样本（10ms）处理、参考输入、重置和销毁。AP03a已通过磁盘Worker生成独立麦克风增强轨，带设置/回听/导出；ASR仍走原轨，AP03b外放AEC实验版已接入，真人/设备质量待验收。

## 构建

先使用项目现有 `npm run bootstrap` 准备 `.tools/python`/Cargo。一次性安装固定构建工具并下载源码：

```sh
.tools/python/bin/python3 -m pip install --require-hashes -r native/audio-processing/build-requirements.txt
python3 native/audio-processing/build.py --download
```

上述命令只在项目 `.tools` 写入工具和依赖，保留 TLS 证书校验。缓存就绪后，普通构建完全离线：

```sh
python3 native/audio-processing/build.py
.tools/audio-processing/bin/apm-probe --self-test
.tools/audio-processing/bin/apm-probe --benchmark /private/tmp/helmmeet-apm-synthetic 6000
```

`--download` 仅允许获取 lock 中的 HTTPS 源码，并在解压前验证 SHA-256；不是自动更新。Meson 禁止依赖回退下载，使用项目编译的 pkgconf，Abseil 修补版本替换上游未修补的默认 fallback。源码和所有构建产物均在 Git 忽略目录；第三方许可正文保存在 `licenses/`。

输出为含 APM、Abseil 和桥接代码的单个 `.a`，及可执行实验工具。归档体积包含未链接使用的对象，不能当作最终应用增量。显式 arm64 配置防止构建机架构检测与编译器目标不一致；NEON启用，CoreFoundation显式链接。此处没有引入完整RTC网络栈。

## 应用与 Rust 验证

```sh
sh scripts/with-toolchain.sh cargo test --workspace --locked --features audio-processing
npm run desktop:build -- --features audio-processing
target/release/bundle/macos/HelmMeet.app/Contents/MacOS/helmmeet-desktop --check-audio-processing
```

Cargo feature audio-processing现在默认编入；用户降噪偏好仍默认关闭。也提供显式合成CLI探针。探针在应用数据、模型与UI初始化前返回，不读取开发凭据或真实会议、不采集、不联网。feature 构建自动调用离线原生构建，缓存缺失时报错并提示准备命令；不会在Cargo构建中偷偷下载。

使用`--no-default-features`构建的应用遇到同一探针参数明确 exit 2，不启动 GUI 或数据库。打包带上第三方许可文本，系统 C++/Foundation/CoreFoundation 之外无需用户安装动态库。

## ABI 约束

- ABI 1；只接受准确160样本、有限、幅度在[-1,1]的float PCM。NS级别0/1/2为关闭/轻度/中度，AGC关闭。错误返回负数，不修改输出。输入和输出可以原位复用。
- 调用方独占一个实例、串行处理/重置/销毁。C++异常不跨C ABI；Rust使用RAII且实例不可Send/Sync。指针必须来自本接口；销毁后不可再用。
- AEC需要每个采集处理前提供一次有效参考。缺少/无效/已消费参考或reset后返回不可用；同一音频帧的真实时间、延迟和丢帧对齐属于AP03，当前接口不替代实机验证；Rust派生链的参考/路由/旁路及128/224样本补偿见[AP03b记录](../../docs/音频增强组件_AP03b验证.md)。
- APM日志关闭。不要向诊断日志写入PCM、转写或用户路径。

- 新增`hm_apm_delay_ms`读取可选内部统计，-1不可用；不等同物理时延。AEC强制高通关闭，AGC仍关闭。

## 测量边界

工具生成固定种子的谐波信号+噪声及增强WAV（非真实语音）。P50/P95/P99仅算法调用，排除100帧预热；另报全循环wall/CPU时间。`realtime_one_core_percent`是CPU秒/音频秒×100的单核实时等效负载，不是任务管理器即时CPU。峰值RSS是整个实验进程，包含输入/输出数组，不能声称是引擎独占内存。噪声改善只在合成噪声窗口计算，不表示人声保护、真实房间AEC或CER通过。

完整数据与采用结论见[AP02验证](../../docs/音频增强组件_AP02验证.md)。降噪派生轨与音轨选择见[AP03a执行记录](../../docs/音频增强组件_AP03a验证.md)，真人质量/AEC/长时与整体产品改版后续推进。
