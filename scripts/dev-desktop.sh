#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$TASK_ROOT"
HELMMEET_BUILD_TARGET=${CARGO_BUILD_TARGET:-$(sh scripts/with-toolchain.sh rustc -vV | sed -n 's/^host: //p')}
export HELMMEET_BUILD_TARGET
node scripts/prepare-agent.mjs --target "$HELMMEET_BUILD_TARGET"
python3 scripts/prepare-voice.py
exec sh scripts/with-toolchain.sh npm run tauri -w @helmmeet/desktop -- dev "$@"
