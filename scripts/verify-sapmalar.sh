#!/bin/sh
set -eu

readonly REPOSITORY_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPOSITORY_ROOT"

echo "Verifying recorded rich text divergence"
cargo test -p gpui --lib --locked text_system

if [ "$(uname -s)" = "Darwin" ]; then
    cargo test -p gpui_macos --lib --features font-kit --locked text_system::tests::
    cargo check -p gpui_macos --bench layout_rich_line --features font-kit --locked
else
    echo "WARNING: partial verification; CoreText evidence was not run because this host is not macOS" >&2
fi

# The full backend suites include environment-dependent pasteboard and GPU-adapter tests. The
# recorded divergence is covered by the text-system modules below and stays deterministic on
# headless/sandboxed hosts.
cargo test -p gpui_wgpu --lib --locked cosmic_text_system::tests::
cargo check -p gpui_wgpu --bench layout_line --locked
