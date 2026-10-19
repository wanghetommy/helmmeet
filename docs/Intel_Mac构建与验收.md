# Intel Mac 独立包：构建与验收

2026年10月19日 · R54/A54/TC-54 · 源码接入，Intel真人/设备验收待用户提供机器

最新小舵首版双架构结果见[开发验证](./小舵全局助手开发验证.md)。下方“本轮工程结果”及ZIP校验值为小舵接入前的历史构建；当前新包以该开发验证中的.app与架构结果为准，旧ZIP不包含小舵功能。

## 支持范围

- 独立`x86_64-apple-darwin`包和`aarch64-apple-darwin`包，最低macOS 14.0。不支持32位x86；本轮不合并Universal包，不降低系统版本要求。
- Intel使用Whisper CPU/Accelerate路径，只有Apple Silicon编入Metal；正式ASR与合成诊断使用同一架构策略。APM降噪/AEC编入两种架构，用户开关仍默认关闭。
- GGML Intel发行构建通过项目工具链脚本显式关闭native、AVX/AVX2/FMA/F16C/BMI2/AVX512/AMX等编译选项，避免按构建机器生成更高指令集要求。APM采用上游x86实现及其运行时SIMD探测，不给全部源文件统一追加AVX2标志。Intel性能优化待实机测量后另定。
- 现有GGML模型文件可复用；large-v3-turbo在Intel上的实时性、CPU/内存、散热与积压尚未验收。录音与转写独立，不能为追赶推理阻塞录音。

## 在真实Intel Mac构建

前提：macOS 14+、兼容的Xcode/Command Line Tools、Node.js 22.19+、npm 10+、Python 3.11+。不要复制其他架构的`.tools`或`target`缓存。

```sh
npm run bootstrap
.tools/python/bin/python3 -m pip install --require-hashes -r native/audio-processing/build-requirements.txt
.tools/python/bin/python3 native/audio-processing/build.py --target x86_64-apple-darwin --download
npm run desktop:build:intel
npm run desktop:run:intel
```

产物：`target/x86_64-apple-darwin/release/bundle/macos/HelmMeet.app`。配置模型路径/文本服务后再创建会议；本机`config/local.json`与模型不进入安装包。也可以直接把已构建的Intel `.app`复制到Intel Mac，无需在验收机安装开发工具。

## 在Apple Silicon交叉构建Intel包

先按README准备本机开发环境及固定APM依赖。新增Rust目标安装到现有项目隔离工具链，不替换宿主工具链：

```sh
sh scripts/with-toolchain.sh rustup target add x86_64-apple-darwin
node scripts/prepare-agent.mjs --download --target x86_64-apple-darwin
npm run desktop:build:intel
```

编译不运行Intel探针、不启动应用、不依赖Rosetta作为编译步骤。包交由真实Intel Mac验收；不将Rosetta下运行当作Intel设备验收。

显式Apple Silicon独立包使用`npm run desktop:build:arm64`与`npm run desktop:run:arm64`，位于`target/aarch64-apple-darwin/release/bundle/macos/HelmMeet.app`。原`npm run desktop:build`/`desktop:run`仍使用宿主默认路径`target/release/...`，兼容此前入口。`CARGO_TARGET_DIR`可更换输出根目录，`--target`与`CARGO_BUILD_TARGET`选择受支持目标；不接受Universal及任意自定义target。脚本为实际目标产物签名，避免误签旧包。

## 原生依赖与缓存

APM源包与SHA256继续锁定原版本，下载/解压共用受锁保护的缓存。目标产物改为：

```text
.tools/audio-processing/
  downloads/                         # 共用、校验后的源码包
  src/                               # 共用上游源码
  host/arm64/ 或 host/x86_64/         # 在构建机运行的pkgconf
  targets/<Rust target>/
    build/ prefix/ lib/ bin/
    build-manifest.json
```

Abseil、APM、C ABI及探针的编译/链接参数均使用目标架构。`build.rs`按Cargo TARGET查找静态库；Swift原生构建沿用swift-rs，跨架构构建使用显式`--triple`和独立OUT_DIR，绕开swift-rs 1.0.9对macOS包使用宿主`--arch`的问题。首次使用新目录会重新编译，旧arm64缓存不自动删除、不再作为应用链接目标。

