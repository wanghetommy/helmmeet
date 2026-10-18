#!/bin/sh
set -eu
TASK_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$TASK_ROOT"
sh scripts/with-toolchain.sh npm run tauri -w @helmmeet/desktop -- build
# Ad-hoc signing binds Info.plist and resources for local development; not notarization.
/usr/bin/codesign --force --deep --sign - --identifier com.helmmeet.desktop target/release/bundle/macos/HelmMeet.app
/usr/bin/codesign --verify --deep --strict target/release/bundle/macos/HelmMeet.app
