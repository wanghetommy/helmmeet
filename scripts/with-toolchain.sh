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
# Distribution builds must not inherit the build machine's Intel instruction set.
# The target is explicit for cross builds; uname is only the native-build default.
TASK_BUILD_TARGET=${HELMMEET_BUILD_TARGET:-${CARGO_BUILD_TARGET:-}}
if [ -z "$TASK_BUILD_TARGET" ] && [ "$(uname -m)" = x86_64 ]; then
  TASK_BUILD_TARGET=x86_64-apple-darwin
fi
if [ "$TASK_BUILD_TARGET" = x86_64-apple-darwin ]; then
  export GGML_NATIVE=OFF
  export GGML_METAL=OFF
  export GGML_SSE42=OFF
  export GGML_AVX=OFF
  export GGML_AVX2=OFF
  export GGML_FMA=OFF
  export GGML_F16C=OFF
  export GGML_BMI2=OFF
  export GGML_AVX_VNNI=OFF
  export GGML_AVX512=OFF
  export GGML_AVX512_VBMI=OFF
  export GGML_AVX512_VNNI=OFF
  export GGML_AVX512_BF16=OFF
  export GGML_AMX_TILE=OFF
  export GGML_AMX_INT8=OFF
  export GGML_AMX_BF16=OFF
fi
exec "$@"
