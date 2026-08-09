use anyhow::anyhow;
use cocoa::appkit::CGFloat;
use collections::{HashMap, HashSet};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    attributed_string::CFMutableAttributedString,
    base::{CFIndex, CFRange, CFType, TCFType},
    number::CFNumber,
    string::CFString,
};
use core_graphics::{
    base::{CGGlyph, kCGImageAlphaPremultipliedLast},
    color_space::CGColorSpace,
    context::{CGContext, CGTextDrawingMode},
    display::CGPoint,
    geometry::CGSize,
};
use core_text::{
    font::CTFont,
    font_collection::CTFontCollectionRef,
    font_descriptor::{
        CTFontDescriptor, kCTFontSlantTrait, kCTFontSymbolicTrait, kCTFontWeightTrait,
        kCTFontWidthTrait,
    },
    line::{CTLine, CTLineRef},
    run::CTRunRef,
    string_attributes::kCTFontAttributeName,
};
use font_kit::{
    font::Font as FontKitFont,
    handle::Handle,
    hinting::HintingOptions,
    metrics::Metrics,
    properties::{Style as FontkitStyle, Weight as FontkitWeight},
    source::SystemSource,
    sources::mem::MemSource,
};
use gpui::{
    Bounds, CaretAffinity, CaretStop, DevicePixels, Font, FontFallbacks, FontFeatures, FontId,
    FontMetrics, FontRun, FontSourceFingerprint, FontStyle, FontWeight, GlyphId, Hsla, LineLayout,
    Pixels, PlatformFontFace, PlatformTextSystem, RenderGlyphParams, Result, Rgba, RichFontRun,
    SUBPIXEL_VARIANTS_X, ShapedGlyph, ShapedRun, SharedString, Size, TextDirection,
    TextRenderingMode, point, px, size, swap_rgba_pa_to_bgra,
};
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use pathfinder_geometry::{
    rect::{RectF, RectI},
    transform2d::Transform2F,
    vector::Vector2F,
};
use smallvec::SmallVec;
use std::{
    borrow::Cow,
    cell::RefCell,
    char,
    convert::TryFrom,
    ffi::c_void,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, OnceLock},
};

use crate::open_type::apply_features_and_fallbacks;

#[allow(non_upper_case_globals)]
const kCGImageAlphaOnly: u32 = 7;

/// macOS text system using CoreText for font shaping.
pub struct MacTextSystem(
    RwLock<MacTextSystemState>,
    RwLock<HashMap<String, Arc<OnceLock<PlatformFontFace>>>>,
);

#[derive(Clone, PartialEq, Eq, Hash)]
struct FontKey {
    font_family: SharedString,
    font_features: FontFeatures,
    font_fallbacks: Option<FontFallbacks>,
}

struct MacTextSystemState {
    memory_source: MemSource,
    system_source: SystemSource,
    fonts: Vec<FontKitFont>,
    font_selections: HashMap<Font, FontId>,
    font_ids_by_postscript_name: HashMap<String, FontId>,
    font_ids_by_font_key: HashMap<FontKey, SmallVec<[FontId; 4]>>,
    postscript_names_by_font_id: HashMap<FontId, String>,
}

struct MacFontFaceCandidate {
    family: SharedString,
    postscript_name: Option<SharedString>,
    source: MacFontFaceSource,
}

enum MacFontFaceSource {
    Path {
        path: PathBuf,
        face_index: u32,
    },
    Memory {
        bytes: Arc<Vec<u8>>,
        face_index: u32,
    },
    NativeKey(Vec<u8>),
}

impl MacFontFaceCandidate {
    fn resolve(self) -> PlatformFontFace {
        let (source, face_index) = match self.source {
            MacFontFaceSource::Path { path, face_index } => {
                let source = std::fs::read(&path)
                    .map(|bytes| FontSourceFingerprint::from_bytes(&bytes))
                    .unwrap_or_else(|_| {
                        FontSourceFingerprint::from_native_key(path.to_string_lossy().as_bytes())
                    });
                (source, face_index)
            }
            MacFontFaceSource::Memory { bytes, face_index } => {
                (FontSourceFingerprint::from_bytes(&bytes), face_index)
            }
            MacFontFaceSource::NativeKey(key) => (FontSourceFingerprint::from_native_key(&key), 0),
        };
        let source = {
            let discriminator = self
                .postscript_name
                .as_deref()
                .unwrap_or(self.family.as_ref());
            source.with_discriminator(discriminator.as_bytes())
        };
        PlatformFontFace::new(self.family, self.postscript_name, source, face_index)
    }
}

impl MacTextSystem {
    /// Create a new MacTextSystem.
    pub fn new() -> Self {
        Self(
            RwLock::new(MacTextSystemState {
                memory_source: MemSource::empty(),
                system_source: SystemSource::new(),
                fonts: Vec::new(),
                font_selections: HashMap::default(),
                font_ids_by_postscript_name: HashMap::default(),
                font_ids_by_font_key: HashMap::default(),
                postscript_names_by_font_id: HashMap::default(),
            }),
            RwLock::default(),
        )
    }
}

impl Default for MacTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformTextSystem for MacTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.write().add_fonts(fonts)?;
        self.1.write().clear();
        Ok(())
    }

    fn all_font_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let collection = core_text::font_collection::create_for_all_families();
        // NOTE: We intentionally avoid using `collection.get_descriptors()` here because
        // it has a memory leak bug in core-text v21.0.0. The upstream code uses
        // `wrap_under_get_rule` but `CTFontCollectionCreateMatchingFontDescriptors`
        // follows the Create Rule (caller owns the result), so it should use
        // `wrap_under_create_rule`. We call the function directly with correct memory management.
        unsafe extern "C" {
            fn CTFontCollectionCreateMatchingFontDescriptors(
                collection: CTFontCollectionRef,
            ) -> CFArrayRef;
        }
        let descriptors: Option<CFArray<CTFontDescriptor>> = unsafe {
            let array_ref =
                CTFontCollectionCreateMatchingFontDescriptors(collection.as_concrete_TypeRef());
            if array_ref.is_null() {
                None
            } else {
                Some(CFArray::wrap_under_create_rule(array_ref))
            }
        };
        let Some(descriptors) = descriptors else {
            return names;
        };
        for descriptor in descriptors.into_iter() {
            names.extend(lenient_font_attributes::family_name(&descriptor));
        }
        if let Ok(fonts_in_memory) = self.0.read().memory_source.all_families() {
            names.extend(fonts_in_memory);
        }
        names
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        let lock = self.0.upgradable_read();
        if let Some(font_id) = lock.font_selections.get(font) {
            Ok(*font_id)
        } else {
            let mut lock = RwLockUpgradableReadGuard::upgrade(lock);
            let font_key = FontKey {
                font_family: font.family.clone(),
                font_features: font.features.clone(),
                font_fallbacks: font.fallbacks.clone(),
            };
            let candidates = if let Some(font_ids) = lock.font_ids_by_font_key.get(&font_key) {
                font_ids.as_slice()
            } else {
                let font_ids =
                    lock.load_family(&font.family, &font.features, font.fallbacks.as_ref())?;
                lock.font_ids_by_font_key.insert(font_key.clone(), font_ids);
                lock.font_ids_by_font_key[&font_key].as_ref()
            };

            let candidate_properties = candidates
                .iter()
                .map(|font_id| lock.fonts[font_id.0].properties())
                .collect::<SmallVec<[_; 4]>>();

            let ix = font_kit::matching::find_best_match(
                &candidate_properties,
                &font_kit::properties::Properties {
                    style: fontkit_style(font.style),
                    weight: fontkit_weight(font.weight),
                    stretch: Default::default(),
                },
            )?;

            let font_id = candidates[ix];
            lock.font_selections.insert(font.clone(), font_id);
            Ok(font_id)
        }
    }

    fn font_face(&self, font_id: FontId) -> Option<PlatformFontFace> {
        let candidate = { self.0.read().font_face_candidate(font_id)? };
        let Some(cache_key) = candidate.postscript_name.as_deref().map(str::to_owned) else {
            return Some(candidate.resolve());
        };
        let cache_entry = if let Some(entry) = self.1.read().get(&cache_key).cloned() {
            entry
        } else {
            self.1
                .write()
                .entry(cache_key)
                .or_insert_with(|| Arc::new(OnceLock::new()))
                .clone()
        };

        // Keep the potentially blocking file read and content hash outside both maps. Cache hits
        // use ordinary parallel reads, while the per-face cell keeps first resolution single-shot.
        Some(cache_entry.get_or_init(|| candidate.resolve()).clone())
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        font_kit_metrics_to_metrics(self.0.read().fonts[font_id.0].metrics())
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        Ok(bounds_from_rect(
            self.0.read().fonts[font_id.0].typographic_bounds(glyph_id.0)?,
        ))
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.read().advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.read().glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.read().raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        glyph_id: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.read().rasterize_glyph(glyph_id, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, font_runs: &[FontRun]) -> LineLayout {
        self.0.write().layout_line(text, font_size, font_runs)
    }

    fn layout_rich_line(&self, text: &str, font_runs: &[RichFontRun]) -> Result<LineLayout> {
        Ok(self.0.write().layout_rich_line(text, font_runs))
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }

    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        // When font smoothing is enabled, CoreGraphics thickens glyph strokes by an amount that
        // depends on the foreground color's luminance. We replicate the logic used by CoreGraphics
        // to select between the different levels of dilation.
        if !font_smoothing_allowed_by_user() {
            return 0;
        }
        let rgba: Rgba = color.into();
        let luminance = 0.2126 * rgba.r + 0.7152 * rgba.g + 0.0722 * rgba.b;
        let level = ((4.0 * luminance) + 0.5).floor() as i32;
        level.clamp(0, 4) as u8
    }
}

