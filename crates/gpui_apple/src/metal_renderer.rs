use crate::external_registry::{ExternalSurfaceProducer, ExternalSurfaceRegistry};
use crate::metal_atlas::MetalAtlas;
use anyhow::{Context as _, Result};
use block::ConcreteBlock;
use cocoa::{
    base::{NO, YES},
    foundation::{NSSize, NSUInteger},
    quartzcore::AutoresizingMask,
};
use gpui::{
    AtlasTextureId, Background, Bounds, ContentMask, DevicePixels, ExternalSampling,
    ExternalSurfaceCapabilities, PaintSurface, Path, Point, PrimitiveBatch, ScaledPixels, Scene,
    Size, SurfaceSource, TransformationMatrix, point, size,
};
#[cfg(any(test, feature = "test-support"))]
use image::RgbaImage;

use core_foundation::base::TCFType;
use core_video::{
    metal_texture::CVMetalTextureGetTexture, metal_texture_cache::CVMetalTextureCache,
    pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use foreign_types::{ForeignType, ForeignTypeRef};
use metal::{
    CAMetalLayer, CommandQueue, MTLGPUFamily, MTLPixelFormat, MTLResourceOptions, NSRange,
};
use objc::{self, msg_send, sel, sel_impl};
use parking_lot::Mutex;

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    mem,
    mem::MaybeUninit,
    ops::Range,
    ptr,
    rc::Rc,
    slice,
    sync::Arc,
};

// Exported to metal
pub(crate) type PointF = gpui::Point<f32>;

#[cfg(not(feature = "runtime_shaders"))]
const SHADERS_METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
#[cfg(feature = "runtime_shaders")]
const SHADERS_SOURCE_FILE: &str = include_str!(concat!(env!("OUT_DIR"), "/stitched_shaders.metal"));
// Use 4x MSAA, all devices support it.
// https://developer.apple.com/documentation/metal/mtldevice/1433355-supportstexturesamplecount
const PATH_SAMPLE_COUNT: u32 = 4;
/// Metal requires the offset a buffer is bound at to be 256-byte aligned.
const INSTANCE_BUFFER_ALIGNMENT: usize = 256;
const MAX_INSTANCE_BUFFER_SIZE: usize = 256 * 1024 * 1024;
/// How many drawables the layer may have outstanding, and with it how many external surfaces the
/// bridge admits in flight: one per frame the presentation path can have going at once.
pub(crate) const MAX_DRAWABLE_COUNT: u64 = 3;

pub type Context = Arc<Mutex<InstanceBufferPool>>;
pub type Renderer = MetalRenderer;

pub unsafe fn new_renderer(
    context: self::Context,
    _native_window: *mut c_void,
    _native_view: *mut c_void,
    _bounds: gpui::Size<f32>,
    transparent: bool,
) -> Renderer {
    MetalRenderer::new(context, transparent)
}

pub struct InstanceBufferPool {
    buffer_size: usize,
    buffers: Vec<metal::Buffer>,
}

impl Default for InstanceBufferPool {
    fn default() -> Self {
        Self {
            buffer_size: 2 * 1024 * 1024,
            buffers: Vec::new(),
        }
    }
}

pub(crate) struct InstanceBuffer {
    metal_buffer: metal::Buffer,
    size: usize,
}

impl InstanceBufferPool {
    pub(crate) fn reset(&mut self, buffer_size: usize) {
        self.buffer_size = buffer_size;
        self.buffers.clear();
    }

    pub(crate) fn acquire(
        &mut self,
        device: &metal::Device,
        unified_memory: bool,
    ) -> InstanceBuffer {
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            let options = if unified_memory {
                MTLResourceOptions::StorageModeShared
                    // Buffers are write only which can benefit from the combined cache
                    // https://developer.apple.com/documentation/metal/mtlresourceoptions/cpucachemodewritecombined
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            };

            device.new_buffer(self.buffer_size as u64, options)
        });
        InstanceBuffer {
            metal_buffer: buffer,
            size: self.buffer_size,
        }
    }

    pub(crate) fn release(&mut self, buffer: InstanceBuffer) {
        if buffer.size == self.buffer_size {
            self.buffers.push(buffer.metal_buffer)
        }
    }
}

pub struct MetalRenderer {
    device: metal::Device,
    layer: Option<metal::MetalLayer>,
    is_apple_gpu: bool,
    is_unified_memory: bool,
    presents_with_transaction: bool,
    /// For headless rendering, tracks whether output should be opaque
    opaque: bool,
    command_queue: CommandQueue,
    paths_rasterization_pipeline_state: metal::RenderPipelineState,
    path_sprites_pipeline_state: metal::RenderPipelineState,
    shadows_pipeline_state: metal::RenderPipelineState,
    quads_pipeline_state: metal::RenderPipelineState,
    underlines_pipeline_state: metal::RenderPipelineState,
    monochrome_sprites_pipeline_state: metal::RenderPipelineState,
    polychrome_sprites_pipeline_state: metal::RenderPipelineState,
    surfaces_pipeline_state: metal::RenderPipelineState,
    external_surfaces: ExternalSurfacePipeline,
    /// The external-surface bridge's resource storage.
    ///
    /// Shared with whatever producer the window has handed out
    /// (through the platform producer accessor): the renderer resolves handles against it while
    /// drawing, and the external compositor registers and retires through it.
    external_registry: Rc<RefCell<ExternalSurfaceRegistry>>,
    unit_vertices: metal::Buffer,
    #[allow(clippy::arc_with_non_send_sync)]
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    sprite_atlas: Arc<MetalAtlas>,
    core_video_texture_cache: core_video::metal_texture_cache::CVMetalTextureCache,
    path_intermediate_texture: Option<metal::Texture>,
    path_intermediate_msaa_texture: Option<metal::Texture>,
    path_sample_count: u32,
    /// Offscreen render target reused across `render_scene` calls when
    /// rendering headlessly without reading pixels back.
    #[cfg(any(test, feature = "test-support"))]
    headless_render_target: Option<metal::Texture>,
}

#[repr(C)]
pub struct PathRasterizationVertex {
    pub xy_position: Point<ScaledPixels>,
    pub st_position: Point<f32>,
    pub color: Background,
    pub bounds: Bounds<ScaledPixels>,
}

/// The external-surface pipeline and the state it needs that no other pipeline here does.
pub(crate) struct ExternalSurfacePipeline {
    pipeline_state: metal::RenderPipelineState,
    /// `ExternalSampling::Nearest`.
    sampler_nearest: metal::SamplerState,
    /// `ExternalSampling::Linear`.
    sampler_linear: metal::SamplerState,
}

impl ExternalSurfacePipeline {
    pub(crate) fn new(device: &metal::DeviceRef, library: &metal::LibraryRef) -> Self {
        Self {
            pipeline_state: build_external_surface_pipeline_state(
                device,
                library,
                "external_surfaces",
                "external_surface_vertex",
                "external_surface_fragment",
                MTLPixelFormat::BGRA8Unorm,
            ),
            sampler_nearest: build_external_surface_sampler(
                device,
                "external_surface_sampler_nearest",
                metal::MTLSamplerMinMagFilter::Nearest,
            ),
            sampler_linear: build_external_surface_sampler(
                device,
                "external_surface_sampler_linear",
                metal::MTLSamplerMinMagFilter::Linear,
            ),
        }
    }

    fn sampler(&self, sampling: ExternalSampling) -> &metal::SamplerStateRef {
        match sampling {
            ExternalSampling::Nearest => &self.sampler_nearest,
            ExternalSampling::Linear => &self.sampler_linear,
        }
    }
}

impl MetalRenderer {
    /// Creates a new MetalRenderer with a CAMetalLayer for window-based rendering.
    pub fn new(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>, transparent: bool) -> Self {
        let device = Self::create_device();

        let layer = metal::MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        // Support direct-to-display rendering if the window is not transparent
        // https://developer.apple.com/documentation/metal/managing-your-game-window-for-metal-in-macos
        layer.set_opaque(!transparent);
        layer.set_maximum_drawable_count(MAX_DRAWABLE_COUNT);
        // Allow texture reading for visual tests (captures screenshots without ScreenCaptureKit)
        #[cfg(any(test, feature = "test-support"))]
        layer.set_framebuffer_only(false);
        unsafe {
            let _: () = msg_send![&*layer, setAllowsNextDrawableTimeout: NO];
            let _: () = msg_send![&*layer, setNeedsDisplayOnBoundsChange: YES];
            let _: () = msg_send![
                &*layer,
                setAutoresizingMask: AutoresizingMask::WIDTH_SIZABLE
                    | AutoresizingMask::HEIGHT_SIZABLE
            ];
        }

        Self::new_internal(device, Some(layer), !transparent, instance_buffer_pool)
    }

