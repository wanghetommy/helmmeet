#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$TASK_ROOT"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TASK_HOST_TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TASK_HOST_TARGET=x86_64-apple-darwin ;;
  *) echo '桌面构建支持 macOS arm64 / x86_64。'; exit 1 ;;
esac
mkdir -p .tools
if ! command -v cargo >/dev/null 2>&1 && [ ! -x .tools/cargo/bin/cargo ]; then
  curl --fail --location "https://static.rust-lang.org/rustup/dist/$TASK_HOST_TARGET/rustup-init" -o .tools/rustup-init
  chmod +x .tools/rustup-init
  CARGO_HOME="$TASK_ROOT/.tools/cargo" RUSTUP_HOME="$TASK_ROOT/.tools/rustup" .tools/rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.99.1
  sh scripts/with-toolchain.sh rustup component add rustfmt clippy
fi
if [ ! -x .tools/python/bin/python3 ]; then
  python3 -m venv .tools/python
fi
if ! command -v cmake >/dev/null 2>&1 && [ ! -x .tools/python/bin/cmake ]; then
  .tools/python/bin/pip install cmake==4.4.4
fi
npm ci
sh scripts/with-toolchain.sh cargo fetch --locked
printf '%s\n' '基础构建环境已准备。首次还需按 README 准备固定 APM 缓存，再运行 npm run desktop:dev 或 npm run desktop:build。'
