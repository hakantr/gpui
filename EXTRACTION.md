# Extraction decisions

## Upstream parity

The Zed revision in `UPSTREAM.md` is the sole authority for GPUI implementation changes. This
repository does not carry local GPUI features, fixes, APIs, optimizations, renderer paths, or
platform behavior. GPUI, platform, and renderer production code paths match that Zed revision; the
only permitted differences are the standalone build, test, dependency-decoupling, documentation,
and provenance adaptations listed below. Those adaptations must not change retained GPUI runtime
behavior, public API, or feature semantics.

## Repo-local API addition: retained scaled paths

One documented exception to the rule above currently exists. It is a deliberate, tracked deviation,
not an oversight.

`Window` carries two methods that have never existed in Zed:

- `paint_scaled_path(path: Path<ScaledPixels>, color)` — submit an already device-scaled path,
  sharing its immutable tessellated vertex storage instead of rescaling it per frame.
- `paint_transformed_scaled_path(path, transformation, color)` — the same, plus a late GPU
  transform applied to the shared vertices.

Supporting changes: `Path` gains a `transformation` field, `Scene` culls retained paths after the
transform is applied, and the Metal, DirectX, WGPU and WGSL/HLSL/Metal shader paths carry the
per-path transform.

Consumer: the `uPlot.rs` chart library retains tessellated data-layer paths across frames. Upstream
`paint_path` rescales vertices on every submission, which dominates frame cost on vertex-heavy
surfaces.

Sync behaviour: the 2026-07-28 sync removed this addition, correctly applying the parity policy in
`AGENTS.md` ("never preserve a repo-local implementation difference"). It was reintroduced
afterwards by merging `feat/retained-path-transforms`. Any future sync will remove it again unless
that merge is repeated. Two ways out, in order of preference:

1. Land the API in Zed and import it through the normal sync path, then delete this section.
2. Keep it here as an acknowledged exception and re-apply it after every sync.

Until one of those is chosen, this section and the `AGENTS.md` note are the only record that the
addition is intentional.

## Dependency closure

The closure was derived from `cargo metadata --locked --format-version 1` at upstream commit
`7b030b500810b04cf5fb4aa5973be99a502d9f36`, classifying normal, build, dev, target-specific, and
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
Web retains its WASM platform implementation.

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

The initial extraction was validated on macOS (`aarch64-apple-darwin`). The 2026-07-28 upstream
sync was validated on x86_64 Linux with formatting, locked workspace checks, the
`input-latency-histogram` feature, the `gpui_wgpu` benchmark target, and the full workspace test
suite. The default multithreaded web configuration was checked for `wasm32-unknown-unknown` using
Zed's atomics target features and a bootstrap scoped to the upstream `wasm_thread` dependency.
FreeBSD, Windows, and macOS were not cross-compiled during that sync. GUI and browser launch were
not used as automated assertions; the hello-world binary was compile-checked.

The upstream `gpui_web` default enables `multithreaded`, which requires atomics and the
`wasm_thread` nightly-only `stdarch_wasm_atomic_wait` feature. Its `--no-default-features` path
currently also fails to compile because `shared_memory_supported()` is referenced outside its
feature gate. Per the parity policy, this repository does not carry the former local single-thread
workaround; either limitation must be fixed in Zed first.