    /// Creates a new headless MetalRenderer for offscreen rendering without a window.
    ///
    /// This renderer can render scenes to images without requiring a CAMetalLayer,
    /// window, or AppKit. Use `render_scene_to_image()` to render scenes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_headless(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>) -> Self {
        let device = Self::create_device();
        Self::new_internal(device, None, true, instance_buffer_pool)
    }

    fn create_device() -> metal::Device {
        // Prefer low‐power integrated GPUs on Intel Mac. On Apple
        // Silicon, there is only ever one GPU, so this is equivalent to
        // `metal::Device::system_default()`.
        if let Some(d) = metal::Device::all()
            .into_iter()
            .min_by_key(|d| (d.is_removable(), !d.is_low_power()))
        {
            d
        } else {
            // For some reason `all()` can return an empty list, see https://github.com/zed-industries/zed/issues/37689
            // In that case, we fall back to the system default device.
            log::error!(
                "Unable to enumerate Metal devices; attempting to use system default device"
            );
            metal::Device::system_default().unwrap_or_else(|| {
                log::error!("unable to access a compatible graphics device");
                std::process::exit(1);
            })
        }
    }

    fn new_internal(
        device: metal::Device,
        layer: Option<metal::MetalLayer>,
        opaque: bool,
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    ) -> Self {
        let library = load_shader_library(&device);

        // Shared memory can be used only if CPU and GPU share the same memory space.
        // https://developer.apple.com/documentation/metal/setting-resource-storage-modes
        let is_unified_memory = device.has_unified_memory();
        // Apple GPU families support memoryless textures, which can significantly reduce
        // memory usage by keeping render targets in on-chip tile memory instead of
        // allocating backing store in system memory.
        // https://developer.apple.com/documentation/metal/mtlgpufamily
        let is_apple_gpu = device.supports_family(MTLGPUFamily::Apple1);

        let unit_vertices = build_unit_vertices(&device, is_unified_memory);

        let paths_rasterization_pipeline_state = build_path_rasterization_pipeline_state(
            &device,
            &library,
            "paths_rasterization",
            "path_rasterization_vertex",
            "path_rasterization_fragment",
            MTLPixelFormat::BGRA8Unorm,
            PATH_SAMPLE_COUNT,
        );
        let path_sprites_pipeline_state = build_path_sprite_pipeline_state(
            &device,
            &library,
            "path_sprites",
            "path_sprite_vertex",
            "path_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let shadows_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "shadows",
            "shadow_vertex",
            "shadow_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let quads_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "quads",
            "quad_vertex",
            "quad_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let underlines_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "underlines",
            "underline_vertex",
            "underline_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let monochrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "monochrome_sprites",
            "monochrome_sprite_vertex",
            "monochrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let polychrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "polychrome_sprites",
            "polychrome_sprite_vertex",
            "polychrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "surfaces",
            "surface_vertex",
            "surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let external_surfaces = ExternalSurfacePipeline::new(&device, &library);
        let external_registry = Rc::new(RefCell::new(ExternalSurfaceRegistry::new(device.clone())));

        let command_queue = device.new_command_queue();
        let sprite_atlas = Arc::new(MetalAtlas::new(device.clone(), is_apple_gpu));
        let core_video_texture_cache =
            CVMetalTextureCache::new(None, device.clone(), None).unwrap();

        Self {
            device,
            layer,
            presents_with_transaction: false,
            is_apple_gpu,
            is_unified_memory,
            opaque,
            command_queue,
            paths_rasterization_pipeline_state,
            path_sprites_pipeline_state,
            shadows_pipeline_state,
            quads_pipeline_state,
            underlines_pipeline_state,
            monochrome_sprites_pipeline_state,
            polychrome_sprites_pipeline_state,
            surfaces_pipeline_state,
            external_surfaces,
            external_registry,
            unit_vertices,
            instance_buffer_pool,
            sprite_atlas,
            core_video_texture_cache,
            path_intermediate_texture: None,
            path_intermediate_msaa_texture: None,
            path_sample_count: PATH_SAMPLE_COUNT,
            #[cfg(any(test, feature = "test-support"))]
            headless_render_target: None,
        }
    }

    pub fn layer(&self) -> Option<&metal::MetalLayerRef> {
        self.layer.as_ref().map(|l| l.as_ref())
    }

    pub fn layer_ptr(&self) -> *mut CAMetalLayer {
        self.layer
            .as_ref()
            .map(|l| l.as_ptr())
            .unwrap_or(ptr::null_mut())
    }

    pub fn sprite_atlas(&self) -> &Arc<MetalAtlas> {
        &self.sprite_atlas
    }

    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        if let Some(layer) = &self.layer {
            layer.set_presents_with_transaction(presents_with_transaction);
        }
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        if let Some(layer) = &self.layer {
            let ns_size = NSSize {
                width: size.width.0 as f64,
                height: size.height.0 as f64,
            };
            unsafe {
                let _: () = msg_send![
                    layer.as_ref(),
                    setDrawableSize: ns_size
                ];
            }
        }
        self.update_path_intermediate_textures(size);
    }

    fn update_path_intermediate_textures(&mut self, size: Size<DevicePixels>) {
        // We are uncertain when this happens, but sometimes size can be 0 here. Most likely before
        // the layout pass on window creation. Zero-sized texture creation causes SIGABRT.
        // https://github.com/zed-industries/zed/issues/36229
        if size.width.0 <= 0 || size.height.0 <= 0 {
            self.path_intermediate_texture = None;
            self.path_intermediate_msaa_texture = None;
            return;
        }

        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        self.path_intermediate_texture = Some(self.device.new_texture(&texture_descriptor));

        if self.path_sample_count > 1 {
            // https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus
            // Rendering MSAA textures are done in a single pass, so we can use memory-less storage on Apple Silicon
            let storage_mode = if self.is_apple_gpu {
                metal::MTLStorageMode::Memoryless
            } else {
                metal::MTLStorageMode::Private
            };

            let msaa_descriptor = texture_descriptor;
            msaa_descriptor.set_texture_type(metal::MTLTextureType::D2Multisample);
            msaa_descriptor.set_storage_mode(storage_mode);
            msaa_descriptor.set_sample_count(self.path_sample_count as _);
            self.path_intermediate_msaa_texture = Some(self.device.new_texture(&msaa_descriptor));
        } else {
            self.path_intermediate_msaa_texture = None;
        }
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.opaque = !transparent;
        if let Some(layer) = &self.layer {
            layer.set_opaque(!transparent);
        }
    }

    pub fn destroy(&self) {
        // nothing to do
    }

    pub fn draw(&mut self, scene: &Scene) {
        let layer = match &self.layer {
            Some(l) => l.clone(),
            None => {
                log::error!(
                    "draw() called on headless renderer - use render_scene_to_image() instead"
                );
                return;
            }
        };
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = if let Some(drawable) = layer.next_drawable() {
            drawable
        } else {
            log::error!(
                "failed to retrieve next drawable, drawable size: {:?}",
                viewport_size
            );
            return;
        };

        let command_buffer = match self.render_frame(scene, drawable.texture(), viewport_size) {
            Ok(command_buffer) => command_buffer,
            Err(error) => {
                log::error!("failed to render: {error:#}");
                return;
            }
        };

        if self.presents_with_transaction {
            command_buffer.commit();
            command_buffer.wait_until_scheduled();
            drawable.present();
        } else {
            command_buffer.present_drawable(drawable);
            command_buffer.commit();
        }
    }

    fn render_frame(
        &mut self,
        scene: &Scene,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let mut writer = InstanceBufferWriter::new(
            &self.device,
            &self.instance_buffer_pool,
            self.is_unified_memory,
        );
        let instance_bindings = write_instances(scene, &mut writer).with_context(|| {
            format!(
                "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} mono, {} poly, {} surfaces",
                scene.paths.len(),
                scene.shadows.len(),
                scene.quads.len(),
                scene.underlines.len(),
                scene.monochrome_sprites.len(),
                scene.polychrome_sprites.len(),
                scene.surfaces.len(),
            )
        })?;
        let command_buffer = self.draw_primitives_to_texture(
            scene,
            &instance_bindings,
            &mut writer,
            texture,
            viewport_size,
        )?;

        let instance_buffer_pool = self.instance_buffer_pool.clone();
        let instance_buffer = Cell::new(Some(writer.finish()));
        let block = ConcreteBlock::new(move |_| {
            if let Some(instance_buffer) = instance_buffer.take() {
                instance_buffer_pool.lock().release(instance_buffer);
            }
        });
        let block = block.copy();
        command_buffer.add_completed_handler(&block);

        Ok(command_buffer)
    }

    /// Renders the scene to a texture and returns the pixel data as an RGBA image.
    /// This does not present the frame to screen - useful for visual testing
    /// where we want to capture what would be rendered without displaying it.
    ///
    /// Note: This requires a layer-backed renderer. For headless rendering,
    /// use `render_scene_to_image()` instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<RgbaImage> {
        let layer = self
            .layer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("render_to_image requires a layer-backed renderer"))?;
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = layer
            .next_drawable()
            .ok_or_else(|| anyhow::anyhow!("Failed to get drawable for render_to_image"))?;

        let command_buffer = self.render_frame(scene, drawable.texture(), viewport_size)?;

        // Commit and wait for completion without presenting
        command_buffer.commit();
        command_buffer.wait_until_completed();

        read_texture_to_image(drawable.texture())
    }

    /// Renders a scene to an image without requiring a window or CAMetalLayer.
    ///
    /// This is the primary method for headless rendering. It creates an offscreen
    /// texture, renders the scene to it, and returns the pixel data as an RGBA image.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene_to_image: {:?}", size);
        }

        // Update path intermediate textures for this size
        self.update_path_intermediate_textures(size);

        // Create an offscreen texture as render target
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Managed);
        let target_texture = self.device.new_texture(&texture_descriptor);

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // On discrete GPUs (non-unified memory), Managed textures require an
        // explicit blit synchronize before the CPU can read back the rendered
        // data. Without this, get_bytes returns stale zeros.
        if !self.is_unified_memory {
            let blit = command_buffer.new_blit_command_encoder();
            blit.synchronize_resource(&target_texture);
            blit.end_encoding();
        }

        // Commit and wait for completion
        command_buffer.commit();
        command_buffer.wait_until_completed();

        read_texture_to_image(&target_texture)
    }

    /// Renders a scene to a reused offscreen texture without reading pixels
    /// back or blocking on GPU completion.
    ///
    /// This mirrors the CPU cost of presenting a frame to a window (scene
    /// encoding, instance buffer writes, command submission) and is used by
    /// headless benchmark rendering, where the produced pixels are never
    /// inspected.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene: {:?}", size);
        }

        self.update_path_intermediate_textures(size);

        let needs_new_target = self.headless_render_target.as_ref().is_none_or(|texture| {
            texture.width() != size.width.0 as u64 || texture.height() != size.height.0 as u64
        });
        if needs_new_target {
            let texture_descriptor = metal::TextureDescriptor::new();
            texture_descriptor.set_width(size.width.0 as u64);
            texture_descriptor.set_height(size.height.0 as u64);
            texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            texture_descriptor.set_usage(
                metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead,
            );
            texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            self.headless_render_target = Some(self.device.new_texture(&texture_descriptor));
        }
        let target_texture = self
            .headless_render_target
            .clone()
            .expect("just ensured the render target exists");

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // Commit without waiting, mirroring presentation to a real window where
        // the CPU doesn't block on the GPU.
        command_buffer.commit();
        Ok(())
    }

    fn draw_primitives_to_texture(
        &mut self,
        scene: &Scene,
        instance_bindings: &InstanceBindings,
        writer: &mut InstanceBufferWriter,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let command_queue = self.command_queue.clone();
        let command_buffer = command_queue.new_command_buffer();
        let alpha = if self.opaque { 1. } else { 0. };

        let mut command_encoder = new_command_encoder_for_texture(
            command_buffer,
            texture,
            viewport_size,
            Some(metal::MTLClearColor::new(0., 0., 0., alpha)),
        );

        for batch in scene.batches() {
            match batch {
                PrimitiveBatch::Shadows(range) => {
                    self.draw_shadows(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Quads(range) => {
                    self.draw_quads(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Paths(range) => {
                    let paths = &scene.paths[range];
                    command_encoder.end_encoding();

                    let did_draw = self.draw_paths_to_intermediate(
                        paths,
                        writer,
                        viewport_size,
                        command_buffer,
                    )?;

                    command_encoder = new_command_encoder_for_texture(
                        command_buffer,
                        texture,
                        viewport_size,
                        None,
                    );

                    if did_draw {
                        if let Err(error) = self.draw_paths_from_intermediate(
                            paths,
                            writer,
                            viewport_size,
                            command_encoder,
                        ) {
                            command_encoder.end_encoding();
                            return Err(error);
                        }
                    }
                }
                PrimitiveBatch::Underlines(range) => {
                    self.draw_underlines(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::MonochromeSprites { texture_id, range } => self
                    .draw_monochrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::PolychromeSprites { texture_id, range } => self
                    .draw_polychrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::Surfaces(range) => self.draw_surfaces(
                    &scene.surfaces[range.clone()],
                    range.start,
                    instance_bindings,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::SubpixelSprites { .. } => unreachable!(),
            }
        }

        command_encoder.end_encoding();

        Ok(command_buffer.to_owned())
    }

    fn draw_paths_to_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_buffer: &metal::CommandBufferRef,
    ) -> Result<bool> {
        if paths.is_empty() {
            return Ok(false);
        }
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        let mut vertices = Vec::new();
        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.bounds.intersect(&path.content_mask.bounds),
            }));
        }
        let vertex_instance_bindings = writer.write(&vertices)?;

        let render_pass_descriptor = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_descriptor
            .color_attachments()
            .object_at(0)
            .unwrap();
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0., 0., 0., 0.));

        if let Some(msaa_texture) = &self.path_intermediate_msaa_texture {
            color_attachment.set_texture(Some(msaa_texture));
            color_attachment.set_resolve_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::MultisampleResolve);
        } else {
            color_attachment.set_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);
        }

        let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
        command_encoder.set_render_pipeline_state(&self.paths_rasterization_pipeline_state);
        command_encoder.set_vertex_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            PathRasterizationInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.draw_primitives(
            metal::MTLPrimitiveType::Triangle,
            0,
            vertices.len() as u64,
        );

        command_encoder.end_encoding();
        Ok(true)
    }

    fn draw_shadows(
        &self,
        shadows: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if shadows.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.shadows_pipeline_state);
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            ShadowInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            shadows.len() as u64,
            shadows.start as u64,
        );
    }

    fn draw_quads(
        &self,
        quads: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if quads.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.quads_pipeline_state);
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            QuadInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            quads.len() as u64,
            quads.start as u64,
        );
    }

    fn draw_paths_from_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> Result<()> {
        let Some(first_path) = paths.first() else {
            return Ok(());
        };
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        command_encoder.set_render_pipeline_state(&self.path_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.set_fragment_texture(
            SpriteInputIndex::AtlasTexture as u64,
            Some(intermediate_texture),
        );

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites;
        if paths.last().unwrap().order == first_path.order {
            sprites = paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect();
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            sprites = vec![PathSprite { bounds }];
        }

        let sprite_instance_bindings = writer.write(&sprites)?;
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&sprite_instance_bindings.buffer),
            sprite_instance_bindings.offset as u64,
        );

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        Ok(())
    }

    fn draw_underlines(
        &self,
        underlines: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if underlines.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.underlines_pipeline_state);
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            UnderlineInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            underlines.len() as u64,
            underlines.start as u64,
        );
    }

    fn draw_monochrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.monochrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    fn draw_polychrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.polychrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    /// Composites one batch of surfaces, of either kind, in scene order.
    ///
    /// `SurfaceSource` has two variants on this platform and they are drawn by two different
    /// pipelines: the CoreVideo/NV12 path that has always been here, and the bounded
    /// external-surface bridge. They interleave in a single batch because `Scene::batches()` groups
    /// by primitive kind and not by source, so the batch is walked as maximal runs of one kind and
    /// each run is handed to its own drawer. Splitting instead into "all CoreVideo, then all
    /// external" would be simpler and wrong: it would silently reorder two overlapping surfaces.
    fn draw_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        first_surface: usize,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if surfaces.is_empty() {
            return;
        }

        // Cloned rather than borrowed in place so that the borrow of the registry does not outlive
        // this statement and keep `self` locked for the whole walk.
        let registry = Rc::clone(&self.external_registry);
        let mut registry = registry.borrow_mut();

        let is_external =
            |surface: &PaintSurface| matches!(surface.source, SurfaceSource::External(_));
        let mut start = 0;
        while start < surfaces.len() {
            let external = is_external(&surfaces[start]);
            let mut end = start + 1;
            while end < surfaces.len() && is_external(&surfaces[end]) == external {
                end += 1;
            }

            if external {
                draw_external_surfaces_into_encoder(
                    &self.external_surfaces,
                    &self.unit_vertices,
                    viewport_size,
                    &mut registry,
                    &surfaces[start..end],
                    command_encoder,
                );
            } else {
                self.draw_core_video_surfaces(
                    &surfaces[start..end],
                    first_surface + start,
                    instance_bindings,
                    viewport_size,
                    command_encoder,
                );
            }
            start = end;
        }
    }

    /// The CoreVideo/NV12 half of [`Self::draw_surfaces`], unchanged.
    fn draw_core_video_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        first_surface: usize,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if surfaces.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.surfaces_pipeline_state);
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Surfaces as u64,
            Some(&instance_bindings.surfaces.buffer),
            instance_bindings.surfaces.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SurfaceInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        for (index, surface) in surfaces.iter().enumerate() {
            let image_buffer = match &surface.source {
                SurfaceSource::Surface(image_buffer) => image_buffer,
                // `draw_surfaces` splits the batch by source before it gets here, so this run holds
                // no external surfaces at all. The arm exists only to keep the match total.
                SurfaceSource::External(_) => continue,
            };

            let texture_size = size(
                DevicePixels::from(image_buffer.get_width() as i32),
                DevicePixels::from(image_buffer.get_height() as i32),
            );

            assert_eq!(
                image_buffer.get_pixel_format(),
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            );

            let y_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::R8Unorm,
                    image_buffer.get_width_of_plane(0),
                    image_buffer.get_height_of_plane(0),
                    0,
                )
                .unwrap();
            let cb_cr_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::RG8Unorm,
                    image_buffer.get_width_of_plane(1),
                    image_buffer.get_height_of_plane(1),
                    1,
                )
                .unwrap();

            command_encoder.set_vertex_bytes(
                SurfaceInputIndex::TextureSize as u64,
                mem::size_of_val(&texture_size) as u64,
                &texture_size as *const Size<DevicePixels> as *const _,
            );
            // let y_texture = y_texture.get_texture().unwrap().
            command_encoder.set_fragment_texture(SurfaceInputIndex::YTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(y_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });
            command_encoder.set_fragment_texture(SurfaceInputIndex::CbCrTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(cb_cr_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });

            command_encoder.draw_primitives_instanced_base_instance(
                metal::MTLPrimitiveType::Triangle,
                0,
                6,
                1,
                (first_surface + index) as u64,
            );
        }
    }

    /// The external-surface capability and budget snapshot of this backend.
    ///
    /// This is what `Window::external_surface_capabilities` reports on macOS, and the budgets in it
    /// are the ones the registry actually enforces.
    #[doc(hidden)]
    pub fn external_surface_capabilities(&self) -> ExternalSurfaceCapabilities {
        self.external_registry.borrow().capabilities()
    }

    /// The producer face of the external-surface bridge for this renderer.
    ///
    /// This is the accessor decision D-K16 names, and it is for the single privileged external
    /// compositor. See [`ExternalSurfaceProducer`] for what it grants and what it deliberately does
    /// not. Ordinary GPUI consumers want `Window::paint_external_surface` and
    /// `Window::external_surface_capabilities` instead, which never expose a device.
    #[doc(hidden)]
    pub fn external_surface_producer(&self) -> ExternalSurfaceProducer {
        ExternalSurfaceProducer::new(
            self.device.clone(),
            self.command_queue.clone(),
            Rc::clone(&self.external_registry),
        )
    }
}