fn font_smoothing_allowed_by_user() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| {
        use core_foundation_sys::preferences::{
            CFPreferencesCopyAppValue, kCFPreferencesCurrentApplication,
        };

        let key = CFString::new("AppleFontSmoothing");
        let value_ref = unsafe {
            CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), kCFPreferencesCurrentApplication)
        };
        if value_ref.is_null() {
            return true;
        }
        let value = unsafe { CFType::wrap_under_create_rule(value_ref) };
        let Some(number) = value.downcast_into::<CFNumber>() else {
            return true;
        };
        // Only an explicit value of `0` means that font smoothing is disabled.
        number.to_i64() != Some(0)
    })
}

impl MacTextSystemState {
    fn add_fonts(&mut self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let fonts = fonts
            .into_iter()
            .map(|bytes| match bytes {
                Cow::Borrowed(embedded_font) => {
                    let data_provider = unsafe {
                        core_graphics::data_provider::CGDataProvider::from_slice(embedded_font)
                    };
                    let font = core_graphics::font::CGFont::from_data_provider(data_provider)
                        .map_err(|()| anyhow!("Could not load an embedded font."))?;
                    let font = font_kit::loaders::core_text::Font::from_core_graphics_font(font);
                    Ok(Handle::from_native(&font))
                }
                Cow::Owned(bytes) => Ok(Handle::from_memory(Arc::new(bytes), 0)),
            })
            .collect::<Result<Vec<_>>>()?;
        self.memory_source.add_fonts(fonts.into_iter())?;
        Ok(())
    }

    fn load_family(
        &mut self,
        name: &str,
        features: &FontFeatures,
        fallbacks: Option<&FontFallbacks>,
    ) -> Result<SmallVec<[FontId; 4]>> {
        let name = gpui::font_name_with_fallbacks(name, ".AppleSystemUIFont");

        let mut font_ids = SmallVec::new();
        let mut postscript_names_seen = HashSet::default();
        let family = self
            .memory_source
            .select_family_by_name(name)
            .or_else(|_| self.system_source.select_family_by_name(name))?;
        for font in family.fonts() {
            let mut font = font.load()?;

            apply_features_and_fallbacks(&mut font, features, fallbacks)?;
            // This block contains a precautionary fix to guard against loading fonts
            // that might cause panics due to `.unwrap()`s up the chain.
            {
                // We use the 'm' character for text measurements in various spots
                // (e.g., the editor). However, at time of writing some of those usages
                // will panic if the font has no 'm' glyph.
                //
                // Therefore, we check up front that the font has the necessary glyph.
                let has_m_glyph = font.glyph_for_char('m').is_some();

                // HACK: The 'Segoe Fluent Icons' font does not have an 'm' glyph,
                // but we need to be able to load it for rendering Windows icons in
                // the Storybook (on macOS).
                let is_segoe_fluent_icons = font.full_name() == "Segoe Fluent Icons";

                if !has_m_glyph && !is_segoe_fluent_icons {
                    // I spent far too long trying to track down why a font missing the 'm'
                    // character wasn't loading. This log statement will hopefully save
                    // someone else from suffering the same fate.
                    log::warn!(
                        "font '{}' has no 'm' character and was not loaded",
                        font.full_name()
                    );
                    continue;
                }
            }

            // We've seen a number of panics in production caused by calling font.properties()
            // which unwraps a downcast to CFNumber. This is an attempt to avoid the panic,
            // and to try and identify the incalcitrant font.
            let traits = font.native_font().all_traits();
            if unsafe {
                !(traits
                    .get(kCTFontSymbolicTrait)
                    .downcast::<CFNumber>()
                    .is_some()
                    && traits
                        .get(kCTFontWidthTrait)
                        .downcast::<CFNumber>()
                        .is_some()
                    && traits
                        .get(kCTFontWeightTrait)
                        .downcast::<CFNumber>()
                        .is_some()
                    && traits
                        .get(kCTFontSlantTrait)
                        .downcast::<CFNumber>()
                        .is_some())
            } {
                log::error!(
                    "Failed to read traits for font {:?} (PostScript name {:?})",
                    font.full_name(),
                    font.postscript_name(),
                );
                continue;
            }

            let Some(postscript_name) = font.postscript_name() else {
                log::warn!(
                    "font {:?} in family {:?} has no PostScript name; skipping",
                    font.full_name(),
                    name,
                );
                continue;
            };
            // Dedup is scoped to this single `load_family` call (issue #55472).
            // The same family can be reloaded later under a different `FontKey`
            // (different features/fallbacks); a global check against
            // `font_ids_by_postscript_name` would skip every already-registered
            // font and leave the second call's `font_ids` empty.
            if !postscript_names_seen.insert(postscript_name.clone()) {
                log::warn!(
                    "skipping duplicate font {:?} with PostScript name {:?} \
                     in family {:?}",
                    font.full_name(),
                    postscript_name,
                    name,
                );
                continue;
            }
            let font_id = FontId(self.fonts.len());
            font_ids.push(font_id);
            self.font_ids_by_postscript_name
                .insert(postscript_name.clone(), font_id);
            self.postscript_names_by_font_id
                .insert(font_id, postscript_name);
            self.fonts.push(font);
        }
        Ok(font_ids)
    }

