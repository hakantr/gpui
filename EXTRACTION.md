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

## Test-only and lint-only adaptations

`AGENTS.md` allows test-only adaptations and standalone wiring outside the deliberate-divergence
process, but requires each one to be recorded here. These are the complete list.

### Three `PathBuilder` tests that upstream does not have

`crates/gpui/src/path_builder.rs` carries a `mod tests` that the recorded Zed revision does not
have: a rounded-rectangle fixture plus three tests over it —
`rounded_path_fixture_is_finite_and_stays_inside_its_outer_bounds`,
`rounded_path_fixture_keeps_the_rectangular_content_mask_boundary_explicit`, and
`rounded_path_fixture_survives_scene_insertion_and_replay`.

They are what remained after the retained scaled-path API above was reverted. They assert **only**
retained upstream behaviour — that a built path's vertices are finite and inside the builder's own
bounds, that `clipped_bounds` is the intersection of `bounds` and the content mask, and that a
scaled path keeps its bounds and mask through `Scene::insert_primitive`, `finish`, and `replay`.
None of them touches a recorded divergence, and none asserts an API this repository added.

They are kept rather than dropped because upstream has no equivalent coverage: at
`1b86941cf7298912af31b56f16990cf65b3ecbd3` there is no `mod tests` in `crates/gpui/src/scene.rs`
at all, and no test anywhere in `crates/gpui` exercises `Scene::replay` or `clipped_bounds`. Since
that replay path is exactly what the reverted work perturbed, the tests are the standing evidence
that reverting it restored upstream behaviour. Drop them if Zed grows its own coverage for the
same three properties.

### One scoped `clippy::redundant_clone` allow in the web dispatcher

`crates/gpui_web/src/dispatcher.rs` carries a
`#[cfg_attr(not(feature = "multithreaded"), allow(clippy::redundant_clone))]` above the
`run_waker_loop` call. The statement itself is upstream's, unchanged.

The workspace denies `clippy::redundant_clone`, inherited from Zed's own lint table. Without the
`multithreaded` feature the `browser_window.clone()` there really is the value's last use, so the
lint is correct and fires; with the feature on, the `hardware_concurrency` read below still needs
the value and removing the clone would be a use-after-move. Zed does not hit this because it does
not gate `gpui_web` on `wasm32-unknown-unknown --no-default-features`; this repository does.

An earlier attempt changed `run_waker_loop` to take `&web_sys::Window` and clone inside. That was
behaviour-preserving but it edited retained production code without a `SAPMALAR.md` entry, which
`AGENTS.md` forbids during a sync. The attribute replaces it: upstream's function signature and
call site are restored byte-for-byte, and the local change is confined to a lint scope that names
the exact configuration in which the lint is right. Both wasm feature configurations pass
`cargo clippy --all-targets` with it.

### Wasm target isolation for the host bench and device-backed tests (24 August 2026)

The `gpui_wgpu` `layout_line` Criterion bench is host-only: Criterion's default Rayon path rejects
wasm with a `compile_error!`, and under `--all-targets` for `wasm32-unknown-unknown` that error
aborted the whole check before any production code was reached. The isolation is at the Cargo
target/cfg level only: `criterion` moved from plain dev-dependencies into the existing
`cfg(not(target_family = "wasm"))` dev-dependency section, and every top-level item of
`benches/layout_line.rs` is gated on the same cfg with an empty `fn main` for wasm. The host
invocation is unchanged — `cargo bench -p gpui_wgpu --bench layout_line` — and was re-verified
with Criterion's `--test` mode after the change.

With Criterion out of the wasm graph, the wasm test target compiled further and exposed that three
device- or native-backed `#[cfg(test)]` modules never belonged in the wasm scope:
`external_registry::tests` and `wgpu_renderer::external_surface_draw_tests` acquire real
adapters/devices through `pollster`, and `wgpu_context::tests` imports the
`cfg(not(target_family = "wasm"))`-gated `parse_pci_id`. All three now use
`cfg(all(test, not(target_family = "wasm")))`, following the pre-existing `wgpu_atlas` gate. No
production cfg or runtime behavior changed; the wasm production `--lib` compiles were green before
and after, and `cargo check --all-targets` for wasm32 (the atomics/build-std invocation) now
passes instead of failing on the host-only bench.

### wgpu consumer-semantics tests (24 August 2026)

Focused tests pinning the wgpu contract the renderer consumes, without changing any production
error or alpha policy:

- **Queue error routing** (`wgpu_renderer::queue_error_routing_tests`, device-backed, non-wasm):
  controlled validation errors prove that `Queue::write_buffer` and `Queue::submit` never return
  errors synchronously — they arrive through the device's uncaptured-error handler with their
  calling context in the message; that an open validation error scope (the pattern
  `try_adapter_with_surface` uses around its probe configure) captures the error and keeps the
  uncaptured handler silent; and that a routed error leaves the device usable for follow-up work,
  matching the frame-start drain into the renderer's failure counter. The counter itself only
  runs behind a configured window surface, so its behavior stays runtime evidence; these tests
  pin the channel that feeds it.
- **Alpha-mode preference** (`wgpu_renderer::alpha_mode_preference_tests`, portable): the
  production preference arrays were hoisted to named constants and the selection closure to a
  named function with identical logic (the only observable difference is that the adapter name
  for the error message is now fetched eagerly — one cheap `get_info` per surface
  configuration). Tests pin the array contents (`PreMultiplied`/`Opaque` first, `Inherit`
  second), the resolution order, the first-supported-element last resort, and the empty-set
  error. Changing any of these is an owner decision on the recorded divergence, not a refactor.
- **Real Metal capability fingerprint** (`wgpu_context::metal_surface_capability_tests`,
  macOS-only): a headless `CAMetalLayer`-backed surface records the real capability set and
  asserts the two assumptions the code makes — both picker first-preferences are present, and
  `alpha_modes[0]` (which the adapter probe configures with) is one of the two modes the Metal
  HAL accepts before its fail-fast. A probe-shaped 64x64 configure under a validation scope also
  passes. Observed on this host (Apple M4 Pro, wgpu `d4359d749…`):
  `formats=[Bgra8UnormSrgb, Bgra8Unorm, Rgba16Float, Rgb10a2Unorm]`,
  `present_modes=[Fifo, Immediate]`, `alpha_modes=[Opaque, PreMultiplied]`. Windowed swapchain
  behavior remains real-platform runtime evidence.

Test-only dev-dependencies for the fingerprint tests: `objc2` (workspace) and
`objc2-quartz-core 0.3.2` with the same version/features the sibling wgpu-hal locks, so the
dependency graph gained no new packages — the lock diff is two dependency-list lines.

### Deterministic frame pumping in two spring-element tests (24 August 2026)

The flake recorded under the 24 August 2026 hygiene pass —
`test_spring_animation_preserves_velocity_when_retargeted` failing
`value_before_retargeting > px(0.0)` twice in serial full-workspace runs — was root-caused. The
premise recorded there, that "the test drives the deterministic test clock", turned out to be
false: `SpringAnimationElement::request_layout` samples `scheduler::Instant::now()`, which is
`web_time::Instant` — the real monotonic clock on native — while the test executor's
`advance_clock` advances only the `TestClock` behind timers and `BackgroundExecutor::now()`. The
spring therefore integrates only the wall-clock microseconds that elapse between simulated
frames; the test's `advance_clock(50ms)` never reaches it. Measured in a passing run, the value
after the "50ms" advance was `2^-16` px instead of the ~11.8 px the virtual clock would produce.
The failure mode is f32 catastrophic cancellation: for roughly 11% of nanosecond-scale deltas
below ~35µs, the analytic propagator's `target + p00 * displacement` against the distant target
rounds the position step back to exactly the resting value, so a single simulated frame can
render `0.0` and fail the strict inequality.