/// One external surface, as the vertex shader reads it.
///
/// The field order and the trailing pad word mirror `ExternalSurfaceInstance` in `shaders.metal`
/// exactly — the header the shader includes is generated from this very declaration — and they
/// mirror the D3D11 and WGSL backends' instance structs too, so the three sides of the bridge stay
/// diffable. The affine is carried as `TransformationMatrix`'s two row-major rows plus a
/// translation, which is how every other GPUI pipeline already carries it.
///
/// The content mask is absent on purpose: the clip is a scissor rectangle, not a shader input.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ExternalSurfaceInstance {
    /// The target placement.
    pub bounds: Bounds<ScaledPixels>,
    /// The crop, normalized against the registered surface size. The whole surface is `0..1`.
    pub source_uv: Bounds<f32>,
    /// The affine, applied about the top-left corner of `bounds`.
    pub transform: TransformationMatrix,
    /// The group opacity, of which this composite is the sole owner.
    pub opacity: f32,
    /// Keeps the struct's size the same 64 bytes the other backends' instances are, and keeps the
    /// C layout free of tail padding the shader would have to guess at.
    pub pad: u32,
}

const _: () = assert!(std::mem::size_of::<ExternalSurfaceInstance>() == 64);

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
) -> Option<Bounds<f32>> {
    let whole_surface = Bounds {
        origin: Point { x: 0.0, y: 0.0 },
        size: Size {
            width: 1.0,
            height: 1.0,
        },
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
    Some(Bounds {
        origin: Point {
            x: x as f32 / surface_width,
            y: y as f32 / surface_height,
        },
        size: Size {
            width: width as f32 / surface_width,
            height: height as f32 / surface_height,
        },
    })
}

/// Turns a content mask into a scissor rectangle clamped to the viewport, or `None` when it clips
/// everything away.
///
/// GPUI snaps a content mask to whole device pixels before it reaches a primitive, so rounding to
/// the nearest integer is exact here rather than a policy choice. Metal rejects a scissor rectangle
/// that leaves the render target, hence the clamp.
fn scissor_rect(
    content_mask: Bounds<ScaledPixels>,
    viewport_size: Size<DevicePixels>,
) -> Option<metal::MTLScissorRect> {
    let width = viewport_size.width.0.max(0) as f32;
    let height = viewport_size.height.0.max(0) as f32;
    let to_pixel = |value: f32, limit: f32| value.round().clamp(0.0, limit) as u64;

    let left = to_pixel(content_mask.origin.x.0, width);
    let top = to_pixel(content_mask.origin.y.0, height);
    let right = to_pixel(content_mask.origin.x.0 + content_mask.size.width.0, width);
    let bottom = to_pixel(content_mask.origin.y.0 + content_mask.size.height.0, height);

    (left < right && top < bottom).then_some(metal::MTLScissorRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// Composites externally produced surfaces into whatever render target `command_encoder` is
/// attached to, in the frozen order of the bridge contract.
///
/// This is the whole external half of [`MetalRenderer::draw_surfaces`], lifted out of the
/// renderer's own borrows and given the four things it actually needs — the external-surface
/// pipeline with its two samplers, the shared unit-quad vertex buffer, the viewport the content
/// mask is clamped against, and the registry the opaque handles resolve against. Nothing about the
/// sequence changes: crop, placement into `bounds`, the affine about the top-left corner of
/// `bounds`, the content-mask clip, then the group opacity — the first three computed here and
/// finished in `external_surface_vertex`, the clip a scissor rectangle and the opacity the fragment
/// shader's single multiply.
///
/// The seam exists so that the pixels this path produces can be observed without a window: a test
/// attaches an offscreen texture to its own encoder, calls this, and reads the result back. Every
/// other caller reaches it through [`MetalRenderer::draw_surfaces`].
///
/// A surface whose handle no longer resolves — a stale generation, or an id that was retired — is
/// skipped and counted, never a panic and never a draw from stale content: `allow_stale_reuse` is
/// off in the capability snapshot.
fn draw_external_surfaces_into_encoder(
    pipeline: &ExternalSurfacePipeline,
    unit_vertices: &metal::Buffer,
    viewport_size: Size<DevicePixels>,
    registry: &mut ExternalSurfaceRegistry,
    surfaces: &[PaintSurface],
    command_encoder: &metal::RenderCommandEncoderRef,
) {
    let mut drew_anything = false;
    for surface in surfaces {
        let descriptor = match &surface.source {
            SurfaceSource::External(descriptor) => descriptor,
            // The CoreVideo path is drawn by `MetalRenderer::draw_core_video_surfaces`; the batch is
            // split by source before either of them is called.
            SurfaceSource::Surface(_) => continue,
        };
        let handle = descriptor.handle;

        let resolved = registry
            .resolve(handle)
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
        let Some(scissor) = scissor_rect(surface.content_mask.bounds, viewport_size) else {
            continue;
        };

        let instance = ExternalSurfaceInstance {
            bounds: surface.bounds,
            source_uv,
            transform: surface.transform,
            opacity: surface.opacity,
            pad: 0,
        };

        command_encoder.set_render_pipeline_state(&pipeline.pipeline_state);
        command_encoder.set_scissor_rect(scissor);
        command_encoder.set_vertex_buffer(
            ExternalSurfaceInputIndex::Vertices as u64,
            Some(unit_vertices),
            0,
        );
        // One instance per draw, bound inline rather than through the frame's instance buffer:
        // external surfaces are few, and each one needs its own scissor rectangle and texture
        // anyway, so there is nothing a shared buffer would amortize.
        command_encoder.set_vertex_bytes(
            ExternalSurfaceInputIndex::Surface as u64,
            mem::size_of::<ExternalSurfaceInstance>() as u64,
            &instance as *const ExternalSurfaceInstance as *const _,
        );
        command_encoder.set_vertex_bytes(
            ExternalSurfaceInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder
            .set_fragment_texture(ExternalSurfaceInputIndex::Texture as u64, Some(&texture));
        command_encoder.set_fragment_sampler_state(
            ExternalSurfaceInputIndex::Sampler as u64,
            Some(pipeline.sampler(descriptor.sampling)),
        );
        command_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 6);
        // Contract 1.1: the binding proof is recorded here and nowhere earlier. `resolve` above is
        // not evidence — a resolved surface can still be skipped by the crop or content-mask
        // checks — so a publication becomes `Bound` only once a draw command has actually been
        // issued for one of its occurrences. A partially clipped surface reaches this line and
        // counts; a fully clipped one never does.
        registry.note_drawn(handle);
        drew_anything = true;
    }

    if drew_anything {
        // Restored unconditionally after the run: a scissor rectangle outlives the draw that set
        // it, so leaving the last surface's content mask installed would clip every batch drawn
        // after this one.
        command_encoder.set_scissor_rect(metal::MTLScissorRect {
            x: 0,
            y: 0,
            width: viewport_size.width.0.max(0) as u64,
            height: viewport_size.height.0.max(0) as u64,
        });
    }
}

fn new_command_encoder_for_texture<'a>(
    command_buffer: &'a metal::CommandBufferRef,
    texture: &'a metal::TextureRef,
    viewport_size: Size<DevicePixels>,
    clear_color: Option<metal::MTLClearColor>,
) -> &'a metal::RenderCommandEncoderRef {
    let render_pass_descriptor = metal::RenderPassDescriptor::new();
    let color_attachment = render_pass_descriptor
        .color_attachments()
        .object_at(0)
        .unwrap();
    color_attachment.set_texture(Some(texture));
    color_attachment.set_store_action(metal::MTLStoreAction::Store);
    if let Some(clear_color) = clear_color {
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(clear_color);
    } else {
        color_attachment.set_load_action(metal::MTLLoadAction::Load);
    }

    let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
    command_encoder.set_viewport(metal::MTLViewport {
        originX: 0.0,
        originY: 0.0,
        width: i32::from(viewport_size.width) as f64,
        height: i32::from(viewport_size.height) as f64,
        znear: 0.0,
        zfar: 1.0,
    });
    command_encoder
}

#[cfg(any(test, feature = "test-support"))]
fn read_texture_to_image(texture: &metal::TextureRef) -> Result<RgbaImage> {
    let width = texture.width() as u32;
    let height = texture.height() as u32;
    let bytes_per_row = width as usize * 4;
    let mut pixels = vec![0u8; height as usize * bytes_per_row];

    let region = metal::MTLRegion {
        origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        },
    };
    texture.get_bytes(
        pixels.as_mut_ptr() as *mut std::ffi::c_void,
        bytes_per_row as u64,
        region,
        0,
    );

    // Convert BGRA to RGBA (swap B and R channels)
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    RgbaImage::from_raw(width, height, pixels).context("failed to create RgbaImage from pixel data")
}

