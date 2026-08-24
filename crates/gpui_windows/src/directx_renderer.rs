use std::{
    cell::RefCell,
    rc::Rc,
    slice,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result};
use gpui_util::ResultExt;
use windows::{
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            DirectComposition::*,
            DirectWrite::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{HSTRING, Interface},
};

use crate::directx_renderer::shader_resources::{
    RawShaderBytes, ShaderModel, ShaderModule, ShaderTarget,
};
use crate::*;
use gpui::*;

pub(crate) const DISABLE_DIRECT_COMPOSITION: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";
const RENDER_TARGET_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;
// This configuration is used for MSAA rendering on paths only, and it's guaranteed to be supported by DirectX 11.
const PATH_MULTISAMPLE_COUNT: u32 = 4;
const MAX_INSTANCE_BUFFER_SIZE: usize = 256 * 1024 * 1024;

pub(crate) struct FontInfo {
    pub gamma_ratios: [f32; 4],
    pub grayscale_enhanced_contrast: f32,
    pub subpixel_enhanced_contrast: f32,
    pub is_bgr: bool,
}

pub(crate) struct DirectXRenderer {
    hwnd: HWND,
    atlas: Arc<DirectXAtlas>,
    devices: Option<DirectXRendererDevices>,
    resources: Option<DirectXResources>,
    globals: DirectXGlobalElements,
    pipelines: DirectXRenderPipelines,
    direct_composition: Option<DirectComposition>,
    font_info: &'static FontInfo,
    /// The external-surface bridge's resource storage.
    ///
    /// It is shared rather than owned outright because the producer accessor
    /// ([`crate::external_surface_producer`]) holds the other end: the renderer resolves handles
    /// against it while drawing, and the external compositor registers and retires through it.
    /// The same allocation survives a device-lost recovery, so a producer that outlives the
    /// device still observes the raised generation instead of a dangling registry.
    external_registry: Rc<RefCell<ExternalSurfaceRegistry>>,

    width: u32,
    height: u32,

    /// Whether we want to skip drwaing due to device lost events.
    ///
    /// In that case we want to discard the first frame that we draw as we got reset in the middle of a frame
    /// meaning we lost all the allocated gpu textures and scene resources.
    skip_draws: bool,
}

/// Direct3D objects
#[derive(Clone)]
pub(crate) struct DirectXRendererDevices {
    pub(crate) adapter: IDXGIAdapter1,
    pub(crate) dxgi_factory: IDXGIFactory6,
    pub(crate) device: ID3D11Device,
    pub(crate) device_context: ID3D11DeviceContext,
    dxgi_device: Option<IDXGIDevice>,
    annotation: Option<ID3DUserDefinedAnnotation>,
}

struct DirectXResources {
    // Direct3D rendering objects
    swap_chain: IDXGISwapChain1,
    render_target: Option<ID3D11Texture2D>,
    render_target_view: Option<ID3D11RenderTargetView>,

    // Path intermediate textures (with MSAA)
    path_intermediate_texture: ID3D11Texture2D,
    path_intermediate_srv: Option<ID3D11ShaderResourceView>,
    path_intermediate_msaa_texture: ID3D11Texture2D,
    path_intermediate_msaa_view: Option<ID3D11RenderTargetView>,

    // Cached viewport
    viewport: D3D11_VIEWPORT,
}

struct DirectXRenderPipelines {
    shadow_pipeline: PipelineState<Shadow>,
    quad_pipeline: PipelineState<Quad>,
    path_rasterization_pipeline: PipelineState<PathRasterizationSprite>,
    path_sprite_pipeline: PipelineState<PathSprite>,
    underline_pipeline: PipelineState<Underline>,
    mono_sprites: PipelineState<MonochromeSprite>,
    subpixel_sprites: PipelineState<SubpixelSprite>,
    poly_sprites: PipelineState<PolychromeSprite>,
    external_surfaces: ExternalSurfacePipeline,
}

/// The external-surface pipeline and the state it needs that no other pipeline here does.
///
/// It differs from every other GPUI pipeline in three ways, all of them required by the bridge
/// contract rather than chosen:
///
/// * its blend state is **premultiplied**, because contract v1 has premultiplied alpha as its only
///   alpha mode, while GPUI's general blend state is straight alpha;
/// * it carries both sampling modes, because the descriptor picks between them per surface;
/// * it clips with a scissor rectangle, so it also owns the scissor-enabled rasterizer state and
///   the plain one it restores afterwards.
struct ExternalSurfacePipeline {
    pipeline: PipelineState<ExternalSurfaceInstance>,
    /// `ExternalSampling::Nearest`.
    sampler_nearest: Option<ID3D11SamplerState>,
    /// `ExternalSampling::Linear`.
    sampler_linear: Option<ID3D11SamplerState>,
    /// Used while external surfaces are drawn, so the content mask becomes a scissor rectangle.
    scissor_rasterizer_state: ID3D11RasterizerState,
    /// Restored afterwards, so no later batch inherits the scissor rectangle. It is identical to
    /// the state [`set_rasterizer_state`] installs at startup.
    default_rasterizer_state: ID3D11RasterizerState,
}

struct DirectXGlobalElements {
    global_params_buffer: Option<ID3D11Buffer>,
    batch_params_buffer: Option<ID3D11Buffer>,
    sampler: Option<ID3D11SamplerState>,
}

struct Annotation<'a>(&'a ID3DUserDefinedAnnotation);

impl<'a> Annotation<'a> {
    fn new(annotation: &'a ID3DUserDefinedAnnotation, label: HSTRING) -> Self {
        unsafe { annotation.BeginEvent(&label) };
        Self(annotation)
    }
}

impl Drop for Annotation<'_> {
    fn drop(&mut self) {
        unsafe { self.0.EndEvent() };
    }
}

struct DirectComposition {
    comp_device: IDCompositionDevice,
    comp_target: IDCompositionTarget,
    comp_visual: IDCompositionVisual,
}

impl DirectXRendererDevices {
    pub(crate) fn new(
        directx_devices: &DirectXDevices,
        disable_direct_composition: bool,
    ) -> Result<Self> {
        let DirectXDevices {
            adapter,
            dxgi_factory,
            device,
            device_context,
        } = directx_devices;
        let dxgi_device = if disable_direct_composition {
            None
        } else {
            Some(device.cast().context("Creating DXGI device")?)
        };
        let annotation = device_context.cast().ok();

        Ok(Self {
            adapter: adapter.clone(),
            dxgi_factory: dxgi_factory.clone(),
            device: device.clone(),
            device_context: device_context.clone(),
            dxgi_device,
            annotation,
        })
    }
}

impl DirectXRenderer {
    pub(crate) fn new(
        hwnd: HWND,
        directx_devices: &DirectXDevices,
        disable_direct_composition: bool,
    ) -> Result<Self> {
        if disable_direct_composition {
            log::info!("Direct Composition is disabled.");
        }

        let devices = DirectXRendererDevices::new(directx_devices, disable_direct_composition)
            .context("Creating DirectX devices")?;
        let atlas = Arc::new(DirectXAtlas::new(&devices.device, &devices.device_context));

        let resources = DirectXResources::new(&devices, 1, 1, hwnd, disable_direct_composition)
            .context("Creating DirectX resources")?;
        let globals = DirectXGlobalElements::new(&devices.device)
            .context("Creating DirectX global elements")?;
        let pipelines = DirectXRenderPipelines::new(&devices.device)
            .context("Creating DirectX render pipelines")?;

        let direct_composition = if disable_direct_composition {
            None
        } else {
            let composition = DirectComposition::new(devices.dxgi_device.as_ref().unwrap(), hwnd)
                .context("Creating DirectComposition")?;
            composition
                .set_swap_chain(&resources.swap_chain)
                .context("Setting swap chain for DirectComposition")?;
            Some(composition)
        };

        let external_registry =
            Rc::new(RefCell::new(ExternalSurfaceRegistry::new(&devices.device)));

        Ok(DirectXRenderer {
            hwnd,
            atlas,
            devices: Some(devices),
            resources: Some(resources),
            globals,
            pipelines,
            direct_composition,
            font_info: Self::get_font_info(),
            external_registry,
            width: 1,
            height: 1,
            skip_draws: false,
        })
    }

