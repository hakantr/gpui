# Extraction decisions

## Upstream parity

The Zed revision in `UPSTREAM.md` is the default source authority and the local Zed checkout is
read-only. GPUI, platform, and renderer production paths match that revision except for the
deliberate, evidence-backed local differences in `SAPMALAR.md`. Standalone build, test,
dependency-decoupling, documentation and provenance adaptations remain listed below; runtime or
public-API differences are permitted only through the recorded divergence process and must be
reapplied and revalidated after every sync.

## Package version

The `gpui` package declares `version = "0.3.0"` here, against `0.2.2` upstream. `0.2.2` is also the
newest version on crates.io, so a local development build reported the exact same version string as
the distributed crate and the two were repeatedly confused. The local number is bumped at the minor
level rather than the patch level so the difference is visible at a glance, and no prerelease
suffix is used because the build is an ordinary source snapshot, not a preview of a future release.

The version field is manifest metadata: that field changes no runtime behavior, public API, or
feature semantics, so it stays inside the repo-local allowance in `AGENTS.md`. The repository may
also carry separately justified runtime/API differences, but those are governed exclusively by
the deliberate-divergence process and enumerated in `SAPMALAR.md`. The crate is not published from
this repository.

The upstream sync preserves this field instead of resolving it to the Zed revision. It is recorded
in `SAPMALAR.md` under the deliberate-divergence process, which is what makes the sync reapply it
rather than drop it; the sync rule and the collision-avoidance requirement are also stated in
`AGENTS.md` and `UPSTREAM.md`. All three must be updated together if the number changes again.

## Evaluated and rejected: retained scaled-path API

A repo-local addition to `Window` was carried briefly and then removed. It is recorded here so the
same path is not re-taken without new evidence.

The addition gave `Path` an `Arc<Vec<_>>` vertex store plus `paint_scaled_path` and
`paint_transformed_scaled_path`, letting a consumer submit an already device-scaled path and share
its tessellated vertices across frames. The consumer was the `uPlot.rs` chart library, whose data
layer retains tessellated paths.

It was removed after measurement. Upstream `paint_path` rescales vertices per submission and
`Scene::replay` deep-copies them on cached-subtree reuse; both were measured at roughly 0.9 ns per
vertex. At the consumer's heaviest observed load — 3.08M vertices per second — the upstream path
costs about 0.6% CPU more than the retained one. That is below the maintenance cost of carrying a
GPUI fork through every upstream sync, so strict parity was restored.

If Zed later adds an equivalent API, import it through the normal sync. Do not reintroduce it here.

## Dependency closure

The closure was derived from `cargo metadata --locked --format-version 1` at upstream commit
`e9b5778e420fc69702630e1c12a93bb55c11486f`, classifying normal, build, dev, target-specific, and
feature-gated dependencies separately. Starting packages were `gpui` and `gpui_platform`; all four
platform implementations and their renderer were retained so the manifests remain portable.

The resulting internal runtime/build closure is the crate list in `UPSTREAM.md`. `collections`,
`http_client`, `media`, `refineable`, `scheduler`, and `sum_tree` are included because GPUI or a
platform implementation uses them directly. `derive_refineable` is required by `refineable`.

`gpui_tokio` is an optional adapter rather than a GPUI runtime requirement, but is included because
this extraction tracks the complete upstream `gpui_*` crate family. Zed editor, UI component,
cloud, collaboration, telemetry, project, workspace, and language crates are outside the closure.
Upstream examples and integration tests were excluded; unit tests embedded in the retained source
and the `gpui_wgpu` text-layout benchmark are preserved.

The standalone workspace raises `stacksafe` from upstream's `0.1` requirement to `1.0.3`.
`stacksafe 0.1.4` pulls `proc-macro-error2 2.0.1`, which Rust 1.97 reports as future-incompatible;
the warning is inherited by every local GPUI consumer and cannot be replaced by adding a second
dependency at the consumer. The 1.x series retains the `StackSafe` and `#[stacksafe]` API used by
GPUI while removing that macro dependency. This deliberate dependency-resolution divergence is
specified in `SAPMALAR.md` and must be dropped when the recorded Zed revision adopts a compatible
1.x-or-newer requirement.

## Workspace reconstruction

The root manifest uses resolver 2 and contains only extracted members plus the hello-world package.
Workspace dependency declarations were copied from the upstream manifest with their exact version,
feature, git revision, and target settings. Internal dependencies are local paths. Only the
`async-task` and `calloop` upstream patches needed by this closure were retained. Zed application
profiles and release metadata were omitted because they do not affect GPUI correctness.

The retained `calloop` patch is pinned to
`eb6b4fd17b9af5ecc226546bdd04185391b3e265`. The recorded Zed manifest names the same Git repository
without a `rev`, while its lockfile resolves that commit; the standalone manifest carries the
resolved revision explicitly so a later remote branch move cannot silently change this extraction.