/// The unit quad every pipeline here draws: two triangles walking `(0,0)..(1,1)`.
///
/// Extracted alongside [`load_shader_library`] so that the external-surface pipeline's GPU corpus
/// test draws from the very same vertices the renderer does, rather than from a second copy of them
/// that could drift.
pub(crate) fn build_unit_vertices(
    device: &metal::DeviceRef,
    is_unified_memory: bool,
) -> metal::Buffer {
    fn to_float2_bits(point: PointF) -> u64 {
        let mut output = point.y.to_bits() as u64;
        output <<= 32;
        output |= point.x.to_bits() as u64;
        output
    }

    let unit_vertices = [
        to_float2_bits(point(0., 0.)),
        to_float2_bits(point(1., 0.)),
        to_float2_bits(point(0., 1.)),
        to_float2_bits(point(0., 1.)),
        to_float2_bits(point(1., 0.)),
        to_float2_bits(point(1., 1.)),
    ];
    device.new_buffer_with_data(
        unit_vertices.as_ptr() as *const c_void,
        mem::size_of_val(&unit_vertices) as u64,
        if is_unified_memory {
            MTLResourceOptions::StorageModeShared | MTLResourceOptions::CPUCacheModeWriteCombined
        } else {
            MTLResourceOptions::StorageModeManaged
        },
    )
}

