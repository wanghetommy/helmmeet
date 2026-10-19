#!/bin/sh
# Shared by build/run scripts. TASK_TARGET is empty for the legacy native output.
task_validate_target() {
  case "$1" in
    ''|aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo '仅支持 aarch64-apple-darwin / x86_64-apple-darwin 独立包；Universal 尚未接入。' >&2; return 1 ;;
  esac
}
task_bundle_directory() {
  TASK_OUTPUT_ROOT=${CARGO_TARGET_DIR:-"$TASK_ROOT/target"}
  case "$TASK_OUTPUT_ROOT" in /*) ;; *) TASK_OUTPUT_ROOT="$TASK_ROOT/$TASK_OUTPUT_ROOT" ;; esac
  if [ -n "$TASK_TARGET" ]; then TASK_OUTPUT_ROOT="$TASK_OUTPUT_ROOT/$TASK_TARGET"; fi
  TASK_BUNDLE="$TASK_OUTPUT_ROOT/${TASK_PROFILE:-release}/bundle/macos/HelmMeet.app"
}
