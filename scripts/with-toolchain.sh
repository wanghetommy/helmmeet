#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -x "$TASK_ROOT/.tools/cargo/bin/cargo" ]; then
  export CARGO_HOME="$TASK_ROOT/.tools/cargo"
  export RUSTUP_HOME="$TASK_ROOT/.tools/rustup"
  PATH="$CARGO_HOME/bin:$PATH"
fi
if [ -d "$TASK_ROOT/.tools/python/bin" ]; then
  PATH="$TASK_ROOT/.tools/python/bin:$PATH"
fi
if [ -f "$TASK_ROOT/config/local.json" ]; then
  export HELMMEET_DEV_CONFIG="$TASK_ROOT/config/local.json"
fi
export PATH
export MACOSX_DEPLOYMENT_TARGET=14.0
export CLANG_MODULE_CACHE_PATH="$TASK_ROOT/.tools/clang-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$TASK_ROOT/.tools/swift-cache"
exec "$@"
