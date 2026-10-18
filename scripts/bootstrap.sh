#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$TASK_ROOT"
case "$(uname -s)-$(uname -m)" in Darwin-arm64) ;; *) echo '首期构建仅支持 Apple Silicon macOS。'; exit 1;; esac
mkdir -p .tools
if ! command -v cargo >/dev/null 2>&1 && [ ! -x .tools/cargo/bin/cargo ]; then
  curl --fail --location https://static.rust-lang.org/rustup/dist/aarch64-apple-darwin/rustup-init -o .tools/rustup-init
  chmod +x .tools/rustup-init
  CARGO_HOME="$TASK_ROOT/.tools/cargo" RUSTUP_HOME="$TASK_ROOT/.tools/rustup" .tools/rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.99.1
  sh scripts/with-toolchain.sh rustup component add rustfmt clippy
fi
if ! command -v cmake >/dev/null 2>&1 && [ ! -x .tools/python/bin/cmake ]; then
  python3 -m venv .tools/python
  .tools/python/bin/pip install cmake==4.4.4
fi
npm ci
sh scripts/with-toolchain.sh cargo fetch --locked
printf '%s\n' '构建环境已准备。运行 npm run desktop:dev 或 npm run desktop:build。'
