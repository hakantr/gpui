use crate::external_registry::{ExternalSurfaceProducer, ExternalSurfaceRegistry, external_format};
use crate::{CompositorGpuHint, WgpuAtlas, WgpuContext};
use anyhow::{Context as _, Result};
use bytemuck::{Pod, Zeroable};
use gpui::{
    AtlasTextureId, Background, Bounds, DevicePixels, ExternalSurfaceCapabilities,
    ExternalSurfaceFormat, GpuSpecs, PaintSurface, Path, Point, PrimitiveBatch, ScaledPixels,
    Scene, Size, SurfaceSource, get_gamma_correction_ratios,
};
use log::warn;
#[cfg(not(target_family = "wasm"))]
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::cell::RefCell;
use std::num::NonZeroU64;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const MAX_INSTANCE_BUFFER_SIZE: u64 = 256 * 1024 * 1024;

const INSTANCE_TEXTURE_TEXEL_SIZE: u64 = 16;

/// Shader variant for backends with storage buffer support: the shared shader
/// logic plus the storage-buffer instance transport.
const STORAGE_BUFFER_SHADERS: &str = concat!(
    include_str!("shaders.wgsl"),
    include_str!("shaders_storage.wgsl"),
);

/// Shader variant for WebGL2, which has no storage buffers: the shared shader
/// logic plus the texture-based instance transport.
const WEBGL_SHADERS: &str = concat!(
    include_str!("shaders.wgsl"),
    include_str!("shaders_webgl.wgsl"),
);

/// Subpixel text rendering requires dual-source blending, which WebGL2 lacks, so
/// this variant only ever runs with the storage-buffer transport. The `enable`
/// directive must precede all declarations.
const SUBPIXEL_SHADERS: &str = concat!(
    "enable dual_source_blending;\n",
    include_str!("shaders.wgsl"),
    include_str!("shaders_storage.wgsl"),
    include_str!("shaders_subpixel.wgsl"),
);