    pub(crate) fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.atlas.clone()
    }

    fn pre_draw(&self, clear_color: &[f32; 4]) -> Result<()> {
        let resources = self.resources.as_ref().expect("resources missing");
        let device_context = &self
            .devices
            .as_ref()
            .expect("devices missing")
            .device_context;
        bind_frame(
            device_context,
            &resources.render_target_view,
            &resources.viewport,
            &self.globals,
            &GlobalParams {
                gamma_ratios: self.font_info.gamma_ratios,
                viewport_size: [resources.viewport.Width, resources.viewport.Height],
                grayscale_enhanced_contrast: self.font_info.grayscale_enhanced_contrast,
                subpixel_enhanced_contrast: self.font_info.subpixel_enhanced_contrast,
                is_bgr: self.font_info.is_bgr as u32,
                _pad: [0; 3],
            },
            clear_color,
        )
    }

    #[inline]
    fn present(&mut self) -> Result<()> {
        let result = unsafe {
            self.resources
                .as_ref()
                .expect("resources missing")
                .swap_chain
                .Present(0, DXGI_PRESENT(0))
        };
        result.ok().context("Presenting swap chain failed")
    }

    pub(crate) fn handle_device_lost(&mut self, directx_devices: &DirectXDevices) -> Result<()> {
        try_to_recover_from_device_lost(|| {
            self.handle_device_lost_impl(directx_devices)
                .context("DirectXRenderer handling device lost")
        })
    }

    fn handle_device_lost_impl(&mut self, directx_devices: &DirectXDevices) -> Result<()> {
        let disable_direct_composition = self.direct_composition.is_none();

        unsafe {
            #[cfg(debug_assertions)]
            if let Some(devices) = &self.devices {
                report_live_objects(&devices.device)
                    .context("Failed to report live objects after device lost")
                    .log_err();
            }

            self.resources.take();
            if let Some(devices) = &self.devices {
                devices.device_context.OMSetRenderTargets(None, None);
                devices.device_context.ClearState();
                devices.device_context.Flush();
                #[cfg(debug_assertions)]
                report_live_objects(&devices.device)
                    .context("Failed to report live objects after device lost")
                    .log_err();
            }

            self.direct_composition.take();
            self.devices.take();
        }

        let devices = DirectXRendererDevices::new(directx_devices, disable_direct_composition)
            .context("Recreating DirectX devices")?;
        let resources = DirectXResources::new(
            &devices,
            self.width,
            self.height,
            self.hwnd,
            disable_direct_composition,
        )
        .context("Creating DirectX resources")?;
        let globals = DirectXGlobalElements::new(&devices.device)
            .context("Creating DirectXGlobalElements")?;
        let pipelines = DirectXRenderPipelines::new(&devices.device)
            .context("Creating DirectXRenderPipelines")?;

        let direct_composition = if disable_direct_composition {
            None
        } else {
            let composition =
                DirectComposition::new(devices.dxgi_device.as_ref().unwrap(), self.hwnd)?;
            composition.set_swap_chain(&resources.swap_chain)?;
            Some(composition)
        };

        self.atlas
            .handle_device_lost(&devices.device, &devices.device_context);
        // `DXGI_ERROR_DEVICE_REMOVED` and `DXGI_ERROR_DEVICE_RESET` both land here, and the S1
        // spike showed the loss is adapter-wide: no subset of external surfaces survives it. The
        // registry therefore adopts the recreated device and raises the generation, which makes
        // *every* outstanding handle stale at once. Drawing with one of them is skipped and
        // counted, and the consumer's only recovery is a full rebuild.
        self.external_registry
            .borrow_mut()
            .handle_device_lost(&devices.device);

        unsafe {
            devices
                .device_context
                .OMSetRenderTargets(Some(slice::from_ref(&resources.render_target_view)), None);
        }
        self.devices = Some(devices);
        self.resources = Some(resources);
        self.globals = globals;
        self.pipelines = pipelines;
        self.direct_composition = direct_composition;
        self.skip_draws = true;
        Ok(())
    }

    pub(crate) fn draw(
        &mut self,
        scene: &Scene,
        background_appearance: WindowBackgroundAppearance,
    ) -> Result<()> {
        if self.skip_draws {
            // skip drawing this frame, we just recovered from a device lost event
            // and so likely do not have the textures anymore that are required for drawing
            return Ok(());
        }
        self.pre_draw(&match background_appearance {
            WindowBackgroundAppearance::Opaque => [1.0f32; 4],
            _ => [0.0f32; 4],
        })?;

        self.upload_scene_buffers(scene)?;

        let annotation = self
            .devices
            .as_ref()
            .and_then(|devices| devices.annotation.clone())
            .filter(|annotation| unsafe { annotation.GetStatus().as_bool() });
        for batch in scene.batches() {
            let _annotation = annotation
                .as_ref()
                .map(|annotation| Annotation::new(annotation, HSTRING::from(batch.label())));
            match batch {
                PrimitiveBatch::Shadows(range) => self.draw_shadows(range.start, range.len()),
                PrimitiveBatch::Quads(range) => self.draw_quads(range.start, range.len()),
                PrimitiveBatch::Paths(range) => {
                    let paths = &scene.paths[range];
                    self.draw_paths_to_intermediate(paths)?;
                    self.draw_paths_from_intermediate(paths)
                }
                PrimitiveBatch::Underlines(range) => self.draw_underlines(range.start, range.len()),
                PrimitiveBatch::MonochromeSprites { texture_id, range } => {
                    self.draw_monochrome_sprites(texture_id, range.start, range.len())
                }
                PrimitiveBatch::SubpixelSprites { texture_id, range } => {
                    self.draw_subpixel_sprites(texture_id, range.start, range.len())
                }
                PrimitiveBatch::PolychromeSprites { texture_id, range } => {
                    self.draw_polychrome_sprites(texture_id, range.start, range.len())
                }
                PrimitiveBatch::Surfaces(range) => self.draw_surfaces(&scene.surfaces[range]),
            }
            .with_context(|| {
                format!(
                    "scene too large:\
                    {} paths, {} shadows, {} quads, {} underlines, {} mono, {} subpixel, {} poly, {} surfaces",
                    scene.paths.len(),
                    scene.shadows.len(),
                    scene.quads.len(),
                    scene.underlines.len(),
                    scene.monochrome_sprites.len(),
                    scene.subpixel_sprites.len(),
                    scene.polychrome_sprites.len(),
                    scene.surfaces.len(),
                )
            })?;
        }
        self.present()
    }

    pub(crate) fn resize(&mut self, new_size: Size<DevicePixels>) -> Result<()> {
        let width = new_size.width.0.max(1) as u32;
        let height = new_size.height.0.max(1) as u32;
        if self.width == width && self.height == height {
            return Ok(());
        }
        self.width = width;
        self.height = height;

        // Clear the render target before resizing
        let devices = self.devices.as_ref().context("devices missing")?;
        unsafe { devices.device_context.OMSetRenderTargets(None, None) };
        let resources = self.resources.as_mut().context("resources missing")?;
        resources.render_target.take();
        resources.render_target_view.take();

        // Resizing the swap chain requires a call to the underlying DXGI adapter, which can return the device removed error.
        // The app might have moved to a monitor that's attached to a different graphics device.
        // When a graphics device is removed or reset, the desktop resolution often changes, resulting in a window size change.
        // But here we just return the error, because we are handling device lost scenarios elsewhere.
        unsafe {
            resources
                .swap_chain
                .ResizeBuffers(
                    BUFFER_COUNT as u32,
                    width,
                    height,
                    RENDER_TARGET_FORMAT,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .context("Failed to resize swap chain")?;
        }

        resources.recreate_resources(devices, width, height)?;

        unsafe {
            devices
                .device_context
                .OMSetRenderTargets(Some(slice::from_ref(&resources.render_target_view)), None);
        }

        Ok(())
    }

    fn upload_scene_buffers(&mut self, scene: &Scene) -> Result<()> {
        let devices = self.devices.as_ref().context("devices missing")?;

        if !scene.shadows.is_empty() {
            self.pipelines.shadow_pipeline.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.shadows,
            )?;
        }

        if !scene.quads.is_empty() {
            self.pipelines.quad_pipeline.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.quads,
            )?;
        }

        if !scene.underlines.is_empty() {
            self.pipelines.underline_pipeline.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.underlines,
            )?;
        }

        if !scene.monochrome_sprites.is_empty() {
            self.pipelines.mono_sprites.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.monochrome_sprites,
            )?;
        }

        if !scene.subpixel_sprites.is_empty() {
            self.pipelines.subpixel_sprites.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.subpixel_sprites,
            )?;
        }

        if !scene.polychrome_sprites.is_empty() {
            self.pipelines.poly_sprites.update_buffer(
                &devices.device,
                &devices.device_context,
                &scene.polychrome_sprites,
            )?;
        }

        Ok(())
    }

    fn draw_shadows(&mut self, start: usize, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        self.pipelines.shadow_pipeline.draw_range(
            &devices.device_context,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            start as u32,
            len as u32,
        )
    }

    fn draw_quads(&mut self, start: usize, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        self.pipelines.quad_pipeline.draw_range(
            &devices.device_context,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            start as u32,
            len as u32,
        )
    }

    fn draw_paths_to_intermediate(&mut self, paths: &[Path<ScaledPixels>]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let devices = self.devices.as_ref().context("devices missing")?;
        let resources = self.resources.as_ref().context("resources missing")?;
        // Clear intermediate MSAA texture
        unsafe {
            devices.device_context.ClearRenderTargetView(
                resources.path_intermediate_msaa_view.as_ref().unwrap(),
                &[0.0; 4],
            );
            // Set intermediate MSAA texture as render target
            devices.device_context.OMSetRenderTargets(
                Some(slice::from_ref(&resources.path_intermediate_msaa_view)),
                None,
            );
        }

        // Collect all vertices and sprites for a single draw call
        let mut vertices = Vec::new();

        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationSprite {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.clipped_bounds(),
            }));
        }

        self.pipelines.path_rasterization_pipeline.update_buffer(
            &devices.device,
            &devices.device_context,
            &vertices,
        )?;

        self.pipelines.path_rasterization_pipeline.draw(
            &devices.device_context,
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            vertices.len() as u32,
            1,
        )?;

        // Resolve MSAA to non-MSAA intermediate texture
        unsafe {
            devices.device_context.ResolveSubresource(
                &resources.path_intermediate_texture,
                0,
                &resources.path_intermediate_msaa_texture,
                0,
                RENDER_TARGET_FORMAT,
            );
            // Restore main render target
            devices
                .device_context
                .OMSetRenderTargets(Some(slice::from_ref(&resources.render_target_view)), None);
        }

        Ok(())
    }

    fn draw_paths_from_intermediate(&mut self, paths: &[Path<ScaledPixels>]) -> Result<()> {
        let Some(first_path) = paths.first() else {
            return Ok(());
        };

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites = if paths.last().unwrap().order == first_path.order {
            paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect::<Vec<_>>()
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            vec![PathSprite { bounds }]
        };

        let devices = self.devices.as_ref().context("devices missing")?;
        let resources = self.resources.as_ref().context("resources missing")?;
        self.pipelines.path_sprite_pipeline.update_buffer(
            &devices.device,
            &devices.device_context,
            &sprites,
        )?;

        // Draw the sprites with the path texture
        self.pipelines.path_sprite_pipeline.draw_with_texture(
            &devices.device_context,
            slice::from_ref(&resources.path_intermediate_srv),
            slice::from_ref(&self.globals.sampler),
            sprites.len() as u32,
        )
    }

    fn draw_underlines(&mut self, start: usize, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        self.pipelines.underline_pipeline.draw_range(
            &devices.device_context,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            start as u32,
            len as u32,
        )
    }

    fn draw_monochrome_sprites(
        &mut self,
        texture_id: AtlasTextureId,
        start: usize,
        len: usize,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        let texture_view = self.atlas.get_texture_view(texture_id);
        self.pipelines.mono_sprites.draw_range_with_texture(
            &devices.device_context,
            &texture_view,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            slice::from_ref(&self.globals.sampler),
            start as u32,
            len as u32,
        )
    }

    fn draw_subpixel_sprites(
        &mut self,
        texture_id: AtlasTextureId,
        start: usize,
        len: usize,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        let texture_view = self.atlas.get_texture_view(texture_id);
        self.pipelines.subpixel_sprites.draw_range_with_texture(
            &devices.device_context,
            &texture_view,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            slice::from_ref(&self.globals.sampler),
            start as u32,
            len as u32,
        )
    }

    fn draw_polychrome_sprites(
        &mut self,
        texture_id: AtlasTextureId,
        start: usize,
        len: usize,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let devices = self.devices.as_ref().context("devices missing")?;
        let texture_view = self.atlas.get_texture_view(texture_id);
        self.pipelines.poly_sprites.draw_range_with_texture(
            &devices.device_context,
            &texture_view,
            self.globals
                .batch_params_buffer
                .as_ref()
                .context("batch params buffer missing")?,
            slice::from_ref(&self.globals.sampler),
            start as u32,
            len as u32,
        )
    }

    /// Draws one batch of surfaces.
    ///
    /// The placement fields are applied in the frozen order of the bridge contract: crop, then
    /// placement into `bounds`, then the affine about the top-left corner of `bounds`, then the
    /// content-mask clip, then the group opacity. The first three are computed here and finished
    /// in `external_surface_vertex`; the clip is a scissor rectangle; the opacity is the fragment
    /// shader's single multiply.
    ///
    /// A surface whose handle no longer resolves — a stale generation after a device loss, or an
    /// id that was retired — is skipped and counted, never a panic and never a draw from stale
    /// content: `allow_stale_reuse` is off in the capability snapshot.
    fn draw_surfaces(&mut self, surfaces: &[PaintSurface]) -> Result<()> {
        if surfaces.is_empty() {
            return Ok(());
        }

        let viewport = self
            .resources
            .as_ref()
            .context("resources missing")?
            .viewport;
        let devices = self.devices.as_ref().context("devices missing")?;
        let batch_params_buffer = self
            .globals
            .batch_params_buffer
            .as_ref()
            .context("batch params buffer missing")?;
        let mut registry = self.external_registry.borrow_mut();

        draw_surfaces_into_frame(
            &devices.device,
            &devices.device_context,
            &mut self.pipelines.external_surfaces,
            batch_params_buffer,
            viewport,
            &mut registry,
            surfaces,
        )
    }

    /// The external-surface capability and budget snapshot of this backend.
    ///
    /// This is what `Window::external_surface_capabilities` reports on Windows, and the budgets in
    /// it are the ones the registry actually enforces.
    /// Contract 1.1: publishes `handle` as a tracked publication, or returns the identity it has.
    pub(crate) fn publish_external_tracked(
        &self,
        handle: ExternalSurfaceHandle,
    ) -> Result<gpui::PublicationId, gpui::TrackedPublishError> {
        self.external_registry
            .borrow_mut()
            .publications_mut()
            .publish_tracked(handle)
    }

    /// Contract 1.1: hands the incoming scene's live handles over as one atomic step.
    pub(crate) fn handover_external_scene(
        &self,
        handover: &gpui::SceneHandover,
    ) -> gpui::SceneReplaceOutcome {
        self.external_registry
            .borrow_mut()
            .publications_mut()
            .handover(handover)
    }

    /// Contract 1.1: what the registry knows about `handle`. Mutation-free.
    pub(crate) fn external_publication_admission(
        &self,
        handle: ExternalSurfaceHandle,
    ) -> gpui::PublicationAdmission {
        self.external_registry
            .borrow()
            .publications()
            .admission(handle)
    }

    pub(crate) fn external_surface_capabilities(&self) -> ExternalSurfaceCapabilities {
        self.external_registry.borrow().capabilities()
    }

    /// The producer face of the external-surface bridge for this renderer, or `None` while there
    /// is no device (a device-lost recovery in flight).
    ///
    /// See [`ExternalSurfaceProducer`] for what it grants and what it deliberately does not.
    pub(crate) fn external_surface_producer(&self) -> Option<ExternalSurfaceProducer> {
        let devices = self.devices.as_ref()?;
        Some(ExternalSurfaceProducer::new(
            devices.device.clone(),
            self.external_registry.clone(),
        ))
    }

    pub(crate) fn gpu_specs(&self) -> Result<GpuSpecs> {
        let devices = self.devices.as_ref().context("devices missing")?;
        let desc = unsafe { devices.adapter.GetDesc1() }?;
        let is_software_emulated = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
        let device_name = String::from_utf16_lossy(&desc.Description)
            .trim_matches(char::from(0))
            .to_string();
        let driver_name = match desc.VendorId {
            0x10DE => "NVIDIA Corporation".to_string(),
            0x1002 => "AMD Corporation".to_string(),
            0x8086 => "Intel Corporation".to_string(),
            id => format!("Unknown Vendor (ID: {:#X})", id),
        };
        let driver_version = match desc.VendorId {
            0x10DE => nvidia::get_driver_version(),
            0x1002 => amd::get_driver_version(),
            // For Intel and other vendors, we use the DXGI API to get the driver version.
            _ => dxgi::get_driver_version(&devices.adapter),
        }
        .context("Failed to get gpu driver info")
        .log_err()
        .unwrap_or("Unknown Driver".to_string());
        Ok(GpuSpecs {
            is_software_emulated,
            device_name,
            driver_name,
            driver_info: driver_version,
        })
    }

    pub(crate) fn get_font_info() -> &'static FontInfo {
        static CACHED_FONT_INFO: OnceLock<FontInfo> = OnceLock::new();
        CACHED_FONT_INFO.get_or_init(|| unsafe {
            let factory: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let render_params: IDWriteRenderingParams1 =
                factory.CreateRenderingParams().unwrap().cast().unwrap();
            FontInfo {
                gamma_ratios: gpui::get_gamma_correction_ratios(render_params.GetGamma()),
                grayscale_enhanced_contrast: render_params.GetGrayscaleEnhancedContrast(),
                subpixel_enhanced_contrast: render_params.GetEnhancedContrast(),
                is_bgr: render_params.GetPixelGeometry() == DWRITE_PIXEL_GEOMETRY_BGR,
            }
        })
    }

    pub(crate) fn mark_drawable(&mut self) {
        self.skip_draws = false;
    }
}

