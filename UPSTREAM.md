# Upstream provenance

- Repository: https://github.com/zed-industries/zed
- Commit: `5e1fd392f67e27fa1da91bad43eef7db1a5dec23`
- Source branch at extraction: `main`
- Extraction date: 2026-08-01

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

## Upstream parity policy

Zed is the sole implementation authority for this repository. Retained GPUI runtime code, public
APIs, behavior, and feature semantics must match the commit recorded above. No feature, platform
fix, workaround, optimization, or behavior change may originate in this repository; it must land
in Zed first and then be copied through the upstream sync process. The only local differences are
the standalone extraction adaptations enumerated below, none of which may change GPUI runtime
semantics.

## Package version

The `gpui` package version in this repository is `0.3.0`. Upstream declares `0.2.2`, which is also
the newest version Zed has published to crates.io, so keeping the upstream number made a local
development build indistinguishable from the distributed crate. The local version is deliberately
ahead of the published `0.2.x` line and is never a prerelease.

This version is repo-local metadata only. It does not imply any GPUI feature, behavior, or API
difference from the commit recorded above, and it is not published anywhere.

**Sync rule:** the upstream sync must preserve `version = "0.3.0"` in `crates/gpui/Cargo.toml` and
the matching `Cargo.lock` entry. Do not resolve this field in favor of the Zed revision, and do not
introduce an `-alpha`/`-beta`/`-rc` suffix. If Zed's own version ever reaches `0.3.0` or higher,
raise the local minor version again and update this section, `AGENTS.md`, and `SAPMALAR.md`
together. The divergence is recorded in `SAPMALAR.md`, which is the list the sync reapplies.

## Minimal extraction changes

- Set the `gpui` package version to `0.3.0` so local development builds are not confused with the
  published `0.2.2` crate. See the section above for the sync rule.
- Replaced `ztracing::instrument` in `sum_tree` with the API-compatible
  `tracing::instrument`, removing the mandatory GPL-only dependency.
- Removed `zlog` test initialization from `sum_tree`.
- Replaced three `util_macros::perf` test attributes with ordinary Rust `#[test]` attributes,
  avoiding the Zed performance-tooling dependency.
- Enabled `gpui/test-support` for `gpui_macros` doctests so the standalone workspace's
  property-test examples compile.
- Removed the optional Zed-specific `http_client/github-download` integration.
- Excluded upstream examples; a dependency-minimal example lives under `examples/hello-world`.
- Retained the `gpui_wgpu` text-layout benchmark.
- Retained the new cross-platform system-notification API and added its platform dependencies.
- Relocated the EXIF-orientation unit-test fixture from the excluded upstream examples tree to
  `crates/gpui/tests/fixtures` without changing its bytes.

For updates, inspect the manifests and sources of every crate listed above, `assets/fonts`, the
root `Cargo.toml` and `Cargo.lock`, and all upstream changes reported by
`scripts/check-upstream.sh`.
