#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$TASK_ROOT"
. "$TASK_ROOT/scripts/macos-target.sh"
TASK_TARGET=${CARGO_BUILD_TARGET:-}
TASK_PROFILE=release
TASK_EXPECT_TARGET=false
for TASK_ARG in "$@"; do
  if [ "$TASK_EXPECT_TARGET" = true ]; then
    TASK_TARGET=$TASK_ARG; TASK_EXPECT_TARGET=false
  else
    case "$TASK_ARG" in
      --target|-t) TASK_EXPECT_TARGET=true ;;
      --target=*) TASK_TARGET=${TASK_ARG#--target=} ;;
      --debug|-d) TASK_PROFILE=debug ;;
      --profile|--profile=*) echo '构建脚本暂不支持自定义profile，请使用release或--debug。' >&2; exit 1 ;;
    esac
  fi
done
if [ "$TASK_EXPECT_TARGET" = true ]; then echo '--target缺少值' >&2; exit 1; fi
task_validate_target "$TASK_TARGET"
export HELMMEET_BUILD_TARGET="$TASK_TARGET"
TASK_AGENT_TARGET=$TASK_TARGET
if [ -z "$TASK_AGENT_TARGET" ]; then
  TASK_AGENT_TARGET=$(sh scripts/with-toolchain.sh rustc -vV | sed -n 's/^host: //p')
fi
export HELMMEET_BUILD_TARGET="$TASK_AGENT_TARGET"
node scripts/prepare-agent.mjs --target "$TASK_AGENT_TARGET"
python3 scripts/prepare-voice.py
TASK_RESOURCE_CONFIG="$TASK_ROOT/.tools/bundle-config/$TASK_AGENT_TARGET.json"
node scripts/agent-bundle-config.mjs --target "$TASK_AGENT_TARGET"
sh scripts/with-toolchain.sh npm run tauri -w @helmmeet/desktop -- build --config "$TASK_RESOURCE_CONFIG" "$@"
task_bundle_directory
# Each executable is signed explicitly before the containing app.
/usr/bin/codesign --force --sign - --identifier com.helmmeet.agent.node "$TASK_BUNDLE/Contents/Resources/agent-runtime/bin/node"
# Ad-hoc signing binds Info.plist and resources for local development; not notarization.
/usr/bin/codesign --force --deep --sign - --identifier com.helmmeet.desktop "$TASK_BUNDLE"
/usr/bin/codesign --verify --deep --strict "$TASK_BUNDLE"
printf 'Local signed app: %s\n' "$TASK_BUNDLE"