impl DirectXResources {
    pub fn new(
        devices: &DirectXRendererDevices,
        width: u32,
        height: u32,
        hwnd: HWND,
        disable_direct_composition: bool,
    ) -> Result<Self> {
        let swap_chain = if disable_direct_composition {
            create_swap_chain(&devices.dxgi_factory, &devices.device, hwnd, width, height)?
        } else {
            create_swap_chain_for_composition(
                &devices.dxgi_factory,
                &devices.device,
                width,
                height,
            )?
        };

        let (
            render_target,
            render_target_view,
            path_intermediate_texture,
            path_intermediate_srv,
            path_intermediate_msaa_texture,
            path_intermediate_msaa_view,
            viewport,
        ) = create_resources(devices, &swap_chain, width, height)?;
        set_rasterizer_state(&devices.device, &devices.device_context)?;

        Ok(Self {
            swap_chain,
            render_target: Some(render_target),
            render_target_view,
            path_intermediate_texture,
            path_intermediate_msaa_texture,
            path_intermediate_msaa_view,
            path_intermediate_srv,
            viewport,
        })
    }

    #[inline]
    fn recreate_resources(
        &mut self,
        devices: &DirectXRendererDevices,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let (
            render_target,
            render_target_view,
            path_intermediate_texture,
            path_intermediate_srv,
            path_intermediate_msaa_texture,
            path_intermediate_msaa_view,
            viewport,
        ) = create_resources(devices, &self.swap_chain, width, height)?;
        self.render_target = Some(render_target);
        self.render_target_view = render_target_view;
        self.path_intermediate_texture = path_intermediate_texture;
        self.path_intermediate_msaa_texture = path_intermediate_msaa_texture;
        self.path_intermediate_msaa_view = path_intermediate_msaa_view;
        self.path_intermediate_srv = path_intermediate_srv;
        self.viewport = viewport;
        Ok(())
    }
}

impl DirectXRenderPipelines {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let shadow_pipeline = PipelineState::new(
            device,
            "shadow_pipeline",
            ShaderModule::Shadow,
            4,
            create_blend_state(device)?,
        )?;
        let quad_pipeline = PipelineState::new(
            device,
            "quad_pipeline",
            ShaderModule::Quad,
            64,
            create_blend_state(device)?,
        )?;
        let path_rasterization_pipeline = PipelineState::new(
            device,
            "path_rasterization_pipeline",
            ShaderModule::PathRasterization,
            32,
            create_blend_state_for_path_rasterization(device)?,
        )?;
        let path_sprite_pipeline = PipelineState::new(
            device,
            "path_sprite_pipeline",
            ShaderModule::PathSprite,
            4,
            create_blend_state_for_path_sprite(device)?,
        )?;
        let underline_pipeline = PipelineState::new(
            device,
            "underline_pipeline",
            ShaderModule::Underline,
            4,
            create_blend_state(device)?,
        )?;
        let mono_sprites = PipelineState::new(
            device,
            "monochrome_sprite_pipeline",
            ShaderModule::MonochromeSprite,
            512,
            create_blend_state(device)?,
        )?;
        let subpixel_sprites = PipelineState::new(
            device,
            "subpixel_sprite_pipeline",
            ShaderModule::SubpixelSprite,
            512,
            create_blend_state_for_subpixel_rendering(device)?,
        )?;
        let poly_sprites = PipelineState::new(
            device,
            "polychrome_sprite_pipeline",
            ShaderModule::PolychromeSprite,
            16,
            create_blend_state(device)?,
        )?;
        let external_surfaces = ExternalSurfacePipeline::new(device)?;

        Ok(Self {
            shadow_pipeline,
            quad_pipeline,
            path_rasterization_pipeline,
            path_sprite_pipeline,
            underline_pipeline,
            mono_sprites,
            subpixel_sprites,
            poly_sprites,
            external_surfaces,
        })
    }
}

impl ExternalSurfacePipeline {
    fn new(device: &ID3D11Device) -> Result<Self> {
        // The shader model is chosen from the device's feature level rather than fixed: shader
        // model 5.0 bytecode is rejected with `E_INVALIDARG` at feature level 10.1, which GPUI
        // still creates devices at, and that failure is at pipeline creation rather than at draw.
        let shader_model = ShaderModel::for_feature_level(unsafe { device.GetFeatureLevel() });
        let pipeline = PipelineState::new_with_shader_model(
            device,
            "external_surface_pipeline",
            ShaderModule::ExternalSurface,
            shader_model,
            // External surfaces are few and large; one instance per registered surface in flight
            // is the honest starting point, and the buffer grows on demand like every other.
            4,
            create_blend_state_for_external_surface(device)?,
        )?;

        Ok(Self {
            pipeline,
            sampler_nearest: create_external_surface_sampler(
                device,
                D3D11_FILTER_MIN_MAG_MIP_POINT,
            )?,
            sampler_linear: create_external_surface_sampler(
                device,
                D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            )?,
            scissor_rasterizer_state: create_rasterizer_state(device, true)?,
            default_rasterizer_state: create_rasterizer_state(device, false)?,
        })
    }
}

impl DirectComposition {
    pub fn new(dxgi_device: &IDXGIDevice, hwnd: HWND) -> Result<Self> {
        let comp_device = get_comp_device(dxgi_device)?;
        let comp_target = unsafe { comp_device.CreateTargetForHwnd(hwnd, true) }?;
        let comp_visual = unsafe { comp_device.CreateVisual() }?;

        Ok(Self {
            comp_device,
            comp_target,
            comp_visual,
        })
    }

    pub fn set_swap_chain(&self, swap_chain: &IDXGISwapChain1) -> Result<()> {
        unsafe {
            self.comp_visual.SetContent(swap_chain)?;
            self.comp_target.SetRoot(&self.comp_visual)?;
            self.comp_device.Commit()?;
        }
        Ok(())
    }
}

impl DirectXGlobalElements {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let global_params_buffer = create_constant_buffer::<GlobalParams>(device)?;
        let batch_params_buffer = create_constant_buffer::<BatchParams>(device)?;

        let sampler = unsafe {
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_WRAP,
                AddressV: D3D11_TEXTURE_ADDRESS_WRAP,
                AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_ALWAYS,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: D3D11_FLOAT32_MAX,
            };
            let mut output = None;
            device.CreateSamplerState(&desc, Some(&mut output))?;
            output
        };

        Ok(Self {
            global_params_buffer,
            batch_params_buffer,
            sampler,
        })
    }
}

#[derive(Debug, Default)]
#[repr(C)]
struct GlobalParams {
    gamma_ratios: [f32; 4],
    viewport_size: [f32; 2],
    grayscale_enhanced_contrast: f32,
    subpixel_enhanced_contrast: f32,
    is_bgr: u32,
    _pad: [u32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(16))]
struct BatchParams {
    start_index: u32,
    _padding: [u32; 3],
}

const _: () = assert!(std::mem::size_of::<BatchParams>() == 16);

struct PipelineState<T> {
    label: &'static str,
    vertex: ID3D11VertexShader,
    fragment: ID3D11PixelShader,
    buffer: ID3D11Buffer,
    buffer_size: usize,
    view: Option<ID3D11ShaderResourceView>,
    blend_state: ID3D11BlendState,
    _marker: std::marker::PhantomData<T>,
}

impl<T> PipelineState<T> {
    fn new(
        device: &ID3D11Device,
        label: &'static str,
        shader_module: ShaderModule,
        buffer_size: usize,
        blend_state: ID3D11BlendState,
    ) -> Result<Self> {
        Self::new_with_shader_model(
            device,
            label,
            shader_module,
            ShaderModel::DEFAULT,
            buffer_size,
            blend_state,
        )
    }

    /// Builds a pipeline whose shaders are compiled against an explicit shader model.
    ///
    /// Only the external-surface pipeline needs this: its model is chosen from the device's
    /// feature level, because shader model 5.0 bytecode is rejected at feature level 10.1.
    fn new_with_shader_model(
        device: &ID3D11Device,
        label: &'static str,
        shader_module: ShaderModule,
        shader_model: ShaderModel,
        buffer_size: usize,
        blend_state: ID3D11BlendState,
    ) -> Result<Self> {
        let vertex = {
            let raw_shader = RawShaderBytes::new_with_shader_model(
                shader_module,
                ShaderTarget::Vertex,
                shader_model,
            )?;
            create_vertex_shader(device, raw_shader.as_bytes())?
        };
        let fragment = {
            let raw_shader = RawShaderBytes::new_with_shader_model(
                shader_module,
                ShaderTarget::Fragment,
                shader_model,
            )?;
            create_fragment_shader(device, raw_shader.as_bytes())?
        };
        let buffer = create_buffer(device, std::mem::size_of::<T>(), buffer_size)?;
        let view = create_buffer_view(device, &buffer)?;

        Ok(PipelineState {
            label,
            vertex,
            fragment,
            buffer,
            buffer_size,
            view,
            blend_state,
            _marker: std::marker::PhantomData,
        })
    }

    fn update_buffer(
        &mut self,
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
        data: &[T],
    ) -> Result<()> {
        if self.buffer_size < data.len() {
            let element_size = std::mem::size_of::<T>();
            let required_size = std::mem::size_of_val(data);
            anyhow::ensure!(
                required_size <= MAX_INSTANCE_BUFFER_SIZE,
                "{} buffer needs {required_size} bytes, above the maximum of {MAX_INSTANCE_BUFFER_SIZE}",
                self.label
            );
            let new_buffer_size = data
                .len()
                .next_power_of_two()
                .min(MAX_INSTANCE_BUFFER_SIZE / element_size);
            log::debug!(
                "Updating {} buffer size from {} to {}",
                self.label,
                self.buffer_size,
                new_buffer_size
            );
            let buffer = create_buffer(device, std::mem::size_of::<T>(), new_buffer_size)?;
            let view = create_buffer_view(device, &buffer)?;
            self.buffer = buffer;
            self.view = view;
            self.buffer_size = new_buffer_size;
        }
        update_buffer(device_context, &self.buffer, data)
    }