/// The shader library every pipeline here is built from.
///
/// Extracted so the external-surface pipeline can be built outside a full renderer — its GPU corpus
/// test builds one on a bare device — without that test reaching for a second copy of the
/// `runtime_shaders` branch.
pub(crate) fn load_shader_library(device: &metal::DeviceRef) -> metal::Library {
    #[cfg(feature = "runtime_shaders")]
    let library = device
        .new_library_with_source(SHADERS_SOURCE_FILE, &metal::CompileOptions::new())
        .expect("error building metal library");
    #[cfg(not(feature = "runtime_shaders"))]
    let library = device
        .new_library_with_data(SHADERS_METALLIB)
        .expect("error building metal library");
    library
}

fn build_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::SourceAlpha);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_sprite_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

/// The pipeline of the bounded external-surface bridge: **premultiplied** alpha.
///
/// This is the one place GPUI's Metal blend state had to be pinned rather than inherited. The
/// general [`build_pipeline_state`] — which the CoreVideo `surfaces` pipeline uses — sets
/// `SourceAlpha`/`OneMinusSourceAlpha` for colour, i.e. **straight** alpha: it premultiplies the
/// source itself. Feeding already-premultiplied external content through it would premultiply a
/// second time and darken the surface against its own alpha. So the external pipeline takes the
/// premultiplied pair the path pipelines use instead — `One`/`OneMinusSourceAlpha`, on colour and
/// on alpha alike — which is the same `ONE`/`INV_SRC_ALPHA` on all four channels the D3D11 and wgpu
/// backends install, and the reason the three backends' corpus results are comparable at all
/// (D-K13).
///
/// No existing pipeline is touched. There is deliberately no cull mode set either: the default is
/// `None`, and that is what makes a negative-determinant affine legal — mirroring is a legitimate
/// transform under the contract, not a reason to drop the quad.
fn build_external_surface_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