    fn font_face_candidate(&self, font_id: FontId) -> Option<MacFontFaceCandidate> {
        let font = self.fonts.get(font_id.0)?;
        let family = SharedString::from(font.family_name());
        let postscript_name = font.postscript_name().map(SharedString::from);
        let handle = font.handle().and_then(|handle| match handle {
            Handle::Path { .. } | Handle::Memory { .. } => Some(handle),
            Handle::Native { .. } => None,
        });
        let handle = handle.or_else(|| {
            let postscript_name = postscript_name.as_deref()?;
            self.memory_source
                .select_by_postscript_name(postscript_name)
                .ok()
                .or_else(|| {
                    self.system_source
                        .select_by_postscript_name(postscript_name)
                        .ok()
                })
        });
        let source = match handle {
            Some(Handle::Path { path, font_index }) => MacFontFaceSource::Path {
                path,
                face_index: font_index,
            },
            Some(Handle::Memory { bytes, font_index }) => MacFontFaceSource::Memory {
                bytes,
                face_index: font_index,
            },
            Some(Handle::Native { .. }) | None => {
                if let Some(bytes) = font.copy_font_data() {
                    MacFontFaceSource::Memory {
                        bytes,
                        face_index: 0,
                    }
                } else if let Some(path) = font.native_font().url().and_then(|url| url.to_path()) {
                    MacFontFaceSource::Path {
                        path,
                        face_index: 0,
                    }
                } else {
                    let key = format!(
                        "core-text:{}:{}",
                        family,
                        postscript_name.as_deref().unwrap_or("unknown")
                    );
                    MacFontFaceSource::NativeKey(key.into_bytes())
                }
            }
        };
        Some(MacFontFaceCandidate {
            family,
            postscript_name,
            source,
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        Ok(size_from_vector2f(
            self.fonts[font_id.0].advance(glyph_id.0)?,
        ))
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.fonts[font_id.0].glyph_for_char(ch).map(GlyphId)
    }

    fn id_for_native_font(&mut self, requested_font: CTFont) -> FontId {
        let postscript_name = requested_font.postscript_name();
        if let Some(font_id) = self.font_ids_by_postscript_name.get(&postscript_name) {
            *font_id
        } else {
            let font_id = FontId(self.fonts.len());
            self.font_ids_by_postscript_name
                .insert(postscript_name.clone(), font_id);
            self.postscript_names_by_font_id
                .insert(font_id, postscript_name);
            self.fonts
                .push(font_kit::font::Font::from_core_graphics_font(
                    requested_font.copy_to_CGFont(),
                ));
            font_id
        }
    }

    fn is_emoji(&self, font_id: FontId) -> bool {
        self.postscript_names_by_font_id
            .get(&font_id)
            .is_some_and(|postscript_name| {
                postscript_name == "AppleColorEmoji" || postscript_name == ".AppleColorEmojiUI"
            })
    }

    fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let font = &self.fonts[params.font_id.0];
        let scale = Transform2F::from_scale(params.scale_factor);
        let bounds: Bounds<DevicePixels> = bounds_from_rect_i(font.raster_bounds(
            params.glyph_id.0,
            params.font_size.into(),
            scale,
            HintingOptions::None,
            font_kit::canvas::RasterizationOptions::GrayscaleAa,
        )?);

        // Expand the bounds by 1 pixel on each side to give CG room for anti-aliasing.
        Ok(bounds.dilate(DevicePixels(1)))
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        glyph_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        if glyph_bounds.size.width.0 == 0 || glyph_bounds.size.height.0 == 0 {
            anyhow::bail!("glyph bounds are empty");
        } else {
            // Add an extra pixel when the subpixel variant isn't zero to make room for anti-aliasing.
            let mut bitmap_size = glyph_bounds.size;
            if params.subpixel_variant.x > 0 {
                bitmap_size.width += DevicePixels(1);
            }
            if params.subpixel_variant.y > 0 {
                bitmap_size.height += DevicePixels(1);
            }
            let bitmap_size = bitmap_size;

            let mut bytes;
            let cx;
            if params.is_emoji {
                bytes = vec![0; bitmap_size.width.0 as usize * 4 * bitmap_size.height.0 as usize];
                cx = CGContext::create_bitmap_context(
                    Some(bytes.as_mut_ptr() as *mut _),
                    bitmap_size.width.0 as usize,
                    bitmap_size.height.0 as usize,
                    8,
                    bitmap_size.width.0 as usize * 4,
                    &CGColorSpace::create_device_rgb(),
                    kCGImageAlphaPremultipliedLast,
                );
            } else {
                bytes = vec![0; bitmap_size.width.0 as usize * bitmap_size.height.0 as usize];
                cx = CGContext::create_bitmap_context(
                    Some(bytes.as_mut_ptr() as *mut _),
                    bitmap_size.width.0 as usize,
                    bitmap_size.height.0 as usize,
                    8,
                    bitmap_size.width.0 as usize,
                    &CGColorSpace::create_device_gray(),
                    kCGImageAlphaOnly,
                );
            }

            // Move the origin to bottom left and account for scaling, this
            // makes drawing text consistent with the font-kit's raster_bounds.
            cx.translate(
                -glyph_bounds.origin.x.0 as CGFloat,
                (glyph_bounds.origin.y.0 + glyph_bounds.size.height.0) as CGFloat,
            );
            cx.scale(
                params.scale_factor as CGFloat,
                params.scale_factor as CGFloat,
            );

            let subpixel_shift = params
                .subpixel_variant
                .map(|v| v as f32 / SUBPIXEL_VARIANTS_X as f32);
            cx.set_text_drawing_mode(CGTextDrawingMode::CGTextFill);
            cx.set_allows_antialiasing(true);
            cx.set_should_antialias(true);
            cx.set_allows_font_subpixel_positioning(true);
            cx.set_should_subpixel_position_fonts(true);
            cx.set_allows_font_subpixel_quantization(false);
            cx.set_should_subpixel_quantize_fonts(false);

            if params.dilation > 0 {
                let luminance = params.dilation as f64 * 0.25;
                cx.set_should_smooth_fonts(true);
                cx.set_gray_fill_color(luminance, 1.0);
            } else {
                cx.set_gray_fill_color(0.0, 1.0);
            }
            self.fonts[params.font_id.0]
                .native_font()
                .clone_with_font_size(f32::from(params.font_size) as CGFloat)
                .draw_glyphs(
                    &[params.glyph_id.0 as CGGlyph],
                    &[CGPoint::new(
                        (subpixel_shift.x / params.scale_factor) as CGFloat,
                        (subpixel_shift.y / params.scale_factor) as CGFloat,
                    )],
                    cx,
                );

            if params.is_emoji {
                // Convert from RGBA with premultiplied alpha to BGRA with straight alpha.
                for pixel in bytes.chunks_exact_mut(4) {
                    swap_rgba_pa_to_bgra(pixel);
                }
            }

            Ok((bitmap_size, bytes))
        }
    }

    fn layout_line(&mut self, text: &str, font_size: Pixels, font_runs: &[FontRun]) -> LineLayout {
        let rich_runs = font_runs
            .iter()
            .map(|run| RichFontRun {
                len: run.len,
                font_id: run.font_id,
                font_size,
                minimum_line_height: Pixels::ZERO,
                baseline_shift: Pixels::ZERO,
            })
            .collect::<SmallVec<[_; 4]>>();
        self.layout_rich_line_impl(text, &rich_runs, false, font_size)
    }

    fn layout_rich_line(&mut self, text: &str, font_runs: &[RichFontRun]) -> LineLayout {
        let fallback_font_size = font_runs
            .first()
            .map(|run| run.font_size)
            .unwrap_or(Pixels::ZERO);
        self.layout_rich_line_impl(text, font_runs, true, fallback_font_size)
    }

    fn layout_rich_line_impl(
        &mut self,
        text: &str,
        font_runs: &[RichFontRun],
        heterogeneous_metrics: bool,
        fallback_font_size: Pixels,
    ) -> LineLayout {
        let font_size = font_runs
            .first()
            .map(|run| run.font_size)
            .unwrap_or(fallback_font_size);
        let font_run_ends = if heterogeneous_metrics {
            cumulative_rich_run_ends(font_runs)
        } else {
            SmallVec::new()
        };
        // Construct the attributed string, converting UTF8 ranges to UTF16 ranges.
        let mut string = CFMutableAttributedString::new();
        let mut max_ascent = 0.0f32;
        let mut max_descent = 0.0f32;

        {
            let mut text = text;
            let mut break_ligature = true;
            for run in font_runs {
                let text_run;
                (text_run, text) = text.split_at(run.len);

                let utf16_start = string.char_len(); // insert at end of string
                // note: replace_str may silently ignore codepoints it dislikes (e.g., BOM at start of string)
                string.replace_str(&CFString::new(text_run), CFRange::init(utf16_start, 0));
                let utf16_end = string.char_len();

                let length = utf16_end - utf16_start;
                let cf_range = CFRange::init(utf16_start, length);
                let font = &self.fonts[run.font_id.0];

                let font_metrics = font.metrics();
                let font_scale = run.font_size.as_f32() / font_metrics.units_per_em as f32;
                max_ascent =
                    max_ascent.max(font_metrics.ascent * font_scale + run.baseline_shift.as_f32());
                max_descent = max_descent
                    .max(-font_metrics.descent * font_scale - run.baseline_shift.as_f32());

                let run_font_size = if !heterogeneous_metrics && break_ligature {
                    px(run.font_size.as_f32().next_up())
                } else {
                    run.font_size
                };
                unsafe {
                    string.set_attribute(
                        cf_range,
                        kCTFontAttributeName,
                        &font
                            .native_font()
                            .clone_with_font_size(run_font_size.into()),
                    );
                }
                break_ligature = !break_ligature;
            }
        }
        // Retrieve the glyphs from the shaped line, converting UTF16 offsets to UTF8 offsets.
        let line = CTLine::new_with_attributed_string(string.as_concrete_TypeRef());
        let glyph_runs = line.glyph_runs();
        let mut runs = <Vec<ShapedRun>>::with_capacity(glyph_runs.len() as usize);
        let mut ix_converter = StringIndexConverter::new(text);
        for run in glyph_runs.into_iter() {
            let glyph_capacity = run.glyphs().len();
            let attributes = run.attributes().unwrap();
            let font = unsafe {
                attributes
                    .get(kCTFontAttributeName)
                    .downcast::<CTFont>()
                    .unwrap()
            };
            let font_id = self.id_for_native_font(font);

            for ((&glyph_id, position), &glyph_utf16_ix) in run
                .glyphs()
                .iter()
                .zip(run.positions().iter())
                .zip(run.string_indices().iter())
            {
                let glyph_utf16_ix = usize::try_from(glyph_utf16_ix).unwrap();
                if ix_converter.utf16_ix > glyph_utf16_ix {
                    // We cannot reuse current index converter, as it can only seek forward. Restart the search.
                    ix_converter = StringIndexConverter::new(text);
                }
                ix_converter.advance_to_utf16_ix(glyph_utf16_ix);
                let (run_font_size, baseline_shift) = if heterogeneous_metrics {
                    let metrics_run =
                        rich_run_for_index(font_runs, &font_run_ends, ix_converter.utf8_ix);
                    (
                        metrics_run.map(|run| run.font_size).unwrap_or(font_size),
                        metrics_run
                            .map(|run| run.baseline_shift)
                            .unwrap_or(Pixels::ZERO),
                    )
                } else {
                    (font_size, Pixels::ZERO)
                };
                let shaped_glyph = ShapedGlyph {
                    id: GlyphId(glyph_id as u32),
                    position: point(position.x as f32, position.y as f32).map(px),
                    index: ix_converter.utf8_ix,
                    is_emoji: self.is_emoji(font_id),
                };
                if let Some(last_run) = runs.last_mut().filter(|last_run| {
                    last_run.font_id == font_id
                        && last_run.font_size == run_font_size
                        && last_run.baseline_shift == baseline_shift
                }) {
                    last_run.glyphs.push(shaped_glyph);
                } else {
                    // A single CoreText run can be split into many GPUI runs by metrics that
                    // CoreText does not shape with (for example, baseline shift). Reserving the
                    // full CoreText glyph count for every such run makes alternating metrics use
                    // quadratic capacity. The homogeneous path still gets the exact fast-path
                    // reservation; rich runs grow only when they actually receive more glyphs.
                    let mut glyphs = Vec::with_capacity(if heterogeneous_metrics {
                        1
                    } else {
                        glyph_capacity
                    });
                    glyphs.push(shaped_glyph);
                    runs.push(ShapedRun {
                        font_id,
                        font_size: run_font_size,
                        baseline_shift,
                        resolved_face: None,
                        glyphs,
                    });
                }
            }
        }
        let typographic_bounds = line.get_typographic_bounds();
        LineLayout {
            runs,
            font_size,
            width: typographic_bounds.width.into(),
            ascent: max_ascent.max(0.0).into(),
            descent: max_descent.max(0.0).into(),
            minimum_line_height: if heterogeneous_metrics {
                font_runs
                    .iter()
                    .map(|run| run.minimum_line_height)
                    .max()
                    .unwrap_or(Pixels::ZERO)
            } else {
                Pixels::ZERO
            },
            caret_stops: if heterogeneous_metrics {
                core_text_caret_stops(&line, text)
            } else {
                Vec::new()
            },
            generated_caret_stops: Default::default(),
            len: text.len(),
        }
    }
}

fn cumulative_rich_run_ends(font_runs: &[RichFontRun]) -> SmallVec<[usize; 4]> {
    let mut end = 0usize;
    font_runs
        .iter()
        .map(|run| {
            end = end.saturating_add(run.len);
            end
        })
        .collect()
}

fn rich_run_for_index<'a>(
    font_runs: &'a [RichFontRun],
    run_ends: &[usize],
    index: usize,
) -> Option<&'a RichFontRun> {
    font_runs.get(run_ends.partition_point(|end| *end <= index))
}