    fn draw(
        &self,
        device_context: &ID3D11DeviceContext,
        topology: D3D_PRIMITIVE_TOPOLOGY,
        vertex_count: u32,
        instance_count: u32,
    ) -> Result<()> {
        set_pipeline_state(
            device_context,
            slice::from_ref(&self.view),
            topology,
            &self.vertex,
            &self.fragment,
            &self.blend_state,
        );
        unsafe {
            device_context.DrawInstanced(vertex_count, instance_count, 0, 0);
        }
        Ok(())
    }

    fn draw_with_texture(
        &self,
        device_context: &ID3D11DeviceContext,
        texture: &[Option<ID3D11ShaderResourceView>],
        sampler: &[Option<ID3D11SamplerState>],
        instance_count: u32,
    ) -> Result<()> {
        set_pipeline_state(
            device_context,
            slice::from_ref(&self.view),
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
            &self.vertex,
            &self.fragment,
            &self.blend_state,
        );
        unsafe {
            device_context.PSSetSamplers(0, Some(sampler));
            device_context.VSSetShaderResources(0, Some(texture));
            device_context.PSSetShaderResources(0, Some(texture));

            device_context.DrawInstanced(4, instance_count, 0, 0);
        }
        Ok(())
    }

    fn draw_range(
        &self,
        device_context: &ID3D11DeviceContext,
        batch_params_buffer: &ID3D11Buffer,
        first_instance: u32,
        instance_count: u32,
    ) -> Result<()> {
        anyhow::ensure!(
            first_instance as usize + instance_count as usize <= self.buffer_size,
            "DirectX instance range exceeds the {} buffer",
            self.label
        );
        update_batch_start(device_context, batch_params_buffer, first_instance)?;
        set_pipeline_state(
            device_context,
            slice::from_ref(&self.view),
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
            &self.vertex,
            &self.fragment,
            &self.blend_state,
        );
        unsafe {
            device_context.DrawInstanced(4, instance_count, 0, 0);
        }
        Ok(())
    }

    fn draw_range_with_texture(
        &self,
        device_context: &ID3D11DeviceContext,
        texture: &[Option<ID3D11ShaderResourceView>],
        batch_params_buffer: &ID3D11Buffer,
        sampler: &[Option<ID3D11SamplerState>],
        first_instance: u32,
        instance_count: u32,
    ) -> Result<()> {
        anyhow::ensure!(
            first_instance as usize + instance_count as usize <= self.buffer_size,
            "DirectX instance range exceeds the {} buffer",
            self.label
        );
        update_batch_start(device_context, batch_params_buffer, first_instance)?;
        set_pipeline_state(
            device_context,
            slice::from_ref(&self.view),
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
            &self.vertex,
            &self.fragment,
            &self.blend_state,
        );
        unsafe {
            device_context.PSSetSamplers(0, Some(sampler));
            device_context.VSSetShaderResources(0, Some(texture));
            device_context.PSSetShaderResources(0, Some(texture));
            device_context.DrawInstanced(4, instance_count, 0, 0);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct PathRasterizationSprite {
    xy_position: Point<ScaledPixels>,
    st_position: Point<f32>,
    color: Background,
    bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct PathSprite {
    bounds: Bounds<ScaledPixels>,
}

/// One external surface, as the vertex shader reads it.
///
/// The field order and the trailing pad word mirror `ExternalSurface` in `shaders.hlsl` exactly.
/// The layout is deliberately free of any vector that straddles a 16-byte boundary, and the affine
/// is carried as `TransformationMatrix`'s two row-major rows plus a translation rather than as a
/// matrix type, so the two sides cannot disagree about matrix packing.
///
/// The content mask is absent on purpose: the clip is a scissor rectangle, not a shader input.
#[derive(Clone, Copy)]
#[repr(C)]
struct ExternalSurfaceInstance {
    /// The target placement.
    bounds: Bounds<ScaledPixels>,
    /// The crop, normalized against the registered surface size. The whole surface is `0..1`.
    source_uv: Bounds<f32>,
    /// The affine, applied about the top-left corner of `bounds`.
    transform: TransformationMatrix,
    /// The group opacity, of which this composite is the sole owner.
    opacity: f32,
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<ExternalSurfaceInstance>() == 64);

/// The per-surface state that is bound rather than uploaded.
struct ExternalSurfaceDraw {
    /// The registry's view of the surface, resolved from the opaque handle.
    texture: Option<ID3D11ShaderResourceView>,
    /// Which of the pipeline's two samplers this surface is sampled with.
    sampling: ExternalSampling,
    /// The content mask, as a scissor rectangle.
    scissor: RECT,
    /// The publication this occurrence belongs to, carried so the binding proof can be recorded at
    /// the draw command rather than at `resolve`.
    handle: ExternalSurfaceHandle,
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
/// the nearest integer is exact here rather than a policy choice.
fn scissor_rect(content_mask: Bounds<ScaledPixels>, viewport: D3D11_VIEWPORT) -> Option<RECT> {
    let to_pixel = |value: f32, limit: f32| value.round().clamp(0.0, limit) as i32;
    let rect = RECT {
        left: to_pixel(content_mask.origin.x.0, viewport.Width),
        top: to_pixel(content_mask.origin.y.0, viewport.Height),
        right: to_pixel(
            content_mask.origin.x.0 + content_mask.size.width.0,
            viewport.Width,
        ),
        bottom: to_pixel(
            content_mask.origin.y.0 + content_mask.size.height.0,
            viewport.Height,
        ),
    };
    (rect.left < rect.right && rect.top < rect.bottom).then_some(rect)
}

/// Builds the instance buffer for `surfaces` and issues one draw per surface into whatever render
/// target is currently bound.
///
/// This is the whole body of [`DirectXRenderer::draw_surfaces`], lifted out of the renderer's own
/// borrows and given the four things it actually needs — the device and its immediate context, the
/// external-surface pipeline, the batch-parameter constant buffer, and the viewport the content
/// mask is clamped against — plus the registry the opaque handles resolve against. Nothing about
/// the sequence changes: the placement fields are still applied in the frozen order of the bridge
/// contract (crop, placement into `bounds`, the affine about the top-left corner of `bounds`, the
/// content-mask clip, then the group opacity), the first three computed here and finished in
/// `external_surface_vertex`, the clip a scissor rectangle and the opacity the fragment shader's
/// single multiply.
///
/// The seam exists so that the pixels this path produces can be observed without a window: a test
/// binds an offscreen render target with [`bind_frame`], calls this, and reads the result back.
/// Every other caller reaches it through [`DirectXRenderer::draw_surfaces`].
///
/// A surface whose handle no longer resolves — a stale generation after a device loss, or an id
/// that was retired — is skipped and counted, never a panic and never a draw from stale content:
/// `allow_stale_reuse` is off in the capability snapshot.
fn draw_surfaces_into_frame(
    device: &ID3D11Device,
    device_context: &ID3D11DeviceContext,
    pipeline: &mut ExternalSurfacePipeline,
    batch_params_buffer: &ID3D11Buffer,
    viewport: D3D11_VIEWPORT,
    registry: &mut ExternalSurfaceRegistry,
    surfaces: &[PaintSurface],
) -> Result<()> {
    let mut instances = Vec::with_capacity(surfaces.len());
    let mut draws = Vec::with_capacity(surfaces.len());
    for surface in surfaces {
        let descriptor = match &surface.source {
            SurfaceSource::External(descriptor) => descriptor,
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
        let Some(scissor) = scissor_rect(surface.content_mask.bounds, viewport) else {
            continue;
        };

        instances.push(ExternalSurfaceInstance {
            bounds: surface.bounds,
            source_uv,
            transform: surface.transform,
            opacity: surface.opacity,
            _pad: 0,
        });
        draws.push(ExternalSurfaceDraw {
            texture: Some(texture),
            sampling: descriptor.sampling,
            scissor,
            handle,
        });
    }

    if draws.is_empty() {
        return Ok(());
    }

    pipeline
        .pipeline
        .update_buffer(device, device_context, &instances)?;

    unsafe { device_context.RSSetState(&pipeline.scissor_rasterizer_state) };
    let mut result = Ok(());
    for (index, draw) in draws.iter().enumerate() {
        unsafe { device_context.RSSetScissorRects(Some(slice::from_ref(&draw.scissor))) };
        let sampler = match draw.sampling {
            ExternalSampling::Nearest => &pipeline.sampler_nearest,
            ExternalSampling::Linear => &pipeline.sampler_linear,
        };
        // Contract 1.1: the binding proof is recorded only for a draw command that actually
        // succeeded. `resolve` above is not evidence — a resolved surface can still be skipped by
        // the crop check or by a content mask that clips it away entirely — and a failed draw is
        // not evidence either, which is why this sits behind the result check below.
        let cizilen_handle = draw.handle;
        result = pipeline.pipeline.draw_range_with_texture(
            device_context,
            slice::from_ref(&draw.texture),
            batch_params_buffer,
            slice::from_ref(sampler),
            index as u32,
            1,
        );
        if result.is_err() {
            break;
        }
        registry.note_drawn(cizilen_handle);
    }
    // Restored unconditionally: leaving the scissor-enabled state installed would clip every batch
    // drawn after this one to the last surface's content mask.
    unsafe { device_context.RSSetState(&pipeline.default_rasterizer_state) };
    result
}

impl Drop for DirectXRenderer {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        if let Some(devices) = &self.devices {
            report_live_objects(&devices.device).ok();
        }
    }
}

#[inline]
fn get_comp_device(dxgi_device: &IDXGIDevice) -> Result<IDCompositionDevice> {
    Ok(unsafe { DCompositionCreateDevice(dxgi_device)? })
}

fn create_swap_chain_for_composition(
    dxgi_factory: &IDXGIFactory6,
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: RENDER_TARGET_FORMAT,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFER_COUNT as u32,
        // Composition SwapChains only support the DXGI_SCALING_STRETCH Scaling.
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };
    Ok(unsafe { dxgi_factory.CreateSwapChainForComposition(device, &desc, None)? })
}

fn create_swap_chain(
    dxgi_factory: &IDXGIFactory6,
    device: &ID3D11Device,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    use windows::Win32::Graphics::Dxgi::DXGI_MWA_NO_ALT_ENTER;

    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: RENDER_TARGET_FORMAT,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFER_COUNT as u32,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    let swap_chain =
        unsafe { dxgi_factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None) }?;
    unsafe { dxgi_factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER) }?;
    Ok(swap_chain)
}

#[inline]
fn create_resources(
    devices: &DirectXRendererDevices,
    swap_chain: &IDXGISwapChain1,
    width: u32,
    height: u32,
) -> Result<(
    ID3D11Texture2D,
    Option<ID3D11RenderTargetView>,
    ID3D11Texture2D,
    Option<ID3D11ShaderResourceView>,
    ID3D11Texture2D,
    Option<ID3D11RenderTargetView>,
    D3D11_VIEWPORT,
)> {
    let (render_target, render_target_view) =
        create_render_target_and_its_view(swap_chain, &devices.device)?;
    let (path_intermediate_texture, path_intermediate_srv) =
        create_path_intermediate_texture(&devices.device, width, height)?;
    let (path_intermediate_msaa_texture, path_intermediate_msaa_view) =
        create_path_intermediate_msaa_texture_and_view(&devices.device, width, height)?;
    let viewport = D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: width as f32,
        Height: height as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };
    Ok((
        render_target,
        render_target_view,
        path_intermediate_texture,
        path_intermediate_srv,
        path_intermediate_msaa_texture,
        path_intermediate_msaa_view,
        viewport,
    ))
}

#[inline]
fn create_render_target_and_its_view(
    swap_chain: &IDXGISwapChain1,
    device: &ID3D11Device,
) -> Result<(ID3D11Texture2D, Option<ID3D11RenderTargetView>)> {
    let render_target: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0) }?;
    let mut render_target_view = None;
    unsafe { device.CreateRenderTargetView(&render_target, None, Some(&mut render_target_view))? };
    Ok((render_target, render_target_view))
}

#[inline]
fn create_path_intermediate_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, Option<ID3D11ShaderResourceView>)> {
    let texture = unsafe {
        let mut output = None;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: RENDER_TARGET_FORMAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        device.CreateTexture2D(&desc, None, Some(&mut output))?;
        output.unwrap()
    };

    let mut shader_resource_view = None;
    unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut shader_resource_view))? };

    Ok((texture, Some(shader_resource_view.unwrap())))
}