fn least_common_multiple(left: u64, right: u64) -> u64 {
    let mut first = left;
    let mut second = right;
    while second != 0 {
        let remainder = first % second;
        first = second;
        second = remainder;
    }
    left / first * right
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlobalParams {
    viewport_size: [f32; 2],
    premultiplied_alpha: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PodBounds {
    origin: [f32; 2],
    size: [f32; 2],
}

impl From<Bounds<ScaledPixels>> for PodBounds {
    fn from(bounds: Bounds<ScaledPixels>) -> Self {
        Self {
            origin: [bounds.origin.x.0, bounds.origin.y.0],
            size: [bounds.size.width.0, bounds.size.height.0],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceParams {
    bounds: PodBounds,
    content_mask: PodBounds,
}

/// One external surface, as the vertex shader reads it.
///
/// The field order and the trailing pad word mirror `ExternalSurface` in `shaders.wgsl` exactly,
/// and they mirror the D3D11 backend's `ExternalSurfaceInstance` too, so the three sides of the
/// bridge describe one struct. No vector member straddles a 16-byte boundary and the affine is
/// carried as `TransformationMatrix`'s two row-major rows plus a translation rather than as a
/// matrix type, so the WGSL uniform layout and this one cannot disagree about matrix stride — the
/// uniform address space packs a `mat2x2<f32>` with a 16-byte column stride, which a plain
/// `[[f32; 2]; 2]` would not match.
///
/// The content mask is absent on purpose: the clip is a scissor rectangle, not a shader input.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ExternalSurfaceInstance {
    /// The target placement.
    bounds: PodBounds,
    /// The crop, normalized against the registered surface size. The whole surface is `0..1`.
    source_uv: PodBounds,
    /// The affine's first row, applied about the top-left corner of `bounds`.
    rotation_scale_row0: [f32; 2],
    /// The affine's second row.
    rotation_scale_row1: [f32; 2],
    /// The affine's translation.
    translation: [f32; 2],
    /// The group opacity, of which this composite is the sole owner.
    opacity: f32,
    _pad: u32,
}

const EXTERNAL_SURFACE_INSTANCE_SIZE: u64 = std::mem::size_of::<ExternalSurfaceInstance>() as u64;

const _: () = assert!(EXTERNAL_SURFACE_INSTANCE_SIZE == 64);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GammaParams {
    gamma_ratios: [f32; 4],
    grayscale_enhanced_contrast: f32,
    subpixel_enhanced_contrast: f32,
    is_bgr: u32,
    _pad: u32,
}

#[derive(Clone, Debug)]
#[repr(C)]
struct PathSprite {
    bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug)]
#[repr(C)]
struct PathRasterizationVertex {
    xy_position: Point<ScaledPixels>,
    st_position: Point<f32>,
    color: Background,
    bounds: Bounds<ScaledPixels>,
}

pub struct WgpuSurfaceConfig {
    pub size: Size<DevicePixels>,
    pub transparent: bool,
    /// Preferred presentation mode. When `Some`, the renderer will use this
    /// mode if supported by the surface, falling back to `Fifo`.
    /// When `None`, defaults to `Fifo` (VSync).
    ///
    /// Mobile platforms may prefer `Mailbox` (triple-buffering) to avoid
    /// blocking in `get_current_texture()` during lifecycle transitions.
    pub preferred_present_mode: Option<wgpu::PresentMode>,
}

struct WgpuPipelines {
    quads: wgpu::RenderPipeline,
    shadows: wgpu::RenderPipeline,
    path_rasterization: wgpu::RenderPipeline,
    paths: wgpu::RenderPipeline,
    underlines: wgpu::RenderPipeline,
    mono_sprites: wgpu::RenderPipeline,
    subpixel_sprites: Option<wgpu::RenderPipeline>,
    poly_sprites: wgpu::RenderPipeline,
    /// The macOS NV12 video path, which this renderer does not drive; the bounded
    /// external-surface bridge is [`WgpuPipelines::external_surfaces`] instead.
    #[allow(dead_code)]
    surfaces: wgpu::RenderPipeline,
    external_surfaces: ExternalSurfacePipeline,
}

/// The external-surface pipeline and the state it needs that no other pipeline here does.
struct ExternalSurfacePipeline {
    pipeline: wgpu::RenderPipeline,
    /// The layout of the single-instance uniform bind group.
    uniform_layout: wgpu::BindGroupLayout,
    /// One uniform buffer and bind group per surface drawn in a frame, kept across frames and
    /// grown on demand.
    ///
    /// One buffer *per surface* rather than one for the batch: a `Queue::write_buffer` is staged
    /// and replayed before the frame's commands, so writing one buffer twice in a frame would give
    /// both draws the second surface's placement. External surfaces are few — one per special
    /// group — so the pool is short and stops growing after the first frame that uses it.
    slots: Vec<ExternalSurfaceSlot>,
}

/// One surface's per-draw uniform buffer and the bind group over it.
struct ExternalSurfaceSlot {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl ExternalSurfacePipeline {
    /// Builds the pipeline that composites an external surface into the frame.
    ///
    /// `texture_layout` is the registry's own sampling layout, so the bind groups it builds at
    /// registration and this pipeline can never drift apart. The blend is **premultiplied**
    /// (`One` / `OneMinusSrcAlpha` on colour *and* alpha, D-K13) and it is fixed rather than
    /// following the window's alpha mode: the contract's only valid alpha mode is premultiplied,
    /// and a straight-alpha blend would premultiply the content a second time.
    fn new(
        device: &wgpu::Device,
        module: &wgpu::ShaderModule,
        globals_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("external_surface_uniform_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The vertex stage reads the placement and the fragment stage reads the opacity,
                // so the one instance is visible to both.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(EXTERNAL_SURFACE_INSTANCE_SIZE),
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("external_surfaces_layout"),
            bind_group_layouts: &[
                Some(globals_layout),
                Some(&uniform_layout),
                Some(texture_layout),
            ],
            immediate_size: 0,
        });

        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("external_surfaces"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_external_surface"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_external_surface"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // No culling, which is what makes a negative-determinant affine legal: mirroring
                // is a legitimate transform under the contract, not a reason to drop the quad.
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            uniform_layout,
            slots: Vec::new(),
        }
    }

    /// Makes sure the pool holds at least `count` slots.
    fn reserve(&mut self, device: &wgpu::Device, count: usize) {
        while self.slots.len() < count {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("external_surface_uniform_buffer"),
                size: EXTERNAL_SURFACE_INSTANCE_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("external_surface_uniform_bind_group"),
                layout: &self.uniform_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            self.slots.push(ExternalSurfaceSlot { buffer, bind_group });
        }
    }
}

/// One frame allocation of instance data, ready to bind.
struct InstanceBinding {
    bind_group: wgpu::BindGroup,
    /// Index of the allocation's first instance within the bound data. Always
    /// zero on the storage-buffer path, where the binding offset already
    /// positions the array; on the WebGL texture path the shader indexes the
    /// shared instance texture absolutely, so draws must offset their
    /// instance (or vertex) ranges by this value.
    first_instance: u32,
}

struct InstanceBindings {
    quads: InstanceBinding,
    shadows: InstanceBinding,
    underlines: InstanceBinding,
    monochrome_sprites: InstanceBinding,
    subpixel_sprites: InstanceBinding,
    polychrome_sprites: InstanceBinding,
}

struct WgpuBindGroupLayouts {
    globals: wgpu::BindGroupLayout,
    instances: wgpu::BindGroupLayout,
    texture: wgpu::BindGroupLayout,
    surfaces: wgpu::BindGroupLayout,
}

/// Shared GPU context reference, used to coordinate device recovery across multiple windows.
pub type GpuContext = Rc<RefCell<Option<WgpuContext>>>;

enum InstanceData {
    Storage(wgpu::Buffer),
    // WebGL2 has no storage buffers. A uint texture keeps the records available to both shader
    // stages while preserving integer and floating-point bit patterns exactly.
    Texture {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        width: u32,
        height: u32,
    },
}

/// GPU resources that must be dropped together during device recovery.
struct WgpuResources {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    pipelines: WgpuPipelines,
    bind_group_layouts: WgpuBindGroupLayouts,
    atlas_sampler: wgpu::Sampler,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    path_globals_bind_group: wgpu::BindGroup,
    instance_data: InstanceData,
    path_intermediate_texture: Option<wgpu::Texture>,
    path_intermediate_view: Option<wgpu::TextureView>,
    path_msaa_texture: Option<wgpu::Texture>,
    path_msaa_view: Option<wgpu::TextureView>,
}

impl WgpuResources {
    fn invalidate_intermediate_textures(&mut self) {
        self.path_intermediate_texture = None;
        self.path_intermediate_view = None;
        self.path_msaa_texture = None;
        self.path_msaa_view = None;
    }
}

pub struct WgpuRenderer {
    /// Shared GPU context for device recovery coordination (unused on WASM).
    #[allow(dead_code)]
    context: Option<GpuContext>,
    /// Compositor GPU hint for adapter selection (unused on WASM).
    #[allow(dead_code)]
    compositor_gpu: Option<CompositorGpuHint>,
    resources: Option<WgpuResources>,
    surface_config: wgpu::SurfaceConfiguration,
    atlas: Arc<WgpuAtlas>,
    /// The external-surface bridge's resource storage.
    ///
    /// It is shared rather than owned so that the producer face
    /// ([`WgpuRenderer::external_surface_producer`]) holds the other end: the renderer resolves
    /// handles against it while drawing, and the external compositor registers and retires through
    /// it. It also survives a device-lost recovery, which is what lets a producer acquired before
    /// the loss observe the raised generation instead of registering onto a dead device.
    external_registry: Rc<RefCell<ExternalSurfaceRegistry>>,
    path_globals_offset: u64,
    gamma_offset: u64,
    instance_data_capacity: u64,
    max_instance_data_size: u64,
    instance_data_alignment: u64,
    uses_webgl_instance_data: bool,
    rendering_params: RenderingParameters,
    is_bgr: bool,
    dual_source_blending: bool,
    adapter_info: wgpu::AdapterInfo,
    transparent_alpha_mode: wgpu::CompositeAlphaMode,
    opaque_alpha_mode: wgpu::CompositeAlphaMode,
    max_texture_size: u32,
    last_error: Arc<Mutex<Option<String>>>,
    failed_frame_count: u32,
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    surface_configured: bool,
    needs_redraw: bool,
}

impl WgpuRenderer {
    fn resources(&self) -> &WgpuResources {
        self.resources
            .as_ref()
            .expect("GPU resources not available")
    }

    fn resources_mut(&mut self) -> &mut WgpuResources {
        self.resources
            .as_mut()
            .expect("GPU resources not available")
    }

    /// Creates a new WgpuRenderer from raw window handles.
    ///
    /// The `gpu_context` is a shared reference that coordinates GPU context across
    /// multiple windows. The first window to create a renderer will initialize the
    /// context; subsequent windows will share it.
    ///
    /// # Safety
    /// The caller must ensure that the window handle remains valid for the lifetime
    /// of the returned renderer.
    #[cfg(not(target_family = "wasm"))]
    pub fn new<W>(
        gpu_context: GpuContext,
        window: &W,
        config: WgpuSurfaceConfig,
        compositor_gpu: Option<CompositorGpuHint>,
    ) -> anyhow::Result<Self>
    where
        W: HasWindowHandle + HasDisplayHandle + std::fmt::Debug + Send + Sync + Clone + 'static,
    {
        let window_handle = window
            .window_handle()
            .map_err(|e| anyhow::anyhow!("Failed to get window handle: {e}"))?;

        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            // Fall back to the display handle already provided via InstanceDescriptor::display.
            raw_display_handle: None,
            raw_window_handle: window_handle.as_raw(),
        };

        // Use the existing context's instance if available, otherwise create a new one.
        // The surface must be created with the same instance that will be used for
        // adapter selection, otherwise wgpu will panic.
        let instance = gpu_context
            .borrow()
            .as_ref()
            .map(|ctx| ctx.instance.clone())
            .unwrap_or_else(|| WgpuContext::instance(Box::new(window.clone())));

        // Safety: The caller guarantees that the window handle is valid for the
        // lifetime of this renderer. In practice, the RawWindow struct is created
        // from the native window handles and the surface is dropped before the window.
        let surface = unsafe {
            instance
                .create_surface_unsafe(target)
                .map_err(|e| anyhow::anyhow!("Failed to create surface: {e}"))?
        };

        let mut ctx_ref = gpu_context.borrow_mut();
        let context = match ctx_ref.as_mut() {
            Some(context) => {
                context.check_compatible_with_surface(&surface)?;
                context
            }
            None => ctx_ref.insert(WgpuContext::new(instance, &surface, compositor_gpu)?),
        };

        let atlas = Arc::new(WgpuAtlas::from_context(context));

        Self::new_internal(
            Some(Rc::clone(&gpu_context)),
            context,
            surface,
            config,
            compositor_gpu,
            atlas,
            None,
        )
    }

    #[cfg(target_family = "wasm")]
    pub fn new_from_canvas(
        context: &WgpuContext,
        canvas: &web_sys::HtmlCanvasElement,
        config: WgpuSurfaceConfig,
    ) -> anyhow::Result<Self> {
        let surface = context
            .instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| anyhow::anyhow!("Failed to create surface: {e}"))?;
        Self::new_from_surface(context, surface, config)
    }

    #[cfg(target_family = "wasm")]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new_from_surface(
        context: &WgpuContext,
        surface: wgpu::Surface<'static>,
        config: WgpuSurfaceConfig,
    ) -> anyhow::Result<Self> {
        let atlas = Arc::new(WgpuAtlas::from_context(context));
        Self::new_internal(None, context, surface, config, None, atlas, None)
    }

    /// Builds a renderer on `context`.
    ///
    /// `external_registry` is `None` for a fresh renderer and `Some` only on the device-lost
    /// recovery path, where the registry object has to outlive the device it was built on so that
    /// a producer acquired before the loss sees the raised generation. See [`Self::recover`].
    fn new_internal(
        gpu_context: Option<GpuContext>,
        context: &WgpuContext,
        surface: wgpu::Surface<'static>,
        config: WgpuSurfaceConfig,
        compositor_gpu: Option<CompositorGpuHint>,
        atlas: Arc<WgpuAtlas>,
        external_registry: Option<Rc<RefCell<ExternalSurfaceRegistry>>>,
    ) -> anyhow::Result<Self> {
        let surface_caps = surface.get_capabilities(&context.adapter);
        let preferred_formats = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ];
        let surface_format = preferred_formats
            .iter()
            .find(|f| surface_caps.formats.contains(f))
            .copied()
            .or_else(|| {
                surface_caps
                    .formats
                    .iter()
                    .find(|f| !f.has_srgb_suffix())
                    .copied()
            })
            .or_else(|| surface_caps.formats.first().copied())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Surface reports no supported texture formats for adapter {:?}",
                    context.adapter.get_info().name
                )
            })?;

        let pick_alpha_mode =
            |preferences: &[wgpu::CompositeAlphaMode]| -> anyhow::Result<wgpu::CompositeAlphaMode> {
                preferences
                    .iter()
                    .find(|p| surface_caps.alpha_modes.contains(p))
                    .copied()
                    .or_else(|| surface_caps.alpha_modes.first().copied())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Surface reports no supported alpha modes for adapter {:?}",
                            context.adapter.get_info().name
                        )
                    })
            };

        let transparent_alpha_mode = pick_alpha_mode(&[
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ])?;

        let opaque_alpha_mode = pick_alpha_mode(&[
            wgpu::CompositeAlphaMode::Opaque,
            wgpu::CompositeAlphaMode::Inherit,
        ])?;

        let alpha_mode = if config.transparent {
            transparent_alpha_mode
        } else {
            opaque_alpha_mode
        };

        let device = Arc::clone(&context.device);
        let max_texture_size = device.limits().max_texture_dimension_2d;

        let requested_width = config.size.width.0 as u32;
        let requested_height = config.size.height.0 as u32;
        let clamped_width = requested_width.min(max_texture_size);
        let clamped_height = requested_height.min(max_texture_size);

        if clamped_width != requested_width || clamped_height != requested_height {
            warn!(
                "Requested surface size ({}, {}) exceeds maximum texture dimension {}. \
                 Clamping to ({}, {}). Window content may not fill the entire window.",
                requested_width, requested_height, max_texture_size, clamped_width, clamped_height
            );
        }

        let surface_config = wgpu::SurfaceConfiguration {
            color_space: wgpu::SurfaceColorSpace::Auto,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: clamped_width.max(1),
            height: clamped_height.max(1),
            present_mode: config
                .preferred_present_mode
                .filter(|mode| surface_caps.present_modes.contains(mode))
                .unwrap_or(wgpu::PresentMode::Fifo),
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        // Configure the surface immediately. The adapter selection process already validated
        // that this adapter can successfully configure this surface.
        surface.configure(&context.device, &surface_config);

        let queue = Arc::clone(&context.queue);
        let rendering_params = RenderingParameters::new(&context.adapter, surface_format);
        let uses_webgl_instance_data = context.uses_webgl_instance_data();
        let dual_source_blending =
            context.supports_dual_source_blending() && !uses_webgl_instance_data;
        let bind_group_layouts = Self::create_bind_group_layouts(&device, uses_webgl_instance_data);
        let external_registry = external_registry.unwrap_or_else(|| {
            Rc::new(RefCell::new(ExternalSurfaceRegistry::new(
                Arc::clone(&device),
                Self::select_external_surface_format(context),
                uses_webgl_instance_data,
            )))
        });
        let pipelines = Self::create_pipelines(
            &device,
            &bind_group_layouts,
            external_registry.borrow().texture_bind_group_layout(),
            surface_format,
            alpha_mode,
            rendering_params.path_sample_count,
            dual_source_blending,
            uses_webgl_instance_data,
        );

        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let globals_size = std::mem::size_of::<GlobalParams>() as u64;
        let gamma_size = std::mem::size_of::<GammaParams>() as u64;
        let path_globals_offset = globals_size.next_multiple_of(uniform_alignment);
        let gamma_offset = (path_globals_offset + globals_size).next_multiple_of(uniform_alignment);

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals_buffer"),
            size: gamma_offset + gamma_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (
            instance_data,
            instance_data_capacity,
            max_instance_data_size,
            instance_data_alignment,
        ) = if uses_webgl_instance_data {
            let max_texture_dimension = device.limits().max_texture_dimension_2d;
            let max_instance_data_size = (u64::from(max_texture_dimension).pow(2)
                * INSTANCE_TEXTURE_TEXEL_SIZE)
                .min(MAX_INSTANCE_BUFFER_SIZE);
            let initial_capacity = (2 * 1024 * 1024).min(max_instance_data_size);
            let (instance_data, capacity) =
                Self::create_instance_texture(&device, initial_capacity, max_texture_dimension);
            (
                instance_data,
                capacity,
                max_instance_data_size,
                INSTANCE_TEXTURE_TEXEL_SIZE,
            )
        } else {
            // Every frame allocation is exposed as one storage-buffer binding, so
            // its backing buffer must satisfy both the allocation and binding limits.
            let max_buffer_size = device
                .limits()
                .max_buffer_size
                .min(device.limits().max_storage_buffer_binding_size)
                .min(MAX_INSTANCE_BUFFER_SIZE);
            let initial_capacity = (2 * 1024 * 1024).min(max_buffer_size);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instance_buffer"),
                size: initial_capacity,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (
                InstanceData::Storage(buffer),
                initial_capacity,
                max_buffer_size,
                device.limits().min_storage_buffer_offset_alignment as u64,
            )
        };

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals_bind_group"),
            layout: &bind_group_layouts.globals,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &globals_buffer,
                        offset: 0,
                        size: Some(NonZeroU64::new(globals_size).unwrap()),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &globals_buffer,
                        offset: gamma_offset,
                        size: Some(NonZeroU64::new(gamma_size).unwrap()),
                    }),
                },
            ],
        });

        let path_globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("path_globals_bind_group"),
            layout: &bind_group_layouts.globals,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &globals_buffer,
                        offset: path_globals_offset,
                        size: Some(NonZeroU64::new(globals_size).unwrap()),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &globals_buffer,
                        offset: gamma_offset,
                        size: Some(NonZeroU64::new(gamma_size).unwrap()),
                    }),
                },
            ],
        });

        let adapter_info = context.adapter.get_info();

        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let last_error_clone = Arc::clone(&last_error);
        device.on_uncaptured_error(Arc::new(move |error| {
            let mut guard = last_error_clone.lock().unwrap();
            *guard = Some(error.to_string());
        }));

        let resources = WgpuResources {
            device,
            queue,
            surface,
            pipelines,
            bind_group_layouts,
            atlas_sampler,
            globals_buffer,
            globals_bind_group,
            path_globals_bind_group,
            instance_data,
            // Defer intermediate texture creation to first draw call via ensure_intermediate_textures().
            // This avoids panics when the device/surface is in an invalid state during initialization.
            path_intermediate_texture: None,
            path_intermediate_view: None,
            path_msaa_texture: None,
            path_msaa_view: None,
        };

        Ok(Self {
            context: gpu_context,
            compositor_gpu,
            resources: Some(resources),
            surface_config,
            atlas,
            external_registry,
            path_globals_offset,
            gamma_offset,
            instance_data_capacity,
            max_instance_data_size,
            instance_data_alignment,
            uses_webgl_instance_data,
            rendering_params,
            is_bgr: false,
            dual_source_blending,
            adapter_info,
            transparent_alpha_mode,
            opaque_alpha_mode,
            max_texture_size,
            last_error,
            failed_frame_count: 0,
            device_lost: context.device_lost_flag(),
            surface_configured: true,
            needs_redraw: false,
        })
    }

    fn create_bind_group_layouts(
        device: &wgpu::Device,
        uses_webgl_instance_data: bool,
    ) -> WgpuBindGroupLayouts {
        let globals =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("globals_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<GlobalParams>() as u64
                            ),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<GammaParams>() as u64
                            ),
                        },
                        count: None,
                    },
                ],
            });

        let instance_data_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: if uses_webgl_instance_data {
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }
            } else {
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                }
            },
            count: None,
        };

        let instances = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("instances_layout"),
            entries: &[instance_data_entry],
        });

        let texture = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let surfaces = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surfaces_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(
                            std::mem::size_of::<SurfaceParams>() as u64
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        WgpuBindGroupLayouts {
            globals,
            instances,
            texture,
            surfaces,
        }
    }

    fn create_instance_texture(
        device: &wgpu::Device,
        requested_capacity: u64,
        max_texture_dimension: u32,
    ) -> (InstanceData, u64) {
        let texel_count = requested_capacity.div_ceil(INSTANCE_TEXTURE_TEXEL_SIZE);
        let width = texel_count.min(u64::from(max_texture_dimension)).max(1) as u32;
        let height = texel_count
            .div_ceil(u64::from(width))
            .min(u64::from(max_texture_dimension))
            .max(1) as u32;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("instance_texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let capacity = u64::from(width) * u64::from(height) * INSTANCE_TEXTURE_TEXEL_SIZE;
        (
            InstanceData::Texture {
                texture,
                view,
                width,
                height,
            },
            capacity,
        )
    }

    /// The byte order external surfaces are registered in.
    ///
    /// It is the one the context already settled on for its colour textures
    /// (`WgpuContext::select_color_texture_format`): `Bgra8Unorm` first, and `Rgba8Unorm` on
    /// wasm+GL, which is exactly the preference and the fallback the contract names (§6). The
    /// difference from the atlas is the usage an external surface needs — it is a
    /// `RENDER_ATTACHMENT` the producer draws into, not a `COPY_DST` GPUI uploads to — so the
    /// choice is re-checked against that usage and falls back rather than failing the window: an
    /// adapter that cannot render to BGRA still gets the bridge, in the fallback byte order.
    fn select_external_surface_format(context: &WgpuContext) -> ExternalSurfaceFormat {
        let required =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let usable = |format: wgpu::TextureFormat| {
            context
                .adapter
                .get_texture_format_features(format)
                .allowed_usages
                .contains(required)
        };

        let selected = context.color_texture_format();
        if usable(selected)
            && let Some(format) = external_format(selected)
        {
            return format;
        }

        for (candidate, format) in [
            (
                wgpu::TextureFormat::Bgra8Unorm,
                ExternalSurfaceFormat::Bgra8Unorm,
            ),
            (
                wgpu::TextureFormat::Rgba8Unorm,
                ExternalSurfaceFormat::Rgba8Unorm,
            ),
        ] {
            if usable(candidate) {
                log::warn!(
                    "Adapter {:?} cannot use {selected:?} as an external surface with {required:?}; \
                     registering external surfaces as {format:?} instead.",
                    context.adapter.get_info().name,
                );
                return format;
            }
        }

        // Neither byte order is renderable and samplable, which means no surface will ever be
        // admitted. The preference is still reported honestly rather than guessed at, and every
        // registration then fails at the device rather than silently landing in the other order.
        log::error!(
            "Adapter {:?} supports neither Bgra8Unorm nor Rgba8Unorm with {required:?}; external \
             surfaces will not be allocatable.",
            context.adapter.get_info().name,
        );
        ExternalSurfaceFormat::Bgra8Unorm
    }

    fn create_pipelines(
        device: &wgpu::Device,
        layouts: &WgpuBindGroupLayouts,
        external_texture_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        alpha_mode: wgpu::CompositeAlphaMode,
        path_sample_count: u32,
        dual_source_blending: bool,
        uses_webgl_instance_data: bool,
    ) -> WgpuPipelines {
        // Diagnostic guard: verify the device actually has
        // DUAL_SOURCE_BLENDING. We have a crash report (ZED-5G1) where a
        // feature mismatch caused a wgpu-hal abort, but we haven't
        // identified the code path that produces the mismatch. This
        // guard prevents the crash and logs more evidence.
        // Remove this check once:
        // a) We find and fix the root cause, or
        // b) There are no reports of this warning appearing for some time.
        let device_has_feature = device
            .features()
            .contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        if dual_source_blending && !device_has_feature {
            log::error!(
                "BUG: dual_source_blending flag is true but device does not \
                 have DUAL_SOURCE_BLENDING enabled (device features: {:?}). \
                 Falling back to mono text rendering. Please report this at \
                 https://github.com/zed-industries/zed/issues",
                device.features(),
            );
        }
        let dual_source_blending =
            dual_source_blending && device_has_feature && !uses_webgl_instance_data;

        let shader_source = if uses_webgl_instance_data {
            WEBGL_SHADERS
        } else {
            STORAGE_BUFFER_SHADERS
        };
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gpui_shaders"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let subpixel_shader_module = if dual_source_blending {
            Some(device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("gpui_subpixel_shaders"),
                source: wgpu::ShaderSource::Wgsl(SUBPIXEL_SHADERS.into()),
            }))
        } else {
            None
        };

        let blend_mode = match alpha_mode {
            wgpu::CompositeAlphaMode::PreMultiplied => {
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
            }
            _ => wgpu::BlendState::ALPHA_BLENDING,
        };

        let color_target = wgpu::ColorTargetState {
            format: surface_format,
            blend: Some(blend_mode),
            write_mask: wgpu::ColorWrites::ALL,
        };

        let create_pipeline = |name: &str,
                               vs_entry: &str,
                               fs_entry: &str,
                               globals_layout: &wgpu::BindGroupLayout,
                               data_layout: &wgpu::BindGroupLayout,
                               texture_layout: Option<&wgpu::BindGroupLayout>,
                               topology: wgpu::PrimitiveTopology,
                               color_targets: &[Option<wgpu::ColorTargetState>],
                               sample_count: u32,
                               module: &wgpu::ShaderModule| {
            let mut bind_group_layouts = vec![Some(globals_layout), Some(data_layout)];
            bind_group_layouts.extend(texture_layout.map(Some));
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("{name}_layout")),
                bind_group_layouts: &bind_group_layouts,
                immediate_size: 0,
            });

            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vs_entry),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs_entry),
                    targets: color_targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };

        let quads = create_pipeline(
            "quads",
            "vs_quad",
            "fs_quad",
            &layouts.globals,
            &layouts.instances,
            None,
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target.clone())],
            1,
            &shader_module,
        );

        let shadows = create_pipeline(
            "shadows",
            "vs_shadow",
            "fs_shadow",
            &layouts.globals,
            &layouts.instances,
            None,
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target.clone())],
            1,
            &shader_module,
        );

        let path_rasterization = create_pipeline(
            "path_rasterization",
            "vs_path_rasterization",
            "fs_path_rasterization",
            &layouts.globals,
            &layouts.instances,
            None,
            wgpu::PrimitiveTopology::TriangleList,
            &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            path_sample_count,
            &shader_module,
        );

        let paths_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let paths = create_pipeline(
            "paths",
            "vs_path",
            "fs_path",
            &layouts.globals,
            &layouts.instances,
            Some(&layouts.texture),
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(paths_blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            1,
            &shader_module,
        );

        let underlines = create_pipeline(
            "underlines",
            "vs_underline",
            "fs_underline",
            &layouts.globals,
            &layouts.instances,
            None,
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target.clone())],
            1,
            &shader_module,
        );

        let mono_sprites = create_pipeline(
            "mono_sprites",
            "vs_mono_sprite",
            "fs_mono_sprite",
            &layouts.globals,
            &layouts.instances,
            Some(&layouts.texture),
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target.clone())],
            1,
            &shader_module,
        );

        let subpixel_sprites = if let Some(subpixel_module) = &subpixel_shader_module {
            let subpixel_blend = wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Src1,
                    dst_factor: wgpu::BlendFactor::OneMinusSrc1,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
            };

            Some(create_pipeline(
                "subpixel_sprites",
                "vs_subpixel_sprite",
                "fs_subpixel_sprite",
                &layouts.globals,
                &layouts.instances,
                Some(&layouts.texture),
                wgpu::PrimitiveTopology::TriangleStrip,
                &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(subpixel_blend),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
                1,
                subpixel_module,
            ))
        } else {
            None
        };

        let poly_sprites = create_pipeline(
            "poly_sprites",
            "vs_poly_sprite",
            "fs_poly_sprite",
            &layouts.globals,
            &layouts.instances,
            Some(&layouts.texture),
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target.clone())],
            1,
            &shader_module,
        );

        let surfaces = create_pipeline(
            "surfaces",
            "vs_surface",
            "fs_surface",
            &layouts.globals,
            &layouts.surfaces,
            None,
            wgpu::PrimitiveTopology::TriangleStrip,
            &[Some(color_target)],
            1,
            &shader_module,
        );

        // Deliberately not built through `create_pipeline`: the external-surface pipeline is the
        // one whose blend is fixed rather than following the window's alpha mode, and whose second
        // bind group is a single-instance uniform rather than the instance transport.
        let external_surfaces = ExternalSurfacePipeline::new(
            device,
            &shader_module,
            &layouts.globals,
            external_texture_layout,
            surface_format,
        );

        WgpuPipelines {
            quads,
            shadows,
            path_rasterization,
            paths,
            underlines,
            mono_sprites,
            subpixel_sprites,
            poly_sprites,
            surfaces,
            external_surfaces,
        }
    }

    fn create_path_intermediate(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("path_intermediate"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn create_msaa_if_needed(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        sample_count: u32,
    ) -> Option<(wgpu::Texture, wgpu::TextureView)> {
        if sample_count <= 1 {
            return None;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("path_msaa"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some((texture, view))
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        let width = size.width.0 as u32;
        let height = size.height.0 as u32;

        if width != self.surface_config.width || height != self.surface_config.height {
            let clamped_width = width.min(self.max_texture_size);
            let clamped_height = height.min(self.max_texture_size);

            if clamped_width != width || clamped_height != height {
                warn!(
                    "Requested surface size ({}, {}) exceeds maximum texture dimension {}. \
                     Clamping to ({}, {}). Window content may not fill the entire window.",
                    width, height, self.max_texture_size, clamped_width, clamped_height
                );
            }

            self.surface_config.width = clamped_width.max(1);
            self.surface_config.height = clamped_height.max(1);
            let surface_config = self.surface_config.clone();

            let Some(resources) = self.resources.as_mut() else {
                return;
            };

            // Wait for any in-flight GPU work to complete before destroying textures
            if let Err(e) = resources.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            }) {
                warn!("Failed to poll device during resize: {e:?}");
            }

            // Destroy old textures before allocating new ones to avoid GPU memory spikes
            if let Some(ref texture) = resources.path_intermediate_texture {
                texture.destroy();
            }
            if let Some(ref texture) = resources.path_msaa_texture {
                texture.destroy();
            }

            resources
                .surface
                .configure(&resources.device, &surface_config);

            // Invalidate intermediate textures - they will be lazily recreated
            // in draw() after we confirm the surface is healthy. This avoids
            // panics when the device/surface is in an invalid state during resize.
            resources.invalidate_intermediate_textures();
        }
    }

    fn ensure_intermediate_textures(&mut self) {
        if self.resources().path_intermediate_texture.is_some() {
            return;
        }

        let format = self.surface_config.format;
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        let path_sample_count = self.rendering_params.path_sample_count;
        let resources = self.resources_mut();

        let (t, v) = Self::create_path_intermediate(&resources.device, format, width, height);
        resources.path_intermediate_texture = Some(t);
        resources.path_intermediate_view = Some(v);

        let (path_msaa_texture, path_msaa_view) = Self::create_msaa_if_needed(
            &resources.device,
            format,
            width,
            height,
            path_sample_count,
        )
        .map(|(t, v)| (Some(t), Some(v)))
        .unwrap_or((None, None));
        resources.path_msaa_texture = path_msaa_texture;
        resources.path_msaa_view = path_msaa_view;
    }

    pub fn set_subpixel_layout(&mut self, is_bgr: bool) {
        self.is_bgr = is_bgr;
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        let new_alpha_mode = if transparent {
            self.transparent_alpha_mode
        } else {
            self.opaque_alpha_mode
        };

        if new_alpha_mode != self.surface_config.alpha_mode {
            self.surface_config.alpha_mode = new_alpha_mode;
            let surface_config = self.surface_config.clone();
            let path_sample_count = self.rendering_params.path_sample_count;
            let dual_source_blending = self.dual_source_blending;
            let uses_webgl_instance_data = self.uses_webgl_instance_data;
            // Taken before `resources` is borrowed mutably, and only for its layout: the
            // external-surface pipeline's own blend does not depend on the window's alpha mode,
            // but it is rebuilt with the rest so that there is one construction path.
            let external_registry = Rc::clone(&self.external_registry);
            let external_registry = external_registry.borrow();
            let Some(resources) = self.resources.as_mut() else {
                return;
            };
            resources
                .surface
                .configure(&resources.device, &surface_config);
            resources.pipelines = Self::create_pipelines(
                &resources.device,
                &resources.bind_group_layouts,
                external_registry.texture_bind_group_layout(),
                surface_config.format,
                surface_config.alpha_mode,
                path_sample_count,
                dual_source_blending,
                uses_webgl_instance_data,
            );
        }
    }

    #[allow(dead_code)]
    pub fn viewport_size(&self) -> Size<DevicePixels> {
        Size {
            width: DevicePixels(self.surface_config.width as i32),
            height: DevicePixels(self.surface_config.height as i32),
        }
    }

    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        &self.atlas
    }

    pub fn supports_dual_source_blending(&self) -> bool {
        self.dual_source_blending
    }

    pub fn gpu_specs(&self) -> GpuSpecs {
        GpuSpecs {
            is_software_emulated: self.adapter_info.device_type == wgpu::DeviceType::Cpu,
            device_name: self.adapter_info.name.clone(),
            driver_name: self.adapter_info.driver.clone(),
            driver_info: self.adapter_info.driver_info.clone(),
        }
    }

    pub fn max_texture_size(&self) -> u32 {
        self.max_texture_size
    }

    pub fn draw(&mut self, scene: &Scene) -> bool {
        #[cfg(target_family = "wasm")]
        if self.device_lost() {
            if self.surface_configured {
                log::error!(
                    "Browser graphics context was lost; rendering has stopped. Reload the page to recover."
                );
                // Losing the browser's graphics context kills every external-surface handle at
                // once, exactly as a native device loss does. There is no recovery on this path —
                // the page has to be reloaded — but raising the generation is still what turns a
                // producer's next `register` into an observable `DeviceLost` instead of a write to
                // a texture that no longer exists. `surface_configured` makes this fire once.
                self.external_registry.borrow_mut().invalidate_all();
                self.surface_configured = false;
            }
            return false;
        }

        // Bail out early if the surface has been unconfigured (e.g. during
        // Android background/rotation transitions).  Attempting to acquire
        // a texture from an unconfigured surface can block indefinitely on
        // some drivers (Adreno).
        if !self.surface_configured {
            return false;
        }

        let last_error = self.last_error.lock().unwrap().take();
        if let Some(error) = last_error {
            self.failed_frame_count += 1;
            log::error!(
                "GPU error during frame (failure {} of 10): {error}",
                self.failed_frame_count
            );

            // TBD. Does retrying more actually help?
            if self.failed_frame_count > 10 {
                panic!("Too many consecutive GPU errors. Last error: {error}");
            } else if self.failed_frame_count > 5 {
                if let Some(res) = self.resources.as_mut() {
                    res.invalidate_intermediate_textures();
                }
                self.atlas.clear();
                self.needs_redraw = true;
                self.failed_frame_count = 0;
                return false;
            }
        } else {
            self.failed_frame_count = 0;
        }

        self.atlas.before_frame();

        let frame = match self.resources().surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // Textures must be destroyed before the surface can be reconfigured.
                drop(frame);
                let surface_config = self.surface_config.clone();
                let resources = self.resources_mut();
                resources
                    .surface
                    .configure(&resources.device, &surface_config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                let surface_config = self.surface_config.clone();
                let resources = self.resources_mut();
                resources
                    .surface
                    .configure(&resources.device, &surface_config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return false;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                *self.last_error.lock().unwrap() =
                    Some("Surface texture validation error".to_string());
                return false;
            }
        };

        // Now that we know the surface is healthy, ensure intermediate textures exist
        self.ensure_intermediate_textures();

        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let gamma_params = GammaParams {
            gamma_ratios: self.rendering_params.gamma_ratios,
            grayscale_enhanced_contrast: self.rendering_params.grayscale_enhanced_contrast,
            subpixel_enhanced_contrast: self.rendering_params.subpixel_enhanced_contrast,
            is_bgr: self.is_bgr as u32,
            _pad: 0,
        };

        let globals = GlobalParams {
            viewport_size: [
                self.surface_config.width as f32,
                self.surface_config.height as f32,
            ],
            premultiplied_alpha: if self.surface_config.alpha_mode
                == wgpu::CompositeAlphaMode::PreMultiplied
            {
                1
            } else {
                0
            },
            pad: 0,
        };

        let path_globals = GlobalParams {
            premultiplied_alpha: 0,
            ..globals
        };

        {
            let resources = self.resources();
            resources.queue.write_buffer(
                &resources.globals_buffer,
                0,
                bytemuck::bytes_of(&globals),
            );
            resources.queue.write_buffer(
                &resources.globals_buffer,
                self.path_globals_offset,
                bytemuck::bytes_of(&path_globals),
            );
            resources.queue.write_buffer(
                &resources.globals_buffer,
                self.gamma_offset,
                bytemuck::bytes_of(&gamma_params),
            );
        }

        if let Err(error) = self.record_frame(scene, &frame_view) {
            log::error!("{error:#}");
            self.resources().queue.submit(std::iter::empty());
            return false;
        }

        self.resources().queue.present(frame);
        true
    }

    fn record_frame(&mut self, scene: &Scene, frame_view: &wgpu::TextureView) -> Result<()> {
        let mut instance_offset = 0;
        let instance_bindings = self
            .write_instances(scene, &mut instance_offset)
            .with_context(|| {
                format!(
                    "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} monochrome sprites, {} subpixel sprites, {} polychrome sprites",
                    scene.paths.len(),
                    scene.shadows.len(),
                    scene.quads.len(),
                    scene.underlines.len(),
                    scene.monochrome_sprites.len(),
                    scene.subpixel_sprites.len(),
                    scene.polychrome_sprites.len(),
                )
            })?;

        let mut encoder =
            self.resources()
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("main_encoder"),
                });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            for batch in scene.batches() {
                match batch {
                    PrimitiveBatch::Quads(range) => self.draw_instances(
                        &instance_bindings.quads,
                        &self.resources().pipelines.quads,
                        instance_range(range),
                        &mut pass,
                    ),
                    PrimitiveBatch::Shadows(range) => self.draw_instances(
                        &instance_bindings.shadows,
                        &self.resources().pipelines.shadows,
                        instance_range(range),
                        &mut pass,
                    ),
                    PrimitiveBatch::Paths(range) => {
                        let paths = &scene.paths[range];
                        if paths.is_empty() {
                            continue;
                        }

                        drop(pass);
                        let rasterized = self.draw_paths_to_intermediate(
                            &mut encoder,
                            paths,
                            &mut instance_offset,
                        )?;

                        pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("main_pass_continued"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: frame_view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Load,
                                    store: wgpu::StoreOp::Store,
                                },
                                depth_slice: None,
                            })],
                            depth_stencil_attachment: None,
                            ..Default::default()
                        });

                        if rasterized {
                            self.draw_paths_from_intermediate(
                                paths,
                                &mut instance_offset,
                                &mut pass,
                            )?;
                        }
                    }
                    PrimitiveBatch::Underlines(range) => self.draw_instances(
                        &instance_bindings.underlines,
                        &self.resources().pipelines.underlines,
                        instance_range(range),
                        &mut pass,
                    ),
                    PrimitiveBatch::MonochromeSprites { texture_id, range } => self.draw_sprites(
                        &instance_bindings.monochrome_sprites,
                        texture_id,
                        &self.resources().pipelines.mono_sprites,
                        instance_range(range),
                        &mut pass,
                    ),
                    PrimitiveBatch::SubpixelSprites { texture_id, range } => {
                        let resources = self.resources();
                        self.draw_sprites(
                            &instance_bindings.subpixel_sprites,
                            texture_id,
                            resources
                                .pipelines
                                .subpixel_sprites
                                .as_ref()
                                .unwrap_or(&resources.pipelines.mono_sprites),
                            instance_range(range),
                            &mut pass,
                        );
                    }
                    PrimitiveBatch::PolychromeSprites { texture_id, range } => self.draw_sprites(
                        &instance_bindings.polychrome_sprites,
                        texture_id,
                        &self.resources().pipelines.poly_sprites,
                        instance_range(range),
                        &mut pass,
                    ),
                    // The bounded external-surface bridge. The CoreVideo half of `SurfaceSource`
                    // is macOS-only video playback, which this renderer does not drive; the
                    // external half is the one it does, and it interleaves with every other
                    // primitive by draw order because the bridge reuses this batch rather than
                    // adding a primitive kind of its own.
                    PrimitiveBatch::Surfaces(range) => {
                        self.draw_external_surfaces(&scene.surfaces[range], &mut pass)
                    }
                }
            }
        }

        self.resources()
            .queue
            .submit(std::iter::once(encoder.finish()));
        Ok(())
    }

    fn write_instances(
        &mut self,
        scene: &Scene,
        instance_offset: &mut u64,
    ) -> Result<InstanceBindings> {
        Ok(InstanceBindings {
            quads: self.write_instance_binding(
                "quads_bind_group",
                instance_offset,
                &scene.quads,
            )?,
            shadows: self.write_instance_binding(
                "shadows_bind_group",
                instance_offset,
                &scene.shadows,
            )?,
            underlines: self.write_instance_binding(
                "underlines_bind_group",
                instance_offset,
                &scene.underlines,
            )?,
            monochrome_sprites: self.write_instance_binding(
                "monochrome_sprites_bind_group",
                instance_offset,
                &scene.monochrome_sprites,
            )?,
            subpixel_sprites: self.write_instance_binding(
                "subpixel_sprites_bind_group",
                instance_offset,
                &scene.subpixel_sprites,
            )?,
            polychrome_sprites: self.write_instance_binding(
                "polychrome_sprites_bind_group",
                instance_offset,
                &scene.polychrome_sprites,
            )?,
        })
    }

    fn create_texture_bind_group(
        &self,
        label: &str,
        texture_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let resources = self.resources();
        resources
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &resources.bind_group_layouts.texture,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&resources.atlas_sampler),
                    },
                ],
            })
    }

    fn draw_instances(
        &self,
        instances: &InstanceBinding,
        pipeline: &wgpu::RenderPipeline,
        range: Range<u32>,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.resources().globals_bind_group, &[]);
        pass.set_bind_group(1, &instances.bind_group, &[]);
        pass.draw(
            0..4,
            instances.first_instance + range.start..instances.first_instance + range.end,
        );
    }

    fn draw_sprites(
        &self,
        sprite_instances: &InstanceBinding,
        texture_id: AtlasTextureId,
        pipeline: &wgpu::RenderPipeline,
        range: Range<u32>,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if range.is_empty() {
            return;
        }
        let texture_info = self.atlas.get_texture_info(texture_id);
        let texture =
            self.create_texture_bind_group("atlas_texture_bind_group", &texture_info.view);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.resources().globals_bind_group, &[]);
        pass.set_bind_group(1, &sprite_instances.bind_group, &[]);
        pass.set_bind_group(2, &texture, &[]);
        pass.draw(
            0..4,
            sprite_instances.first_instance + range.start
                ..sprite_instances.first_instance + range.end,
        );
    }

    /// Composites externally produced surfaces into the frame, in the frozen order of the bridge
    /// contract: crop, placement into `bounds`, the affine about the top-left corner of `bounds`,
    /// the content-mask clip, then the group opacity.
    ///
    /// The work itself is [`draw_external_surfaces_into_pass`], which takes the four things it
    /// actually needs rather than the whole renderer so that the pixels it produces can be
    /// observed without a window.
    fn draw_external_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if surfaces.is_empty() {
            return;
        }
        let viewport = (self.surface_config.width, self.surface_config.height);
        // Cloned before `resources` is borrowed mutably; the registry lives beside the resources
        // rather than inside them, because it outlives a device-lost recovery.
        let registry = Rc::clone(&self.external_registry);
        let mut registry = registry.borrow_mut();
        let Some(resources) = self.resources.as_mut() else {
            return;
        };
        draw_external_surfaces_into_pass(
            &resources.device,
            &resources.queue,
            &resources.globals_bind_group,
            &mut resources.pipelines.external_surfaces,
            viewport,
            &mut registry,
            surfaces,
            pass,
        );
    }

    /// The external-surface capability and budget snapshot of this backend.
    ///
    /// This is what `Window::external_surface_capabilities` reports on every wgpu-backed platform
    /// — Browser WebGL2, Browser WebGPU, Linux wgpu-Vulkan and Linux wgpu-GL — and the budgets in
    /// it are the ones the registry actually enforces.
    pub fn external_surface_capabilities(&self) -> ExternalSurfaceCapabilities {
        self.external_registry.borrow().capabilities()
    }

    /// The producer face of the external-surface bridge for this renderer, or `None` while there
    /// is no device (after [`Self::destroy`], or during a device-lost recovery).
    ///
    /// This is the one entry point named by decision D-K16, and it is intended for the single
    /// privileged external compositor. See [`ExternalSurfaceProducer`] for what it grants and what
    /// it deliberately does not. Ordinary GPUI consumers want `Window::paint_external_surface` and
    /// `Window::external_surface_capabilities` instead, which never expose a device.
    pub fn external_surface_producer(&self) -> Option<ExternalSurfaceProducer> {
        let resources = self.resources.as_ref()?;
        Some(ExternalSurfaceProducer::new(
            Arc::clone(&resources.device),
            Arc::clone(&resources.queue),
            Rc::clone(&self.external_registry),
        ))
    }

    unsafe fn instance_bytes<T>(instances: &[T]) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                instances.as_ptr() as *const u8,
                std::mem::size_of_val(instances),
            )
        }
    }

    fn draw_paths_from_intermediate(
        &mut self,
        paths: &[Path<ScaledPixels>],
        instance_offset: &mut u64,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<()> {
        let first_path = &paths[0];
        let sprites: Vec<PathSprite> = if paths.last().map(|p| &p.order) == Some(&first_path.order)
        {
            paths
                .iter()
                .map(|p| PathSprite {
                    bounds: p.clipped_bounds(),
                })
                .collect()
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            vec![PathSprite { bounds }]
        };

        let Some(path_intermediate_view) = self.resources().path_intermediate_view.clone() else {
            return Ok(());
        };
        let instances =
            self.write_instance_binding("path_sprites_bind_group", instance_offset, &sprites)?;
        let texture = self.create_texture_bind_group(
            "path_intermediate_texture_bind_group",
            &path_intermediate_view,
        );
        let resources = self.resources();
        pass.set_pipeline(&resources.pipelines.paths);
        pass.set_bind_group(0, &resources.globals_bind_group, &[]);
        pass.set_bind_group(1, &instances.bind_group, &[]);
        pass.set_bind_group(2, &texture, &[]);
        pass.draw(
            0..4,
            instances.first_instance..instances.first_instance + sprites.len() as u32,
        );
        Ok(())
    }

    fn draw_paths_to_intermediate(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        paths: &[Path<ScaledPixels>],
        instance_offset: &mut u64,
    ) -> Result<bool> {
        let mut vertices = Vec::new();
        for path in paths {
            let bounds = path.clipped_bounds();
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds,
            }));
        }

        if vertices.is_empty() {
            return Ok(false);
        }

        let vertex_binding = self.write_instance_binding(
            "path_rasterization_bind_group",
            instance_offset,
            &vertices,
        )?;

        let resources = self.resources();
        let Some(path_intermediate_view) = resources.path_intermediate_view.as_ref() else {
            return Ok(false);
        };

        let (target_view, resolve_target) = if let Some(ref msaa_view) = resources.path_msaa_view {
            (msaa_view, Some(path_intermediate_view))
        } else {
            (path_intermediate_view, None)
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("path_rasterization_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            pass.set_pipeline(&resources.pipelines.path_rasterization);
            pass.set_bind_group(0, &resources.path_globals_bind_group, &[]);
            pass.set_bind_group(1, &vertex_binding.bind_group, &[]);
            // The path rasterization shader loads records by vertex index
            // rather than instance index, so the allocation's base shifts the
            // vertex range here.
            pass.draw(
                vertex_binding.first_instance
                    ..vertex_binding.first_instance + vertices.len() as u32,
                0..1,
            );
        }

        Ok(true)
    }

    fn write_instance_binding<T>(
        &mut self,
        label: &str,
        instance_offset: &mut u64,
        instances: &[T],
    ) -> Result<InstanceBinding> {
        let data = unsafe { Self::instance_bytes(instances) };
        // wgpu rejects zero-sized bindings, so empty primitive arrays still
        // reserve the 16-byte minimum.
        let size = (data.len() as u64).max(16);
        let stride = (std::mem::size_of::<T>() as u64).max(1);
        let (alignment, allocation_size) = if self.uses_webgl_instance_data {
            // The texture transport has no binding offset: the shader indexes
            // the instance texture absolutely, so each allocation must start on
            // a whole instance (a stride multiple) and a whole texel, and must
            // end on a texel boundary so the zero padding of its final partial
            // texel cannot overlap the next allocation.
            (
                least_common_multiple(self.instance_data_alignment, stride),
                size.next_multiple_of(INSTANCE_TEXTURE_TEXEL_SIZE),
            )
        } else {
            (self.instance_data_alignment.max(1), size)
        };
        let mut offset = (*instance_offset).next_multiple_of(alignment);
        if offset + allocation_size > self.instance_data_capacity {
            self.grow_instance_data(allocation_size)?;
            offset = 0;
        }
        *instance_offset = offset + allocation_size;

        let first_instance = if self.uses_webgl_instance_data {
            u32::try_from(offset / stride).context("instance index exceeds u32 range")?
        } else {
            0
        };

        let resources = self.resources();
        if !data.is_empty() {
            match &resources.instance_data {
                InstanceData::Storage(buffer) => resources.queue.write_buffer(buffer, offset, data),
                InstanceData::Texture { .. } => {
                    Self::write_instance_texture(resources, offset, data)
                }
            }
        }
        let bind_group = resources
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &resources.bind_group_layouts.instances,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: match &resources.instance_data {
                        InstanceData::Storage(buffer) => {
                            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer,
                                offset,
                                size: NonZeroU64::new(size),
                            })
                        }
                        InstanceData::Texture { view, .. } => {
                            wgpu::BindingResource::TextureView(view)
                        }
                    },
                }],
            });
        Ok(InstanceBinding {
            bind_group,
            first_instance,
        })
    }

    fn write_instance_texture(resources: &WgpuResources, offset: u64, data: &[u8]) {
        let InstanceData::Texture {
            texture,
            width,
            height,
            ..
        } = &resources.instance_data
        else {
            return;
        };
        let mut byte_offset = 0usize;
        let mut texel_offset = offset / INSTANCE_TEXTURE_TEXEL_SIZE;
        while byte_offset < data.len() {
            let x = (texel_offset % u64::from(*width)) as u32;
            let y = (texel_offset / u64::from(*width)) as u32;
            if y >= *height {
                // The capacity check in write_instance_binding should make this
                // unreachable. Truncating silently would leave stale bytes in the
                // texture and draw garbage for the remaining instances.
                debug_assert!(
                    false,
                    "instance texture write out of bounds: row {y} >= height {}",
                    *height
                );
                log::error!(
                    "instance texture write out of bounds; dropping {} bytes of instance data",
                    data.len() - byte_offset
                );
                return;
            }
            let available_texels = u64::from(*width - x);
            let remaining_bytes = data.len() - byte_offset;
            let complete_texels = remaining_bytes as u64 / INSTANCE_TEXTURE_TEXEL_SIZE;
            let texels = complete_texels.min(available_texels);
            if texels > 0 {
                let byte_count = (texels * INSTANCE_TEXTURE_TEXEL_SIZE) as usize;
                resources.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &data[byte_offset..byte_offset + byte_count],
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(byte_count as u32),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: texels as u32,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
                byte_offset += byte_count;
                texel_offset += texels;
                continue;
            }

            let mut final_texel = [0; INSTANCE_TEXTURE_TEXEL_SIZE as usize];
            final_texel[..remaining_bytes].copy_from_slice(&data[byte_offset..]);
            resources.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &final_texel,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(INSTANCE_TEXTURE_TEXEL_SIZE as u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            break;
        }
    }

    fn grow_instance_data(&mut self, required: u64) -> Result<()> {
        let capacity = (self.instance_data_capacity * 2)
            .max(required.next_power_of_two())
            .min(self.max_instance_data_size);
        anyhow::ensure!(
            capacity >= required,
            "instance data needs {required} bytes, above the maximum of {}",
            self.max_instance_data_size
        );
        anyhow::ensure!(
            capacity > self.instance_data_capacity,
            "frame instance data exceeds the {}-byte maximum",
            self.max_instance_data_size
        );
        log::debug!(
            "instance data grown from {} to {capacity}",
            self.instance_data_capacity
        );
        // Bind groups created earlier in the frame keep the previous buffer or
        // texture alive, so allocations written before the grow remain valid;
        // only subsequent writes land in the new allocation.
        let uses_webgl_instance_data = self.uses_webgl_instance_data;
        let resources = self.resources_mut();
        if uses_webgl_instance_data {
            let max_texture_dimension = resources.device.limits().max_texture_dimension_2d;
            let (instance_data, actual_capacity) =
                Self::create_instance_texture(&resources.device, capacity, max_texture_dimension);
            resources.instance_data = instance_data;
            self.instance_data_capacity = actual_capacity;
        } else {
            resources.instance_data =
                InstanceData::Storage(resources.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instance_buffer"),
                    size: capacity,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            self.instance_data_capacity = capacity;
        }
        Ok(())
    }

    /// Mark the surface as unconfigured so rendering is skipped until a new
    /// surface is provided via [`replace_surface`](Self::replace_surface).
    ///
    /// This does **not** drop the renderer — the device, queue, atlas, and
    /// pipelines stay alive.  Use this when the native window is destroyed
    /// (e.g. Android `TerminateWindow`) but you intend to re-create the
    /// surface later without losing cached atlas textures.
    pub fn unconfigure_surface(&mut self) {
        self.surface_configured = false;
        // Drop intermediate textures since they reference the old surface size.
        if let Some(res) = self.resources.as_mut() {
            res.invalidate_intermediate_textures();
        }
    }

    /// Replace the wgpu surface with a new one (e.g. after Android destroys
    /// and recreates the native window).  Keeps the device, queue, atlas, and
    /// all pipelines intact so cached `AtlasTextureId`s remain valid.
    ///
    /// The `instance` **must** be the same [`wgpu::Instance`] that was used to
    /// create the adapter and device (i.e. from the [`WgpuContext`]).  Using a
    /// different instance will cause a "Device does not exist" panic because
    /// the wgpu device is bound to its originating instance.
    #[cfg(not(target_family = "wasm"))]
    pub fn replace_surface<W: HasWindowHandle>(
        &mut self,
        window: &W,
        config: WgpuSurfaceConfig,
        instance: &wgpu::Instance,
    ) -> anyhow::Result<()> {
        let window_handle = window
            .window_handle()
            .map_err(|e| anyhow::anyhow!("Failed to get window handle: {e}"))?;

        let surface = create_surface(instance, window_handle.as_raw())?;

        let width = (config.size.width.0 as u32).max(1);
        let height = (config.size.height.0 as u32).max(1);

        let alpha_mode = if config.transparent {
            self.transparent_alpha_mode
        } else {
            self.opaque_alpha_mode
        };

        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface_config.alpha_mode = alpha_mode;
        if let Some(mode) = config.preferred_present_mode {
            self.surface_config.present_mode = mode;
        }

        {
            let res = self
                .resources
                .as_mut()
                .expect("GPU resources not available");
            surface.configure(&res.device, &self.surface_config);
            res.surface = surface;

            // Invalidate intermediate textures — they'll be recreated lazily.
            res.invalidate_intermediate_textures();
        }

        self.surface_configured = true;

        Ok(())
    }

    pub fn destroy(&mut self) {
        // Release surface-bound GPU resources eagerly so the underlying native
        // window can be destroyed before the renderer itself is dropped.
        self.resources.take();
    }

    /// Returns true if the GPU device was lost and recovery is needed.
    pub fn device_lost(&self) -> bool {
        self.device_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns true if a redraw is needed because GPU state was cleared.
    /// Calling this method clears the flag.
    pub fn needs_redraw(&mut self) -> bool {
        std::mem::take(&mut self.needs_redraw)
    }

    /// Recovers from a lost GPU device by recreating the renderer with a new context.
    ///
    /// Call this after detecting `device_lost()` returns true.
    ///
    /// This method coordinates recovery across multiple windows:
    /// - The first window to call this will recreate the shared context
    /// - Subsequent windows will adopt the already-recovered context
    #[cfg(not(target_family = "wasm"))]
    pub fn recover<W>(&mut self, window: &W) -> anyhow::Result<()>
    where
        W: HasWindowHandle + HasDisplayHandle + std::fmt::Debug + Send + Sync + Clone + 'static,
    {
        let gpu_context = self.context.as_ref().expect("recover requires gpu_context");

        // Check if another window already recovered the context
        let needs_new_context = gpu_context
            .borrow()
            .as_ref()
            .is_none_or(|ctx| ctx.device_lost());

        let window_handle = window
            .window_handle()
            .map_err(|e| anyhow::anyhow!("Failed to get window handle: {e}"))?;

        let surface = if needs_new_context {
            log::warn!("GPU device lost, recreating context...");

            // Drop old resources to release Arc<Device>/Arc<Queue> and GPU resources
            self.resources = None;
            *gpu_context.borrow_mut() = None;

            // Wait briefly for the GPU driver to stabilize, then try to
            // recreate the context without software renderers. If this fails
            // the caller should request another frame and retry — the real GPU
            // may need more time to come back (e.g. after suspend/resume).
            std::thread::sleep(std::time::Duration::from_millis(350));

            let instance = WgpuContext::instance(Box::new(window.clone()));
            let surface = create_surface(&instance, window_handle.as_raw())?;
            let new_context =
                WgpuContext::new_rejecting_software(instance, &surface, self.compositor_gpu)?;
            *gpu_context.borrow_mut() = Some(new_context);
            surface
        } else {
            let ctx_ref = gpu_context.borrow();
            let instance = &ctx_ref.as_ref().unwrap().instance;
            create_surface(instance, window_handle.as_raw())?
        };

        let config = WgpuSurfaceConfig {
            size: gpui::Size {
                width: gpui::DevicePixels(self.surface_config.width as i32),
                height: gpui::DevicePixels(self.surface_config.height as i32),
            },
            transparent: self.surface_config.alpha_mode != wgpu::CompositeAlphaMode::Opaque,
            preferred_present_mode: Some(self.surface_config.present_mode),
        };
        let gpu_context = Rc::clone(gpu_context);
        let ctx_ref = gpu_context.borrow();
        let context = ctx_ref.as_ref().expect("context should exist");

        self.resources = None;
        self.atlas.handle_device_lost(context);
        // Losing the device kills every external-surface handle at once rather than one by one:
        // the loss is adapter-wide (S1 evidence), so raising the generation is the whole
        // invalidation. The registry object itself is carried into the rebuilt renderer instead of
        // being replaced, which is what lets a producer acquired before the loss see the raised
        // generation and report `DeviceLost` rather than registering onto a dead device.
        self.external_registry
            .borrow_mut()
            .handle_device_lost(Arc::clone(&context.device));
        let external_registry = Rc::clone(&self.external_registry);

        *self = Self::new_internal(
            Some(gpu_context.clone()),
            context,
            surface,
            config,
            self.compositor_gpu,
            self.atlas.clone(),
            Some(external_registry),
        )?;

        log::info!("GPU recovery complete");
        Ok(())
    }
}

fn instance_range(range: Range<usize>) -> Range<u32> {
    range.start as u32..range.end as u32
}

/// The per-surface state that is bound rather than uploaded.
struct ExternalSurfaceDraw {
    /// The registry's sampling bind group, resolved from the opaque handle in the sampling mode
    /// the descriptor asked for.
    texture: wgpu::BindGroup,
    /// The content mask, as a scissor rectangle: `(x, y, width, height)`.
    scissor: (u32, u32, u32, u32),
}

/// Normalizes a crop against the registered surface size, or `None` when it does not lie inside
/// the surface.
///
/// `None` for `source_bounds` means the whole surface, which is the full `0..1` rectangle rather
/// than an empty one. An out-of-surface crop is already refused by
/// `Window::paint_external_surface`, which validates it against the descriptor; the check is
/// repeated here against the *resource*, because that is what actually bounds the sampling.
fn source_uv(
    source_bounds: Option<Bounds<DevicePixels>>,
    surface_size: Size<DevicePixels>,
) -> Option<PodBounds> {
    let whole_surface = PodBounds {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
    };
    let Some(crop) = source_bounds else {
        return Some(whole_surface);
    };

    let (x, y) = (i64::from(crop.origin.x.0), i64::from(crop.origin.y.0));
    let (width, height) = (i64::from(crop.size.width.0), i64::from(crop.size.height.0));
    let (surface_width, surface_height) = (
        i64::from(surface_size.width.0),
        i64::from(surface_size.height.0),
    );
    let inside = width > 0
        && height > 0
        && x >= 0
        && y >= 0
        && x + width <= surface_width
        && y + height <= surface_height;
    if !inside {
        return None;
    }

    let (surface_width, surface_height) = (surface_width as f32, surface_height as f32);
    Some(PodBounds {
        origin: [x as f32 / surface_width, y as f32 / surface_height],
        size: [width as f32 / surface_width, height as f32 / surface_height],
    })
}

/// Turns a content mask into a scissor rectangle clamped to the viewport, or `None` when it clips
/// everything away.
///
/// GPUI snaps a content mask to whole device pixels before it reaches a primitive, so rounding to
/// the nearest integer is exact here rather than a policy choice. The clamp is not optional either:
/// wgpu rejects a scissor rectangle that reaches past the render target.
fn scissor_rect(
    content_mask: Bounds<ScaledPixels>,
    viewport: (u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let to_pixel = |value: f32, limit: u32| value.round().clamp(0.0, limit as f32) as u32;
    let left = to_pixel(content_mask.origin.x.0, viewport.0);
    let top = to_pixel(content_mask.origin.y.0, viewport.1);
    let right = to_pixel(
        content_mask.origin.x.0 + content_mask.size.width.0,
        viewport.0,
    );
    let bottom = to_pixel(
        content_mask.origin.y.0 + content_mask.size.height.0,
        viewport.1,
    );
    (left < right && top < bottom).then_some((left, top, right - left, bottom - top))
}

/// Composites `surfaces` into whatever render target `pass` is recording into, one draw per
/// surface.
///
/// This is the whole body of [`WgpuRenderer::draw_external_surfaces`], lifted out of the renderer's
/// own borrows and given the things it actually needs — the device and queue, the globals bind
/// group the vertex shader reads the viewport from, the external-surface pipeline with its uniform
/// pool, and the viewport the content mask is clamped against — plus the registry the opaque
/// handles resolve against. Nothing about the sequence changes: the placement fields are still
/// applied in the frozen order of the bridge contract (crop, placement into `bounds`, the affine
/// about the top-left corner of `bounds`, the content-mask clip, then the group opacity), the first
/// three computed here and finished in `vs_external_surface`, the clip a scissor rectangle and the
/// opacity the fragment shader's single multiply.
///
/// The seam exists so that the pixels this path produces can be observed without a window: a test
/// begins a pass on an offscreen texture, calls this, and reads the result back. Every other caller
/// reaches it through [`WgpuRenderer::draw_external_surfaces`].
///
/// A surface whose handle no longer resolves — a stale generation after a device loss, or an id
/// that was retired — is skipped and counted, never a panic and never a draw from stale content:
/// `allow_stale_reuse` is off in the capability snapshot.
#[allow(clippy::too_many_arguments)]
fn draw_external_surfaces_into_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    globals_bind_group: &wgpu::BindGroup,
    pipeline: &mut ExternalSurfacePipeline,
    viewport: (u32, u32),
    registry: &mut ExternalSurfaceRegistry,
    surfaces: &[PaintSurface],
    pass: &mut wgpu::RenderPass<'_>,
) {
    let mut instances: Vec<ExternalSurfaceInstance> = Vec::with_capacity(surfaces.len());
    let mut draws: Vec<ExternalSurfaceDraw> = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        let descriptor = match &surface.source {
            SurfaceSource::External(descriptor) => descriptor,
            // The CoreVideo path belongs to the Metal backend and cannot reach this renderer. The
            // arm exists only because `SurfaceSource` carries that variant whenever this crate is
            // built for macOS, which `cargo test -p gpui_wgpu` on a Mac does.
            #[cfg(target_os = "macos")]
            SurfaceSource::Surface(_) => continue,
        };
        let handle = descriptor.handle;

        let resolved = registry
            .resolve(handle, descriptor.sampling)
            .cloned()
            .zip(registry.surface_size(handle));
        let Some((texture, surface_size)) = resolved else {
            let generation = registry.device_generation();
            let skipped = registry.note_skipped_draw();
            log::warn!(
                "Skipping an external surface: handle {handle:?} no longer resolves at \
                 device generation {generation} ({skipped} skipped so far)"
            );
            continue;
        };

        // The crop is normalized against the *registered* size rather than the descriptor's, so a
        // descriptor that disagrees with the resource can never sample outside it.
        let Some(source_uv) = source_uv(surface.source_bounds, surface_size) else {
            let skipped = registry.note_skipped_draw();
            log::warn!(
                "Skipping an external surface: crop {:?} lies outside the {surface_size:?} \
                 surface of handle {handle:?} ({skipped} skipped so far)",
                surface.source_bounds
            );
            continue;
        };

        // A content mask that clips the surface away entirely is not a failure; there is simply
        // nothing to draw, and an empty scissor rectangle is not a legal one.
        let Some(scissor) = scissor_rect(surface.content_mask.bounds, viewport) else {
            continue;
        };

        instances.push(ExternalSurfaceInstance {
            bounds: surface.bounds.into(),
            source_uv,
            rotation_scale_row0: surface.transform.rotation_scale[0],
            rotation_scale_row1: surface.transform.rotation_scale[1],
            translation: surface.transform.translation,
            opacity: surface.opacity,
            _pad: 0,
        });
        draws.push(ExternalSurfaceDraw { texture, scissor });
    }

    if draws.is_empty() {
        return;
    }

    pipeline.reserve(device, draws.len());
    for (slot, instance) in pipeline.slots.iter().zip(&instances) {
        queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(instance));
    }

    pass.set_pipeline(&pipeline.pipeline);
    pass.set_bind_group(0, globals_bind_group, &[]);
    for (draw, slot) in draws.iter().zip(&pipeline.slots) {
        let (x, y, width, height) = draw.scissor;
        pass.set_scissor_rect(x, y, width, height);
        pass.set_bind_group(1, &slot.bind_group, &[]);
        pass.set_bind_group(2, &draw.texture, &[]);
        pass.draw(0..4, 0..1);
    }
    // Restored unconditionally: a scissor rectangle outlives the draw that set it, so leaving the
    // last surface's content mask installed would clip every batch drawn after this one.
    pass.set_scissor_rect(0, 0, viewport.0, viewport.1);
}