fn core_text_enumerated_caret_offsets(line: &CTLine) -> Vec<(f32, usize, bool)> {
    let offsets = Rc::new(RefCell::new(Vec::new()));
    let block_offsets = Rc::clone(&offsets);
    let block = block::ConcreteBlock::new(
        move |offset: f64, char_index: CFIndex, leading_edge: bool, _stop: *mut bool| {
            if let Ok(char_index) = usize::try_from(char_index) {
                block_offsets
                    .borrow_mut()
                    .push((offset as f32, char_index, leading_edge));
            }
        },
    )
    .copy();
    unsafe {
        CTLineEnumerateCaretOffsets(
            line.as_concrete_TypeRef(),
            (&*block as *const block::Block<(f64, CFIndex, bool, *mut bool), ()>).cast(),
        );
    }
    drop(block);
    Rc::try_unwrap(offsets)
        .expect("CoreText caret enumeration block must not escape the synchronous call")
        .into_inner()
}

#[cold]
#[inline(never)]
fn core_text_caret_stops(line: &CTLine, text: &str) -> Vec<CaretStop> {
    let stops = core_text_caret_stops_enumerated(line, text);
    #[cfg(debug_assertions)]
    debug_assert_eq!(stops, core_text_caret_stops_general(line, text));
    stops
}

