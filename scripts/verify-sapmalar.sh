#!/bin/sh
set -eu

readonly REPOSITORY_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPOSITORY_ROOT"

echo "Verifying recorded rich text divergence"
cargo test -p gpui --lib text_system

if [ "$(uname -s)" = "Darwin" ]; then
    cargo test -p gpui_macos --lib --features font-kit
else
    echo "WARNING: partial verification; CoreText evidence was not run because this host is not macOS" >&2
fi

cargo test -p gpui_wgpu --lib
