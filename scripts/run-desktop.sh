#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$TASK_ROOT/scripts/macos-target.sh"
TASK_TARGET=${CARGO_BUILD_TARGET:-}
case "${1:-}" in
  --target) if [ "$#" -lt 2 ]; then echo '--target缺少值' >&2; exit 1; fi; TASK_TARGET=$2; shift 2 ;;
  --target=*) TASK_TARGET=${1#--target=}; shift ;;
esac
task_validate_target "$TASK_TARGET"
task_bundle_directory
APP="$TASK_BUNDLE/Contents/MacOS/helmmeet-desktop"
if [ ! -x "$APP" ]; then
  printf '未找到应用：%s\n请先构建相同target的应用。\n' "$TASK_BUNDLE" >&2
  exit 1
fi
exec sh "$TASK_ROOT/scripts/with-toolchain.sh" "$APP" "$@"