fn core_text_caret_stops_enumerated(line: &CTLine, text: &str) -> Vec<CaretStop> {
    #[derive(Clone, Copy)]
    struct Cluster {
        start: usize,
        end: usize,
        left: f32,
        right: f32,
        direction: TextDirection,
    }

    let utf16_len = text.encode_utf16().count();
    let mut cluster_starts = vec![0usize, utf16_len];
    for run in line.glyph_runs().into_iter() {
        cluster_starts.extend(
            run.string_indices()
                .iter()
                .filter_map(|index| usize::try_from(*index).ok()),
        );
    }
    cluster_starts.sort_unstable();
    cluster_starts.dedup();

    let mut utf_boundaries = Vec::with_capacity(text.chars().count() + 1);
    utf_boundaries.push((0usize, 0usize));
    let mut utf16_offset = 0usize;
    for (utf8_offset, character) in text.char_indices() {
        utf16_offset += character.len_utf16();
        utf_boundaries.push((utf16_offset, utf8_offset + character.len_utf8()));
    }
    let utf8_for_utf16 = |index: usize| {
        let boundary = utf_boundaries.partition_point(|(utf16, _)| *utf16 <= index);
        utf_boundaries[boundary.saturating_sub(1)].1
    };

    let mut clusters = Vec::<Cluster>::new();
    let mut cluster_indices = HashMap::<(usize, usize, bool), usize>::default();
    for run in line.glyph_runs().into_iter() {
        let glyph_count = usize::try_from(run.glyph_count()).unwrap_or(0);
        let mut advances = vec![CGSize::new(0.0, 0.0); glyph_count];
        unsafe {
            CTRunGetAdvances(
                run.as_concrete_TypeRef(),
                CFRange::init(0, 0),
                advances.as_mut_ptr(),
            );
        }
        let rtl = unsafe { CTRunGetStatus(run.as_concrete_TypeRef()) & CTRUN_STATUS_RTL != 0 };
        for ((position, advance), string_index) in run
            .positions()
            .iter()
            .zip(advances.iter())
            .zip(run.string_indices().iter())
        {
            let Ok(start_utf16) = usize::try_from(*string_index) else {
                continue;
            };
            let next = cluster_starts.partition_point(|start| *start <= start_utf16);
            let end_utf16 = cluster_starts.get(next).copied().unwrap_or(utf16_len);
            let start = utf8_for_utf16(start_utf16);
            let end = utf8_for_utf16(end_utf16);
            let left = (position.x.min(position.x + advance.width)) as f32;
            let right = (position.x.max(position.x + advance.width)) as f32;
            let key = (start, end, rtl);
            if let Some(index) = cluster_indices.get(&key).copied() {
                let cluster = &mut clusters[index];
                cluster.left = cluster.left.min(left);
                cluster.right = cluster.right.max(right);
            } else {
                cluster_indices.insert(key, clusters.len());
                clusters.push(Cluster {
                    start,
                    end,
                    left,
                    right,
                    direction: if rtl {
                        TextDirection::RightToLeft
                    } else {
                        TextDirection::LeftToRight
                    },
                });
            }
        }
    }

    clusters.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.direction.cmp(&right.direction))
    });

    let mut stops = Vec::with_capacity(clusters.len() * 2 + 2);
    for cluster in &clusters {
        let (leading, trailing) = match cluster.direction {
            TextDirection::LeftToRight => (cluster.left, cluster.right),
            TextDirection::RightToLeft => (cluster.right, cluster.left),
        };
        stops.push(CaretStop {
            index: cluster.start,
            affinity: CaretAffinity::Downstream,
            direction: cluster.direction,
            x: px(leading),
        });
        stops.push(CaretStop {
            index: cluster.end,
            affinity: CaretAffinity::Upstream,
            direction: cluster.direction,
            x: px(trailing),
        });
    }

    let has_internal_character_boundaries = clusters.iter().any(|cluster| {
        text.get(cluster.start..cluster.end)
            .is_some_and(|cluster_text| cluster_text.chars().count() > 1)
    });
    if has_internal_character_boundaries {
        let mut stop_keys = stops
            .iter()
            .map(|stop| (stop.index, stop.affinity, stop.direction))
            .collect::<HashSet<_>>();
        for (offset, char_index, leading_edge) in core_text_enumerated_caret_offsets(line) {
            let boundary = utf_boundaries.partition_point(|(utf16, _)| *utf16 <= char_index);
            let utf8_index = if leading_edge {
                utf_boundaries[boundary.saturating_sub(1)].1
            } else {
                utf_boundaries
                    .get(boundary)
                    .or_else(|| utf_boundaries.last())
                    .map_or(0, |(_, utf8)| *utf8)
            };
            let (affinity, cluster) = if leading_edge {
                let cluster_ix = clusters.partition_point(|cluster| cluster.start <= utf8_index);
                (
                    CaretAffinity::Downstream,
                    cluster_ix.checked_sub(1).and_then(|index| {
                        clusters.get(index).filter(|cluster| {
                            cluster.start <= utf8_index && utf8_index < cluster.end
                        })
                    }),
                )
            } else {
                let cluster_ix = clusters.partition_point(|cluster| cluster.start < utf8_index);
                (
                    CaretAffinity::Upstream,
                    cluster_ix.checked_sub(1).and_then(|index| {
                        clusters.get(index).filter(|cluster| {
                            cluster.start < utf8_index && utf8_index <= cluster.end
                        })
                    }),
                )
            };
            if let Some(cluster) = cluster
                && stop_keys.insert((utf8_index, affinity, cluster.direction))
            {
                stops.push(CaretStop {
                    index: utf8_index,
                    affinity,
                    direction: cluster.direction,
                    x: px(offset),
                });
            }
        }
    }

    if stops.is_empty() {
        stops.push(CaretStop {
            index: 0,
            affinity: CaretAffinity::Downstream,
            direction: TextDirection::LeftToRight,
            x: Pixels::ZERO,
        });
    }
    stops.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.x.as_f32().total_cmp(&right.x.as_f32()))
            .then_with(|| left.affinity.cmp(&right.affinity))
            .then_with(|| left.direction.cmp(&right.direction))
    });
    stops.dedup_by(|left, right| {
        left.index == right.index
            && left.x.as_f32().to_bits() == right.x.as_f32().to_bits()
            && left.affinity == right.affinity
            && left.direction == right.direction
    });
    stops
}

