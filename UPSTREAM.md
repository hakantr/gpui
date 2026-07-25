# Upstream provenance

- Repository: https://github.com/zed-industries/zed
- Commit: `401a0c7e3dee885c20e61858a0eefe760e4ec7b3`
- Source branch at extraction: `main`
- Extraction date: 2026-07-26

## Extracted crates

- `gpui`
- `gpui_platform`
- `gpui_linux`
- `gpui_macos`
- `gpui_windows`
- `gpui_web`
- `gpui_wgpu`
- `gpui_macros`
- `gpui_shared_string`
- `gpui_tokio`
- `gpui_util`
- `collections`
- `http_client`
- `media`
- `refineable` and `derive_refineable`
- `scheduler`
- `sum_tree`

The non-`gpui_*` crates above are required by GPUI's normal, build, platform, or public API
dependency closure. The standalone `gpui-hello-world` package was created for this repository.

## Minimal extraction changes

- Replaced `ztracing::instrument` in `sum_tree` with the API-compatible
  `tracing::instrument`, removing the mandatory GPL-only dependency.
- Removed `zlog` test initialization from `sum_tree`.
- Replaced three `util_macros::perf` test attributes with ordinary Rust `#[test]` attributes,
  avoiding the Zed performance-tooling dependency.
- Removed the optional Zed-specific `http_client/github-download` integration.
- Excluded upstream examples and benchmarks; a dependency-minimal example lives under
  `examples/hello-world`.
- Retained the new cross-platform system-notification API and added its platform dependencies.
- Relocated the EXIF-orientation unit-test fixture from the excluded upstream examples tree to
  `crates/gpui/tests/fixtures` without changing its bytes.

For updates, inspect the manifests and sources of every crate listed above, `assets/fonts`, the
root `Cargo.toml` and `Cargo.lock`, and all upstream changes reported by
`scripts/check-upstream.sh`.