#[inline]
fn create_path_intermediate_msaa_texture_and_view(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, Option<ID3D11RenderTargetView>)> {
    let msaa_texture = unsafe {
        let mut output = None;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: RENDER_TARGET_FORMAT,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: PATH_MULTISAMPLE_COUNT,
                Quality: D3D11_STANDARD_MULTISAMPLE_PATTERN.0 as u32,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        device.CreateTexture2D(&desc, None, Some(&mut output))?;
        output.unwrap()
    };
    let mut msaa_view = None;
    unsafe { device.CreateRenderTargetView(&msaa_texture, None, Some(&mut msaa_view))? };
    Ok((msaa_texture, Some(msaa_view.unwrap())))
}

#[inline]
fn set_rasterizer_state(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Result<()> {
    let rasterizer_state = create_rasterizer_state(device, false)?;
    unsafe { device_context.RSSetState(&rasterizer_state) };
    Ok(())
}

/// The renderer's rasterizer state, optionally with scissor testing enabled.
///
/// Everything except `ScissorEnable` is the same in both, so the scissor-enabled state the
/// external-surface pipeline installs differs from the default one in exactly that respect and
/// nothing else.
#[inline]
fn create_rasterizer_state(
    device: &ID3D11Device,
    scissor_enable: bool,
) -> Result<ID3D11RasterizerState> {
    let desc = D3D11_RASTERIZER_DESC {
        FillMode: D3D11_FILL_SOLID,
        CullMode: D3D11_CULL_NONE,
        FrontCounterClockwise: false.into(),
        DepthBias: 0,
        DepthBiasClamp: 0.0,
        SlopeScaledDepthBias: 0.0,
        DepthClipEnable: true.into(),
        ScissorEnable: scissor_enable.into(),
        MultisampleEnable: true.into(),
        AntialiasedLineEnable: false.into(),
    };
    unsafe {
        let mut state = None;
        device.CreateRasterizerState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

/// The sampler of the external-surface pipeline, in one of the contract's two sampling modes.
///
/// The address mode is `CLAMP` rather than the `WRAP` of GPUI's general sampler: a crop samples a
/// sub-rectangle of the surface, and wrapping would fetch the opposite edge of the surface into
/// the filter footprint at the boundary.
#[inline]
fn create_external_surface_sampler(
    device: &ID3D11Device,
    filter: D3D11_FILTER,
) -> Result<Option<ID3D11SamplerState>> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: filter,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_ALWAYS,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: D3D11_FLOAT32_MAX,
    };
    unsafe {
        let mut state = None;
        device.CreateSamplerState(&desc, Some(&mut state))?;
        Ok(state)
    }
}

// https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_blend_desc
#[inline]
fn create_blend_state(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

#[inline]
fn create_blend_state_for_subpixel_rendering(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC1_COLOR;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC1_COLOR;
    // It does not make sense to draw transparent subpixel-rendered text, since it cannot be meaningfully alpha-blended onto anything else.
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO;
    desc.RenderTarget[0].RenderTargetWriteMask =
        D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8 & !D3D11_COLOR_WRITE_ENABLE_ALPHA.0 as u8;

    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

#[inline]
fn create_blend_state_for_path_rasterization(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    // If the feature level is set to greater than D3D_FEATURE_LEVEL_9_3, the display
    // device performs the blend in linear space, which is ideal.
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE;
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

#[inline]
fn create_blend_state_for_path_sprite(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    // If the feature level is set to greater than D3D_FEATURE_LEVEL_9_3, the display
    // device performs the blend in linear space, which is ideal.
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE;
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

/// The blend state of the external-surface pipeline: **premultiplied** alpha.
///
/// This is the one place in this renderer where the blend state is dictated by a contract rather
/// than by GPUI's own conventions. Contract v1 has premultiplied alpha as its only alpha mode,
/// because the bridge includes linear sampling and an affine transform, and straight alpha
/// corrupts edge colors under both; group opacity is also exactly a scalar multiply on
/// premultiplied content. GPUI's general blend state ([`create_blend_state`]) is straight alpha
/// (`SRC_ALPHA`/`INV_SRC_ALPHA`) and must not be used here.
///
/// The destination alpha factor is `INV_SRC_ALPHA` rather than the `ONE` of the path-sprite state:
/// compositing premultiplied source over destination is `src + dst * (1 - src.a)` in all four
/// channels, and the swap chain is `DXGI_ALPHA_MODE_PREMULTIPLIED`.
#[inline]
fn create_blend_state_for_external_surface(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        Ok(state.unwrap())
    }
}

#[inline]
fn create_vertex_shader(device: &ID3D11Device, bytes: &[u8]) -> Result<ID3D11VertexShader> {
    unsafe {
        let mut shader = None;
        device.CreateVertexShader(bytes, None, Some(&mut shader))?;
        Ok(shader.unwrap())
    }
}

#[inline]
fn create_fragment_shader(device: &ID3D11Device, bytes: &[u8]) -> Result<ID3D11PixelShader> {
    unsafe {
        let mut shader = None;
        device.CreatePixelShader(bytes, None, Some(&mut shader))?;
        Ok(shader.unwrap())
    }
}

#[inline]
fn create_constant_buffer<T>(device: &ID3D11Device) -> Result<Option<ID3D11Buffer>> {
    const { assert!(std::mem::size_of::<T>() != 0 && std::mem::size_of::<T>().is_multiple_of(16)) };
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of::<T>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer = None;
    unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer)) }?;
    Ok(buffer)
}

#[inline]
fn create_buffer(
    device: &ID3D11Device,
    element_size: usize,
    buffer_size: usize,
) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: (element_size * buffer_size) as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
        StructureByteStride: element_size as u32,
    };
    let mut buffer = None;
    unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer)) }?;
    Ok(buffer.unwrap())
}

#[inline]
fn create_buffer_view(
    device: &ID3D11Device,
    buffer: &ID3D11Buffer,
) -> Result<Option<ID3D11ShaderResourceView>> {
    let mut view = None;
    unsafe { device.CreateShaderResourceView(buffer, None, Some(&mut view)) }?;
    Ok(view)
}

#[inline]
fn update_buffer<T>(
    device_context: &ID3D11DeviceContext,
    buffer: &ID3D11Buffer,
    data: &[T],
) -> Result<()> {
    unsafe {
        let mut dest = std::mem::zeroed();
        device_context.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut dest))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), dest.pData as _, data.len());
        device_context.Unmap(buffer, 0);
    }
    Ok(())
}

#[inline]
fn update_batch_start(
    device_context: &ID3D11DeviceContext,
    buffer: &ID3D11Buffer,
    first_instance: u32,
) -> Result<()> {
    update_buffer(
        device_context,
        buffer,
        &[BatchParams {
            start_index: first_instance,
            _padding: [0; 3],
        }],
    )
}

/// Clears `render_target_view` and installs the per-frame state every pipeline in this renderer
/// reads: the render target itself, the viewport, and the two constant buffers — the global
/// parameters at `b0` and the batch parameters at `b1`.
///
/// This is the whole body of [`DirectXRenderer::pre_draw`], lifted out of the renderer's own
/// borrows so that it can also be pointed at an offscreen render target. `viewport_size` in
/// `global_params` is what every vertex shader divides by to reach normalized device coordinates,
/// so a caller that binds a frame through anything but this function would be testing its own
/// arithmetic rather than the renderer's.
fn bind_frame(
    device_context: &ID3D11DeviceContext,
    render_target_view: &Option<ID3D11RenderTargetView>,
    viewport: &D3D11_VIEWPORT,
    globals: &DirectXGlobalElements,
    global_params: &GlobalParams,
    clear_color: &[f32; 4],
) -> Result<()> {
    update_buffer(
        device_context,
        globals.global_params_buffer.as_ref().unwrap(),
        slice::from_ref(global_params),
    )?;
    unsafe {
        device_context.ClearRenderTargetView(
            render_target_view
                .as_ref()
                .context("missing render target view")?,
            clear_color,
        );
        device_context.OMSetRenderTargets(Some(slice::from_ref(render_target_view)), None);
        device_context.RSSetViewports(Some(slice::from_ref(viewport)));
        device_context
            .VSSetConstantBuffers(0, Some(slice::from_ref(&globals.global_params_buffer)));
        device_context.VSSetConstantBuffers(1, Some(slice::from_ref(&globals.batch_params_buffer)));
        device_context
            .PSSetConstantBuffers(0, Some(slice::from_ref(&globals.global_params_buffer)));
    }
    Ok(())
}

#[inline]
fn set_pipeline_state(
    device_context: &ID3D11DeviceContext,
    buffer_view: &[Option<ID3D11ShaderResourceView>],
    topology: D3D_PRIMITIVE_TOPOLOGY,
    vertex_shader: &ID3D11VertexShader,
    fragment_shader: &ID3D11PixelShader,
    blend_state: &ID3D11BlendState,
) {
    unsafe {
        device_context.VSSetShaderResources(1, Some(buffer_view));
        device_context.PSSetShaderResources(1, Some(buffer_view));
        device_context.IASetPrimitiveTopology(topology);
        device_context.VSSetShader(vertex_shader, None);
        device_context.PSSetShader(fragment_shader, None);
        device_context.OMSetBlendState(blend_state, None, 0xFFFFFFFF);
    }
}

#[cfg(debug_assertions)]
fn report_live_objects(device: &ID3D11Device) -> Result<()> {
    let debug_device: ID3D11Debug = device.cast()?;
    unsafe {
        debug_device.ReportLiveDeviceObjects(D3D11_RLDO_DETAIL)?;
    }
    Ok(())
}

pub(crate) const BUFFER_COUNT: usize = 3;