#[cfg(debug_assertions)]
fn core_text_caret_stops_general(line: &CTLine, text: &str) -> Vec<CaretStop> {
    #[derive(Clone, Copy)]
    struct Cluster {
        start: usize,
        end: usize,
        left: f32,
        right: f32,
        direction: TextDirection,
    }

    let utf16_len = text.encode_utf16().count();
    let mut cluster_starts = vec![0usize, utf16_len];
    for run in line.glyph_runs().into_iter() {
        cluster_starts.extend(
            run.string_indices()
                .iter()
                .filter_map(|index| usize::try_from(*index).ok()),
        );
    }
    cluster_starts.sort_unstable();
    cluster_starts.dedup();

    let mut utf_boundaries = Vec::with_capacity(text.chars().count() + 1);
    utf_boundaries.push((0usize, 0usize));
    let mut utf16_offset = 0usize;
    for (utf8_offset, character) in text.char_indices() {
        utf16_offset += character.len_utf16();
        utf_boundaries.push((utf16_offset, utf8_offset + character.len_utf8()));
    }
    let utf8_for_utf16 = |index: usize| {
        let boundary = utf_boundaries.partition_point(|(utf16, _)| *utf16 <= index);
        utf_boundaries[boundary.saturating_sub(1)].1
    };

    let mut clusters = Vec::<Cluster>::new();
    let mut cluster_indices = HashMap::<(usize, usize, bool), usize>::default();
    for run in line.glyph_runs().into_iter() {
        let glyph_count = usize::try_from(run.glyph_count()).unwrap_or(0);
        let mut advances = vec![CGSize::new(0.0, 0.0); glyph_count];
        unsafe {
            CTRunGetAdvances(
                run.as_concrete_TypeRef(),
                CFRange::init(0, 0),
                advances.as_mut_ptr(),
            );
        }
        let rtl = unsafe { CTRunGetStatus(run.as_concrete_TypeRef()) & CTRUN_STATUS_RTL != 0 };
        for ((position, advance), string_index) in run
            .positions()
            .iter()
            .zip(advances.iter())
            .zip(run.string_indices().iter())
        {
            let Ok(start_utf16) = usize::try_from(*string_index) else {
                continue;
            };
            let next = cluster_starts.partition_point(|start| *start <= start_utf16);
            let end_utf16 = cluster_starts.get(next).copied().unwrap_or(utf16_len);
            let start = utf8_for_utf16(start_utf16);
            let end = utf8_for_utf16(end_utf16);
            let left = (position.x.min(position.x + advance.width)) as f32;
            let right = (position.x.max(position.x + advance.width)) as f32;
            let key = (start, end, rtl);
            if let Some(index) = cluster_indices.get(&key).copied() {
                let cluster = &mut clusters[index];
                cluster.left = cluster.left.min(left);
                cluster.right = cluster.right.max(right);
            } else {
                cluster_indices.insert(key, clusters.len());
                clusters.push(Cluster {
                    start,
                    end,
                    left,
                    right,
                    direction: if rtl {
                        TextDirection::RightToLeft
                    } else {
                        TextDirection::LeftToRight
                    },
                });
            }
        }
    }

    clusters.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.direction.cmp(&right.direction))
    });

    let mut stops = Vec::with_capacity(clusters.len() * 2 + 2);
    for cluster in &clusters {
        let (leading, trailing) = match cluster.direction {
            TextDirection::LeftToRight => (cluster.left, cluster.right),
            TextDirection::RightToLeft => (cluster.right, cluster.left),
        };
        stops.push(CaretStop {
            index: cluster.start,
            affinity: CaretAffinity::Downstream,
            direction: cluster.direction,
            x: px(leading),
        });
        stops.push(CaretStop {
            index: cluster.end,
            affinity: CaretAffinity::Upstream,
            direction: cluster.direction,
            x: px(trailing),
        });
    }
    let mut stop_keys = stops
        .iter()
        .map(|stop| (stop.index, stop.affinity, stop.direction))
        .collect::<HashSet<_>>();

    let offset_nearest_to = |expected: f32, primary: f32, secondary: f32| {
        if (primary - expected).abs() <= (secondary - expected).abs() {
            primary
        } else {
            secondary
        }
    };
    let expected_cluster_x = |cluster: &Cluster, index: usize| {
        let span = cluster.end.saturating_sub(cluster.start);
        let ratio = if span == 0 {
            0.0
        } else {
            index.saturating_sub(cluster.start) as f32 / span as f32
        };
        match cluster.direction {
            TextDirection::LeftToRight => cluster.left + (cluster.right - cluster.left) * ratio,
            TextDirection::RightToLeft => cluster.right - (cluster.right - cluster.left) * ratio,
        }
    };
    for (utf16_index, utf8_index) in utf_boundaries {
        let mut secondary = 0.0;
        let primary = unsafe {
            CTLineGetOffsetForStringIndex(
                line.as_concrete_TypeRef(),
                utf16_index as CFIndex,
                &mut secondary,
            ) as f32
        };
        let secondary = secondary as f32;

        let upstream_ix = clusters.partition_point(|cluster| cluster.start < utf8_index);
        if let Some(cluster) = upstream_ix
            .checked_sub(1)
            .and_then(|index| clusters.get(index))
            .filter(|cluster| cluster.start < utf8_index && utf8_index <= cluster.end)
            && stop_keys.insert((utf8_index, CaretAffinity::Upstream, cluster.direction))
        {
            stops.push(CaretStop {
                index: utf8_index,
                affinity: CaretAffinity::Upstream,
                direction: cluster.direction,
                x: px(offset_nearest_to(
                    expected_cluster_x(cluster, utf8_index),
                    primary,
                    secondary,
                )),
            });
        }
        let downstream_ix = clusters.partition_point(|cluster| cluster.start <= utf8_index);
        if let Some(cluster) = downstream_ix
            .checked_sub(1)
            .and_then(|index| clusters.get(index))
            .filter(|cluster| cluster.start <= utf8_index && utf8_index < cluster.end)
            && stop_keys.insert((utf8_index, CaretAffinity::Downstream, cluster.direction))
        {
            stops.push(CaretStop {
                index: utf8_index,
                affinity: CaretAffinity::Downstream,
                direction: cluster.direction,
                x: px(offset_nearest_to(
                    expected_cluster_x(cluster, utf8_index),
                    primary,
                    secondary,
                )),
            });
        }
    }
    if stops.is_empty() {
        stops.push(CaretStop {
            index: 0,
            affinity: CaretAffinity::Downstream,
            direction: TextDirection::LeftToRight,
            x: Pixels::ZERO,
        });
    }
    stops.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.x.as_f32().total_cmp(&right.x.as_f32()))
            .then_with(|| left.affinity.cmp(&right.affinity))
            .then_with(|| left.direction.cmp(&right.direction))
    });
    stops.dedup_by(|left, right| {
        left.index == right.index
            && left.x.as_f32().to_bits() == right.x.as_f32().to_bits()
            && left.affinity == right.affinity
            && left.direction == right.direction
    });
    stops
}

const CTRUN_STATUS_RTL: u32 = 1;

#[cfg_attr(target_os = "macos", link(name = "CoreText", kind = "framework"))]
unsafe extern "C" {
    fn CTLineEnumerateCaretOffsets(line: CTLineRef, block: *const c_void);
    #[cfg(debug_assertions)]
    fn CTLineGetOffsetForStringIndex(
        line: CTLineRef,
        char_index: CFIndex,
        secondary_offset: *mut CGFloat,
    ) -> CGFloat;
    fn CTRunGetAdvances(run: CTRunRef, range: CFRange, buffer: *mut CGSize);
    fn CTRunGetStatus(run: CTRunRef) -> u32;
}

#[derive(Debug, Clone)]
struct StringIndexConverter<'a> {
    text: &'a str,
    /// Index in UTF-8 bytes
    utf8_ix: usize,
    /// Index in UTF-16 code units
    utf16_ix: usize,
}

impl<'a> StringIndexConverter<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            utf8_ix: 0,
            utf16_ix: 0,
        }
    }

    fn advance_to_utf16_ix(&mut self, utf16_target: usize) {
        for (ix, c) in self.text[self.utf8_ix..].char_indices() {
            if self.utf16_ix >= utf16_target {
                self.utf8_ix += ix;
                return;
            }
            self.utf16_ix += c.len_utf16();
        }
        self.utf8_ix = self.text.len();
    }
}

