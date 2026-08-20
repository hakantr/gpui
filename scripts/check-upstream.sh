#!/bin/sh
set -eu

readonly BASE_REVISION=cef06d351bec10d0fb6176018ce8624e97baeb40
readonly SOURCE_REPOSITORY=${1:-}
readonly NEW_REVISION=${2:-HEAD}

if [ -z "$SOURCE_REPOSITORY" ]; then
    echo "usage: $0 /path/to/zed [revision]" >&2
    exit 2
fi

if ! git -C "$SOURCE_REPOSITORY" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "not a git repository: $SOURCE_REPOSITORY" >&2
    exit 2
fi

readonly RESOLVED_REVISION=$(git -C "$SOURCE_REPOSITORY" rev-parse --verify "${NEW_REVISION}^{commit}")
git -C "$SOURCE_REPOSITORY" cat-file -e "${BASE_REVISION}^{commit}"

echo "GPUI extraction upstream comparison"
echo "base: $BASE_REVISION"
echo "new:  $RESOLVED_REVISION"
echo
echo "Relevant changed files:"

git -C "$SOURCE_REPOSITORY" diff --name-status "$BASE_REVISION" "$RESOLVED_REVISION" -- \
    Cargo.toml Cargo.lock rust-toolchain.toml .cargo/config.toml LICENSE-APACHE LICENSE-GPL \
    assets/fonts/ibm-plex-sans assets/fonts/lilex \
    crates/collections crates/gpui crates/gpui_apple crates/gpui_linux crates/gpui_macos crates/gpui_macros \
    crates/gpui_platform crates/gpui_shared_string crates/gpui_tokio crates/gpui_util crates/gpui_web \
    crates/gpui_wgpu crates/gpui_windows crates/http_client crates/media crates/refineable \
    crates/scheduler crates/sum_tree crates/util_macros tooling/perf crates/zlog crates/ztracing

echo
echo "Changed relevant manifests:"
git -C "$SOURCE_REPOSITORY" diff --name-only "$BASE_REVISION" "$RESOLVED_REVISION" -- \
    Cargo.toml 'crates/*/Cargo.toml' 'crates/refineable/*/Cargo.toml' 'tooling/*/Cargo.toml'