The fix is confined to `mod tests` in `crates/gpui/src/elements/animation.rs`: a
`simulate_frames_until_movement` helper pumps frames until the rendered position leaves its
resting value, used at the two single-step-from-rest assertions
(`test_spring_animation_preserves_velocity_when_retargeted` and the `> px(20.0)` step in
`test_cancelled_and_completed_springs_resolve_their_endpoints`). Termination is guaranteed, not
probabilistic: position cancellation does not affect the velocity row of the propagator, so
velocity strictly accrues on every nonzero delta, and the next frame's `p01 * velocity` term is
added near zero where f32 registers it exactly — verified with worst-case 1ns deltas on
consecutive frames (frame 1 stalls at 0, frame 2 lands at 1e-14 > 0). The other spring tests'
assertions only require accrued velocity, which has no cancellation, and were left untouched.
Production animation code is byte-identical to upstream; the underlying clock-source mismatch is
upstream behavior and any real fix (stepping springs by the executor's clock) belongs in Zed
first. Drop this helper if upstream switches the element to the scheduler clock or reworks these
tests.

## Dependency closure

The closure was derived from `cargo metadata --locked --format-version 1` at upstream commit
`1b86941cf7298912af31b56f16990cf65b3ecbd3`, classifying normal, build, dev, target-specific, and
feature-gated dependencies separately. Starting packages were `gpui` and `gpui_platform`; all four
platform implementations and their renderer were retained so the manifests remain portable.

The resulting internal runtime/build closure is the crate list in `UPSTREAM.md`. `collections`,
`http_client`, `media`, `refineable`, `scheduler`, and `sum_tree` are included because GPUI or a
platform implementation uses them directly. `derive_refineable` is required by `refineable`.

The 2026-08-20 revision extracted the Metal renderer and atlas from `gpui_macos` into the new
`gpui_apple` crate. That crate is now part of the required runtime/build closure. The recorded
external-surface divergence follows the same ownership boundary: its Metal registry, shaders,
renderer integration, and cbindgen inputs live in `gpui_apple`; `gpui_macos` retains only the
AppKit-specific lookup from a live `NSView` to the renderer's producer face.

`gpui_tokio` is an optional adapter rather than a GPUI runtime requirement, but is included because
this extraction tracks the complete upstream `gpui_*` crate family. Zed editor, UI component,
cloud, collaboration, telemetry, project, workspace, and language crates are outside the closure.
Upstream examples and integration tests were excluded; unit tests embedded in the retained source
and the `gpui_wgpu` text-layout benchmark are preserved.

The recorded upstream revision now selects `stacksafe = "1.0"`. The earlier standalone
`stacksafe 1.0.3` override and its `SAPMALAR.md` entry were removed during the 2026-08-12 sync
because upstream met the divergence's drop condition. The standalone manifest now uses the same
requirement as Zed and Cargo resolves the compatible locked release normally.

### Sibling `wgpu` API tracking (20 August 2026)

This checkout builds `wgpu` from the sibling `../wgpu` path rather than from crates.io, so it sees
upstream wgpu changes before a release carries them.

#### Selected wgpu input

The path dependency deliberately carries no pinned revision, and `Cargo.lock` records the package
without a source, so the sibling working tree — not this repository — is the input. That is
load-bearing rather than an oversight: Rust treats a registry crate and a path crate as distinct
types even at the same version, so the counterpart `gpui-ec` repository can only borrow a device
from here while both sides point at the same checkout. The cost is that, unlike the Zed revision in
`UPSTREAM.md`, the wgpu input is not reproducible from this repository alone. It is therefore
recorded by observation and re-recorded at every sync:

| item | value at this sync |
|---|---|
| checkout | `../wgpu`, branch `trunk` |
| revision | `bbac60da54794f532c890fe985c92616cfc5f2fd` (19 August 2026) |
| version it declares | `30.0.0`, unreleased |
| version the recorded Zed revision selects | `29.0.4` from crates.io |

The renderer here therefore compiles against a wgpu **major version ahead** of the recorded Zed
revision. That is not an ordinary standalone adaptation and is **not** classified as one here.
A wgpu major release carries application-visible behavior, validation, and backend changes of its
own, and the reason for pointing at the sibling checkout is a capability the consumer gets: two
repositories resolving `wgpu` to the same crate instance is what lets `gpui-ec` borrow an
`Arc<wgpu::Device>` across the bridge at all. The **selection** is therefore a recorded deliberate
divergence — see “Kardeş `wgpu` checkout'unun seçilmesi” in `SAPMALAR.md`, which carries its limit,
the ruled-out consumer-side routes, the gain, and the drop condition. What stays in this file is
the mechanical consequence: the call-site signature adaptations below, and the revision table
above that has to be re-observed at every sync.

#### Adaptations the 30.0.0 move required

- `TextureFormat::is_srgb` was renamed to `has_srgb_suffix`
  ([#9758](https://github.com/gfx-rs/wgpu/pull/9758)); `wgpu_renderer` uses the new name.
- Presentation moved from `SurfaceTexture::present` to `Queue::present`; `wgpu_renderer` presents
  through `queue.present(frame)`.
- `SurfaceConfiguration` gained `color_space`, set to `SurfaceColorSpace::Auto` in `wgpu_renderer`
  and `wgpu_context`. `Auto` is documented as reproducing wgpu's historical behaviour, resolving to
  sRGB on unorm formats exactly as before. That matters here because the external-surface contract
  fixes surfaces as sRGB-encoded unorm with no hardware conversion.
- `RequestAdapterOptions` gained the required `apply_limit_buckets` field, set to `false` at every
  `request_adapter` call site so that real adapter limits are reported rather than the buckets
  meant for untrusted callers.
- `BufferSlice::get_mapped_range` and `get_mapped_range_mut` now return `Result`; the readback and
  vertex-staging call sites in `wgpu_renderer` carry an explicit expectation message.
- `VertexState::buffers` became a slice of `Option<VertexBufferLayout>`.

Three of these were reachable only by building the tests, not the library. That is the same lesson
the `default_queue` break below taught, and it is why the sync gate is
`cargo check --workspace --all-targets` rather than `cargo build`.

The `apply_limit_buckets` bullet was completed during the 2026-08-20 sync. The 30.0.0 move had
reached only the two test `request_adapter` calls. The third, in
`WgpuContext::new_web_with_backend`, is behind `#[cfg(target_family = "wasm")]` and so was
invisible to every host-target gate, leaving `gpui_wgpu` unable to compile for
`wasm32-unknown-unknown`. The cross-target check found it and the call site now matches the other
two. The lesson generalizes the one above: a host-target `--all-targets` check still does not see a
`cfg`-gated call site, so a wgpu move has to be gated on the wasm32 target as well.

#### Adaptations wgpu PR #10109 required

Two further adaptations were required when the sibling checkout advanced past wgpu PR
[#10109](https://github.com/gfx-rs/wgpu/pull/10109), which added a required
`DeviceDescriptor::default_queue: QueueDescriptor` field:

- Three `request_device` call sites now pass `default_queue` — `gpui_wgpu::wgpu_context` (the real
  device), plus the `wgpu_atlas` and `external_registry` test devices. The field only labels the
  queue; no runtime behaviour, public API, or retained feature semantics change.
- `Cargo.lock` moved `js-sys` 0.3.103 → 0.3.104 with its `wasm-bindgen` chain (0.2.126 → 0.2.127,
  `wasm-bindgen-futures` 0.4.76 → 0.4.77). The sibling wgpu requires `js-sys` 0.3.104 while the
  lock pinned 0.3.103 through `chrono`, which made the workspace unresolvable.

The call-site adaptations above are ordinary standalone adaptations under `AGENTS.md`: each is a
signature change forced by the selected crate, plus the lockfile update required to build outside
the Zed workspace. None of them is itself a deliberate divergence — the divergence is the
selection, recorded in `SAPMALAR.md`. When the recorded Zed revision adopts the same wgpu line,
these call sites match upstream again, the `SAPMALAR.md` entry's drop condition is met, and both
records disappear together.

#### Validation tightenings checked against the retained shaders

The wgpu 30 line also tightens pipeline-creation validation. These are runtime rejections that a
host-target `cargo check` cannot see, so each was checked against the retained shaders rather than
assumed:

- Inter-stage numeric types must now match exactly rather than by subtyping
  ([#9999](https://github.com/gfx-rs/wgpu/pull/9999)). Every GPUI pipeline declares one struct
  (`QuadVarying`, `ShadowVarying`, `PathVarying`, and the rest) as both the vertex return type and
  the fragment parameter type, so the two sides are the same type by construction.
- A shader output must now exist for every color attachment with a non-zero write mask, and must
  include alpha when the blend operation reads source alpha
  ([#9939](https://github.com/gfx-rs/wgpu/pull/9939)). The external-surface pipeline pins
  premultiplied blend, which reads source alpha, and its fragment entry point returns
  `vec4<f32>`.
- `DownlevelFlags::LINEAR_INTERPOLATION` was added and is absent on GLES/WebGL2
  ([#9972](https://github.com/gfx-rs/wgpu/pull/9972)). No retained shader uses
  `@interpolate(linear)`; all four shader variants were scanned.
- Pipeline-layout `immediate_size` must now be at least the shader's requirement
  ([#9711](https://github.com/gfx-rs/wgpu/pull/9711)). Every retained pipeline layout declares
  `immediate_size: 0` and no retained shader uses immediates, so the rule holds.

`external_surface_draw_tests::the_s1_corpus_pixels_survive_the_real_draw_path` builds the external
pipeline on a real adapter and draws through it, which exercises the first two of these on the host
backend.

#### New capabilities evaluated

The current sibling tree was checked for capabilities that could remove or improve a recorded GPUI
limit (including descriptor validators and wasm64 support that predate the latest pull but had not
been evaluated here). None justifies an additional production change in this revision:

- `TextureDescriptor::theoretical_memory_footprint` returns the same `width × height × 4` for the
  bridge's fixed one-mip, one-sample `Bgra8Unorm`/`Rgba8Unorm` texture. Its own contract warns that
  actual allocation may be larger because of padding and alignment, so replacing the bridge's
  published logical-byte budget with it would add no coverage and could be misreported as VRAM.
- The separate device/texture descriptor validators are `wgpu-core` APIs, not portable `wgpu`
  facade APIs. Taking a new direct dependency would bypass the dispatch abstraction used by the
  native, WebGPU, and WebGL2 profiles, while the bridge's fixed descriptor is already bounded by
  the device extent limit before allocation. They therefore do not supply a common fail-closed
  layer for this repository.
- `wasm64-unknown-unknown` support does not remove a measured address-space limit here: the web
  profiles and their provisional external-surface budgets remain within wasm32. It is tracked as a
  future target, not enabled without a consumer case and matching browser/runtime evidence.
- The hal-level queue-family ownership transfer added for images imported from external memory
  ([#9668](https://github.com/gfx-rs/wgpu/pull/9668)) does not apply despite its name matching the
  external-surface bridge's subject. The bridge creates its surfaces with `Device::create_texture`
  on GPUI's own device and hands the producer a clone; it never imports external memory and never
  touches `wgpu_hal`, so there is no foreign queue family to acquire from or release to.

### Zed sync notes (20 August 2026)

This sync moved the retained source to Zed `cef06d351bec10d0fb6176018ce8624e97baeb40`. Besides the Apple
renderer split, this imports Zed's unified profiler and foreground-work journal, frame-time debug
overlay, spring and configurable-FPS animations, exact-size/binary SVG support, web streaming and
image/async-clipboard paths, wasm dedicated scheduler support, and the new line-layout split/paint
entry points.

The line APIs partially overlap the rich-text divergence but do not meet its drop condition. The
upstream layout still has one font size, no physical fallback-face identity, no affinity/direction
caret geometry, and its monotone `partition_point` split assumes logical glyph order. The public
upstream entry points were retained; the existing rich implementation now lives behind
`LineLayout::split_at`, preserves run metrics/faces and BiDi caret bounds, and
`ShapedLine::split_at` delegates to that common seam.

### Zed sync notes (24 August 2026)

The retained source now follows Zed `1b86941cf7298912af31b56f16990cf65b3ecbd3` (52 upstream
commits; 28 extraction-relevant files). This imports the demand-driven Wayland frame loop
(`PlatformWindow::completed_frame` becomes `schedule_frame`, with an explicit
Unconfigured/Ticking/RescheduleRequested/PresentationFailed/AwaitingCallback/Scheduled/
RetryScheduled/Parked state machine and a `calloop` ping wakeup), three X11 fixes (urgency-hint
clearing on activation, buffered-event drain after foreground work, client-state release before
the close callback), `BenchReport::foreground_work()` with the public `ForegroundWorkSummary`,
the Windows Restart Manager shutdown path (`Platform::on_quit` callbacks now return `bool`), and
the upstream cargo-machete→cargo-shear migration.

Sync adaptations, all dependency-closure work rather than behavior changes:

- Upstream's crate-level unused-dependency removals were verified against the retained sources
  (zero local uses) and taken: the `gpui` macOS Cocoa/Core*/Metal/pathfinder block plus `cbindgen`
  and the `gpui_web` wasm dev-dependency; `image`/`itertools`/`pathfinder_geometry`/`pollster`/
  `profiling`/`swash` from `gpui_linux`; `wasm-bindgen`/`wasm-bindgen-futures`/`js-sys` from
  `gpui_wgpu`; `ctor` from `media`. The root `cocoa-foundation` line was dropped with its last
  consumer; every other root line still has retained-crate users and stays.
- `pollster` left `gpui_wgpu`'s production dependencies with upstream, but the local
  external-surface registry tests (`external_registry.rs`) block on headless adapter/device
  acquisition, so it is retained as a non-wasm dev-dependency. Test-only adaptation, not a
  runtime divergence.
- `cargo-machete` package metadata became `cargo-shear` metadata in `gpui` (upstream's ignore
  list, including `ignored-paths` for `src/_ownership_and_data_flow.rs`) and `sum_tree`.
- `Cargo.lock` was regenerated in this workspace, not copied: the diff is exactly the 25 removed
  dependency-list entries above, with no version movement.
- The in-range upstream changes to `crates/zlog/src/filter.rs` and `crates/ztracing` were not
  taken; neither crate is extracted (`check-upstream.sh` lists those paths for observation only).

Verification boundary at this sync: host (aarch64-apple-darwin) format/metadata/check/clippy/serial
test gates and the recorded-divergence gates (38 portable + 13 CoreText + 30 cosmic-text rich-text
tests, `gpui_wgpu` 75-test suite with real Metal external-surface draws). Wasm/Windows/Linux
cross-target and real-platform runtime evidence for the new Wayland/X11/Restart Manager behavior is
tracked separately and was not produced by this sync.

### Sibling wgpu observation (24 August 2026)

| item | value at this sync |
|---|---|
| checkout | `../wgpu`, branch `trunk` |
| revision | `d4359d74946b9908c58eab9e70db061b2b8c8343` (24 August 2026) |
| version it declares | `30.0.0`, unreleased |
| version the recorded Zed revision selects | `29.0.4` from crates.io |

Nine upstream commits since the previous observation (`bbac60da…`), none with a `CHANGELOG.md`
entry yet:

- Queue error routing moved into core: `wgpu-core`'s `Queue::write_buffer`/`write_texture` no
  longer return `Result` and `submit` returns `SubmissionIndex` directly; errors flow through
  `Device::handle_error` into error scopes or the uncaptured-error handler. This breaks direct
  `wgpu-core` consumers only. The renderer here uses the public `wgpu::Queue` surface and its
  existing `on_uncaptured_error` handler plus per-frame failure counter, so no callsite changed.
- Metal HAL now panics (`unreachable!`) on composite alpha modes other than `Opaque`/
  `PreMultiplied`, relying on core to resolve `Inherit`. The renderer's preference order
  (`PreMultiplied`→`Inherit` transparent, `Opaque`→`Inherit` opaque) never hands HAL an
  unsupported mode on the observed adapters; the real-Metal external-surface draw tests pass.
- New `wgpu-core-remote(-types)` queue/encoder protocol surfaces: not taken; this repository has
  no remote-WebGPU consumer and the crates are not in the dependency closure.

### cargo-shear closure (24 August 2026)

Tool: cargo-shear 1.13.4 over the whole workspace. Every removal carries the dual evidence the
sync plan requires — the upstream tree only uses the dependency in sources this extraction
excludes, and the retained sources have zero uses:

- `gpui` dev-dependencies `env_logger`, `unicode-segmentation`, and the wasm dev-dependency
  `wasm-bindgen`: upstream keeps all three solely for its `crates/gpui/examples` tree, which this
  extraction excludes. Removed, along with the now-empty wasm dev-dependency target section.
- `http_client` `async-compression`: upstream uses it only in the `github-download` integration
  this extraction already removed. Removed.
- Root workspace lines `async-compression`, `env_logger`, and `gpui_tokio`: no retained member
  consumes them. `gpui_tokio` remains a workspace member and a consumer-usable crate; only the
  unused root dependency alias was dropped.

Kept deliberately: the `tracing` entries in the `gpui` and `sum_tree` cargo-shear ignored lists
are redundant here — the ztracing→tracing extraction swap makes `tracing` directly used — but
they are upstream's own metadata and cargo-shear reports them as warnings, not errors, so they
stay for parity. The `block 0.1.6` future-incompat warning also stays dependency-closure-owned:
`block` is still consumed by `gpui_apple` and `gpui_macos`.

Lock impact: 196 lines removed; the `async-compression`/`bzip2` compression family and the
`env_logger` family left the graph entirely, with no version movement elsewhere.

## Workspace reconstruction

The root manifest uses resolver 2 and contains only extracted members plus the hello-world package.
Workspace dependency declarations were copied from the upstream manifest with their exact version,
feature, git revision, and target settings. Internal dependencies are local paths. Only the
`async-task` and `calloop` upstream patches needed by this closure were retained. Zed application
profiles and release metadata were omitted because they do not affect GPUI correctness. The
standalone workspace does retain an explicit `profile.bench` equivalent to the recorded Zed
workspace's optimized release settings (ThinLTO, one codegen unit); without it, cross-checkout
`cargo bench` results measure different code-generation profiles rather than just the retained GPUI
implementation.

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

It deliberately runs the `gpui` text-system tests, the macOS `text_system::tests` module with the
otherwise non-default `font-kit` feature, and the WGPU `cosmic_text_system::tests` module. The
backend filters keep unrelated environment-dependent pasteboard and GPU-adapter tests out of this
divergence gate. A plain `cargo test -p gpui_macos` does not compile the feature-gated text-system
evidence and must not be reported as proof for this divergence. On a non-macOS host the script runs
the portable and WGPU evidence but prints an explicit partial-result warning instead of presenting
the skipped CoreText suite as a complete verification.

The retained `gpui_wgpu` Criterion target measures the three recorded upstream legacy corpora plus
a short UI-sized line, homogeneous and heterogeneous rich layouts, 64-run metric and baseline-only
stress cases, and the steady-state physical-face cache. Its bundled fonts intentionally keep results reproducible;
Arabic, Hebrew, and emoji throughput depends on system fallback fonts and is compared with
cosmic-text's own upstream benchmark suite rather than represented by missing-glyph measurements.
The rich ASCII corpus also guards the WGPU monotone-cluster fast path: ASCII/LTR caret geometry and
rich-run lookup are linear passes, while unexpected cluster order and BiDi retain the general
grouping, sorting, and binary-search fallbacks.

The macOS `gpui_macos` Criterion target separately measures the unchanged legacy CoreText route,
one homogeneous rich run, and 64 baseline-only runs on the same deterministic approximately 3.8 KB
ASCII corpus. It exposed the former per-UTF-16-boundary CoreText call pattern and records the
single-enumeration replacement independently of WGPU results. DirectWrite uses its native
per-range attributes and glyph-run callbacks rather than either Apple or cosmic-text internals.
It was type-checked for `x86_64-pc-windows-msvc` in this work, with narrow build-tool shims because
the macOS host has neither the Windows SDK nor `lib.exe`; that is Rust/API compile evidence only,
not a Windows runtime, caret, or throughput result.

The same change was also checked through the WGPU crate for `x86_64-unknown-linux-musl`, through
`gpui_web --no-default-features`, and through the multithreaded Web configuration using Zed's
atomics/build-std CI flags. These cross-target checks establish target-specific compilation; only
the host macOS CoreText suite and the platform-independent cosmic-text suite are runtime evidence.

The initial extraction was validated on macOS (`aarch64-apple-darwin`). The 2026-07-28, 2026-07-30,
and 2026-08-01 upstream syncs were validated on x86_64 Linux with formatting, locked workspace
checks, the `input-latency-histogram` feature, the `gpui_wgpu` benchmark target, and the full
workspace test suite. The 2026-08-05 and 2026-08-08 syncs were validated on macOS with formatting,
a locked all-targets workspace check, and the full workspace test suite run serially; the serial
run avoids the process-global pasteboard state shared by otherwise parallel macOS pasteboard
tests. The 2026-08-12 sync used the same macOS formatting, locked all-targets workspace check, and
serial full-workspace test gates, plus `scripts/verify-sapmalar.sh`; cross-target and GUI/browser
runtime checks were not rerun for that sync. The 2026-08-20 sync to
`cef06d351bec10d0fb6176018ce8624e97baeb40` passed formatting, locked metadata and all-targets
workspace checks, the serial full-workspace test suite (including the formerly environment-sensitive
macOS pasteboard tests), and `scripts/verify-sapmalar.sh`. The focused divergence evidence was
38 portable text tests, 13 CoreText tests, 30 cosmic-text tests, 30 `gpui_apple` Metal
registry/draw tests, and 21 wgpu registry tests. It also passed a locked workspace clippy run over
all targets. Browser runtime, release-build, and throughput measurements were not rerun for this
sync. The `wasm32-unknown-unknown` cross-target check *was* rerun for this sync, with Zed's own CI
invocation — `-Zbuild-std=std,panic_abort` under `RUSTC_BOOTSTRAP=1` with
`-C target-feature=+atomics,+bulk-memory,+mutable-globals` — over `gpui_wgpu` and `gpui_web`; that
is the run that found the `cfg`-gated `apply_limit_buckets` break recorded above, and it passes
now. Both web feature configurations were re-checked for this sync too, with `cargo clippy
--all-targets` under the same invocation with and without `--no-default-features`; that pair is
what the scoped `redundant_clone` allow above is gated on. FreeBSD and Windows were not cross-compiled during any of these syncs. GUI and browser
launch were not used as automated assertions; the hello-world binary was compile-checked.

The 2026-08-24 sync to `1b86941cf7298912af31b56f16990cf65b3ecbd3` (with the sibling wgpu observed
at `d4359d74946b9908c58eab9e70db061b2b8c8343`) passed `check-upstream.sh`, formatting, locked
metadata, locked all-targets workspace check and clippy (`-D warnings`), and the serial
full-workspace test suite: 499 tests, 0 failures, including the `gpui_wgpu` 75-test suite with its
real Metal external-surface draw tests. The focused divergence evidence was 38 portable, 13
CoreText, and 30 cosmic-text text tests via `scripts/verify-sapmalar.sh`. A pre-sync baseline run
of the same host gates on the previous source (against the same sibling wgpu revision) also passed,
so post-sync results are attributable to the sync alone. Wasm and Windows cross-target checks and
all real-platform runtime evidence (Wayland demand-driven idle behavior, X11 fixes, Windows Restart
Manager) were not rerun for this sync and are tracked as follow-up work; `cargo fmt --all --
--check` flags only the pristine upstream `../wgpu/deno_webgpu/surface.rs` import order, which is
owned upstream and was not locally reformatted.

The same-day hygiene pass (wasm bench isolation and the cargo-shear closure above) was validated
with the full host gate set — formatting, locked metadata, locked all-targets check and clippy
(`-D warnings`), the serial full-workspace suite (499 tests, 0 failures), and the 38/13/30
divergence gates — plus the wasm32 checks: the atomics/build-std `--all-targets` scope over
`gpui_wgpu` and `gpui_web` now passes, both `--lib` feature paths pass, and the host bench runs
under Criterion's `--test` mode.

The runtime matrix was then executed as far as this host allows (same day). Fingerprint for every
result: GPUI at the commit carrying this note, Zed `1b86941cf7…`, sibling wgpu `d4359d749…`,
Rust 1.97.1, macOS 26.6.2 (build 25G83), Apple M4 Pro, Metal.

- **macOS Metal — measured.** A real `gpui-hello-world` window (`gpui_platform` → `gpui_apple`
  native Metal renderer) opened, stayed alive, and shut down cleanly on SIGTERM. Idle cadence:
  0.24 cumulative CPU-seconds over a 20-second idle window (~1.2% of one core, per-sample
  0.0–3.6%), which rules out a continuous-render loop for an idle window. Device-level runtime:
  `gpui_apple` 30/30 real Metal atlas/registry/renderer tests and `gpui_wgpu` 87/87, including
  the real-Metal external-surface pixel corpus (draw/crop/clip/alpha/stale-generation) and the
  CAMetalLayer capability fingerprint. Boundaries stated rather than glossed: there is no
  windowed transparent-alpha fixture, so transparent selection is proven at capability/device
  level only, and no screenshot assertion was taken — liveness, cadence, and the offscreen pixel
  corpus are the window evidence.
- **Windows D3D11 — not measured.** No Windows session is reachable from this host, and the
  `x86_64-pc-windows-msvc` cross-check still stops environmentally before reaching GPUI sources:
  the `psm`/`stacker` build scripts require the MSVC archiver (`lib.exe`), which this host does
  not have. Restart Manager shutdown, device-lost/registry generation, external draw, and a real
  driver fingerprint remain unproven.
- **Linux wgpu-Vulkan / wgpu-GL — compile-proven, runtime not measured.** The host workspace
  check never touches `gpui_linux` (the crate is `#![cfg(linux/freebsd)]`), so the synced
  Wayland/X11 sources were additionally cross-checked for `x86_64-unknown-linux-musl`:
  `--no-default-features --features wayland` and `--features x11` both pass, which is the compile
  evidence for the demand-driven Wayland state machine and the three X11 fixes. The combined
  default-feature check stops environmentally — the `psm`, `yeslogic-fontconfig-sys`, and
  `freetype-sys` build scripts in that graph need a musl C/C++ cross toolchain
  (`x86_64-linux-musl-g++`) this host lacks — which is a toolchain gap, not a source error.
  Runtime remains unmeasured: no Linux host and no container runtime (neither docker nor podman
  is installed), so Wayland demand-driven idle/fullscreen/retry and the three X11 cases (urgency
  clear, post-foreground map, close-callback reentrancy) are covered only by the host-side
  `TestWindow` frame-protocol tests, which simulate the platform half and are not compositor
  evidence.
- **Browser WebGPU / WebGL2 — not measured.** The repository has no web runtime harness (no HTML
  shim or wasm-bindgen packaging); wasm evidence remains compile-only. Building that harness is
  tracked as follow-up work.

One flake was observed and is recorded rather than hidden:
`gpui`'s `test_spring_animation_preserves_velocity_when_retargeted` failed twice on 24 August —
once in each batch gate run's serial full-workspace suite — and passed every immediate serial
rerun and five isolated runs; nothing in these passes touches animation code, so the
nondeterminism predates this work. It was subsequently root-caused — the spring element steps by
the real monotonic clock, not the virtual test clock, contrary to the assumption first recorded
here — and resolved by the test-only adaptation "Deterministic frame pumping in two
spring-element tests" above.

The upstream `gpui_web` default enables `multithreaded`, which requires atomics and the
`wasm_thread` nightly-only `stdarch_wasm_atomic_wait` feature, so that configuration cannot be
built on a stable toolchain without bootstrap. The `--no-default-features` compile failure recorded
at the previous sync was fixed upstream and no longer applies. Per the parity policy, this
repository does not carry the former local single-thread workaround; any remaining limitation must
be fixed in Zed first.