fn font_kit_metrics_to_metrics(metrics: Metrics) -> FontMetrics {
    FontMetrics {
        units_per_em: metrics.units_per_em,
        ascent: metrics.ascent,
        descent: metrics.descent,
        line_gap: metrics.line_gap,
        underline_position: metrics.underline_position,
        underline_thickness: metrics.underline_thickness,
        cap_height: metrics.cap_height,
        x_height: metrics.x_height,
        bounding_box: bounds_from_rect(metrics.bounding_box),
    }
}

fn bounds_from_rect(rect: RectF) -> Bounds<f32> {
    Bounds {
        origin: point(rect.origin_x(), rect.origin_y()),
        size: size(rect.width(), rect.height()),
    }
}

fn bounds_from_rect_i(rect: RectI) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(DevicePixels(rect.origin_x()), DevicePixels(rect.origin_y())),
        size: size(DevicePixels(rect.width()), DevicePixels(rect.height())),
    }
}

// impl From<Vector2I> for Size<DevicePixels> {
//     fn from(value: Vector2I) -> Self {
//         size(value.x().into(), value.y().into())
//     }
// }

// impl From<RectI> for Bounds<i32> {
//     fn from(rect: RectI) -> Self {
//         Bounds {
//             origin: point(rect.origin_x(), rect.origin_y()),
//             size: size(rect.width(), rect.height()),
//         }
//     }
// }

// impl From<Point<u32>> for Vector2I {
//     fn from(size: Point<u32>) -> Self {
//         Vector2I::new(size.x as i32, size.y as i32)
//     }
// }

fn size_from_vector2f(vec: Vector2F) -> Size<f32> {
    size(vec.x(), vec.y())
}

fn fontkit_weight(value: FontWeight) -> FontkitWeight {
    FontkitWeight(value.0)
}

fn fontkit_style(style: FontStyle) -> FontkitStyle {
    match style {
        FontStyle::Normal => FontkitStyle::Normal,
        FontStyle::Italic => FontkitStyle::Italic,
        FontStyle::Oblique => FontkitStyle::Oblique,
    }
}

// Some fonts may have no attributes despite `core_text` requiring them (and panicking).
// This is the same version as `core_text` has without `expect` calls.
mod lenient_font_attributes {
    use core_foundation::{
        base::{CFRetain, CFType, TCFType},
        string::{CFString, CFStringRef},
    };
    use core_text::font_descriptor::{
        CTFontDescriptor, CTFontDescriptorCopyAttribute, kCTFontFamilyNameAttribute,
    };

    pub fn family_name(descriptor: &CTFontDescriptor) -> Option<String> {
        unsafe { get_string_attribute(descriptor, kCTFontFamilyNameAttribute) }
    }

    fn get_string_attribute(
        descriptor: &CTFontDescriptor,
        attribute: CFStringRef,
    ) -> Option<String> {
        unsafe {
            let value = CTFontDescriptorCopyAttribute(descriptor.as_concrete_TypeRef(), attribute);
            if value.is_null() {
                return None;
            }

            let value = CFType::wrap_under_create_rule(value);
            assert!(value.instance_of::<CFString>());
            let s = wrap_under_get_rule(value.as_CFTypeRef() as CFStringRef);
            Some(s.to_string())
        }
    }

