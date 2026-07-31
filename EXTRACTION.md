# Extraction decisions

## Upstream parity

The Zed revision in `UPSTREAM.md` is the sole authority for GPUI implementation changes. This
repository does not carry local GPUI features, fixes, APIs, optimizations, renderer paths, or
platform behavior. GPUI, platform, and renderer production code paths match that Zed revision; the
only permitted differences are the standalone build, test, dependency-decoupling, documentation,
and provenance adaptations listed below. Those adaptations must not change retained GPUI runtime
behavior, public API, or feature semantics.

## Package version

The `gpui` package declares `version = "0.3.0"` here, against `0.2.2` upstream. `0.2.2` is also the
newest version on crates.io, so a local development build reported the exact same version string as
the distributed crate and the two were repeatedly confused. The local number is bumped at the minor
level rather than the patch level so the difference is visible at a glance, and no prerelease
suffix is used because the build is an ordinary source snapshot, not a preview of a future release.

This is manifest metadata. It changes no runtime behavior, no public API, and no feature semantics,
so it stays inside the repo-local allowance in `AGENTS.md`. The crate is not published from this
repository.

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
`259297035a3fd64be4fb36042c229f59f074e38b`, classifying normal, build, dev, target-specific, and
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

## Workspace reconstruction

The root manifest uses resolver 2 and contains only extracted members plus the hello-world package.
Workspace dependency declarations were copied from the upstream manifest with their exact version,
feature, git revision, and target settings. Internal dependencies are local paths. Only the
`async-task` and `calloop` upstream patches needed by this closure were retained. Zed application
profiles and release metadata were omitted because they do not affect GPUI correctness.

The upstream Rust version was retained, while Zed-only targets and rustflags were removed. This
prevents an ordinary host build from requiring unrelated cross-compilation targets.

## Decoupling

- `sum_tree -> ztracing` was replaced by the Apache/MIT `tracing` attribute macro. Attribute use is
  unchanged (`#[instrument(skip_all)]`), so the public API did not change.
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

The initial extraction was validated on macOS (`aarch64-apple-darwin`). The 2026-07-28 and
2026-07-30 upstream syncs were validated on x86_64 Linux with formatting, locked workspace checks,
the `input-latency-histogram` feature, the `gpui_wgpu` benchmark target, and the full workspace
test suite. Both web feature configurations were checked for `wasm32-unknown-unknown` with Zed's
own CI invocation: `-Zbuild-std=std,panic_abort` under `RUSTC_BOOTSTRAP=1` with
`-C target-feature=+atomics,+bulk-memory,+mutable-globals`. FreeBSD, Windows, and macOS were not
cross-compiled during those syncs. GUI and browser launch were not used as automated assertions;
the hello-world binary was compile-checked.

The upstream `gpui_web` default enables `multithreaded`, which requires atomics and the
`wasm_thread` nightly-only `stdarch_wasm_atomic_wait` feature, so that configuration cannot be
built on a stable toolchain without bootstrap. The `--no-default-features` compile failure recorded
at the previous sync was fixed upstream and no longer applies. Per the parity policy, this
repository does not carry the former local single-thread workaround; any remaining limitation must
be fixed in Zed first.