APM合成探针在对应架构机器运行，例如Intel：

```sh
.tools/audio-processing/targets/x86_64-apple-darwin/bin/apm-probe --self-test
target/x86_64-apple-darwin/release/bundle/macos/HelmMeet.app/Contents/MacOS/helmmeet-desktop --check-audio-processing
```

CI增加两个目标的编译矩阵，交叉目标只编译测试，不运行；仅宿主架构执行Rust/Swift单测。CI任务未实际运行前不记为远程CI通过。构建脚本继续本机ad-hoc签名，Developer ID/公证仍不属于本轮交付。

## Intel实机验收清单（待执行）

记录机型/年份、CPU、内存、macOS版本、输入输出设备、模型及SHA256、实际包版本。

1. 启动/菜单栏/退出，首次权限与拒绝后恢复；模型/文件凭据可保存，历史数据可正常打开。
2. 麦克风、电脑声音、双音源录制；暂停/继续、结束确认、导出WAV回听无咔哒/断续，原轨保留。
3. 本地模型加载、中文转写、停止/退出时工作线程释放；记录每段音频时长与处理耗时、积压变化、CPU/内存和发热。large-v3-turbo若跟不上，另测较小/量化模型，不能把编译通过当作实时性通过。
4. 原声/降噪对照、内置扬声器外放远端独讲/本人独讲/双讲，检查回声减轻与人声完整，旁路状态准确；蓝牙等设备单独登记。
5. 固定合成文本先测试模型连接与JSON，再由用户主动用会议生成总结/纪要；不自动外发已有会议。

两小时稳定性继续按用户此前要求暂缓。本轮工程验证结果追加于下，未完成硬件项均保持待验收。

## 本轮工程结果

本机环境：Apple Silicon，Xcode26.6/Swift6.3.3，项目Rust1.99.1；未启动任何Intel二进制，没有通过Rosetta代替硬件验收。

- `npm run desktop:build:intel -- --bundles app`交叉构建/本机ad-hoc严格签名通过，Tauri报告15.24MiB。lipo确认应用、Swift静态库、APM归档及探针均为x86_64；Info.plist最低系统14.0。动态依赖仅系统框架/库，没有`.tools`或Homebrew动态库；GGML缓存确认METAL/NATIVE/AVX2=OFF，Cargo目标特性确认Intel不编入Metal、arm64保留Metal。系统Swift弱链接overlay中出现libswiftMetal不代表Whisper启用Metal。
- `HELMMEET_BUILD_TARGET=x86_64-apple-darwin sh scripts/with-toolchain.sh cargo clippy --workspace --all-targets --target x86_64-apple-darwin --locked -- -D warnings`通过，包含Intel测试代码编译但未执行。
- 原生arm64：Rust84项通过、1项默认忽略；严格clippy、fmt通过；Swift2项合成转换器XCTest通过（原有编译warning保留），APM C ABI self-test通过。
- `npm run verify`通过：TS7、Python23（含跨架构host工具/target依赖隔离、输出目录及Intel指令集策略3项新回归）、typecheck、31份文档链接与Vite构建；Shell语法和差异检查通过。
- 原默认arm64应用14.88MiB重新构建/验签通过；本地Whisper模型配合合成PCM的AppKit/Metal idle/busy/loading退出2.75/2.07/1.56秒，均join后exit 0，无Metal断言。
- Intel传输压缩包：`target/x86_64-apple-darwin/release/bundle/macos/HelmMeet-macos-x86_64.zip`，5,971,955字节；解压临时副本后x86_64及严格签名校验通过，副本已清理。SHA256：`9e0649b5e2884a697965946b8d0cdd29cb777451d77cebbe24c0caeeb8604c91`，同目录提供`.zip.sha256`。

补齐Git忽略例外，保证两份`build-support/*.ini`目标配置作为源码纳入版本控制候选，不被原`build-*/`通配规则屏蔽。没有提交/推送，本次未实际执行GitHub CI。未录制音频、调用远程模型或操作正式会议数据；真实Intel权限/采集/NS/AEC/中文实时性能及更广设备矩阵仍待验收。