pub(crate) mod shader_resources {
    use anyhow::Result;
    use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0};

    #[cfg(debug_assertions)]
    use windows::{
        Win32::Graphics::Direct3D::{
            Fxc::{D3DCOMPILE_DEBUG, D3DCOMPILE_SKIP_OPTIMIZATION, D3DCompileFromFile},
            ID3DBlob,
        },
        core::{HSTRING, PCSTR},
    };

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub(crate) enum ShaderModule {
        Quad,
        Shadow,
        Underline,
        PathRasterization,
        PathSprite,
        MonochromeSprite,
        SubpixelSprite,
        PolychromeSprite,
        EmojiRasterization,
        ExternalSurface,
    }

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub(crate) enum ShaderTarget {
        Vertex,
        Fragment,
    }

    /// The shader model a module's bytecode is compiled against.
    ///
    /// This exists because bytecode is not portable downwards: the S1 spike showed that shader
    /// model 5.0 bytecode is rejected with `E_INVALIDARG` when it is handed to a device created at
    /// feature level 10.1, which GPUI still accepts. Every GPUI pipeline other than the
    /// external-surface one is compiled once, at [`ShaderModel::DEFAULT`], which every supported
    /// feature level accepts.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub(crate) enum ShaderModel {
        /// `vs_4_0`/`ps_4_0`: the floor, for feature levels below 11.0.
        ///
        /// The external-surface shaders read a `StructuredBuffer`, which needs the
        /// `ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x` device capability that
        /// `DirectXDevices::new` already refuses to start without. Should `fxc` ever reject the
        /// 4.0 profile for it, [`Self::Sm4_1`] is the drop-in replacement here: it is one step up,
        /// every GPUI pipeline already ships as 4.1, and feature level 10.1 accepts it.
        Sm4_0,
        /// `vs_4_1`/`ps_4_1`: what every non-external GPUI pipeline is built with.
        Sm4_1,
        /// `vs_5_0`/`ps_5_0`: feature level 11.0 and above only.
        Sm5_0,
    }

    impl ShaderModel {
        /// The model every pipeline that does not select one explicitly is compiled against.
        pub(crate) const DEFAULT: Self = Self::Sm4_1;

        /// The highest model a device at `feature_level` accepts.
        ///
        /// Feature level 11.0 is the boundary: at and above it shader model 5.0 is available; below
        /// it — which for GPUI means exactly feature level 10.1 — only the 4.x models are, and
        /// handing such a device 5.0 bytecode fails at shader creation.
        pub(crate) fn for_feature_level(feature_level: D3D_FEATURE_LEVEL) -> Self {
            if feature_level.0 >= D3D_FEATURE_LEVEL_11_0.0 {
                Self::Sm5_0
            } else {
                Self::Sm4_0
            }
        }

        /// The fxc profile string of this model for `target`, `NUL`-terminated for `PCSTR`.
        #[cfg(debug_assertions)]
        fn profile(self, target: ShaderTarget) -> &'static str {
            match (self, target) {
                (Self::Sm4_0, ShaderTarget::Vertex) => "vs_4_0\0",
                (Self::Sm4_0, ShaderTarget::Fragment) => "ps_4_0\0",
                (Self::Sm4_1, ShaderTarget::Vertex) => "vs_4_1\0",
                (Self::Sm4_1, ShaderTarget::Fragment) => "ps_4_1\0",
                (Self::Sm5_0, ShaderTarget::Vertex) => "vs_5_0\0",
                (Self::Sm5_0, ShaderTarget::Fragment) => "ps_5_0\0",
            }
        }
    }

    pub(crate) struct RawShaderBytes<'t> {
        inner: &'t [u8],

        #[cfg(debug_assertions)]
        _blob: ID3DBlob,
    }

    impl<'t> RawShaderBytes<'t> {
        pub(crate) fn new(module: ShaderModule, target: ShaderTarget) -> Result<Self> {
            Self::new_with_shader_model(module, target, ShaderModel::DEFAULT)
        }

        pub(crate) fn new_with_shader_model(
            module: ShaderModule,
            target: ShaderTarget,
            shader_model: ShaderModel,
        ) -> Result<Self> {
            #[cfg(not(debug_assertions))]
            {
                Ok(Self::from_bytes(module, target, shader_model))
            }
            #[cfg(debug_assertions)]
            {
                let blob = build_shader_blob(module, target, shader_model)?;
                let inner = unsafe {
                    std::slice::from_raw_parts(
                        blob.GetBufferPointer() as *const u8,
                        blob.GetBufferSize(),
                    )
                };
                Ok(Self { inner, _blob: blob })
            }
        }

        pub(crate) fn as_bytes(&'t self) -> &'t [u8] {
            self.inner
        }

        #[cfg(not(debug_assertions))]
        fn from_bytes(
            module: ShaderModule,
            target: ShaderTarget,
            shader_model: ShaderModel,
        ) -> Self {
            // Only the external-surface module is compiled for more than one model, so every other
            // arm ignores it: `build.rs` emits a single set of constants for them.
            if matches!(module, ShaderModule::ExternalSurface) {
                let bytes = match (shader_model, target) {
                    (ShaderModel::Sm5_0, ShaderTarget::Vertex) => EXTERNAL_SURFACE_VERTEX_BYTES_SM5,
                    (ShaderModel::Sm5_0, ShaderTarget::Fragment) => {
                        EXTERNAL_SURFACE_FRAGMENT_BYTES_SM5
                    }
                    (_, ShaderTarget::Vertex) => EXTERNAL_SURFACE_VERTEX_BYTES_SM4,
                    (_, ShaderTarget::Fragment) => EXTERNAL_SURFACE_FRAGMENT_BYTES_SM4,
                };
                return Self { inner: bytes };
            }

            let bytes = match module {
                ShaderModule::Quad => match target {
                    ShaderTarget::Vertex => QUAD_VERTEX_BYTES,
                    ShaderTarget::Fragment => QUAD_FRAGMENT_BYTES,
                },
                ShaderModule::Shadow => match target {
                    ShaderTarget::Vertex => SHADOW_VERTEX_BYTES,
                    ShaderTarget::Fragment => SHADOW_FRAGMENT_BYTES,
                },
                ShaderModule::Underline => match target {
                    ShaderTarget::Vertex => UNDERLINE_VERTEX_BYTES,
                    ShaderTarget::Fragment => UNDERLINE_FRAGMENT_BYTES,
                },
                ShaderModule::PathRasterization => match target {
                    ShaderTarget::Vertex => PATH_RASTERIZATION_VERTEX_BYTES,
                    ShaderTarget::Fragment => PATH_RASTERIZATION_FRAGMENT_BYTES,
                },
                ShaderModule::PathSprite => match target {
                    ShaderTarget::Vertex => PATH_SPRITE_VERTEX_BYTES,
                    ShaderTarget::Fragment => PATH_SPRITE_FRAGMENT_BYTES,
                },
                ShaderModule::MonochromeSprite => match target {
                    ShaderTarget::Vertex => MONOCHROME_SPRITE_VERTEX_BYTES,
                    ShaderTarget::Fragment => MONOCHROME_SPRITE_FRAGMENT_BYTES,
                },
                ShaderModule::SubpixelSprite => match target {
                    ShaderTarget::Vertex => SUBPIXEL_SPRITE_VERTEX_BYTES,
                    ShaderTarget::Fragment => SUBPIXEL_SPRITE_FRAGMENT_BYTES,
                },
                ShaderModule::PolychromeSprite => match target {
                    ShaderTarget::Vertex => POLYCHROME_SPRITE_VERTEX_BYTES,
                    ShaderTarget::Fragment => POLYCHROME_SPRITE_FRAGMENT_BYTES,
                },
                ShaderModule::EmojiRasterization => match target {
                    ShaderTarget::Vertex => EMOJI_RASTERIZATION_VERTEX_BYTES,
                    ShaderTarget::Fragment => EMOJI_RASTERIZATION_FRAGMENT_BYTES,
                },
                // Handled above, where the shader model selects between two builds.
                ShaderModule::ExternalSurface => unreachable!(),
            };
            Self { inner: bytes }
        }
    }

    #[cfg(debug_assertions)]
    pub(super) fn build_shader_blob(
        entry: ShaderModule,
        target: ShaderTarget,
        shader_model: ShaderModel,
    ) -> Result<ID3DBlob> {
        unsafe {
            use windows::Win32::Graphics::{
                Direct3D::ID3DInclude, Hlsl::D3D_COMPILE_STANDARD_FILE_INCLUDE,
            };

            let shader_name = if matches!(entry, ShaderModule::EmojiRasterization) {
                "color_text_raster.hlsl"
            } else {
                "shaders.hlsl"
            };

            let entry = format!(
                "{}_{}\0",
                entry.as_str(),
                match target {
                    ShaderTarget::Vertex => "vertex",
                    ShaderTarget::Fragment => "fragment",
                }
            );
            let target = shader_model.profile(target);

            let mut compile_blob = None;
            let mut error_blob = None;
            let shader_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(&format!("src/{}", shader_name))
                .canonicalize()?;

            let entry_point = PCSTR::from_raw(entry.as_ptr());
            let target_cstr = PCSTR::from_raw(target.as_ptr());

            // really dirty trick because winapi bindings are unhappy otherwise
            let include_handler = &std::mem::transmute::<usize, ID3DInclude>(
                D3D_COMPILE_STANDARD_FILE_INCLUDE as usize,
            );

            let ret = D3DCompileFromFile(
                &HSTRING::from(shader_path.to_str().unwrap()),
                None,
                include_handler,
                entry_point,
                target_cstr,
                D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION,
                0,
                &mut compile_blob,
                Some(&mut error_blob),
            );
            if ret.is_err() {
                let Some(error_blob) = error_blob else {
                    return Err(anyhow::anyhow!("{ret:?}"));
                };

                let error_string =
                    std::ffi::CStr::from_ptr(error_blob.GetBufferPointer() as *const i8)
                        .to_string_lossy();
                log::error!("Shader compile error: {}", error_string);
                return Err(anyhow::anyhow!("Compile error: {}", error_string));
            }
            Ok(compile_blob.unwrap())
        }
    }

    #[cfg(not(debug_assertions))]
    include!(concat!(env!("OUT_DIR"), "/shaders_bytes.rs"));

    #[cfg(debug_assertions)]
    impl ShaderModule {
        pub fn as_str(self) -> &'static str {
            match self {
                ShaderModule::Quad => "quad",
                ShaderModule::Shadow => "shadow",
                ShaderModule::Underline => "underline",
                ShaderModule::PathRasterization => "path_rasterization",
                ShaderModule::PathSprite => "path_sprite",
                ShaderModule::MonochromeSprite => "monochrome_sprite",
                ShaderModule::SubpixelSprite => "subpixel_sprite",
                ShaderModule::PolychromeSprite => "polychrome_sprite",
                ShaderModule::EmojiRasterization => "emoji_rasterization",
                ShaderModule::ExternalSurface => "external_surface",
            }
        }
    }
}

mod nvidia {
    use std::{
        ffi::CStr,
        os::raw::{c_char, c_int, c_uint},
    };

    use anyhow::Result;
    use windows::{Win32::System::LibraryLoader::GetProcAddress, core::s};

    use crate::with_dll_library;

    // https://github.com/NVIDIA/nvapi/blob/7cb76fce2f52de818b3da497af646af1ec16ce27/nvapi_lite_common.h#L180
    const NVAPI_SHORT_STRING_MAX: usize = 64;

    // https://github.com/NVIDIA/nvapi/blob/7cb76fce2f52de818b3da497af646af1ec16ce27/nvapi_lite_common.h#L235
    #[allow(non_camel_case_types)]
    type NvAPI_ShortString = [c_char; NVAPI_SHORT_STRING_MAX];

    // https://github.com/NVIDIA/nvapi/blob/7cb76fce2f52de818b3da497af646af1ec16ce27/nvapi_lite_common.h#L447
    #[allow(non_camel_case_types)]
    type NvAPI_SYS_GetDriverAndBranchVersion_t = unsafe extern "C" fn(
        driver_version: *mut c_uint,
        build_branch_string: *mut NvAPI_ShortString,
    ) -> c_int;

    pub(super) fn get_driver_version() -> Result<String> {
        #[cfg(target_pointer_width = "64")]
        let nvidia_dll_name = s!("nvapi64.dll");
        #[cfg(target_pointer_width = "32")]
        let nvidia_dll_name = s!("nvapi.dll");

        with_dll_library(nvidia_dll_name, |nvidia_dll| unsafe {
            let nvapi_query_addr = GetProcAddress(nvidia_dll, s!("nvapi_QueryInterface"))
                .ok_or_else(|| anyhow::anyhow!("Failed to get nvapi_QueryInterface address"))?;
            let nvapi_query: extern "C" fn(u32) -> *mut () = std::mem::transmute(nvapi_query_addr);

            // https://github.com/NVIDIA/nvapi/blob/7cb76fce2f52de818b3da497af646af1ec16ce27/nvapi_interface.h#L41
            let nvapi_get_driver_version_ptr = nvapi_query(0x2926aaad);
            if nvapi_get_driver_version_ptr.is_null() {
                anyhow::bail!("Failed to get NVIDIA driver version function pointer");
            }
            let nvapi_get_driver_version: NvAPI_SYS_GetDriverAndBranchVersion_t =
                std::mem::transmute(nvapi_get_driver_version_ptr);

            let mut driver_version: c_uint = 0;
            let mut build_branch_string: NvAPI_ShortString = [0; NVAPI_SHORT_STRING_MAX];
            let result = nvapi_get_driver_version(
                &mut driver_version as *mut c_uint,
                &mut build_branch_string as *mut NvAPI_ShortString,
            );

            if result != 0 {
                anyhow::bail!(
                    "Failed to get NVIDIA driver version, error code: {}",
                    result
                );
            }
            let major = driver_version / 100;
            let minor = driver_version % 100;
            let branch_string = CStr::from_ptr(build_branch_string.as_ptr());
            Ok(format!(
                "{}.{} {}",
                major,
                minor,
                branch_string.to_string_lossy()
            ))
        })
    }
}

mod amd {
    use std::os::raw::{c_char, c_int, c_void};

    use anyhow::Result;
    use windows::{Win32::System::LibraryLoader::GetProcAddress, core::s};

    use crate::with_dll_library;

    // https://github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/blob/5d8812d703d0335741b6f7ffc37838eeb8b967f7/ags_lib/inc/amd_ags.h#L145
    const AGS_CURRENT_VERSION: i32 = (6 << 22) | (3 << 12);

