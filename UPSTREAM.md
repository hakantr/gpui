# Upstream provenance

- Repository: https://github.com/zed-industries/zed
- Commit: `309f91f564fa267820e5d426daeeffafc2599530`
- Source branch at extraction: `main`
- Extraction date: 2026-10-02

## Extracted crates

- `bench_metrics`
- `gpui`
- `gpui_apple`
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
- `refineable` and `derive_refineable`
- `scheduler`
- `sum_tree`
- `zlog`
- `ztracing` and `ztracing_macro`

The non-`gpui_*` crates above are required by GPUI's normal, build, platform, or public API
dependency closure. `bench_metrics` backs GPUI's `bench-support` feature and the retained
benchmarks; `ztracing`, `ztracing_macro`, and `zlog` are `sum_tree`'s upstream instrumentation and
test-logging closure. The standalone `gpui-hello-world` package was created for this repository.

## Upstream parity policy

Zed is the default source authority for this repository and the local `../zed` checkout is strictly
read-only. Retained GPUI runtime code, public APIs, behavior, and feature semantics match the commit
recorded above except for deliberate local divergences recorded in `SAPMALAR.md`. No unrecorded
feature, platform fix, workaround, optimization, or behavior change may originate here. A recorded
divergence is reapplied after every upstream sync and removed when the selected Zed revision gains
an equivalent capability. Editing or submitting changes to Zed requires separate explicit
authorization.

## Package version

The `gpui` package version in this repository is `0.3.0`. Upstream declares `0.2.2`, which is also
the newest version Zed has published to crates.io, so keeping the upstream number made a local
development build indistinguishable from the distributed crate. The local version is deliberately
ahead of the published `0.2.x` line and is never a prerelease.

This version field is repo-local metadata only and is not published anywhere. It does not itself
describe a feature, behavior, or API difference; any such difference in this repository is
separately justified and enumerated in `SAPMALAR.md`.

**Sync rule:** the upstream sync must preserve `version = "0.3.0"` in `crates/gpui/Cargo.toml` and
the matching `Cargo.lock` entry. Do not resolve this field in favor of the Zed revision, and do not
introduce an `-alpha`/`-beta`/`-rc` suffix. If Zed's own version ever reaches `0.3.0` or higher,
raise the local minor version again and update this section, `AGENTS.md`, and `SAPMALAR.md`
together. The divergence is recorded in `SAPMALAR.md`, which is the list the sync reapplies.

## Minimal extraction changes

- Set the `gpui` package version to `0.3.0` so local development builds are not confused with the
  published `0.2.2` crate. See the section above for the sync rule.
- Replaced three `util_macros::perf` test attributes with ordinary Rust `#[test]` attributes,
  avoiding the Zed performance-tooling dependency.
- Enabled `gpui/test-support` for `gpui_macros` doctests so the standalone workspace's
  property-test examples compile.
- Removed the optional Zed-specific `http_client/github-download` integration.
- Excluded upstream examples and integration tests, together with the dev-dependencies only they
  use; a dependency-minimal example lives under `examples/hello-world`.
- Retained the `gpui_wgpu` text-layout benchmark and the two `gpui_platform` benchmarks that the
  upstream manifest declares.
- Pointed the `gpui_wgpu` shader tests' dev `naga` at the sibling `../wgpu/naga` the selected wgpu
  compiles shaders with (see `SAPMALAR.md`, “Kardeş `wgpu` checkout'unun seçilmesi”).
- Retained the new cross-platform system-notification API and added its platform dependencies.
- Relocated the EXIF-orientation unit-test fixture from the excluded upstream examples tree to
  `crates/gpui/tests/fixtures` without changing its bytes.

Runtime and public-API exceptions are not “minimal extraction changes.” They are listed only in
`SAPMALAR.md`, must be reapplied explicitly after a sync, and keep their own backend tests and drop
conditions.

For updates, inspect the manifests and sources of every crate listed above, `assets/fonts`, the
root `Cargo.toml` and `Cargo.lock`, and all upstream changes reported by
`scripts/check-upstream.sh`. Treat the source checkout as read-only, then reapply and revalidate every
entry in `SAPMALAR.md`; never resolve a recorded local runtime/API divergence silently in favor of
the new Zed tree.