/// A sampler of the external-surface pipeline, in one of the contract's two sampling modes.
///
/// The addressing is **clamp-to-edge** in both, and that is load-bearing rather than a default: a
/// crop names a sub-rectangle of the surface, and with repeat or mirror addressing a linear filter
/// on the crop's edge would pull in texels from the opposite side of the surface. Clamping keeps a
/// crop's edge the crop's edge. There are no mipmaps in contract v1, so mip filtering is off.
fn build_external_surface_sampler(
    device: &metal::DeviceRef,
    label: &str,
    filter: metal::MTLSamplerMinMagFilter,
) -> metal::SamplerState {
    let descriptor = metal::SamplerDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_min_filter(filter);
    descriptor.set_mag_filter(filter);
    descriptor.set_mip_filter(metal::MTLSamplerMipFilter::NotMipmapped);
    descriptor.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
    descriptor.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
    descriptor.set_address_mode_r(metal::MTLSamplerAddressMode::ClampToEdge);
    device.new_sampler(&descriptor)
}

fn build_path_rasterization_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
    path_sample_count: u32,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    if path_sample_count > 1 {
        descriptor.set_raster_sample_count(path_sample_count as _);
        descriptor.set_alpha_to_coverage_enabled(false);
    }
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

#[derive(Clone)]
struct InstanceBinding {
    buffer: metal::Buffer,
    offset: usize,
}

struct InstanceBindings {
    quads: InstanceBinding,
    shadows: InstanceBinding,
    underlines: InstanceBinding,
    monochrome_sprites: InstanceBinding,
    polychrome_sprites: InstanceBinding,
    surfaces: InstanceBinding,
}

fn write_instances(scene: &Scene, writer: &mut InstanceBufferWriter) -> Result<InstanceBindings> {
    Ok(InstanceBindings {
        quads: writer.write(&scene.quads)?,
        shadows: writer.write(&scene.shadows)?,
        underlines: writer.write(&scene.underlines)?,
        monochrome_sprites: writer.write(&scene.monochrome_sprites)?,
        polychrome_sprites: writer.write(&scene.polychrome_sprites)?,
        surfaces: writer.write_iter(scene.surfaces.iter().map(|surface| SurfaceBounds {
            bounds: surface.bounds,
            content_mask: surface.content_mask,
        }))?,
    })
}

struct InstanceBufferWriter {
    device: metal::Device,
    pool: Arc<Mutex<InstanceBufferPool>>,
    unified_memory: bool,
    filled: Vec<(InstanceBuffer, usize)>,
    current: InstanceBuffer,
    offset: usize,
}

impl InstanceBufferWriter {
    fn new(
        device: &metal::Device,
        pool: &Arc<Mutex<InstanceBufferPool>>,
        unified_memory: bool,
    ) -> Self {
        let current = pool.lock().acquire(device, unified_memory);
        Self {
            device: device.clone(),
            pool: pool.clone(),
            unified_memory,
            filled: Vec::new(),
            current,
            offset: 0,
        }
    }

    fn allocate<T>(&mut self, count: usize) -> Result<(InstanceBinding, &mut [MaybeUninit<T>])> {
        let size = mem::size_of::<T>() * count;
        let mut offset = self.offset.next_multiple_of(INSTANCE_BUFFER_ALIGNMENT);
        if offset + size > self.current.size {
            self.grow(size)?;
            offset = 0;
        }
        self.offset = offset + size;

        let binding = InstanceBinding {
            buffer: self.current.metal_buffer.clone(),
            offset,
        };
        // Safety: the reservation lies within a buffer this frame owns
        // exclusively, and never overlaps one handed out earlier.
        let values = unsafe {
            let start = (self.current.metal_buffer.contents() as *mut u8).add(offset);
            slice::from_raw_parts_mut(start.cast::<MaybeUninit<T>>(), count)
        };
        Ok((binding, values))
    }

    fn write<T>(&mut self, values: &[T]) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        unsafe {
            ptr::copy_nonoverlapping(
                values.as_ptr(),
                destination.as_mut_ptr().cast::<T>(),
                values.len(),
            );
        }
        Ok(binding)
    }

    fn write_iter<T>(
        &mut self,
        values: impl ExactSizeIterator<Item = T>,
    ) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        for (slot, value) in destination.iter_mut().zip(values) {
            slot.write(value);
        }
        Ok(binding)
    }

    fn grow(&mut self, required: usize) -> Result<()> {
        let mut pool = self.pool.lock();
        let buffer_size = (pool.buffer_size * 2)
            .max(required.next_power_of_two())
            .min(MAX_INSTANCE_BUFFER_SIZE);
        anyhow::ensure!(
            buffer_size >= required,
            "instance buffer needs {required} bytes, above the maximum of {MAX_INSTANCE_BUFFER_SIZE}"
        );
        anyhow::ensure!(
            buffer_size > self.current.size,
            "frame instance data exceeds the {MAX_INSTANCE_BUFFER_SIZE}-byte maximum"
        );
        if buffer_size != pool.buffer_size {
            log::info!("increased instance buffer size to {buffer_size}");
            pool.reset(buffer_size);
        }
        let buffer = pool.acquire(&self.device, self.unified_memory);
        drop(pool);

        let filled = mem::replace(&mut self.current, buffer);
        self.filled.push((filled, self.offset));
        self.offset = 0;
        Ok(())
    }

    fn finish(self) -> InstanceBuffer {
        let Self {
            unified_memory,
            filled,
            current,
            offset,
            ..
        } = self;

        if !unified_memory {
            for (buffer, written) in &filled {
                if *written == 0 {
                    continue;
                }
                buffer.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: *written as NSUInteger,
                });
            }
            if offset > 0 {
                current.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: offset as NSUInteger,
                });
            }
        }

        // Metal retains encoded resources until the command buffer completes.
        // Only the final, largest buffer is worth keeping in the pool.
        drop(filled);
        current
    }
}

