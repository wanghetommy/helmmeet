#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APP="$TASK_ROOT/target/release/bundle/macos/HelmMeet.app/Contents/MacOS/helmmeet-desktop"
if [ ! -x "$APP" ]; then
  echo '请先运行 npm run desktop:build。'
  exit 1
fi
exec sh "$TASK_ROOT/scripts/with-toolchain.sh" "$APP"