The upstream Rust version was retained, while Zed-only targets and rustflags were removed. This
prevents an ordinary host build from requiring unrelated cross-compilation targets.

## Decoupling

- `ztracing::instrument` use in `sum_tree` and GPUI's SVG renderer was replaced by
  the Apache/MIT `tracing` attribute macro. Attribute use is unchanged
  (`#[instrument(skip_all)]`), so the public API did not change.
- The `zlog` dev dependency and its test initializer were removed.
- GPUI's three performance-marked unit tests are ordinary `#[test]` functions here. This removes
  `util_macros -> perf`, which is Zed test tooling and not runtime functionality.
- The `gpui_macros` dev dependency enables GPUI's `test-support` feature so its property-test
  doctests can resolve the test harness exports in this standalone workspace.
- The optional `http_client` GitHub-download feature and its Zed `util` dependency were removed.
- Upstream example target declarations were removed. The dependency-contained `gpui_wgpu`
  text-layout benchmark is retained. No GPUI public API was changed.

## Platform dependencies and assets

macOS retains Metal, Core Graphics/Text, Cocoa, AccessKit, media bindings, and the pinned Zed font-kit
fork. Linux/FreeBSD retains Wayland, X11, WGPU, AccessKit Unix, portal, clipboard, and text-system
dependencies. Windows retains DirectX/DirectWrite, Win32, AccessKit Windows, and manifest support.
Web retains its WASM platform implementation, including
the upstream-pinned `wasm_thread` fork that `gpui_web` depends on by git revision.

The updated platform crates retain GPUI's system-notification implementations through
`notify-rust` on Linux/FreeBSD, UserNotifications on macOS, and WinRT notifications on Windows.

GPUI embeds IBM Plex Sans and Lilex fonts for SVG/Web fallback rendering. Only those required font
families were copied; their upstream license files are retained beside them.
The JPEG used by GPUI's EXIF-orientation unit test is retained under `crates/gpui/tests/fixtures`;
its bytes originate from upstream commit `9552acc2bc242d45342fa9b5a987d43868aee1ec`.

## License review

The GPUI/platform crates and the non-GPUI internal dependencies in the retained closure declare
Apache-2.0 upstream, except `gpui_shared_string` and `gpui_util`, whose manifests declare no license.
Those two crates were included only after explicit user direction; their ambiguity is not resolved
or represented as legal advice. The GPL-only `ztracing` and `zlog` crates are not included.

The IBM Plex Sans and Lilex directories include their respective license texts. Microsoft-derived
shader sections retain their source copyright notices. See `NOTICE`.

## Reproducibility and known gaps

The extraction is a traceable source copy plus the small edits listed above. To evaluate a later Zed
revision, run `scripts/check-upstream.sh`, review the reported manifests/source/assets, recompute
metadata in the upstream checkout, and apply changes manually.

The recorded rich-text runtime divergence has one repository-local verification entry point:

```sh
scripts/verify-sapmalar.sh
```

It deliberately runs `gpui` text-system tests, macOS tests with the otherwise non-default
`font-kit` feature, and WGPU/cosmic-text tests. A plain `cargo test -p gpui_macos` does not compile
the feature-gated text-system evidence and must not be reported as proof for this divergence. On a
non-macOS host the script runs the portable and WGPU evidence but prints an explicit partial-result
warning instead of presenting the skipped CoreText suite as a complete verification.

The initial extraction was validated on macOS (`aarch64-apple-darwin`). The 2026-07-28, 2026-07-30,
and 2026-08-01 upstream syncs were validated on x86_64 Linux with formatting, locked workspace
checks, the `input-latency-histogram` feature, the `gpui_wgpu` benchmark target, and the full
workspace test suite. The 2026-08-05 and 2026-08-08 syncs were validated on macOS with formatting,
a locked all-targets workspace check, and the full workspace test suite run serially; the serial
run avoids the process-global pasteboard state shared by otherwise parallel macOS pasteboard
tests. Both web feature configurations were checked for `wasm32-unknown-unknown` with Zed's own CI invocation:
`-Zbuild-std=std,panic_abort` under `RUSTC_BOOTSTRAP=1` with
`-C target-feature=+atomics,+bulk-memory,+mutable-globals`. FreeBSD and Windows were not
cross-compiled during those syncs. GUI and browser launch were not used as automated assertions;
the hello-world binary was compile-checked.

The upstream `gpui_web` default enables `multithreaded`, which requires atomics and the
`wasm_thread` nightly-only `stdarch_wasm_atomic_wait` feature, so that configuration cannot be
built on a stable toolchain without bootstrap. The `--no-default-features` compile failure recorded
at the previous sync was fixed upstream and no longer applies. Per the parity policy, this
repository does not carry the former local single-thread workaround; any remaining limitation must
be fixed in Zed first.