#[cfg(not(target_family = "wasm"))]
fn create_surface(
    instance: &wgpu::Instance,
    raw_window_handle: raw_window_handle::RawWindowHandle,
) -> anyhow::Result<wgpu::Surface<'static>> {
    unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                // Fall back to the display handle already provided via InstanceDescriptor::display.
                raw_display_handle: None,
                raw_window_handle,
            })
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

struct RenderingParameters {
    path_sample_count: u32,
    gamma_ratios: [f32; 4],
    grayscale_enhanced_contrast: f32,
    subpixel_enhanced_contrast: f32,
}

impl RenderingParameters {
    fn new(adapter: &wgpu::Adapter, surface_format: wgpu::TextureFormat) -> Self {
        use std::env;

        let format_features = adapter.get_texture_format_features(surface_format);
        let path_sample_count = [4, 2, 1]
            .into_iter()
            .find(|&n| format_features.flags.sample_count_supported(n))
            .unwrap_or(1);

        let gamma = env::var("ZED_FONTS_GAMMA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.8_f32)
            .clamp(1.0, 2.2);
        let gamma_ratios = get_gamma_correction_ratios(gamma);

        let grayscale_enhanced_contrast = env::var("ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0_f32)
            .max(0.0);

        let subpixel_enhanced_contrast = env::var("ZED_FONTS_SUBPIXEL_ENHANCED_CONTRAST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.5_f32)
            .max(0.0);

        Self {
            path_sample_count,
            gamma_ratios,
            grayscale_enhanced_contrast,
            subpixel_enhanced_contrast,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{MonochromeSprite, PolychromeSprite, Quad, Shadow, SubpixelSprite, Underline};

    #[test]
    fn webgl_shader_is_valid_wgsl_without_storage_buffers() {
        assert!(!WEBGL_SHADERS.contains("var<storage"));
        validate_wgsl(WEBGL_SHADERS, naga::valid::Capabilities::empty());
    }

    #[test]
    fn storage_buffer_shader_is_valid_wgsl() {
        validate_wgsl(STORAGE_BUFFER_SHADERS, naga::valid::Capabilities::empty());
    }

    #[test]
    fn subpixel_shader_is_valid_wgsl() {
        validate_wgsl(
            SUBPIXEL_SHADERS,
            naga::valid::Capabilities::DUAL_SOURCE_BLENDING,
        );
    }

    fn validate_wgsl(source: &str, capabilities: naga::valid::Capabilities) {
        let module = naga::front::wgsl::parse_str(source).expect("shader should parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
            .validate(&module)
            .expect("shader should validate");
    }

    #[test]
    fn webgl_record_sizes_match_shader_word_strides() {
        assert_eq!(std::mem::size_of::<Quad>(), 40 * 4);
        assert_eq!(std::mem::size_of::<Shadow>(), 28 * 4);
        assert_eq!(std::mem::size_of::<PathRasterizationVertex>(), 26 * 4);
        assert_eq!(std::mem::size_of::<PathSprite>(), 4 * 4);
        assert_eq!(std::mem::size_of::<Underline>(), 16 * 4);
        assert_eq!(std::mem::size_of::<MonochromeSprite>(), 28 * 4);
        assert_eq!(std::mem::size_of::<SubpixelSprite>(), 28 * 4);
        assert_eq!(std::mem::size_of::<PolychromeSprite>(), 24 * 4);
    }

    // --- The external-surface bridge's placement arithmetic ---------------------------------

    fn device_size(width: i32, height: i32) -> Size<DevicePixels> {
        Size {
            width: DevicePixels(width),
            height: DevicePixels(height),
        }
    }

    fn device_bounds(x: i32, y: i32, width: i32, height: i32) -> Bounds<DevicePixels> {
        Bounds {
            origin: Point {
                x: DevicePixels(x),
                y: DevicePixels(y),
            },
            size: device_size(width, height),
        }
    }

    fn scaled_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: Point {
                x: ScaledPixels(x),
                y: ScaledPixels(y),
            },
            size: Size {
                width: ScaledPixels(width),
                height: ScaledPixels(height),
            },
        }
    }

    /// The contract's `None` crop is the *whole* surface, which has to become the full sampling
    /// rectangle rather than an empty one.
    #[test]
    fn no_crop_samples_the_whole_surface() {
        let uv = source_uv(None, device_size(256, 128)).unwrap();
        assert_eq!(uv.origin, [0.0, 0.0]);
        assert_eq!(uv.size, [1.0, 1.0]);
    }

    #[test]
    fn a_crop_is_normalized_against_the_registered_surface() {
        let uv = source_uv(Some(device_bounds(64, 32, 128, 64)), device_size(256, 128)).unwrap();
        assert_eq!(uv.origin, [0.25, 0.25]);
        assert_eq!(uv.size, [0.5, 0.5]);

        // The far edge of a full-surface crop lands exactly on 1.0, not past it.
        let uv = source_uv(Some(device_bounds(0, 0, 256, 128)), device_size(256, 128)).unwrap();
        assert_eq!(uv.origin[0] + uv.size[0], 1.0);
        assert_eq!(uv.origin[1] + uv.size[1], 1.0);
    }

    #[test]
    fn an_empty_or_out_of_surface_crop_is_refused() {
        let surface = device_size(256, 128);
        for crop in [
            device_bounds(0, 0, 0, 128),
            device_bounds(0, 0, 256, 0),
            device_bounds(-1, 0, 8, 8),
            device_bounds(0, -1, 8, 8),
            device_bounds(250, 0, 8, 8),
            device_bounds(0, 124, 8, 8),
        ] {
            assert!(source_uv(Some(crop), surface).is_none(), "{crop:?}");
        }
    }

    #[test]
    fn a_content_mask_becomes_a_scissor_rectangle() {
        assert_eq!(
            scissor_rect(scaled_bounds(10.0, 20.0, 100.0, 50.0), (800, 600)),
            Some((10, 20, 100, 50))
        );
    }

    #[test]
    fn a_content_mask_is_clamped_to_the_viewport() {
        // wgpu rejects a scissor rectangle that reaches past the render target, so this clamp is
        // what keeps an oversized content mask from being a validation error.
        assert_eq!(
            scissor_rect(scaled_bounds(-40.0, -10.0, 2000.0, 2000.0), (800, 600)),
            Some((0, 0, 800, 600))
        );
    }

    #[test]
    fn a_content_mask_that_clips_everything_away_produces_no_rectangle() {
        assert_eq!(
            scissor_rect(scaled_bounds(10.0, 10.0, 0.0, 50.0), (800, 600)),
            None
        );
        assert_eq!(
            scissor_rect(scaled_bounds(10.0, 10.0, 50.0, 0.0), (800, 600)),
            None
        );
        // Entirely off-screen: both edges clamp to the same viewport border.
        assert_eq!(
            scissor_rect(scaled_bounds(900.0, 10.0, 50.0, 50.0), (800, 600)),
            None
        );
        assert_eq!(
            scissor_rect(scaled_bounds(-100.0, 10.0, 50.0, 50.0), (800, 600)),
            None
        );
    }

    /// The uniform buffer is read by `ExternalSurface` in `shaders.wgsl`, and nothing but this
    /// test and the shader's own field order keeps the two in step. The offsets are also the ones
    /// the WGSL uniform address space computes for that struct, which is why no vector member may
    /// straddle a 16-byte boundary.
    #[test]
    fn the_instance_layout_matches_the_shader_struct() {
        use std::mem::{offset_of, size_of};
        assert_eq!(offset_of!(ExternalSurfaceInstance, bounds), 0);
        assert_eq!(offset_of!(ExternalSurfaceInstance, source_uv), 16);
        assert_eq!(offset_of!(ExternalSurfaceInstance, rotation_scale_row0), 32);
        assert_eq!(offset_of!(ExternalSurfaceInstance, rotation_scale_row1), 40);
        assert_eq!(offset_of!(ExternalSurfaceInstance, translation), 48);
        assert_eq!(offset_of!(ExternalSurfaceInstance, opacity), 56);
        assert_eq!(size_of::<ExternalSurfaceInstance>(), 64);
        // The same 64 bytes the D3D11 backend's `ExternalSurfaceInstance` occupies, in the same
        // field order, so the two backends describe one struct.
        assert_eq!(EXTERNAL_SURFACE_INSTANCE_SIZE, 64);
    }

    /// The external-surface entry points live in the shared shader source, so they compile in the
    /// WebGL2 variant too — and they must do so **without** a storage buffer, which is what the
    /// single-instance uniform buys. `webgl_shader_is_valid_wgsl_without_storage_buffers` proves
    /// the module as a whole; this proves the external half is actually in it.
    #[test]
    fn the_external_surface_entry_points_are_in_every_shader_variant() {
        for (name, source) in [
            ("webgl", WEBGL_SHADERS),
            ("storage", STORAGE_BUFFER_SHADERS),
            ("subpixel", SUBPIXEL_SHADERS),
        ] {
            assert!(source.contains("fn vs_external_surface"), "{name}");
            assert!(source.contains("fn fs_external_surface"), "{name}");
        }

        // The instance transport of every other pipeline is a storage buffer or a uint texture;
        // this one is a uniform, which is the only portable choice on WebGL2.
        let module = naga::front::wgsl::parse_str(WEBGL_SHADERS).expect("shader should parse");
        let external_surface = module
            .global_variables
            .iter()
            .find(|(_, variable)| variable.name.as_deref() == Some("external_surface"))
            .map(|(_, variable)| variable.space);
        assert_eq!(external_surface, Some(naga::AddressSpace::Uniform));
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod external_surface_draw_tests {
    //! The GPUI-side counterpart of the S1 spike's pixel corpus, run through the wgpu consumer
    //! path.
    //!
    //! The spike proved the *mechanism* outside GPUI: a producer pass fills a texture, a consumer
    //! pass samples it in the same frame, and named pixels of the result are compared byte for byte
    //! against fixed constants. What it could not prove is that GPUI's own consumer path agrees
    //! with it — that `vs_external_surface` reads the uniform the way [`ExternalSurfaceInstance`]
    //! writes it, that the affine lands the right way round on screen, that the content mask clips
    //! where it says it does, and that the pipeline builds at all.
    //!
    //! These tests run that corpus through the real renderer code, and deliberately against the
    //! same constants and probe coordinates as the D3D11 corpus in
    //! `gpui_windows/src/directx_renderer.rs`: same producer pattern (clear to the generation
    //! colour, yellow marker triangle in the top-left corner at NDC (-1,1), (-0.5,1), (-1,0.5)),
    //! same 800x600 frame, same target rectangle at NDC (-0.5,-0.5)..(0.5,0.5), same colour
    //! constants, same probes. A disagreement between the two harnesses is therefore a
    //! disagreement about a backend's pipeline and nothing else. The only difference is the target
    //! — an offscreen texture instead of a swap chain — reached through
    //! [`draw_external_surfaces_into_pass`], which is the function `WgpuRenderer::draw` itself
    //! calls.
    //!
    //! Everything returns early when the host has no wgpu adapter at all, the same way the
    //! device-backed registry tests do.

    use super::{
        EXTERNAL_SURFACE_INSTANCE_SIZE, ExternalSurfacePipeline, GammaParams, GlobalParams,
        STORAGE_BUFFER_SHADERS, WgpuRenderer, draw_external_surfaces_into_pass,
    };
    use crate::external_registry::{ExternalSurfaceRegistry, external_format};
    use bytemuck::{Pod, Zeroable};
    use gpui::{
        Bounds, ContentMask, DevicePixels, ExternalAlphaMode, ExternalColorSpace, ExternalSampling,
        ExternalSurfaceDescriptor, ExternalSurfaceHandle, ExternalSyncToken, PaintSurface, Point,
        ScaledPixels, Size, SurfaceSource, TransformationMatrix,
    };
    use std::num::NonZeroU64;
    use std::sync::Arc;

    /// The frame the surface is composited into, in device pixels. The S1 corpus coordinates are
    /// stated against exactly this size.
    const FRAME_WIDTH: u32 = 800;
    const FRAME_HEIGHT: u32 = 600;
    /// The external surface itself, in device pixels.
    const SURFACE_EXTENT: i32 = 512;

    /// The S1 colour constants, unchanged. Only generation 0 is used here — a device generation
    /// only advances on device loss, which these tests do not provoke — but the array is kept whole
    /// so the two corpora stay diffable.
    const GENERATION_COLORS: [[u8; 4]; 3] =
        [[0, 180, 180, 255], [230, 120, 0, 255], [140, 0, 200, 255]];
    const PRODUCER_MARK: [u8; 4] = [255, 255, 0, 255];
    const BACKGROUND: [u8; 4] = [30, 30, 30, 255];

    /// The centre of the target rectangle: content was sampled at all.
    const QUAD_CENTER: (u32, u32) = (400, 300);
    /// 20x15 device pixels inside the target rectangle's top-left corner, which is where the
    /// producer's marker triangle lands when the crop, the placement and the UV orientation are all
    /// right. This is the S1 corpus' `producer_mark_visible` case, coordinates included.
    const MARKER: (u32, u32) = (220, 165);
    /// The vertical mirror of [`MARKER`] about the target rectangle's centre line. The marker must
    /// **not** be here: if it were, the sampled `v` axis would run bottom-up.
    const MARKER_FLIPPED_V: (u32, u32) = (220, 435);
    /// The horizontal mirror of [`MARKER`] about the target rectangle's **left edge** (x = 200),
    /// which is where an x-mirror pivoting on the top-left corner of `bounds` puts it.
    const MARKER_MIRRORED_X: (u32, u32) = (179, 165);
    /// Left of the target rectangle: nothing may be painted there.
    const OUTSIDE_QUAD: (u32, u32) = (100, 300);
    /// Inside the right half of the target rectangle, and so inside the half-width content mask.
    const MASK_INSIDE: (u32, u32) = (500, 300);
    /// Inside the target rectangle but in its left half, and so outside the half-width content
    /// mask.
    const MASK_OUTSIDE: (u32, u32) = (300, 300);

    /// The producer's own pipeline, which is deliberately *not* GPUI's: an external compositor
    /// brings its own shaders and draws through the device and queue the producer accessor hands
    /// it.
    const PRODUCER_SHADERS: &str = r#"
struct SolidOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_solid(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> SolidOut {
    var out: SolidOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_solid(input: SolidOut) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct SolidVertex {
        pos: [f32; 2],
        color: [f32; 4],
    }

    fn color_f(color: [u8; 4]) -> [f32; 4] {
        [
            f32::from(color[0]) / 255.0,
            f32::from(color[1]) / 255.0,
            f32::from(color[2]) / 255.0,
            f32::from(color[3]) / 255.0,
        ]
    }

    fn clear_color(color: [u8; 4]) -> wgpu::Color {
        let color = color_f(color);
        wgpu::Color {
            r: f64::from(color[0]),
            g: f64::from(color[1]),
            b: f64::from(color[2]),
            a: f64::from(color[3]),
        }
    }

    fn scaled_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: Point {
                x: ScaledPixels(x),
                y: ScaledPixels(y),
            },
            size: Size {
                width: ScaledPixels(width),
                height: ScaledPixels(height),
            },
        }
    }

    fn device_bounds(x: i32, y: i32, width: i32, height: i32) -> Bounds<DevicePixels> {
        Bounds {
            origin: Point {
                x: DevicePixels(x),
                y: DevicePixels(y),
            },
            size: Size {
                width: DevicePixels(width),
                height: DevicePixels(height),
            },
        }
    }

    fn surface_size() -> Size<DevicePixels> {
        Size {
            width: DevicePixels(SURFACE_EXTENT),
            height: DevicePixels(SURFACE_EXTENT),
        }
    }

    /// The target rectangle. NDC (-0.5,-0.5)..(0.5,0.5) in an 800x600 viewport is
    /// (200,150)..(600,450) in device pixels, which is where the S1 consumer pass puts its
    /// textured quad.
    fn quad_bounds() -> Bounds<ScaledPixels> {
        scaled_bounds(200.0, 150.0, 400.0, 300.0)
    }

    /// A content mask over the whole frame: it clips nothing.
    fn whole_frame_mask() -> ContentMask<ScaledPixels> {
        ContentMask {
            bounds: scaled_bounds(0.0, 0.0, FRAME_WIDTH as f32, FRAME_HEIGHT as f32),
        }
    }

    /// Compositing premultiplied `src` scaled by `opacity` over `dst`, which is what the
    /// external-surface blend state (`One` / `OneMinusSrcAlpha` on all four channels) computes once
    /// the fragment shader has multiplied the sample by the group opacity.
    fn premultiplied_over(src: [u8; 4], dst: [u8; 4], opacity: f32) -> [u8; 4] {
        let source_alpha = f32::from(src[3]) / 255.0 * opacity;
        let mut out = [0u8; 4];
        for channel in 0..4 {
            let source = f32::from(src[channel]) / 255.0 * opacity;
            let destination = f32::from(dst[channel]) / 255.0;
            out[channel] = ((source + destination * (1.0 - source_alpha)) * 255.0).round() as u8;
        }
        out
    }

    #[track_caller]
    fn assert_pixel(actual: [u8; 4], expected: [u8; 4], what: &str) {
        assert_eq!(actual, expected, "{what}");
    }

    /// Like [`assert_pixel`], but tolerant of one unit per channel.
    ///
    /// This is used **only** where the expected value is a blend rather than a colour the pipeline
    /// copies through: the GPU rounds the float result to `unorm8` in the render target, and its
    /// rounding is not required to match `f32::round` exactly. Every unblended assertion in this
    /// module is byte-exact.
    #[track_caller]
    fn assert_pixel_within_one(actual: [u8; 4], expected: [u8; 4], what: &str) {
        let close = actual
            .iter()
            .zip(expected.iter())
            .all(|(a, e)| a.abs_diff(*e) <= 1);
        assert!(close, "{what}: expected {expected:?} +/-1, got {actual:?}");
    }

    /// One read-back copy of the frame.
    struct Frame {
        pixels: Vec<u8>,
        /// The padded row stride of the copy: wgpu requires a multiple of 256 bytes per row, which
        /// 800 * 4 is not.
        pitch: usize,
        format: wgpu::TextureFormat,
    }

    impl Frame {
        /// The pixel at `(x, y)`, converted out of the render target's memory order into RGBA so
        /// that it compares directly against the S1 constants.
        fn pixel(&self, at: (u32, u32)) -> [u8; 4] {
            let offset = at.1 as usize * self.pitch + at.0 as usize * 4;
            let bytes = &self.pixels[offset..offset + 4];
            match self.format {
                wgpu::TextureFormat::Bgra8Unorm => [bytes[2], bytes[1], bytes[0], bytes[3]],
                _ => [bytes[0], bytes[1], bytes[2], bytes[3]],
            }
        }
    }

    /// A device, a GPUI frame to draw into, and one registered external surface the producer has
    /// already filled.
    struct Harness {
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        format: wgpu::TextureFormat,
        globals_bind_group: wgpu::BindGroup,
        pipeline: ExternalSurfacePipeline,
        registry: ExternalSurfaceRegistry,
        frame: wgpu::Texture,
        frame_view: wgpu::TextureView,
        readback: wgpu::Buffer,
        readback_pitch: u32,
        handle: ExternalSurfaceHandle,
    }

    impl Harness {
        /// Builds everything, or returns `None` when the host has no wgpu adapter at all.
        fn new() -> Option<Self> {
            let (adapter, device, queue) =
                crate::external_registry::tests::test_adapter_and_device()?;
            let format = frame_format(&adapter)?;
            let external_format =
                external_format(format).expect("the frame format is a contract byte order");

            let layouts = WgpuRenderer::create_bind_group_layouts(&device, false);
            let (globals_buffer, globals_bind_group) = globals(&device, &layouts.globals);
            queue.write_buffer(
                &globals_buffer,
                0,
                bytemuck::bytes_of(&GlobalParams {
                    // The one field the external-surface vertex shader divides by. It has to be
                    // the frame's own size for the placement to land where the corpus says.
                    viewport_size: [FRAME_WIDTH as f32, FRAME_HEIGHT as f32],
                    premultiplied_alpha: 1,
                    pad: 0,
                }),
            );

            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("gpui_shaders"),
                // The real shared source, in the variant every non-WebGL2 backend uses. The WebGL2
                // variant carries the same external-surface entry points; that it compiles is
                // covered by the naga validation tests, and its pixels are validated in a browser.
                source: wgpu::ShaderSource::Wgsl(STORAGE_BUFFER_SHADERS.into()),
            });

            let mut registry =
                ExternalSurfaceRegistry::new(Arc::clone(&device), external_format, false);
            let pipeline = ExternalSurfacePipeline::new(
                &device,
                &module,
                &layouts.globals,
                registry.texture_bind_group_layout(),
                format,
            );

            let frame = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("corpus_frame"),
                size: wgpu::Extent3d {
                    width: FRAME_WIDTH,
                    height: FRAME_HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let frame_view = frame.create_view(&wgpu::TextureViewDescriptor::default());

            let readback_pitch =
                (FRAME_WIDTH * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("corpus_readback"),
                size: u64::from(readback_pitch) * u64::from(FRAME_HEIGHT),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let (handle, producer_texture) = registry
                .register(surface_size(), external_format)
                .expect("registering a 512x512 surface must be inside the provisional budget");
            producer_pass(&device, &queue, &producer_texture, format);

            Some(Self {
                device,
                queue,
                format,
                globals_bind_group,
                pipeline,
                registry,
                frame,
                frame_view,
                readback,
                readback_pitch,
                handle,
            })
        }

        fn descriptor(&self) -> ExternalSurfaceDescriptor {
            ExternalSurfaceDescriptor {
                handle: self.handle,
                size: surface_size(),
                format: external_format(self.format).expect("a contract byte order"),
                color_space: ExternalColorSpace::SrgbEncodedUnorm,
                alpha_mode: ExternalAlphaMode::Premultiplied,
                // Nearest, so that every unblended assertion below is a texel the producer wrote
                // rather than a filter of four of them, and a mismatch is therefore unambiguous.
                sampling: ExternalSampling::Nearest,
                ready: ExternalSyncToken::SameQueueOrdered,
                allocated_bytes: (SURFACE_EXTENT as u64) * (SURFACE_EXTENT as u64) * 4,
            }
        }

        /// The primitive the corpus draws: the whole surface, into [`quad_bounds`], unclipped,
        /// untransformed and fully opaque. Every test starts from this and changes one field.
        fn paint_surface(&self) -> PaintSurface {
            PaintSurface {
                order: 0,
                bounds: quad_bounds(),
                content_mask: whole_frame_mask(),
                source: SurfaceSource::External(self.descriptor()),
                source_bounds: None,
                transform: TransformationMatrix::unit(),
                opacity: 1.0,
            }
        }

        /// Clears the frame to `BACKGROUND` and composites `surfaces` through the renderer's own
        /// draw path, then reads the result back.
        fn draw(&mut self, surfaces: &[PaintSurface]) -> Frame {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("corpus_encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("corpus_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.frame_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear_color(BACKGROUND)),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });

                draw_external_surfaces_into_pass(
                    &self.device,
                    &self.queue,
                    &self.globals_bind_group,
                    &mut self.pipeline,
                    (FRAME_WIDTH, FRAME_HEIGHT),
                    &mut self.registry,
                    surfaces,
                    &mut pass,
                );
            }

            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.frame,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(self.readback_pitch),
                        rows_per_image: Some(FRAME_HEIGHT),
                    },
                },
                wgpu::Extent3d {
                    width: FRAME_WIDTH,
                    height: FRAME_HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));

            self.read_back()
        }

        fn read_back(&self) -> Frame {
            let slice = self.readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, |result| {
                result.expect("mapping the read-back buffer");
            });
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("waiting for the frame to finish");
            // The mapped range is a temporary of this statement, so it is released before the
            // `unmap` below, which would otherwise be a use-after-unmap.
            let pixels = slice
                .get_mapped_range()
                .expect("readback buffer must map")
                .to_vec();
            self.readback.unmap();
            Frame {
                pixels,
                pitch: self.readback_pitch as usize,
                format: self.format,
            }
        }
    }

    /// The byte order the corpus frame and the external surface are both allocated in: `Bgra8Unorm`
    /// where the adapter can render to it, sample it and copy out of it — which is GPUI's first
    /// preference and what every backend this test runs on offers — and `Rgba8Unorm` otherwise,
    /// which is the contract's fallback and what WebGL2 always lands on.
    ///
    /// The probes below are stated in RGBA either way; [`Frame::pixel`] is what converts out of the
    /// target's memory order.
    fn frame_format(adapter: &wgpu::Adapter) -> Option<wgpu::TextureFormat> {
        let required = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
        .into_iter()
        .find(|format| {
            adapter
                .get_texture_format_features(*format)
                .allowed_usages
                .contains(required)
        })
    }

    /// A globals buffer and bind group laid out exactly the way `WgpuRenderer::new_internal` lays
    /// them out, so the vertex shader reads the viewport from the binding it expects.
    fn globals(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let globals_size = std::mem::size_of::<GlobalParams>() as u64;
        let gamma_size = std::mem::size_of::<GammaParams>() as u64;
        let path_globals_offset = globals_size.next_multiple_of(alignment);
        let gamma_offset = (path_globals_offset + globals_size).next_multiple_of(alignment);

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("corpus_globals"),
            size: gamma_offset + gamma_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("corpus_globals_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: 0,
                        size: Some(NonZeroU64::new(globals_size).unwrap()),
                    }),
                },
                // Read only by the text pipelines, but the layout requires it to be bound.
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: gamma_offset,
                        size: Some(NonZeroU64::new(gamma_size).unwrap()),
                    }),
                },
            ],
        });
        (buffer, bind_group)
    }

    /// The S1 producer pass, run against the texture the registry handed back.
    ///
    /// This is the producer flow of the contract exactly: the producer receives the texture from
    /// `register`, makes its **own** view, pipeline and render pass over it, and submits on GPUI's
    /// queue ahead of GPUI's frame — which is what `SameQueueOrdered` means. It clears to the
    /// generation colour and draws the marker triangle at NDC (-1,1), (-0.5,1), (-1,0.5), which on
    /// a 512x512 surface is the right triangle with corners at texels (0,0), (128,0) and (0,128).
    fn producer_pass(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: &wgpu::Texture,
        format: wgpu::TextureFormat,
    ) {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("producer_shaders"),
            source: wgpu::ShaderSource::Wgsl(PRODUCER_SHADERS.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("producer_layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("producer_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_solid"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SolidVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_solid"),
                // The producer writes its content opaquely; group opacity is GPUI's to apply, not
                // the producer's, so nothing here blends.
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        let mark = color_f(PRODUCER_MARK);
        let vertices = [
            SolidVertex {
                pos: [-1.0, 1.0],
                color: mark,
            },
            SolidVertex {
                pos: [-0.5, 1.0],
                color: mark,
            },
            SolidVertex {
                pos: [-1.0, 0.5],
                color: mark,
            },
        ];
        let vertex_bytes = bytemuck::cast_slice(&vertices);
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("producer_vertices"),
            size: vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: true,
        });
        vertex_buffer
            .slice(..)
            .get_mapped_range_mut()
            .expect("vertex staging buffer must map")
            .copy_from_slice(vertex_bytes);
        vertex_buffer.unmap();

        let view = surface.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("producer_encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("producer_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color(GENERATION_COLORS[0])),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    // --- The corpus ---------------------------------------------------------------------------

    /// The external-surface pipeline builds, and its uniform pool hands out one slot per surface.
    #[test]
    fn the_pipeline_builds_and_pools_one_uniform_per_surface() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        assert!(harness.pipeline.slots.is_empty(), "nothing drawn yet");

        harness.draw(&[harness.paint_surface()]);
        assert_eq!(harness.pipeline.slots.len(), 1);
        for slot in &harness.pipeline.slots {
            assert_eq!(slot.buffer.size(), EXTERNAL_SURFACE_INSTANCE_SIZE);
        }

        // A second surface in one frame gets its own buffer: one `write_buffer` per surface, so a
        // shared buffer would give both draws the last surface's placement.
        harness.draw(&[harness.paint_surface(), harness.paint_surface()]);
        assert_eq!(harness.pipeline.slots.len(), 2);
    }

    /// The S1 corpus' same-device cases — `external_center_generation`, `producer_mark_visible`
    /// and the "nothing outside the quad" one — run through GPUI's own pipeline, at the spike's own
    /// coordinates, plus one negative check the spike did not need.
    ///
    /// Together they prove that the shader's view of [`ExternalSurfaceInstance`] matches the Rust
    /// one: that struct is 64 bytes of `bounds`, `source_uv`, the affine and the opacity, and a
    /// disagreement about the field order or the padding moves `bounds` or `source_uv` so that the
    /// target rectangle stops landing on these pixels at all.
    #[test]
    fn the_s1_corpus_pixels_survive_the_real_draw_path() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let surface = harness.paint_surface();
        let frame = harness.draw(&[surface]);

        assert_pixel(
            frame.pixel(QUAD_CENTER),
            GENERATION_COLORS[0],
            "the centre of the target rectangle must be the surface the producer cleared",
        );
        assert_pixel(
            frame.pixel(MARKER),
            PRODUCER_MARK,
            "the producer's own marker triangle must be visible near the top-left of the target \
             rectangle",
        );
        assert_pixel(
            frame.pixel(OUTSIDE_QUAD),
            BACKGROUND,
            "nothing may be painted outside the target rectangle",
        );
        // The marker lives in the *top* left of the surface. Finding it at the mirrored row would
        // mean the sampled `v` axis runs bottom-up, which is the classic orientation bug.
        assert_pixel(
            frame.pixel(MARKER_FLIPPED_V),
            GENERATION_COLORS[0],
            "the marker must not appear at the vertically mirrored position: the `v` axis runs \
             top-down",
        );
    }

    /// The crop is applied before the placement, and it is normalized against the registered
    /// surface rather than against the descriptor.
    #[test]
    fn a_crop_selects_the_region_of_the_surface_it_names() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        // The bottom-right quadrant: uniformly the generation colour, because the marker occupies
        // the top-left one.
        let mut surface = harness.paint_surface();
        surface.source_bounds = Some(device_bounds(
            SURFACE_EXTENT / 2,
            SURFACE_EXTENT / 2,
            SURFACE_EXTENT / 2,
            SURFACE_EXTENT / 2,
        ));
        let frame = harness.draw(&[surface]);

        assert_pixel(
            frame.pixel(QUAD_CENTER),
            GENERATION_COLORS[0],
            "a cropped surface still fills the whole target rectangle",
        );
        assert_pixel(
            frame.pixel(MARKER),
            GENERATION_COLORS[0],
            "the marker lives in the top-left quadrant, so cropping to the bottom-right one must \
             leave it out of the frame entirely",
        );
        assert_ne!(
            frame.pixel(MARKER),
            PRODUCER_MARK,
            "a crop that excludes the marker must not sample it"
        );
    }

    /// Group opacity is the fragment shader's single multiply on already-premultiplied content,
    /// and the blend state composites the result with `One`/`OneMinusSrcAlpha`.
    #[test]
    fn group_opacity_blends_the_surface_over_the_frame() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let mut surface = harness.paint_surface();
        surface.opacity = 0.5;
        let frame = harness.draw(&[surface]);

        // Computed from the same constants rather than written out: at 0.5 over `BACKGROUND` this
        // is [15, 105, 105, 255]. A straight-alpha blend state would premultiply a second time and
        // land well outside the one-unit tolerance.
        let expected = premultiplied_over(GENERATION_COLORS[0], BACKGROUND, 0.5);
        assert_pixel_within_one(
            frame.pixel(QUAD_CENTER),
            expected,
            "half opacity must be half the surface over the background",
        );
        assert_pixel(
            frame.pixel(OUTSIDE_QUAD),
            BACKGROUND,
            "opacity changes nothing outside the target rectangle",
        );
    }

    /// The content mask becomes a scissor rectangle, and it is taken back off afterwards.
    #[test]
    fn a_content_mask_clips_the_surface_to_a_scissor_rectangle() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let mut surface = harness.paint_surface();
        // The right half of the target rectangle only.
        surface.content_mask = ContentMask {
            bounds: scaled_bounds(400.0, 150.0, 200.0, 300.0),
        };
        let frame = harness.draw(&[surface]);

        assert_pixel(
            frame.pixel(MASK_INSIDE),
            GENERATION_COLORS[0],
            "inside the content mask the surface is drawn",
        );
        assert_pixel(
            frame.pixel(MASK_OUTSIDE),
            BACKGROUND,
            "inside the target rectangle but outside the content mask nothing is drawn",
        );

        // The scissor rectangle is restored, so the next surface in the same pass is not still
        // clipped to the previous one's content mask.
        let frame = harness.draw(&[harness.paint_surface()]);
        assert_pixel(
            frame.pixel(MASK_OUTSIDE),
            GENERATION_COLORS[0],
            "an unclipped surface drawn after a clipped one is not still clipped",
        );
    }

    /// The affine pivots on the **top-left corner of `bounds`**, not on the viewport origin, and a
    /// negative determinant mirrors rather than culling.
    ///
    /// **The observation that proves the handedness is the pair** `MARKER_MIRRORED_X ==
    /// PRODUCER_MARK` **and** `MARKER == BACKGROUND`, not either alone. The marker sits at u ~ 0.05
    /// of the surface, so under the unit matrix it lands 20 device pixels to the *right* of the
    /// rectangle's left edge (x = 220, which the corpus test asserts). Mirroring x about that same
    /// left edge has to put it 20 pixels to the *left* of it, at x = 179, and has to leave x = 220
    /// outside the mirrored rectangle altogether. The three ways this could go wrong are all
    /// distinguishable:
    ///
    /// * were the affine applied about the viewport origin the way the sprite pipelines do it, the
    ///   rectangle would land at x in -600..-200, entirely off-screen, and both probes would read
    ///   `BACKGROUND`;
    /// * were the mirrored winding culled — the external pipeline's `cull_mode` is `None`, which is
    ///   what makes a negative determinant legal at all — the frame would likewise be uniformly
    ///   `BACKGROUND`;
    /// * were the transform dropped or its sign inverted, x = 220 would still be the marker and
    ///   x = 179 would still be `BACKGROUND`.
    ///
    /// This matrix is symmetric, so it does not by itself distinguish a row-major read of
    /// `rotation_scale` from a column-major one; the layout assertion in the sibling test module is
    /// what pins that, together with the translation sitting after both rows.
    #[test]
    fn a_mirroring_transform_pivots_on_the_top_left_of_the_bounds() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let mut surface = harness.paint_surface();
        // Row-major, so this is `x -> -x`, `y -> y`: the same matrix
        // `TransformationMatrix::unit().scale(size(-1.0, 1.0))` builds.
        surface.transform = TransformationMatrix {
            rotation_scale: [[-1.0, 0.0], [0.0, 1.0]],
            translation: [0.0, 0.0],
        };
        let frame = harness.draw(&[surface]);

        assert_pixel(
            frame.pixel(MARKER_MIRRORED_X),
            PRODUCER_MARK,
            "the mirror must move the marker to the far side of the rectangle's left edge",
        );
        assert_pixel(
            frame.pixel(MARKER),
            BACKGROUND,
            "the mirrored rectangle no longer covers the marker's original position",
        );
        assert_pixel(
            frame.pixel(OUTSIDE_QUAD),
            GENERATION_COLORS[0],
            "the mirrored rectangle now covers what the unit matrix left as background",
        );
    }

    /// A handle whose generation died is skipped and counted, never drawn and never a panic.
    #[test]
    fn a_stale_handle_is_skipped_and_counted_rather_than_drawn() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let surface = harness.paint_surface();
        // Everything the old device owned dies at once; the descriptor still names the old handle.
        harness.registry.invalidate_all();

        let frame = harness.draw(&[surface]);

        assert_pixel(
            frame.pixel(QUAD_CENTER),
            BACKGROUND,
            "a stale handle draws nothing at all",
        );
        // One skip for the draw above, and the counter keeps climbing rather than resetting.
        assert_eq!(harness.registry.note_skipped_draw(), 2);
    }
}
