# macOS 音频桥接

已实现 AVAudioEngine 麦克风与 ScreenCaptureKit 系统声音，最低 macOS 14。arm64已有本机验证，x86_64独立包配置已接入、Intel设备待验收。Rust原生构建通过swift-rs静态链接，交叉构建使用显式Swift目标triple。支持设备枚举、显式设备选择、双源启动回滚、16kHz 单声道转换和带主时钟时间的帧回调。

麦克风回调使用预分配缓冲池，转换在独立队列执行；Rust 有界通道进入独立持久化线程。停止时先取消音源、排空原生回调队列，再释放回调与转换器。队列不足、设备配置改变和系统睡眠产生中断信号，不静默切换输入。

权限与实际硬件流程必须在真实应用中验收。原生编译、链接及设备列表读取已通过；本次经用户授权的三秒真实麦克风电平检查也已通过，不保存或转写声音。真实会议、其他设备及长时测试仍待验收，见[运行与验证](../../docs/运行与验证.md)和[麦克风故障记录](../../docs/麦克风启动故障修复.md)。

增补版本 ABI 2：支持停止单源、独立麦克风关闭/恢复和查询本场实际输入设备。默认输入解析为固定设备，恢复不静默换设备。关闭麦克风停止 I/O、移除 tap，等待在途回调并排空转换队列；权限预检在实际启动两路之前完成。Rust 串行化原生控制。

回调数据 source=0/1 为 mic/system，count 为样本数；故障 source=-1 为全局中断、-2 为 mic、-3 为 system，count 为故障原因码（5 缓冲、6 转换、7 设备/中断）。Rust 按源降级，磁盘或共享队列故障仍停止整场。电平检查主动采集三秒后释放，不落盘。

显式切换 CurrentDevice 后，tap 使用 inputNode.inputFormat 的硬件格式；outputFormat 可能暂存旧聚合设备的采样率，不能用它安装新设备的 tap。16kHz 转换在收到硬件帧后执行。配置变更通知只有在引擎仍运行、实际设备 ID 及硬件格式均与本场绑定一致时才忽略；停机、设备/格式变化或读取状态失败仍报告故障。

主动诊断：构建后运行 `npm run desktop:run -- --check-microphone`，使用相同麦克风预览路径采集三秒，只输出电平。该入口不打开数据库、不加载开发配置、不启动 ASR/LLM、不保存音频。原生 hm_audio_pump_events 仅为这一命令维持主线程通知投递；正常 UI 由 AppKit 事件循环处理。

Lifecycle.swift 为本进程安装 NSApplicationDelegate 退出代理，保留原 delegate 的其他方法转发；applicationShouldTerminate 先取消原生直接终止并通知 Rust，再由 Rust 在录制互斥检查、ASR/定时 Worker 停止与 join 完成后退出。显式合成退出探针可调用 hm_application_request_quit 验证相同 AppKit 路径，见[Metal 退出故障记录](../../docs/Metal退出崩溃修复.md)。


原生转换合成测试位于 `Tests/HelmMeetAudioTests/ConversionTests.swift`，覆盖 44.1/48kHz、单双声道和变长分包。测试仅构造 PCM 与 AVAudioConverter，不启动真实音源；命令和证据见[音频修复验证](../../docs/音频连续性修复验证.md)。

转换器输入不能直接借用 tap 缓冲池内存：AVAudioConverter 可能在后续调用继续读取它。PacketConverter 按输入块请求帧数复制到独立 loan，保留到转换器下一次索取输入，输出满时继续排空；缓冲池返回后不会再被转换器读取。回归必须复用并改写同一输入缓冲，与整段转换的公共前缀逐样本比较；仅每包新建内存或只检查包边缘不足以发现该缺陷。增强/AEC候选见[组件选型](../../docs/音频处理组件选型方案.md)，尚未接入。


AEC增补：`hm_audio_output_route`是只读CoreAudio默认输出快照（设备ID/transport/nominalRate三项UInt64），不启动音源或申请权限，由Rust writer按250ms观察并记录保守路由覆盖。当前内置/USB适用，其他路由旁路；不是AEC算法本身，详见[AP03b验证](../../docs/音频增强组件_AP03b验证.md)。