    unsafe fn wrap_under_get_rule(reference: CFStringRef) -> CFString {
        unsafe {
            assert!(!reference.is_null(), "Attempted to create a NULL object.");
            let reference = CFRetain(reference as *const ::std::os::raw::c_void) as CFStringRef;
            TCFType::wrap_under_create_rule(reference)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::MacTextSystem;
    use gpui::{
        FontRun, GlyphId, Pixels, PlatformTextSystem, RichFontRun, RichTextRun, TextDirection,
        TextSystem, WindowTextSystem, black, font, px, red,
    };
    use std::sync::Arc;

    #[test]
    fn empty_legacy_font_runs_retain_requested_font_size() {
        let fonts = MacTextSystem::new();
        let layout = fonts.layout_line("", px(19.0), &[]);

        assert_eq!(layout.font_size, px(19.0));
    }

    #[test]
    fn physical_face_fingerprint_is_cached_by_postscript_name() -> gpui::Result<()> {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica"))?;

        let first = fonts.font_face(font_id).expect("Helvetica face metadata");
        assert_eq!(fonts.1.read().len(), 1);
        let second = fonts.font_face(font_id).expect("cached face metadata");

        assert_eq!(first, second);
        assert_eq!(fonts.1.read().len(), 1);
        Ok(())
    }

    #[test]
    fn test_layout_line_bom_char() {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica")).unwrap();
        let line = "\u{feff}";
        let mut style = FontRun {
            font_id,
            len: line.len(),
        };

        let layout = fonts.layout_line(line, px(16.), &[style]);
        assert_eq!(layout.len, line.len());
        assert!(layout.runs.is_empty());

        let line = "a\u{feff}b";
        style.len = line.len();
        let layout = fonts.layout_line(line, px(16.), &[style]);
        assert_eq!(layout.len, line.len());
        assert_eq!(layout.runs.len(), 1);
        assert_eq!(layout.runs[0].glyphs.len(), 2);
        assert_eq!(layout.runs[0].glyphs[0].id, GlyphId(68u32)); // a
        // There's no glyph for \u{feff}
        assert_eq!(layout.runs[0].glyphs[1].id, GlyphId(69u32)); // b

        let line = "\u{feff}ab";
        let font_runs = &[
            FontRun {
                len: "\u{feff}".len(),
                font_id,
            },
            FontRun {
                len: "ab".len(),
                font_id,
            },
        ];
        let layout = fonts.layout_line(line, px(16.), font_runs);
        assert_eq!(layout.len, line.len());
        assert_eq!(layout.runs.len(), 1);
        assert_eq!(layout.runs[0].glyphs.len(), 2);
        // There's no glyph for \u{feff}
        assert_eq!(layout.runs[0].glyphs[0].id, GlyphId(68u32)); // a
        assert_eq!(layout.runs[0].glyphs[1].id, GlyphId(69u32)); // b
    }

    #[test]
    fn test_layout_line_zwnj_insertion() {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica")).unwrap();

        let text = "hello world";
        let font_runs = &[
            FontRun { font_id, len: 5 }, // "hello"
            FontRun { font_id, len: 6 }, // " world"
        ];

        let layout = fonts.layout_line(text, px(16.), font_runs);
        assert_eq!(layout.len, text.len());

        for run in &layout.runs {
            for glyph in &run.glyphs {
                assert!(
                    glyph.index < text.len(),
                    "Glyph index {} is out of bounds for text length {}",
                    glyph.index,
                    text.len()
                );
            }
        }

        // Test with different font runs - should not insert ZWNJ
        let font_id2 = fonts.font_id(&font("Times")).unwrap_or(font_id);
        let font_runs_different = &[
            FontRun { font_id, len: 5 }, // "hello"
            // " world"
            FontRun {
                font_id: font_id2,
                len: 6,
            },
        ];

        let layout2 = fonts.layout_line(text, px(16.), font_runs_different);
        assert_eq!(layout2.len, text.len());

        for run in &layout2.runs {
            for glyph in &run.glyphs {
                assert!(
                    glyph.index < text.len(),
                    "Glyph index {} is out of bounds for text length {}",
                    glyph.index,
                    text.len()
                );
            }
        }
    }

    #[test]
    fn test_layout_line_zwnj_edge_cases() {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica")).unwrap();

        let text = "hello";
        let font_runs = &[FontRun { font_id, len: 5 }];
        let layout = fonts.layout_line(text, px(16.), font_runs);
        assert_eq!(layout.len, text.len());

        let text = "abc";
        let font_runs = &[
            FontRun { font_id, len: 1 }, // "a"
            FontRun { font_id, len: 1 }, // "b"
            FontRun { font_id, len: 1 }, // "c"
        ];
        let layout = fonts.layout_line(text, px(16.), font_runs);
        assert_eq!(layout.len, text.len());

        for run in &layout.runs {
            for glyph in &run.glyphs {
                assert!(
                    glyph.index < text.len(),
                    "Glyph index {} is out of bounds for text length {}",
                    glyph.index,
                    text.len()
                );
            }
        }

        // Test with empty text
        let text = "";
        let font_runs = &[];
        let layout = fonts.layout_line(text, px(16.), font_runs);
        assert_eq!(layout.len, 0);
        assert!(layout.runs.is_empty());
    }

    #[test]
    fn rich_line_preserves_per_run_metrics() -> gpui::Result<()> {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica"))?;
        let runs = [
            RichFontRun {
                len: 1,
                font_id,
                font_size: px(12.0),
                minimum_line_height: px(16.0),
                baseline_shift: px(0.0),
            },
            RichFontRun {
                len: 1,
                font_id,
                font_size: px(18.0),
                minimum_line_height: px(24.0),
                baseline_shift: px(2.0),
            },
            RichFontRun {
                len: 1,
                font_id,
                font_size: px(24.0),
                minimum_line_height: px(32.0),
                baseline_shift: px(-1.0),
            },
        ];

        let layout = fonts.layout_rich_line("abc", &runs)?;
        let mut sizes = layout
            .runs
            .iter()
            .map(|run| run.font_size.as_f32())
            .collect::<Vec<_>>();
        sizes.sort_by(f32::total_cmp);
        sizes.dedup();
        assert_eq!(sizes, vec![12.0, 18.0, 24.0]);
        assert_eq!(layout.minimum_line_height, px(32.0));
        assert!(layout.runs.iter().any(|run| run.baseline_shift == px(2.0)));
        assert!(layout.runs.iter().any(|run| run.baseline_shift == px(-1.0)));
        Ok(())
    }

    #[test]
    fn rich_baseline_boundaries_do_not_overallocate_glyph_vectors() -> gpui::Result<()> {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica"))?;
        let text = "a".repeat(256);
        let runs = (0..text.len())
            .map(|index| RichFontRun {
                len: 1,
                font_id,
                font_size: px(16.0),
                minimum_line_height: px(20.0),
                baseline_shift: px((index % 2) as f32),
            })
            .collect::<Vec<_>>();

        let layout = fonts.layout_rich_line(&text, &runs)?;
        let glyph_count = layout
            .runs
            .iter()
            .map(|run| run.glyphs.len())
            .sum::<usize>();
        let glyph_capacity = layout
            .runs
            .iter()
            .map(|run| run.glyphs.capacity())
            .sum::<usize>();

        assert!(
            glyph_capacity <= glyph_count * 4,
            "{glyph_count} glyph için {glyph_capacity} öğelik kapasite ayrıldı"
        );
        Ok(())
    }

    #[test]
    fn mixed_bidi_exposes_distinct_directional_caret_stops() -> gpui::Result<()> {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica"))?;
        let text = "abc אבג xyz";
        let layout = fonts.layout_rich_line(
            text,
            &[RichFontRun {
                len: text.len(),
                font_id,
                font_size: px(18.0),
                minimum_line_height: px(24.0),
                baseline_shift: px(0.0),
            }],
        )?;

        let stops = layout.caret_stops_for_index(4);
        assert!(
            stops
                .iter()
                .any(|stop| stop.direction == TextDirection::LeftToRight)
        );
        assert!(
            stops
                .iter()
                .any(|stop| stop.direction == TextDirection::RightToLeft)
        );
        assert!(
            stops
                .iter()
                .any(|left| stops.iter().any(|right| (left.x - right.x).abs() > px(0.1)))
        );
        Ok(())
    }

    #[test]
    fn caret_stops_cover_ligature_internal_boundaries() -> gpui::Result<()> {
        let fonts = MacTextSystem::new();
        let font_id = fonts.font_id(&font("Helvetica"))?;
        let text = "office";
        let layout = fonts.layout_rich_line(
            text,
            &[RichFontRun {
                len: text.len(),
                font_id,
                font_size: px(18.0),
                minimum_line_height: px(24.0),
                baseline_shift: Pixels::ZERO,
            }],
        )?;

        for boundary in text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
        {
            assert!(
                !layout.caret_stops_for_index(boundary).is_empty(),
                "missing caret stop at byte {boundary}"
            );
        }

        let mut glyph_boundaries = layout
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index))
            .collect::<Vec<_>>();
        glyph_boundaries.push(text.len());
        glyph_boundaries.sort_unstable();
        glyph_boundaries.dedup();
        let (cluster_start, cluster_end) = glyph_boundaries
            .windows(2)
            .find_map(|pair| (pair[1] > pair[0] + 1).then_some((pair[0], pair[1])))
            .expect("office should contain a multi-character ligature cluster");
        let internal = cluster_start + 1;
        let edge_x = layout
            .caret_stops_for_index(cluster_start)
            .iter()
            .chain(layout.caret_stops_for_index(cluster_end))
            .map(|stop| stop.x)
            .collect::<Vec<_>>();
        let left = edge_x.iter().copied().min().unwrap();
        let right = edge_x.iter().copied().max().unwrap();
        assert!(
            layout
                .caret_stops_for_index(internal)
                .iter()
                .any(|stop| stop.x > left && stop.x < right),
            "ligature-internal caret must remain between cluster edges"
        );
        Ok(())
    }

    #[test]
    fn paint_only_changes_reuse_real_core_text_geometry() -> gpui::Result<()> {
        let text_system = Arc::new(TextSystem::new(Arc::new(MacTextSystem::new())));
        let window_text_system = WindowTextSystem::new(text_system);
        let text = "CoreText geometry";
        let mut run = RichTextRun {
            len: text.len(),
            font: font("Helvetica"),
            font_size: px(18.0),
            minimum_line_height: px(24.0),
            color: black(),
            ..Default::default()
        };
        let first = window_text_system.shape_rich_line(text.into(), &[run.clone()], None)?;
        run.color = red();
        let second = window_text_system.shape_rich_line(text.into(), &[run], None)?;

        assert!(Arc::ptr_eq(&first.geometry(), &second.geometry()));
        assert_ne!(
            first.paint_payload().runs()[0].color,
            second.paint_payload().runs()[0].color
        );
        Ok(())
    }

    #[test]
    fn shaped_fallback_runs_carry_resolved_physical_faces() -> gpui::Result<()> {
        let text_system = Arc::new(TextSystem::new(Arc::new(MacTextSystem::new())));
        let window_text_system = WindowTextSystem::new(text_system.clone());
        let text = "A🙂אב";
        let shaped = window_text_system.shape_rich_line(
            text.into(),
            &[RichTextRun {
                len: text.len(),
                font: font("Helvetica"),
                font_size: px(18.0),
                minimum_line_height: px(24.0),
                ..Default::default()
            }],
            None,
        )?;

        assert!(!shaped.runs.is_empty());
        assert!(shaped.runs.iter().all(|run| {
            run.resolved_face.as_ref().is_some_and(|face| {
                face.text_system_id() == text_system.id()
                    && face.source_fingerprint().byte_len() > 0
            })
        }));
        assert!(
            shaped
                .runs
                .iter()
                .filter_map(|run| run.resolved_face.as_ref().map(|face| face.identity()))
                .collect::<std::collections::HashSet<_>>()
                .len()
                >= 2
        );
        Ok(())
    }
}
