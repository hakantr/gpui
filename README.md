# Standalone GPUI workspace

This repository is an independently buildable extraction of the GPUI framework from the
[Zed](https://github.com/zed-industries/zed) project. It is an unofficial derivative and is not
maintained or supported by Zed Industries. GPUI is pre-1.0 and its API can change without notice.

The exact upstream revision is recorded in [UPSTREAM.md](UPSTREAM.md). Extraction decisions,
including dependency and licensing notes, are recorded in [EXTRACTION.md](EXTRACTION.md).

## Requirements

- Rust 1.95.0 (selected by `rust-toolchain.toml`)
- macOS: Xcode command-line tools; Metal is the renderer
- Linux/FreeBSD: development packages required by the selected Wayland/X11 stack
- Windows: a supported Visual Studio/MSVC toolchain

Only macOS was built and tested during this extraction. The Linux, FreeBSD, Windows, and Web
source backends are retained, but their targets were not cross-compiled.

## Build and test

From the repository root:

```sh
cargo check --workspace
cargo test --workspace
```

Build and run the standalone example with:

```sh
cargo check -p gpui-hello-world
cargo run -p gpui-hello-world
```

The example opens a window containing a centered text label and does not use Zed themes, UI
components, or application assets.

## Checking a newer upstream revision

Pass a local Zed checkout and an optional revision to the read-only comparison script:

```sh
./scripts/check-upstream.sh ~/github/zed main
```

The script resolves the revision and reports relevant upstream changes. It never overwrites this
workspace or changes the upstream checkout.

## Licensing

Most extracted crates are marked Apache-2.0 upstream; see [LICENSE-APACHE](LICENSE-APACHE).
`gpui_shared_string` and `gpui_util` do not declare a license in their upstream manifests and were
included at the repository owner's explicit direction. See [NOTICE](NOTICE) and
[EXTRACTION.md](EXTRACTION.md) before redistributing this workspace. This is a factual extraction
record, not legal advice.