#[repr(C)]
enum ShadowInputIndex {
    Vertices = 0,
    Shadows = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum QuadInputIndex {
    Vertices = 0,
    Quads = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum UnderlineInputIndex {
    Vertices = 0,
    Underlines = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum SpriteInputIndex {
    Vertices = 0,
    Sprites = 1,
    ViewportSize = 2,
    AtlasTextureSize = 3,
    AtlasTexture = 4,
}

#[repr(C)]
enum SurfaceInputIndex {
    Vertices = 0,
    Surfaces = 1,
    ViewportSize = 2,
    TextureSize = 3,
    YTexture = 4,
    CbCrTexture = 5,
}

/// The binding slots of the external-surface pipeline.
///
/// Buffers, textures and samplers are separate namespaces in Metal, so the values do not have to be
/// distinct across the three; they are kept distinct anyway, the way every other input-index enum
/// here is, so a slot number reads unambiguously in a capture.
#[repr(C)]
enum ExternalSurfaceInputIndex {
    Vertices = 0,
    Surface = 1,
    ViewportSize = 2,
    Texture = 3,
    Sampler = 4,
}

#[repr(C)]
enum PathRasterizationInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PathSprite {
    pub bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SurfaceBounds {
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}

#[cfg(any(test, feature = "test-support"))]
pub struct MetalHeadlessRenderer {
    renderer: MetalRenderer,
}

#[cfg(any(test, feature = "test-support"))]
impl MetalHeadlessRenderer {
    pub fn new() -> Self {
        let instance_buffer_pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let renderer = MetalRenderer::new_headless(instance_buffer_pool);
        Self { renderer }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl gpui::PlatformHeadlessRenderer for MetalHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.renderer.render_scene_to_image(scene, size)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.renderer.render_scene(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }
}

#[cfg(test)]
mod external_surface_layout_tests {
    use super::ExternalSurfaceInstance;
    use std::mem::{offset_of, size_of};

    /// The instance is read by `ExternalSurfaceInstance` in `shaders.metal`, and nothing but this
    /// pins the two together at the byte level.
    ///
    /// The shader's view of the struct is generated from this declaration by cbindgen, so the two
    /// cannot disagree about *field order*. What they can still disagree about is *padding*: the
    /// Metal compiler lays out the generated C struct itself, and the trailing `pad` word is what
    /// keeps its size a round 64 bytes with no tail padding to guess at. The offsets are the same
    /// ones `ExternalSurfaceInstance` in `directx_renderer.rs` and `ExternalSurface` in
    /// `shaders.wgsl` carry, which is what makes the three backends' corpora comparable.
    #[test]
    fn the_instance_layout_matches_the_shader_and_the_other_backends() {
        assert_eq!(offset_of!(ExternalSurfaceInstance, bounds), 0);
        assert_eq!(offset_of!(ExternalSurfaceInstance, source_uv), 16);
        assert_eq!(offset_of!(ExternalSurfaceInstance, transform), 32);
        assert_eq!(offset_of!(ExternalSurfaceInstance, opacity), 56);
        assert_eq!(offset_of!(ExternalSurfaceInstance, pad), 60);
        assert_eq!(size_of::<ExternalSurfaceInstance>(), 64);
    }
}

#[cfg(test)]
mod external_surface_draw_tests {
    //! The GPUI-side counterpart of the S1 spike's pixel corpus, run through the Metal consumer
    //! path — the sixth and last of the six runtime profiles.
    //!
    //! The spikes proved the *mechanism* outside GPUI: a producer pass fills a texture, a consumer
    //! pass samples it in the same frame, and named pixels of the result are compared byte for byte
    //! against fixed constants. What they could not prove is that GPUI's own consumer path agrees
    //! with them — that `external_surface_vertex` reads the instance the way
    //! `ExternalSurfaceInstance` writes it, that the affine lands the right way round on screen,
    //! that the content mask clips where it says it does, and that the pipeline builds at all
    //! against GPUI's own shader library.
    //!
    //! These tests run that corpus through the real renderer code, and deliberately against the
    //! same constants and probe coordinates as the D3D11 corpus in
    //! `gpui_windows/src/directx_renderer.rs` and the wgpu corpus in
    //! `gpui_wgpu/src/wgpu_renderer.rs`: same producer pattern (clear to the generation colour,
    //! yellow marker triangle in the top-left corner at NDC (-1,1), (-0.5,1), (-1,0.5)), same
    //! 800x600 frame, same target rectangle at NDC (-0.5,-0.5)..(0.5,0.5), same colour constants,
    //! same probes. A disagreement between the harnesses is therefore a disagreement about a
    //! backend's pipeline and nothing else. The only difference is the target — an offscreen
    //! texture instead of a drawable — reached through [`draw_external_surfaces_into_encoder`],
    //! which is the function `MetalRenderer::draw_surfaces` itself calls.
    //!
    //! The producer pass here is deliberately encoded in a **separate `MTLCommandBuffer`**,
    //! committed on GPUI's queue before the consumer's is even created, and nothing waits on it.
    //! That is the S3 spike's ordering property under test rather than assumed: if submission order
    //! on a shared queue were not enough, the centre probe would read the clear colour of an
    //! unfinished producer pass instead of the generation colour, and `SameQueueOrdered` in the
    //! capability snapshot would be a claim this backend could not back.
    //!
    //! Everything returns early when the host has no Metal device at all, the same way the
    //! device-backed registry tests do.

    // Deliberately not `use super::*`: the parent module glob-imports nothing, but keeping the
    // imports explicit is what makes the three corpora diffable line for line.
    use super::{
        ExternalSurfacePipeline, MAX_DRAWABLE_COUNT, build_unit_vertices,
        draw_external_surfaces_into_encoder, load_shader_library, new_command_encoder_for_texture,
    };
    use crate::external_registry::ExternalSurfaceRegistry;
    use gpui::{
        Bounds, ContentMask, DevicePixels, ExternalAlphaMode, ExternalColorSpace, ExternalSampling,
        ExternalSurfaceDescriptor, ExternalSurfaceFormat, ExternalSurfaceHandle, ExternalSyncToken,
        PaintSurface, Point, ScaledPixels, Size, SurfaceSource, TransformationMatrix,
    };
    use metal::{MTLPixelFormat, MTLStorageMode, MTLTextureUsage};

    /// The frame the surface is composited into, in device pixels. The S1 corpus coordinates are
    /// stated against exactly this size.
    const FRAME_WIDTH: i32 = 800;
    const FRAME_HEIGHT: i32 = 600;
    /// The external surface itself, in device pixels.
    const SURFACE_EXTENT: i32 = 512;

    /// The S1 colour constants, unchanged. Only generation 0 is used here — a device generation
    /// only advances on an invalidation, which these tests do not provoke — but the array is kept
    /// whole so the corpora stay diffable.
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

    /// The producer's own shaders, which are deliberately *not* GPUI's: an external compositor
    /// brings its own and draws through the device and queue the producer accessor hands it.
    ///
    /// `packed_float2`/`packed_float4` rather than the aligned vector types, so the struct is the
    /// 24 tightly packed bytes [`SolidVertex`] is on the Rust side; `float4` would raise the
    /// alignment to 16 and silently reinterpret the buffer.
    const PRODUCER_SHADERS: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct SolidVertexIn {
  packed_float2 pos;
  packed_float4 color;
};

struct SolidOut {
  float4 pos [[position]];
  float4 color;
};

vertex SolidOut vs_solid(uint vertex_id [[vertex_id]],
                         constant SolidVertexIn *vertices [[buffer(0)]]) {
  SolidOut out;
  out.pos = float4(vertices[vertex_id].pos, 0.0, 1.0);
  out.color = float4(vertices[vertex_id].color);
  return out;
}

fragment float4 fs_solid(SolidOut input [[stage_in]]) {
  return input.color;
}
"#;

    #[repr(C)]
    #[derive(Clone, Copy)]
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

    fn clear_color(color: [u8; 4]) -> metal::MTLClearColor {
        let color = color_f(color);
        metal::MTLClearColor::new(
            f64::from(color[0]),
            f64::from(color[1]),
            f64::from(color[2]),
            f64::from(color[3]),
        )
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

    fn frame_size() -> Size<DevicePixels> {
        Size {
            width: DevicePixels(FRAME_WIDTH),
            height: DevicePixels(FRAME_HEIGHT),
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
    /// external-surface blend state (`One` / `OneMinusSourceAlpha` on all four channels) computes
    /// once the fragment shader has multiplied the sample by the group opacity.
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
        pitch: usize,
    }

    impl Frame {
        /// The pixel at `(x, y)`, converted out of the render target's `BGRA8Unorm` memory order
        /// into RGBA so that it compares directly against the S1 constants.
        fn pixel(&self, at: (u32, u32)) -> [u8; 4] {
            let offset = at.1 as usize * self.pitch + at.0 as usize * 4;
            [
                self.pixels[offset + 2],
                self.pixels[offset + 1],
                self.pixels[offset],
                self.pixels[offset + 3],
            ]
        }
    }

    /// A device, a GPUI frame to draw into, and one registered external surface the producer has
    /// already filled.
    struct Harness {
        device: metal::Device,
        command_queue: metal::CommandQueue,
        pipeline: ExternalSurfacePipeline,
        unit_vertices: metal::Buffer,
        registry: ExternalSurfaceRegistry,
        frame: metal::Texture,
        handle: ExternalSurfaceHandle,
    }

    impl Harness {
        /// Builds everything, or returns `None` when the host has no Metal device at all.
        ///
        /// Building [`ExternalSurfacePipeline`] is part of the coverage rather than setup: it looks
        /// its two entry points up in GPUI's own shader library, so a shader that failed to compile
        /// or an entry point that was renamed fails here rather than silently drawing nothing.
        fn new() -> Option<Self> {
            let device = metal::Device::system_default()?;
            let library = load_shader_library(&device);
            let pipeline = ExternalSurfacePipeline::new(&device, &library);
            let unit_vertices = build_unit_vertices(&device, device.has_unified_memory());
            let command_queue = device.new_command_queue();

            // `Managed` and a blit synchronize before the read-back, exactly as
            // `MetalRenderer::render_scene_to_image` does it: on a discrete GPU the CPU copy is
            // stale zeros without one.
            let descriptor = metal::TextureDescriptor::new();
            descriptor.set_width(FRAME_WIDTH as u64);
            descriptor.set_height(FRAME_HEIGHT as u64);
            // The same byte order every GPUI Metal render target uses, which is what makes the
            // read-back comparable with the other five profiles'.
            descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            descriptor.set_usage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
            descriptor.set_storage_mode(MTLStorageMode::Managed);
            let frame = device.new_texture(&descriptor);

            let mut registry = ExternalSurfaceRegistry::new(device.clone());
            let (handle, producer_texture) = registry
                .register(surface_size(), ExternalSurfaceFormat::Bgra8Unorm)
                .expect("registering a 512x512 surface must be inside the provisional budget");
            producer_pass(&device, &command_queue, &producer_texture);

            Some(Self {
                device,
                command_queue,
                pipeline,
                unit_vertices,
                registry,
                frame,
                handle,
            })
        }

        fn descriptor(&self) -> ExternalSurfaceDescriptor {
            ExternalSurfaceDescriptor {
                handle: self.handle,
                size: surface_size(),
                format: ExternalSurfaceFormat::Bgra8Unorm,
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
            let command_buffer = self.command_queue.new_command_buffer();
            let command_encoder = new_command_encoder_for_texture(
                command_buffer,
                &self.frame,
                frame_size(),
                Some(clear_color(BACKGROUND)),
            );

            draw_external_surfaces_into_encoder(
                &self.pipeline,
                &self.unit_vertices,
                frame_size(),
                &mut self.registry,
                surfaces,
                command_encoder,
            );

            command_encoder.end_encoding();

            if !self.device.has_unified_memory() {
                let blit = command_buffer.new_blit_command_encoder();
                blit.synchronize_resource(&self.frame);
                blit.end_encoding();
            }
            command_buffer.commit();
            command_buffer.wait_until_completed();

            self.read_back()
        }

        fn read_back(&self) -> Frame {
            let pitch = FRAME_WIDTH as usize * 4;
            let mut pixels = vec![0u8; FRAME_HEIGHT as usize * pitch];
            self.frame.get_bytes(
                pixels.as_mut_ptr() as *mut std::ffi::c_void,
                pitch as u64,
                metal::MTLRegion {
                    origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: metal::MTLSize {
                        width: FRAME_WIDTH as u64,
                        height: FRAME_HEIGHT as u64,
                        depth: 1,
                    },
                },
                0,
            );
            Frame { pixels, pitch }
        }
    }

    /// The S1 producer pass, run against the texture the registry handed back.
    ///
    /// This is the producer flow of the contract exactly: the producer receives the texture from
    /// `register`, makes its **own** library, pipeline and render pass over it, and commits its own
    /// command buffer on GPUI's queue ahead of GPUI's frame — which is what `SameQueueOrdered`
    /// means on Metal, and what the S3 spike showed holds even though the two passes never share a
    /// command buffer. Nothing here waits: `wait_until_completed` is deliberately absent, so the
    /// queue's own ordering is the only thing making the content visible downstream.
    ///
    /// It clears to the generation colour and draws the marker triangle at NDC (-1,1), (-0.5,1),
    /// (-1,0.5), which on a 512x512 surface is the right triangle with corners at texels (0,0),
    /// (128,0) and (0,128).
    fn producer_pass(
        device: &metal::Device,
        command_queue: &metal::CommandQueue,
        surface: &metal::Texture,
    ) {
        let library = device
            .new_library_with_source(PRODUCER_SHADERS, &metal::CompileOptions::new())
            .expect("the producer's own shaders must compile");
        let descriptor = metal::RenderPipelineDescriptor::new();
        descriptor.set_label("external_surface_test_producer");
        descriptor.set_vertex_function(Some(&library.get_function("vs_solid", None).unwrap()));
        descriptor.set_fragment_function(Some(&library.get_function("fs_solid", None).unwrap()));
        let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
        color_attachment.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        // The producer writes its content opaquely; group opacity is GPUI's to apply, not the
        // producer's, so nothing here blends.
        color_attachment.set_blending_enabled(false);
        let pipeline = device
            .new_render_pipeline_state(&descriptor)
            .expect("the producer's pipeline must build");

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

        let render_pass = metal::RenderPassDescriptor::new();
        let attachment = render_pass.color_attachments().object_at(0).unwrap();
        attachment.set_texture(Some(surface));
        attachment.set_load_action(metal::MTLLoadAction::Clear);
        attachment.set_clear_color(clear_color(GENERATION_COLORS[0]));
        attachment.set_store_action(metal::MTLStoreAction::Store);

        let command_buffer = command_queue.new_command_buffer();
        let encoder = command_buffer.new_render_command_encoder(render_pass);
        encoder.set_render_pipeline_state(&pipeline);
        encoder.set_viewport(metal::MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: SURFACE_EXTENT as f64,
            height: SURFACE_EXTENT as f64,
            znear: 0.0,
            zfar: 1.0,
        });
        encoder.set_vertex_bytes(
            0,
            std::mem::size_of_val(&vertices) as u64,
            vertices.as_ptr() as *const _,
        );
        encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        encoder.end_encoding();
        command_buffer.commit();
    }

    // --- The corpus ---------------------------------------------------------------------------

    /// The external-surface pipeline builds against GPUI's own shader library, and the in-flight
    /// budget it publishes is the layer's own maximum drawable count.
    #[test]
    fn the_pipeline_builds_from_gpuis_own_shader_library() {
        let Some(device) = metal::Device::system_default() else {
            return;
        };
        let library = load_shader_library(&device);
        // Both entry points have to exist under exactly these names; `get_function` is what would
        // fail otherwise, before any pixel is drawn.
        ExternalSurfacePipeline::new(&device, &library);
        assert_eq!(
            MAX_DRAWABLE_COUNT, 3,
            "the in-flight surface budget follows the layer's drawable count"
        );
    }

    /// The S1 corpus' same-device cases — `external_center_generation`, `producer_mark_visible`
    /// and the "nothing outside the quad" one — run through GPUI's own pipeline, at the spike's own
    /// coordinates, plus one negative check the spike did not need.
    ///
    /// Together they prove that the shader's view of `ExternalSurfaceInstance` matches the Rust
    /// one: that struct is 64 bytes of `bounds`, `source_uv`, the affine and the opacity, and a
    /// disagreement about the field order or the padding moves `bounds` or `source_uv` so that the
    /// target rectangle stops landing on these pixels at all. They also prove the S3 ordering
    /// property, because the producer's separate command buffer is never waited on.
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
    /// and the blend state composites the result with `One`/`OneMinusSourceAlpha`.
    #[test]
    fn group_opacity_blends_the_surface_over_the_frame() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let mut surface = harness.paint_surface();
        surface.opacity = 0.5;
        let frame = harness.draw(&[surface]);

        // Computed from the same constants rather than written out: at 0.5 over `BACKGROUND` this
        // is [15, 105, 105, 255]. The straight-alpha blend state GPUI's general `build_pipeline_state`
        // installs would premultiply a second time and land well outside the one-unit tolerance.
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

    /// The content mask becomes a scissor rectangle, and one surface's mask does not leak into the
    /// next surface in the same pass.
    ///
    /// The second half is the interesting one on Metal, because a scissor rectangle is encoder
    /// state that outlives the draw that set it: the two surfaces below are composited into one
    /// render pass, in order, and the unclipped one has to cover what the clipped one left as
    /// background. The remaining leak — into a *later batch* of some other primitive kind — is what
    /// the unconditional restore at the end of the run closes, and observing that needs a whole
    /// renderer rather than one encoder, so it is not claimed here.
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

        // Two surfaces in one render pass, the clipped one first: the second must set its own
        // scissor rectangle rather than inherit the first one's.
        let unclipped = harness.paint_surface();
        let mut clipped = unclipped.clone();
        clipped.content_mask = ContentMask {
            bounds: scaled_bounds(400.0, 150.0, 200.0, 300.0),
        };
        let frame = harness.draw(&[clipped, unclipped]);
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
    /// * were the affine applied about the viewport origin the way `to_device_position_transformed`
    ///   does it for the sprite pipelines, the rectangle would land at x in -600..-200, entirely
    ///   off-screen, and both probes would read `BACKGROUND`;
    /// * were the mirrored winding culled — the external pipeline sets no cull mode, and Metal's
    ///   default is `None`, which is what makes a negative determinant legal at all — the frame
    ///   would likewise be uniformly `BACKGROUND`;
    /// * were the transform dropped or its sign inverted, x = 220 would still be the marker and
    ///   x = 179 would still be `BACKGROUND`.
    ///
    /// This matrix is symmetric, so it does not by itself distinguish a row-major read of
    /// `rotation_scale` from a column-major one; the layout test in the sibling module is what pins
    /// that, together with the translation sitting after both rows.
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

    /// A handle from a dead generation draws nothing at all, and the skip is counted rather than
    /// fatal.
    #[test]
    fn a_stale_handle_is_skipped_and_counted_rather_than_drawn() {
        let Some(mut harness) = Harness::new() else {
            return;
        };
        let surface = harness.paint_surface();
        // Everything the old generation owned dies at once; the descriptor still names the old
        // handle.
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