    // https://github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/blob/5d8812d703d0335741b6f7ffc37838eeb8b967f7/ags_lib/inc/amd_ags.h#L204
    // This is an opaque type, using struct to represent it properly for FFI
    #[repr(C)]
    struct AGSContext {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct AGSGPUInfo {
        pub driver_version: *const c_char,
        pub radeon_software_version: *const c_char,
        pub num_devices: c_int,
        pub devices: *mut c_void,
    }

    // https://github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/blob/5d8812d703d0335741b6f7ffc37838eeb8b967f7/ags_lib/inc/amd_ags.h#L429
    #[allow(non_camel_case_types)]
    type agsInitialize_t = unsafe extern "C" fn(
        version: c_int,
        config: *const c_void,
        context: *mut *mut AGSContext,
        gpu_info: *mut AGSGPUInfo,
    ) -> c_int;

    // https://github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/blob/5d8812d703d0335741b6f7ffc37838eeb8b967f7/ags_lib/inc/amd_ags.h#L436
    #[allow(non_camel_case_types)]
    type agsDeInitialize_t = unsafe extern "C" fn(context: *mut AGSContext) -> c_int;

    pub(super) fn get_driver_version() -> Result<String> {
        #[cfg(target_pointer_width = "64")]
        let amd_dll_name = s!("amd_ags_x64.dll");
        #[cfg(target_pointer_width = "32")]
        let amd_dll_name = s!("amd_ags_x86.dll");

        with_dll_library(amd_dll_name, |amd_dll| unsafe {
            let ags_initialize_addr = GetProcAddress(amd_dll, s!("agsInitialize"))
                .ok_or_else(|| anyhow::anyhow!("Failed to get agsInitialize address"))?;
            let ags_deinitialize_addr = GetProcAddress(amd_dll, s!("agsDeInitialize"))
                .ok_or_else(|| anyhow::anyhow!("Failed to get agsDeInitialize address"))?;

            let ags_initialize: agsInitialize_t = std::mem::transmute(ags_initialize_addr);
            let ags_deinitialize: agsDeInitialize_t = std::mem::transmute(ags_deinitialize_addr);

            let mut context: *mut AGSContext = std::ptr::null_mut();
            let mut gpu_info: AGSGPUInfo = AGSGPUInfo {
                driver_version: std::ptr::null(),
                radeon_software_version: std::ptr::null(),
                num_devices: 0,
                devices: std::ptr::null_mut(),
            };

            let result = ags_initialize(
                AGS_CURRENT_VERSION,
                std::ptr::null(),
                &mut context,
                &mut gpu_info,
            );
            if result != 0 {
                anyhow::bail!("Failed to initialize AMD AGS, error code: {}", result);
            }

            // Vulkan actually returns this as the driver version
            let software_version = if !gpu_info.radeon_software_version.is_null() {
                std::ffi::CStr::from_ptr(gpu_info.radeon_software_version)
                    .to_string_lossy()
                    .into_owned()
            } else {
                "Unknown Radeon Software Version".to_string()
            };

            let driver_version = if !gpu_info.driver_version.is_null() {
                std::ffi::CStr::from_ptr(gpu_info.driver_version)
                    .to_string_lossy()
                    .into_owned()
            } else {
                "Unknown Radeon Driver Version".to_string()
            };

            ags_deinitialize(context);
            Ok(format!("{} ({})", software_version, driver_version))
        })
    }
}

mod dxgi {
    use windows::{
        Win32::Graphics::Dxgi::{IDXGIAdapter1, IDXGIDevice},
        core::Interface,
    };

    pub(super) fn get_driver_version(adapter: &IDXGIAdapter1) -> anyhow::Result<String> {
        let number = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID as _) }?;
        Ok(format!(
            "{}.{}.{}.{}",
            number >> 48,
            (number >> 32) & 0xFFFF,
            (number >> 16) & 0xFFFF,
            number & 0xFFFF
        ))
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the parent module glob-imports `gpui`, whose own `test`
    // attribute macro would then shadow the standard one.
    use super::{ExternalSurfaceInstance, ShaderModel, scissor_rect, source_uv};
    use gpui::{Bounds, DevicePixels, Point, ScaledPixels, Size, TransformationMatrix};
    use windows::Win32::Graphics::{
        Direct3D::{D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
        Direct3D11::D3D11_VIEWPORT,
    };

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

    fn viewport(width: f32, height: f32) -> D3D11_VIEWPORT {
        D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: width,
            Height: height,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }
    }

    /// The contract's `None` crop is the *whole* surface, which has to become the full sampling
    /// rectangle rather than an empty one.
    #[test]
    fn no_crop_samples_the_whole_surface() {
        let uv = source_uv(None, device_size(256, 128)).unwrap();
        assert_eq!(uv.origin.x, 0.0);
        assert_eq!(uv.origin.y, 0.0);
        assert_eq!(uv.size.width, 1.0);
        assert_eq!(uv.size.height, 1.0);
    }

    #[test]
    fn a_crop_is_normalized_against_the_registered_surface() {
        let uv = source_uv(Some(device_bounds(64, 32, 128, 64)), device_size(256, 128)).unwrap();
        assert_eq!(uv.origin.x, 0.25);
        assert_eq!(uv.origin.y, 0.25);
        assert_eq!(uv.size.width, 0.5);
        assert_eq!(uv.size.height, 0.5);

        // The far edge of a full-surface crop lands exactly on 1.0, not past it.
        let uv = source_uv(Some(device_bounds(0, 0, 256, 128)), device_size(256, 128)).unwrap();
        assert_eq!(uv.origin.x + uv.size.width, 1.0);
        assert_eq!(uv.origin.y + uv.size.height, 1.0);
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
            assert_eq!(source_uv(Some(crop), surface), None, "{crop:?}");
        }
    }

    #[test]
    fn a_content_mask_becomes_a_scissor_rectangle() {
        let rect = scissor_rect(
            scaled_bounds(10.0, 20.0, 100.0, 50.0),
            viewport(800.0, 600.0),
        )
        .expect("a mask inside the viewport clips to itself");
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (10, 20, 110, 70)
        );
    }

    #[test]
    fn a_content_mask_is_clamped_to_the_viewport() {
        let rect = scissor_rect(
            scaled_bounds(-40.0, -10.0, 2000.0, 2000.0),
            viewport(800.0, 600.0),
        )
        .expect("an oversized mask clips to the viewport");
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0, 0, 800, 600)
        );
    }

    #[test]
    fn a_content_mask_that_clips_everything_away_produces_no_rectangle() {
        let viewport = viewport(800.0, 600.0);
        assert_eq!(
            scissor_rect(scaled_bounds(10.0, 10.0, 0.0, 50.0), viewport),
            None
        );
        assert_eq!(
            scissor_rect(scaled_bounds(10.0, 10.0, 50.0, 0.0), viewport),
            None
        );
        // Entirely off-screen: both edges clamp to the same viewport border.
        assert_eq!(
            scissor_rect(scaled_bounds(900.0, 10.0, 50.0, 50.0), viewport),
            None
        );
        assert_eq!(
            scissor_rect(scaled_bounds(-100.0, 10.0, 50.0, 50.0), viewport),
            None
        );
    }

    /// The instance buffer is read by `ExternalSurface` in `shaders.hlsl`, and nothing but this
    /// test and the shader's own field order keeps the two in step.
    #[test]
    fn the_instance_layout_matches_the_shader_struct() {
        use std::mem::{offset_of, size_of};
        assert_eq!(offset_of!(ExternalSurfaceInstance, bounds), 0);
        assert_eq!(offset_of!(ExternalSurfaceInstance, source_uv), 16);
        assert_eq!(offset_of!(ExternalSurfaceInstance, transform), 32);
        assert_eq!(offset_of!(ExternalSurfaceInstance, opacity), 56);
        assert_eq!(size_of::<ExternalSurfaceInstance>(), 64);
        // No vector member of the shader struct may straddle a 16-byte boundary; the two affine
        // rows and the translation sit at 32, 40 and 48.
        assert_eq!(size_of::<TransformationMatrix>(), 24);
    }

    /// S1 evidence: shader model 5.0 bytecode is rejected with `E_INVALIDARG` on feature level
    /// 10.1 hardware, so the external pipeline picks its model from the feature level.
    #[test]
    fn the_shader_model_follows_the_feature_level() {
        assert_eq!(
            ShaderModel::for_feature_level(D3D_FEATURE_LEVEL_10_1),
            ShaderModel::Sm4_0
        );
        assert_eq!(
            ShaderModel::for_feature_level(D3D_FEATURE_LEVEL_11_0),
            ShaderModel::Sm5_0
        );
        assert_eq!(
            ShaderModel::for_feature_level(D3D_FEATURE_LEVEL_11_1),
            ShaderModel::Sm5_0
        );
        // Every other pipeline keeps the model it has always been built with.
        assert_eq!(ShaderModel::DEFAULT, ShaderModel::Sm4_1);
    }
}

#[cfg(test)]
mod external_surface_draw_tests {
    //! The GPUI-side counterpart of the S1 spike's D3D11 pixel corpus.
    //!
    //! The spike proved the *mechanism* outside GPUI: a producer pass fills a texture, a consumer
    //! pass samples it in the same frame, and named pixels of the result are compared byte for byte
    //! against fixed constants. What it could not prove is that GPUI's own consumer path agrees
    //! with it — that `external_surface_vertex` reads the structured buffer the way
    //! [`ExternalSurfaceInstance`] writes it, that the affine lands the right way round on screen,
    //! that the content mask clips where it says it does, and that the pipeline builds at all on
    //! feature level 10.1.
    //!
    //! These tests run that corpus through the real renderer code. Same producer pattern (clear to
    //! the generation colour, yellow marker triangle in the top-left corner at NDC (-1,1),
    //! (-0.5,1), (-1,0.5)), same 800x600 frame, same target rectangle at NDC (-0.5,-0.5)..
    //! (0.5,0.5), same colour constants and the same probe coordinates, so a disagreement between
    //! the two harnesses is a disagreement about GPUI's pipeline and nothing else. The only
    //! difference is the target — an offscreen render target instead of a swap chain — reached
    //! through [`bind_frame`] and [`draw_surfaces_into_frame`], which are the functions
    //! `DirectXRenderer::draw` itself calls.
    //!
    //! Building [`ExternalSurfacePipeline`] is part of the coverage rather than setup: it picks its
    //! shader model from the device's feature level, so on feature level 10.1 hardware this is
    //! where the 4.0 build is exercised and where 5.0 bytecode would be rejected with
    //! `E_INVALIDARG`.
    //!
    //! Like the other device-backed tests in this crate, everything returns early when no D3D11
    //! device can be created at all.

    // Deliberately not `use super::*`: the parent module glob-imports `gpui`, whose own `test`
    // attribute macro would then shadow the standard one.
    use super::{
        DirectXGlobalElements, ExternalSurfacePipeline, GlobalParams, ShaderModel, bind_frame,
        draw_surfaces_into_frame,
    };
    use crate::ExternalSurfaceRegistry;
    use gpui::{
        Bounds, ContentMask, DevicePixels, ExternalAlphaMode, ExternalColorSpace, ExternalSampling,
        ExternalSurfaceDescriptor, ExternalSurfaceFormat, ExternalSurfaceHandle, ExternalSyncToken,
        PaintSurface, Point, ScaledPixels, Size, SurfaceSource, TransformationMatrix,
    };
    use windows::{
        Win32::{
            Foundation::HMODULE,
            Graphics::{
                Direct3D::{
                    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
                    D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_11_1, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile,
                    ID3DBlob,
                },
                Direct3D11::*,
                Dxgi::Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32G32_FLOAT,
                    DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_SAMPLE_DESC,
                },
            },
        },
        core::{PCSTR, s},
    };

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
    /// brings its own shaders and draws through the device the producer accessor hands it.
    const PRODUCER_SHADERS: &str = r#"
struct SolidIn { float2 pos: POSITION; float4 color: COLOR; };
struct SolidOut { float4 pos: SV_Position; float4 color: COLOR; };

SolidOut vs_solid(SolidIn input) {
    SolidOut output;
    output.pos = float4(input.pos, 0.0, 1.0);
    output.color = input.color;
    return output;
}

float4 ps_solid(SolidOut input): SV_Target {
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
    /// external-surface blend state (`ONE` / `INV_SRC_ALPHA` on all four channels) computes once
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
        pitch: usize,
    }

    impl Frame {
        /// The pixel at `(x, y)`, converted out of the render target's `B8G8R8A8` memory order into
        /// RGBA so that it compares directly against the S1 constants.
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
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        globals: DirectXGlobalElements,
        pipeline: ExternalSurfacePipeline,
        registry: ExternalSurfaceRegistry,
        frame: ID3D11Texture2D,
        frame_view: Option<ID3D11RenderTargetView>,
        staging: ID3D11Texture2D,
        handle: ExternalSurfaceHandle,
    }

    impl Harness {
        /// Builds everything, or returns `None` when no D3D11 device can be created at all.
        ///
        /// Hardware is tried first so that a machine with a real adapter proves the path on the
        /// hardware it will ship on, including its feature level; WARP is the fallback so the test
        /// still runs on a headless CI worker.
        fn new() -> Option<Self> {
            let (device, context) = create_device()?;
            let pipeline = ExternalSurfacePipeline::new(&device)
                .expect("the external-surface pipeline must build on this device");
            let globals =
                DirectXGlobalElements::new(&device).expect("global elements must be creatable");

            let frame = texture(
                &device,
                FRAME_WIDTH,
                FRAME_HEIGHT,
                D3D11_BIND_RENDER_TARGET.0 as u32,
                D3D11_USAGE_DEFAULT,
                0,
            )
            .expect("the offscreen frame must be creatable");
            let mut frame_view = None;
            unsafe { device.CreateRenderTargetView(&frame, None, Some(&mut frame_view)) }
                .expect("the frame render target view must be creatable");
            let staging = texture(
                &device,
                FRAME_WIDTH,
                FRAME_HEIGHT,
                0,
                D3D11_USAGE_STAGING,
                D3D11_CPU_ACCESS_READ.0 as u32,
            )
            .expect("the staging copy of the frame must be creatable");

            let mut registry = ExternalSurfaceRegistry::new(&device);
            let (handle, producer_texture) = registry
                .register(surface_size(), ExternalSurfaceFormat::Bgra8Unorm)
                .expect("registering a 512x512 surface must be inside the provisional budget");
            producer_pass(&device, &context, &producer_texture)
                .expect("the producer pass must succeed on its own surface");

            Some(Self {
                device,
                context,
                globals,
                pipeline,
                registry,
                frame,
                frame_view,
                staging,
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
            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: FRAME_WIDTH as f32,
                Height: FRAME_HEIGHT as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            bind_frame(
                &self.context,
                &self.frame_view,
                &viewport,
                &self.globals,
                &GlobalParams {
                    // The font fields are read only by the text pipelines; `viewport_size` is the
                    // one field the external-surface vertex shader divides by, and it has to be
                    // the frame's own size for the placement to land where the corpus says.
                    gamma_ratios: [0.0; 4],
                    viewport_size: [viewport.Width, viewport.Height],
                    grayscale_enhanced_contrast: 0.0,
                    subpixel_enhanced_contrast: 0.0,
                    is_bgr: 0,
                    _pad: [0; 3],
                },
                &color_f(BACKGROUND),
            )
            .expect("binding the offscreen frame");

            draw_surfaces_into_frame(
                &self.device,
                &self.context,
                &mut self.pipeline,
                self.globals
                    .batch_params_buffer
                    .as_ref()
                    .expect("batch params buffer"),
                viewport,
                &mut self.registry,
                surfaces,
            )
            .expect("drawing the external surface");

            self.read_back()
        }

        fn read_back(&self) -> Frame {
            unsafe { self.context.CopyResource(&self.staging, &self.frame) };
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            }
            .expect("mapping the staging copy of the frame");
            let pitch = mapped.RowPitch as usize;
            let pixels = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * FRAME_HEIGHT as usize)
            }
            .to_vec();
            unsafe { self.context.Unmap(&self.staging, 0) };
            Frame { pixels, pitch }
        }
    }

    fn surface_size() -> Size<DevicePixels> {
        Size {
            width: DevicePixels(SURFACE_EXTENT),
            height: DevicePixels(SURFACE_EXTENT),
        }
    }

    /// The feature levels GPUI's own device creation asks for, in its order. 10.1 is the floor it
    /// accepts, and the level the adapter settles on is exactly what
    /// `ExternalSurfacePipeline::new` picks its shader model from.
    const GPUI_FEATURE_LEVELS: [D3D_FEATURE_LEVEL; 3] = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
    ];

    /// A hardware device if there is one, otherwise a WARP device, otherwise nothing.
    fn create_device() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        create_device_at(&GPUI_FEATURE_LEVELS)
    }

    fn create_device_at(
        feature_levels: &[D3D_FEATURE_LEVEL],
    ) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        for driver_type in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            if let Some(created) = create_device_of_type(driver_type, feature_levels) {
                return Some(created);
            }
        }
        None
    }

    /// A device of `driver_type` on the same terms `DirectXDevices::new` demands, or `None`.
    ///
    /// The structured-buffer requirement is copied from GPUI's own device creation deliberately: a
    /// device GPUI would refuse is not one this path has to work on, and admitting one here would
    /// turn an unsupported adapter into a test failure.
    fn create_device_of_type(
        driver_type: D3D_DRIVER_TYPE,
        feature_levels: &[D3D_FEATURE_LEVEL],
    ) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                driver_type,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .ok()?;
        let device = device?;

        let mut options = D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS::default();
        unsafe {
            device.CheckFeatureSupport(
                D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS,
                &mut options as *mut _ as _,
                std::mem::size_of::<D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS>() as u32,
            )
        }
        .ok()?;
        if !options
            .ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x
            .as_bool()
        {
            return None;
        }

        Some((device, context?))
    }

    fn texture(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        bind_flags: u32,
        usage: D3D11_USAGE,
        cpu_access: u32,
    ) -> windows::core::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            // The same byte order GPUI's own render target uses, which is what makes the read-back
            // comparable with the spike's.
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind_flags,
            CPUAccessFlags: cpu_access,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        Ok(texture.expect("CreateTexture2D returned no texture"))
    }

    fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<ID3DBlob> {
        let mut blob = None;
        let mut errors = None;
        let result = unsafe {
            D3DCompile(
                PRODUCER_SHADERS.as_ptr().cast(),
                PRODUCER_SHADERS.len(),
                None,
                None,
                None,
                entry,
                target,
                0,
                0,
                &mut blob,
                Some(&mut errors),
            )
        };
        if let Err(error) = result {
            if let Some(errors) = errors {
                let message = unsafe {
                    std::slice::from_raw_parts(
                        errors.GetBufferPointer().cast::<u8>(),
                        errors.GetBufferSize(),
                    )
                };
                eprintln!(
                    "producer shader compile: {}",
                    String::from_utf8_lossy(message)
                );
            }
            return Err(error);
        }
        Ok(blob.expect("D3DCompile returned no blob"))
    }

    fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
        }
    }

    /// The S1 producer pass, run against the texture the registry handed back.
    ///
    /// This is the producer flow of the contract exactly: the producer receives the texture from
    /// `register`, makes its **own** render target view over it, and submits on GPUI's immediate
    /// context ahead of GPUI's frame — which is what `SameQueueOrdered` means. It clears to the
    /// generation colour and draws the marker triangle at NDC (-1,1), (-0.5,1), (-1,0.5), which on
    /// a 512x512 surface is the right triangle with corners at texels (0,0), (128,0) and (0,128).
    fn producer_pass(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        surface: &ID3D11Texture2D,
    ) -> windows::core::Result<()> {
        let mut view = None;
        unsafe { device.CreateRenderTargetView(surface, None, Some(&mut view))? };
        let view = view.expect("CreateRenderTargetView returned no view");

        let vertex_blob = compile(s!("vs_solid"), s!("vs_4_0"))?;
        let fragment_blob = compile(s!("ps_solid"), s!("ps_4_0"))?;
        let mut vertex_shader = None;
        let mut fragment_shader = None;
        let mut layout = None;
        let elements = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("COLOR"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];
        unsafe {
            device.CreateVertexShader(blob_bytes(&vertex_blob), None, Some(&mut vertex_shader))?;
            device.CreatePixelShader(
                blob_bytes(&fragment_blob),
                None,
                Some(&mut fragment_shader),
            )?;
            device.CreateInputLayout(&elements, blob_bytes(&vertex_blob), Some(&mut layout))?;
        }

        let rasterizer_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let mut rasterizer = None;
        unsafe { device.CreateRasterizerState(&rasterizer_desc, Some(&mut rasterizer))? };

        // The producer writes its content opaquely; group opacity is GPUI's to apply, not the
        // producer's, so nothing here blends. Every factor is still spelled out, because
        // `CreateBlendState` validates them whether or not blending is enabled and the zero of
        // `D3D11_BLEND` is not a legal value.
        let mut blend_desc = D3D11_BLEND_DESC::default();
        blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: false.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_ZERO,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_ZERO,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend = None;
        unsafe { device.CreateBlendState(&blend_desc, Some(&mut blend))? };

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
        let vertex_bytes = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&vertices),
            )
        };
        let buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: vertex_bytes.len() as u32,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: vertex_bytes.as_ptr().cast(),
            ..Default::default()
        };
        let mut buffer = None;
        unsafe { device.CreateBuffer(&buffer_desc, Some(&init), Some(&mut buffer))? };

        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: SURFACE_EXTENT as f32,
            Height: SURFACE_EXTENT as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let stride = std::mem::size_of::<SolidVertex>() as u32;
        unsafe {
            context.OMSetRenderTargets(Some(&[Some(view.clone())]), None);
            context.ClearRenderTargetView(&view, &color_f(GENERATION_COLORS[0]));
            context.RSSetViewports(Some(&[viewport]));
            context.RSSetState(rasterizer.as_ref());
            context.OMSetBlendState(blend.as_ref(), None, u32::MAX);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.IASetInputLayout(layout.as_ref());
            context.IASetVertexBuffers(0, 1, Some(&buffer), Some(&stride), Some(&0));
            context.VSSetShader(vertex_shader.as_ref(), None);
            context.PSSetShader(fragment_shader.as_ref(), None);
            context.Draw(vertices.len() as u32, 0);

            // GPUI's pipelines take no vertex input at all — they build their quad from
            // `SV_VertexID` — so the producer's layout is unbound rather than left installed.
            context.IASetInputLayout(None);
            context.OMSetRenderTargets(None, None);
        }
        Ok(())
    }

    // --- The corpus ---------------------------------------------------------------------------

    /// The external-surface pipeline builds at feature level 10.1, the floor GPUI's own device
    /// creation accepts.
    ///
    /// The sibling tests build the pipeline on whatever level the adapter reports, which on any
    /// modern machine — and on WARP — is 11.x, so they only ever exercise the shader model 5.0
    /// build. This one asks for a 10.1 device explicitly, which every D3D11 driver can create
    /// down-level, and so covers the branch where 5.0 bytecode is rejected with `E_INVALIDARG` and
    /// the 4.0 build has to be the one handed to `CreateVertexShader`. It is the only place that
    /// branch is reachable without owning feature-level-10.1 hardware.
    #[test]
    fn the_pipeline_builds_at_the_feature_level_floor() {
        let Some((device, _context)) = create_device_at(&[D3D_FEATURE_LEVEL_10_1]) else {
            return;
        };
        assert_eq!(
            unsafe { device.GetFeatureLevel() },
            D3D_FEATURE_LEVEL_10_1,
            "asking for 10.1 alone must produce a 10.1 device"
        );
        assert_eq!(
            ShaderModel::for_feature_level(unsafe { device.GetFeatureLevel() }),
            ShaderModel::Sm4_0,
            "10.1 selects the 4.0 build"
        );
        ExternalSurfacePipeline::new(&device)
            .expect("the external-surface pipeline must build at feature level 10.1");
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
        // mean the sampled `v` axis runs bottom-up, which is the classic D3D/GL orientation bug.
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
    /// and the blend state composites the result with `ONE`/`INV_SRC_ALPHA`.
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

    /// The content mask becomes a scissor rectangle, and the scissor-enabled rasterizer state is
    /// installed for the draw and taken back off afterwards.
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
    /// * were the mirrored winding culled — GPUI's rasterizer state is `CULL_NONE`, which is what
    ///   makes a negative determinant legal at all — the frame would likewise be uniformly
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
}
