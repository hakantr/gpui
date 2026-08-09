mod font_fallbacks;
mod font_features;
mod line;
mod line_layout;
mod line_wrapper;

pub use font_fallbacks::*;
pub use font_features::*;
pub use line::*;
pub use line_layout::*;
pub use line_wrapper::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Bounds, DevicePixels, Hsla, Pixels, PlatformTextSystem, Point, Result, SharedString, Size,
    StrikethroughStyle, TextRenderingMode, UnderlineStyle, px,
};
use anyhow::{Context as _, anyhow};
use collections::FxHashMap;
use core::fmt;
use derive_more::{Add, Deref, FromStr, Sub};
use itertools::Itertools;
use parking_lot::{Mutex, RwLock, RwLockUpgradableReadGuard};
use smallvec::{SmallVec, smallvec};
use std::{
    borrow::Cow,
    cmp,
    fmt::{Debug, Display, Formatter},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut, Range},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// An opaque identifier for a specific font.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
#[repr(C)]
pub struct FontId(pub usize);

/// An opaque identifier for one [`TextSystem`] instance.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub struct TextSystemId(u64);

/// An opaque fingerprint of the bytes or native source backing a font file.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub struct FontSourceFingerprint {
    first: u64,
    second: u64,
    byte_len: u64,
}

impl FontSourceFingerprint {
    /// Fingerprint font data without retaining or exposing the bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            first: seahash::hash(bytes),
            second: seahash::hash_seeded(
                bytes,
                0x243f_6a88_85a3_08d3,
                0x1319_8a2e_0370_7344,
                0xa409_3822_299f_31d0,
                0x082e_fa98_ec4e_6c89,
            ),
            byte_len: bytes.len() as u64,
        }
    }

    /// Fingerprint a backend-native source key when the font bytes are unavailable.
    pub fn from_native_key(key: &[u8]) -> Self {
        Self {
            first: seahash::hash_seeded(
                key,
                0x4528_21e6_38d0_1377,
                0xbe54_66cf_34e9_0c6c,
                0xc0ac_29b7_c97c_50dd,
                0x3f84_d5b5_b547_0917,
            ),
            second: seahash::hash_seeded(
                key,
                0x9216_d5d9_8979_fb1b,
                0xd131_0ba6_98df_b5ac,
                0x2ffd_72db_d01a_dfb7,
                0xb8e1_afed_6a26_7e96,
            ),
            byte_len: 0,
        }
    }

    /// Qualify a source fingerprint with backend face metadata such as a PostScript name.
    ///
    /// This is a compact, probabilistic identity aid rather than a cryptographic digest. The
    /// backing byte length is preserved so callers can still distinguish byte-backed and native
    /// fingerprints.
    pub fn with_discriminator(self, discriminator: &[u8]) -> Self {
        let mut input = Vec::with_capacity(24 + discriminator.len());
        input.extend_from_slice(&self.first.to_le_bytes());
        input.extend_from_slice(&self.second.to_le_bytes());
        input.extend_from_slice(&self.byte_len.to_le_bytes());
        input.extend_from_slice(discriminator);
        Self {
            first: seahash::hash_seeded(
                &input,
                0x3bd3_9e10_cb0e_f593,
                0xc0ac_f169_b5f1_8a8c,
                0xbe54_66cf_34e9_0c6c,
                0x4528_21e6_38d0_1377,
            ),
            second: seahash::hash_seeded(
                &input,
                0x9216_d5d9_8979_fb1b,
                0xd131_0ba6_98df_b5ac,
                0x2ffd_72db_d01a_dfb7,
                0xb8e1_afed_6a26_7e96,
            ),
            byte_len: self.byte_len,
        }
    }

    /// Returns the number of fingerprinted bytes, or zero for a native source key.
    pub fn byte_len(self) -> u64 {
        self.byte_len
    }
}

/// Backend-provided metadata for one concrete font face.
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
pub struct PlatformFontFace {
    family: SharedString,
    postscript_name: Option<SharedString>,
    source: FontSourceFingerprint,
    face_index: u32,
}

impl PlatformFontFace {
    /// Create metadata for a concrete platform font face.
    pub fn new(
        family: impl Into<SharedString>,
        postscript_name: Option<impl Into<SharedString>>,
        source: FontSourceFingerprint,
        face_index: u32,
    ) -> Self {
        Self {
            family: family.into(),
            postscript_name: postscript_name.map(Into::into),
            source,
            face_index,
        }
    }
}

/// An opaque physical face identity scoped to one [`TextSystem`].
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub struct ResolvedFontFaceId {
    text_system: TextSystemId,
    source: FontSourceFingerprint,
    face_index: u32,
}

/// The concrete physical font face used by a shaped run.
#[derive(Hash, PartialEq, Eq, Clone, Debug)]
pub struct ResolvedFontFace(Arc<ResolvedFontFaceData>);

#[derive(Hash, PartialEq, Eq, Debug)]
struct ResolvedFontFaceData {
    identity: ResolvedFontFaceId,
    font_id: FontId,
    family: SharedString,
    postscript_name: Option<SharedString>,
}

impl ResolvedFontFace {
    /// Returns the opaque identity of this physical face.
    pub fn identity(&self) -> ResolvedFontFaceId {
        self.0.identity
    }

    /// Returns the text-system-scoped font identifier used for rasterization.
    pub fn font_id(&self) -> FontId {
        self.0.font_id
    }

    /// Returns the resolved family name reported by the backend.
    pub fn family(&self) -> &SharedString {
        &self.0.family
    }

    /// Returns the resolved PostScript name when the backend provides one.
    pub fn postscript_name(&self) -> Option<&SharedString> {
        self.0.postscript_name.as_ref()
    }

    /// Returns the opaque source fingerprint used by the physical face identity.
    pub fn source_fingerprint(&self) -> FontSourceFingerprint {
        self.0.identity.source
    }

    /// Returns the face index within the backing font collection.
    pub fn face_index(&self) -> u32 {
        self.0.identity.face_index
    }

    /// Returns the [`TextSystem`] scope in which this identity is valid.
    pub fn text_system_id(&self) -> TextSystemId {
        self.0.identity.text_system
    }
}

/// An opaque identifier for a specific font family.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub struct FontFamilyId(pub usize);

/// Number of subpixel glyph variants along the X axis.
pub const SUBPIXEL_VARIANTS_X: u8 = 4;

/// Number of subpixel glyph variants along the Y axis.
pub const SUBPIXEL_VARIANTS_Y: u8 = 1;

/// The GPUI text rendering sub system.
pub struct TextSystem {
    id: TextSystemId,
    platform_text_system: Arc<dyn PlatformTextSystem>,
    font_ids_by_font: RwLock<FxHashMap<Font, Result<FontId>>>,
    resolved_font_faces: RwLock<FxHashMap<FontId, Option<ResolvedFontFace>>>,
    font_metrics: RwLock<FxHashMap<FontId, FontMetrics>>,
    raster_bounds: RwLock<FxHashMap<RenderGlyphParams, Bounds<DevicePixels>>>,
    wrapper_pool: Mutex<FxHashMap<FontIdWithSize, Vec<LineWrapper>>>,
    font_runs_pool: Mutex<Vec<Vec<FontRun>>>,
    fallback_font_stack: SmallVec<[Font; 2]>,
}

impl TextSystem {
    /// Create a new TextSystem with the given platform text system.
    pub fn new(platform_text_system: Arc<dyn PlatformTextSystem>) -> Self {
        static NEXT_TEXT_SYSTEM_ID: AtomicU64 = AtomicU64::new(1);

        TextSystem {
            id: TextSystemId(NEXT_TEXT_SYSTEM_ID.fetch_add(1, Ordering::Relaxed)),
            platform_text_system,
            font_metrics: RwLock::default(),
            raster_bounds: RwLock::default(),
            font_ids_by_font: RwLock::default(),
            resolved_font_faces: RwLock::default(),
            wrapper_pool: Mutex::default(),
            font_runs_pool: Mutex::default(),
            fallback_font_stack: smallvec![
                // TODO: Remove this when Linux have implemented setting fallbacks.
                font(".ZedMono"),
                font(".ZedSans"),
                font("Helvetica"),
                font("Segoe UI"),     // Windows
                font("Ubuntu"),       // Gnome (Ubuntu)
                font("Adwaita Sans"), // Gnome 47
                font("Cantarell"),    // Gnome
                font("Noto Sans"),    // KDE
                font("DejaVu Sans"),
                font("Arial"), // macOS, Windows
            ],
        }
    }

    /// Returns the opaque identity of this text-system instance.
    pub fn id(&self) -> TextSystemId {
        self.id
    }

    /// Get a list of all available font names from the operating system.
    pub fn all_font_names(&self) -> Vec<String> {
        let mut names = self.platform_text_system.all_font_names();
        names.extend(
            self.fallback_font_stack
                .iter()
                .map(|font| font.family.to_string()),
        );
        names.push(".SystemUIFont".to_string());
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Add a font's data to the text system.
    pub fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.platform_text_system.add_fonts(fonts)
    }

    /// Get the FontId for the configure font family and style.
    fn font_id(&self, font: &Font) -> Result<FontId> {
        fn clone_font_id_result(font_id: &Result<FontId>) -> Result<FontId> {
            match font_id {
                Ok(font_id) => Ok(*font_id),
                Err(err) => Err(anyhow!("{err}")),
            }
        }

        let font_id = self
            .font_ids_by_font
            .read()
            .get(font)
            .map(clone_font_id_result);
        if let Some(font_id) = font_id {
            font_id
        } else {
            let font_id = self.platform_text_system.font_id(font);
            self.font_ids_by_font
                .write()
                .insert(font.clone(), clone_font_id_result(&font_id));
            font_id
        }
    }

    /// Get the Font for the Font Id.
    pub fn get_font_for_id(&self, id: FontId) -> Option<Font> {
        let lock = self.font_ids_by_font.read();
        lock.iter()
            .filter_map(|(font, result)| match result {
                Ok(font_id) if *font_id == id => Some(font.clone()),
                _ => None,
            })
            .next()
    }

    /// Resolve a shaped [`FontId`] to the concrete physical face selected by the backend.
    pub fn resolved_font_face(&self, font_id: FontId) -> Option<ResolvedFontFace> {
        if let Some(face) = self.resolved_font_faces.read().get(&font_id) {
            return face.clone();
        }

        let face = self.platform_text_system.font_face(font_id).map(|face| {
            ResolvedFontFace(Arc::new(ResolvedFontFaceData {
                identity: ResolvedFontFaceId {
                    text_system: self.id,
                    source: face.source,
                    face_index: face.face_index,
                },
                font_id,
                family: face.family,
                postscript_name: face.postscript_name,
            }))
        });
        self.resolved_font_faces
            .write()
            .entry(font_id)
            .or_insert(face)
            .clone()
    }

    pub(crate) fn finalize_line_layout(&self, layout: &mut LineLayout, rich_metrics: bool) {
        for run in &mut layout.runs {
            if run.font_size == Pixels::ZERO {
                run.font_size = layout.font_size;
            }
            if rich_metrics && run.resolved_face.is_none() {
                run.resolved_face = self.resolved_font_face(run.font_id);
            }
        }

        if rich_metrics {
            if !layout.runs.is_empty() {
                let mut ascent = Pixels::ZERO;
                let mut descent = Pixels::ZERO;
                for run in &layout.runs {
                    self.read_metrics(run.font_id, |metrics| {
                        let scale = run.font_size.as_f32() / metrics.units_per_em as f32;
                        let run_ascent = px(metrics.ascent * scale) + run.baseline_shift;
                        let run_descent = px(-metrics.descent * scale) - run.baseline_shift;
                        if run_ascent > ascent {
                            ascent = run_ascent;
                        }
                        if run_descent > descent {
                            descent = run_descent;
                        }
                    });
                }
                layout.ascent = ascent.max(Pixels::ZERO);
                layout.descent = descent.max(Pixels::ZERO);
            }

            // Empty rich lines have no shaped runs to revisit, but their synthetic layout still
            // carries the first run's font metrics. Keep that physical glyph box as a floor just
            // as we do after recomputing metrics for non-empty rich lines.
            let glyph_height = layout.ascent + layout.descent;
            if glyph_height > layout.minimum_line_height {
                layout.minimum_line_height = glyph_height;
            }
            layout.normalize_caret_stops();
        }
    }

    /// Resolves the specified font, falling back to the default font stack if
    /// the font fails to load.
    ///
    /// # Panics
    ///
    /// Panics if the font and none of the fallbacks can be resolved.
    pub fn resolve_font(&self, font: &Font) -> FontId {
        if let Ok(font_id) = self.font_id(font) {
            return font_id;
        }
        for fallback in &self.fallback_font_stack {
            if let Ok(font_id) = self.font_id(fallback) {
                return font_id;
            }
        }

        panic!(
            "failed to resolve font '{}' or any of the fallbacks: {}",
            font.family,
            self.fallback_font_stack
                .iter()
                .map(|fallback| &fallback.family)
                .join(", ")
        );
    }

    /// Get the bounding box for the given font and font size.
    /// A font's bounding box is the smallest rectangle that could enclose all glyphs
    /// in the font. superimposed over one another.
    pub fn bounding_box(&self, font_id: FontId, font_size: Pixels) -> Bounds<Pixels> {
        self.read_metrics(font_id, |metrics| metrics.bounding_box(font_size))
    }

    /// Get the typographic bounds for the given character, in the given font and size.
    pub fn typographic_bounds(
        &self,
        font_id: FontId,
        font_size: Pixels,
        character: char,
    ) -> Result<Bounds<Pixels>> {
        let glyph_id = self
            .platform_text_system
            .glyph_for_char(font_id, character)
            .with_context(|| format!("glyph not found for character '{character}'"))?;
        let bounds = self
            .platform_text_system
            .typographic_bounds(font_id, glyph_id)?;
        Ok(self.read_metrics(font_id, |metrics| {
            (bounds / metrics.units_per_em as f32 * font_size.0).map(px)
        }))
    }

    /// Get the advance width for the given character, in the given font and size.
    pub fn advance(&self, font_id: FontId, font_size: Pixels, ch: char) -> Result<Size<Pixels>> {
        let glyph_id = self
            .platform_text_system
            .glyph_for_char(font_id, ch)
            .with_context(|| format!("glyph not found for character '{ch}'"))?;
        let result = self.platform_text_system.advance(font_id, glyph_id)?
            / self.units_per_em(font_id) as f32;

        Ok(result * font_size)
    }

    // Consider removing this?
    /// Returns the shaped layout width of for the given character, in the given font and size.
    pub fn layout_width(&self, font_id: FontId, font_size: Pixels, ch: char) -> Pixels {
        let mut buffer = [0; 4];
        let buffer = ch.encode_utf8(&mut buffer);
        self.platform_text_system
            .layout_line(
                buffer,
                font_size,
                &[FontRun {
                    len: buffer.len(),
                    font_id,
                }],
            )
            .width
    }

    /// Returns the width of an `em`.
    ///
    /// Uses the width of the `m` character in the given font and size.
    pub fn em_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.typographic_bounds(font_id, font_size, 'm')?.size.width)
    }

    /// Returns the advance width of an `em`.
    ///
    /// Uses the advance width of the `m` character in the given font and size.
    pub fn em_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.advance(font_id, font_size, 'm')?.width)
    }

    /// Returns the width of an `ch`.
    ///
    /// Uses the width of the `0` character in the given font and size.
    pub fn ch_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.typographic_bounds(font_id, font_size, '0')?.size.width)
    }

    /// Returns the advance width of an `ch`.
    ///
    /// Uses the advance width of the `0` character in the given font and size.
    pub fn ch_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.advance(font_id, font_size, '0')?.width)
    }

    /// Get the number of font size units per 'em square',
    /// Per MDN: "an abstract square whose height is the intended distance between
    /// lines of type in the same type size"
    pub fn units_per_em(&self, font_id: FontId) -> u32 {
        self.read_metrics(font_id, |metrics| metrics.units_per_em)
    }

    /// Get the height of a capital letter in the given font and size.
    pub fn cap_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.cap_height(font_size))
    }

    /// Get the height of the x character in the given font and size.
    pub fn x_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.x_height(font_size))
    }

    /// Get the recommended distance from the baseline for the given font
    pub fn ascent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.ascent(font_size))
    }

    /// Get the recommended distance below the baseline for the given font,
    /// in single spaced text.
    pub fn descent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.descent(font_size))
    }

    /// Get the recommended baseline offset for the given font and line height.
    pub fn baseline_offset(
        &self,
        font_id: FontId,
        font_size: Pixels,
        line_height: Pixels,
    ) -> Pixels {
        let ascent = self.ascent(font_id, font_size);
        let descent = self.descent(font_id, font_size);
        let padding_top = (line_height - ascent - descent) / 2.;
        padding_top + ascent
    }

    fn read_metrics<T>(&self, font_id: FontId, read: impl FnOnce(&FontMetrics) -> T) -> T {
        let lock = self.font_metrics.upgradable_read();

        if let Some(metrics) = lock.get(&font_id) {
            read(metrics)
        } else {
            let mut lock = RwLockUpgradableReadGuard::upgrade(lock);
            let metrics = lock
                .entry(font_id)
                .or_insert_with(|| self.platform_text_system.font_metrics(font_id));
            read(metrics)
        }
    }

    /// Returns a handle to a line wrapper, for the given font and font size.
    pub fn line_wrapper(self: &Arc<Self>, font: Font, font_size: Pixels) -> LineWrapperHandle {
        let lock = &mut self.wrapper_pool.lock();
        let font_id = self.resolve_font(&font);
        let wrappers = lock
            .entry(FontIdWithSize { font_id, font_size })
            .or_default();
        let wrapper = wrappers
            .pop()
            .unwrap_or_else(|| LineWrapper::new(font_id, font_size, self.clone()));

        LineWrapperHandle {
            wrapper: Some(wrapper),
            text_system: self.clone(),
        }
    }

    /// Get the rasterized size and location of a specific, rendered glyph.
    pub(crate) fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let raster_bounds = self.raster_bounds.upgradable_read();
        if let Some(bounds) = raster_bounds.get(params) {
            Ok(*bounds)
        } else {
            let mut raster_bounds = RwLockUpgradableReadGuard::upgrade(raster_bounds);
            let bounds = self.platform_text_system.glyph_raster_bounds(params)?;
            raster_bounds.insert(params.clone(), bounds);
            Ok(bounds)
        }
    }

    pub(crate) fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let raster_bounds = self.raster_bounds(params)?;
        self.platform_text_system
            .rasterize_glyph(params, raster_bounds)
    }

    /// Returns the dilation level to use for a glyph painted in the given color.
    pub(crate) fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.platform_text_system.glyph_dilation_for_color(color)
    }

    /// Returns the text rendering mode recommended by the platform for the given font and size.
    /// The return value will never be [`TextRenderingMode::PlatformDefault`].
    pub(crate) fn recommended_rendering_mode(
        &self,
        font_id: FontId,
        font_size: Pixels,
    ) -> TextRenderingMode {
        self.platform_text_system
            .recommended_rendering_mode(font_id, font_size)
    }
}

/// The GPUI text layout subsystem.
#[derive(Deref)]
pub struct WindowTextSystem {
    line_layout_cache: LineLayoutCache,
    #[deref]
    text_system: Arc<TextSystem>,
}

impl WindowTextSystem {
    /// Create a new WindowTextSystem with the given TextSystem.
    pub fn new(text_system: Arc<TextSystem>) -> Self {
        Self {
            line_layout_cache: LineLayoutCache::new(text_system.clone()),
            text_system,
        }
    }

    pub(crate) fn layout_index(&self) -> LineLayoutIndex {
        self.line_layout_cache.layout_index()
    }

    pub(crate) fn reuse_layouts(&self, index: Range<LineLayoutIndex>) {
        self.line_layout_cache.reuse_layouts(index)
    }

    pub(crate) fn truncate_layouts(&self, index: LineLayoutIndex) {
        self.line_layout_cache.truncate_layouts(index)
    }

    /// Shape the given line, at the given font_size, for painting to the screen.
    /// Subsets of the line can be styled independently with the `runs` parameter.
    ///
    /// Note that this method can only shape a single line of text. It will panic
    /// if the text contains newlines. If you need to shape multiple lines of text,
    /// use [`Self::shape_text`] instead.
    pub fn shape_line(
        &self,
        text: SharedString,
        font_size: Pixels,
        runs: &[TextRun],
        force_width: Option<Pixels>,
    ) -> ShapedLine {
        debug_assert!(
            text.find('\n').is_none(),
            "text argument should not contain newlines"
        );

        let mut decoration_runs = SmallVec::<[DecorationRun; 32]>::new();
        for run in runs {
            if let Some(last_run) = decoration_runs.last_mut()
                && last_run.color == run.color
                && last_run.underline == run.underline
                && last_run.strikethrough == run.strikethrough
                && last_run.background_color == run.background_color
            {
                last_run.len += run.len as u32;
                continue;
            }
            decoration_runs.push(DecorationRun {
                len: run.len as u32,
                color: run.color,
                background_color: run.background_color,
                underline: run.underline,
                strikethrough: run.strikethrough,
            });
        }

        let layout = self.layout_line(&text, font_size, runs, force_width);

        ShapedLine {
            layout,
            text,
            decoration_runs,
        }
    }

    /// Shape a single line whose runs may use different font sizes, minimum line heights, and
    /// baseline shifts.
    ///
    /// Unlike [`Self::shape_line`], this API is fallible because a platform backend may not support
    /// heterogeneous metrics. Positive baseline shifts move glyphs upward from the shared line
    /// baseline. Runs must cover the complete UTF-8 text without splitting a code point.
    pub fn shape_rich_line(
        &self,
        text: SharedString,
        runs: &[RichTextRun],
        force_width: Option<Pixels>,
    ) -> Result<ShapedLine> {
        if text.contains('\n') {
            return Err(anyhow!("text argument should not contain newlines"));
        }

        let font_runs = self.resolve_rich_font_runs(&text, runs)?;
        let decoration_runs = rich_decoration_runs(&text, runs)?;
        let layout = self
            .line_layout_cache
            .layout_rich_line(&text, &font_runs, force_width)?;

        Ok(ShapedLine {
            layout,
            text,
            decoration_runs,
        })
    }

    /// Layout a heterogeneous single line without creating its paint payload.
    pub fn layout_rich_line(
        &self,
        text: &str,
        runs: &[RichTextRun],
        force_width: Option<Pixels>,
    ) -> Result<Arc<LineLayout>> {
        if text.contains('\n') {
            return Err(anyhow!("text argument should not contain newlines"));
        }
        let font_runs = self.resolve_rich_font_runs(text, runs)?;
        self.line_layout_cache
            .layout_rich_line(text, &font_runs, force_width)
    }

    fn resolve_rich_font_runs(&self, text: &str, runs: &[RichTextRun]) -> Result<Vec<RichFontRun>> {
        let mut offset = 0usize;
        let mut font_runs = Vec::<RichFontRun>::with_capacity(runs.len());

        for run in runs {
            let end = offset
                .checked_add(run.len)
                .ok_or_else(|| anyhow!("rich text run length overflow"))?;
            if end > text.len() || !text.is_char_boundary(end) {
                return Err(anyhow!(
                    "rich text run ending at byte {end} does not follow a UTF-8 boundary"
                ));
            }

            let font_size = run.font_size.as_f32();
            let minimum_line_height = run.minimum_line_height.as_f32();
            let baseline_shift = run.baseline_shift.as_f32();
            if !font_size.is_finite() || font_size <= 0.0 {
                return Err(anyhow!(
                    "rich text run font size must be finite and positive"
                ));
            }
            if !minimum_line_height.is_finite() || minimum_line_height < 0.0 {
                return Err(anyhow!(
                    "rich text run minimum line height must be finite and non-negative"
                ));
            }
            if !baseline_shift.is_finite() {
                return Err(anyhow!("rich text run baseline shift must be finite"));
            }

            if run.len > 0 {
                let font_run = RichFontRun {
                    len: run.len,
                    font_id: self.resolve_font(&run.font),
                    font_size: run.font_size,
                    minimum_line_height: run.minimum_line_height,
                    baseline_shift: run.baseline_shift,
                };
                if let Some(previous) = font_runs.last_mut()
                    && previous.font_id == font_run.font_id
                    && previous.font_size == font_run.font_size
                    && previous.minimum_line_height == font_run.minimum_line_height
                    && previous.baseline_shift == font_run.baseline_shift
                {
                    previous.len += font_run.len;
                } else {
                    font_runs.push(font_run);
                }
            }
            offset = end;
        }

        if offset != text.len() {
            return Err(anyhow!(
                "rich text runs cover {offset} bytes but the text contains {} bytes",
                text.len()
            ));
        }
        if !text.is_empty() && font_runs.is_empty() {
            return Err(anyhow!(
                "non-empty rich text requires at least one non-empty run"
            ));
        }
        if text.is_empty()
            && font_runs.is_empty()
            && let Some(run) = runs.first()
        {
            font_runs.push(RichFontRun {
                len: 0,
                font_id: self.resolve_font(&run.font),
                font_size: run.font_size,
                minimum_line_height: run.minimum_line_height,
                baseline_shift: run.baseline_shift,
            });
        }

        Ok(font_runs)
    }

    /// Shape the given line using a caller-provided content hash as the cache key.
    ///
    /// This enables cache hits without materializing a contiguous `SharedString` for the text.
    /// If the cache misses, `materialize_text` is invoked to produce the `SharedString` for shaping.
    ///
    /// Contract (caller enforced):
    /// - Same `text_hash` implies identical text content (collision risk accepted by caller).
    /// - `text_len` should be the UTF-8 byte length of the text (helps reduce accidental collisions).
    ///
    /// Like [`Self::shape_line`], this must be used only for single-line text (no `\n`).
    pub fn shape_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[TextRun],
        force_width: Option<Pixels>,
        materialize_text: impl FnOnce() -> SharedString,
    ) -> ShapedLine {
        let mut decoration_runs = SmallVec::<[DecorationRun; 32]>::new();
        for run in runs {
            if let Some(last_run) = decoration_runs.last_mut()
                && last_run.color == run.color
                && last_run.underline == run.underline
                && last_run.strikethrough == run.strikethrough
                && last_run.background_color == run.background_color
            {
                last_run.len += run.len as u32;
                continue;
            }
            decoration_runs.push(DecorationRun {
                len: run.len as u32,
                color: run.color,
                background_color: run.background_color,
                underline: run.underline,
                strikethrough: run.strikethrough,
            });
        }

        let mut used_force_width = force_width;
        let layout = self.layout_line_by_hash(
            text_hash,
            text_len,
            font_size,
            runs,
            used_force_width,
            || {
                let text = materialize_text();
                debug_assert!(
                    text.find('\n').is_none(),
                    "text argument should not contain newlines"
                );
                text
            },
        );

        // We only materialize actual text on cache miss; on hit we avoid allocations.
        // Since `ShapedLine` carries a `SharedString`, use an empty placeholder for hits.
        // NOTE: Callers must not rely on `ShapedLine.text` for content when using this API.
        let text: SharedString = SharedString::new_static("");

        ShapedLine {
            layout,
            text,
            decoration_runs,
        }
    }

    /// Shape a multi line string of text, at the given font_size, for painting to the screen.
    /// Subsets of the text can be styled independently with the `runs` parameter.
    /// If `wrap_width` is provided, the line breaks will be adjusted to fit within the given width.
    pub fn shape_text(
        &self,
        text: SharedString,
        font_size: Pixels,
        runs: &[TextRun],
        wrap_width: Option<Pixels>,
        line_clamp: Option<usize>,
    ) -> Result<SmallVec<[WrappedLine; 1]>> {
        let mut runs = runs.iter().filter(|run| run.len > 0).cloned().peekable();
        let mut font_runs = self.font_runs_pool.lock().pop().unwrap_or_default();

        let mut lines = SmallVec::new();
        let mut max_wrap_lines = line_clamp;
        let mut wrapped_lines = 0;

        let mut process_line = |line_text: SharedString, line_start, line_end| {
            font_runs.clear();

            let mut decoration_runs = <Vec<DecorationRun>>::with_capacity(32);
            let mut run_start = line_start;
            while run_start < line_end {
                let Some(run) = runs.peek_mut() else {
                    log::warn!("`TextRun`s do not cover the entire to be shaped text");
                    break;
                };

                let run_len_within_line = cmp::min(line_end - run_start, run.len);

                let decoration_changed = if let Some(last_run) = decoration_runs.last_mut()
                    && last_run.color == run.color
                    && last_run.underline == run.underline
                    && last_run.strikethrough == run.strikethrough
                    && last_run.background_color == run.background_color
                {
                    last_run.len += run_len_within_line as u32;
                    false
                } else {
                    decoration_runs.push(DecorationRun {
                        len: run_len_within_line as u32,
                        color: run.color,
                        background_color: run.background_color,
                        underline: run.underline,
                        strikethrough: run.strikethrough,
                    });
                    true
                };

                let font_id = self.resolve_font(&run.font);
                if let Some(font_run) = font_runs.last_mut()
                    && font_id == font_run.font_id
                    && !decoration_changed
                {
                    font_run.len += run_len_within_line;
                } else {
                    font_runs.push(FontRun {
                        len: run_len_within_line,
                        font_id,
                    });
                }

                // Preserve the remainder of the run for the next line
                run.len -= run_len_within_line;
                if run.len == 0 {
                    runs.next();
                }
                run_start += run_len_within_line;
            }

            let layout = self.line_layout_cache.layout_wrapped_line(
                &line_text,
                font_size,
                &font_runs,
                wrap_width,
                max_wrap_lines.map(|max| max.saturating_sub(wrapped_lines)),
            );
            wrapped_lines += layout.wrap_boundaries.len();

            lines.push(WrappedLine {
                layout,
                decoration_runs,
                text: line_text,
            });

            // Skip `\n` character.
            if let Some(run) = runs.peek_mut() {
                run.len -= 1;
                if run.len == 0 {
                    runs.next();
                }
            }
        };

        let mut split_lines = text.split('\n');

        // Special case single lines to prevent allocating a sharedstring
        if let Some(first_line) = split_lines.next()
            && let Some(second_line) = split_lines.next()
        {
            let mut line_start = 0;
            process_line(
                SharedString::new(first_line),
                line_start,
                line_start + first_line.len(),
            );
            line_start += first_line.len() + '\n'.len_utf8();
            process_line(
                SharedString::new(second_line),
                line_start,
                line_start + second_line.len(),
            );
            for line_text in split_lines {
                line_start += line_text.len() + '\n'.len_utf8();
                process_line(
                    SharedString::new(line_text),
                    line_start,
                    line_start + line_text.len(),
                );
            }
        } else {
            let end = text.len();
            process_line(text, 0, end);
        }

        self.font_runs_pool.lock().push(font_runs);

        Ok(lines)
    }

    pub(crate) fn finish_frame(&self) {
        self.line_layout_cache.finish_frame()
    }

    /// Layout the given line of text, at the given font_size.
    /// Subsets of the line can be styled independently with the `runs` parameter.
    /// Generally, you should prefer to use [`Self::shape_line`] instead, which
    /// can be painted directly.
    pub fn layout_line(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[TextRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout> {
        let mut last_run = None::<&TextRun>;
        let mut font_runs = self.font_runs_pool.lock().pop().unwrap_or_default();
        font_runs.clear();

        for run in runs.iter() {
            let decoration_changed = if let Some(last_run) = last_run
                && last_run.color == run.color
                && last_run.underline == run.underline
                && last_run.strikethrough == run.strikethrough
            // we do not consider differing background color relevant, as it does not affect glyphs
            // && last_run.background_color == run.background_color
            {
                false
            } else {
                last_run = Some(run);
                true
            };

            let font_id = self.resolve_font(&run.font);
            if let Some(font_run) = font_runs.last_mut()
                && font_id == font_run.font_id
                && !decoration_changed
            {
                font_run.len += run.len;
            } else {
                font_runs.push(FontRun {
                    len: run.len,
                    font_id,
                });
            }
        }

        let layout = self.line_layout_cache.layout_line(
            &SharedString::new(text),
            font_size,
            &font_runs,
            force_width,
        );

        self.font_runs_pool.lock().push(font_runs);

        layout
    }

    /// Returns the shaped layout width of for the given character, in the given font and size.
    pub fn layout_width(&self, font_id: FontId, font_size: Pixels, ch: char) -> Pixels {
        let mut buffer = [0; 4];
        let buffer: &_ = ch.encode_utf8(&mut buffer);
        self.line_layout_cache
            .layout_line(
                buffer,
                font_size,
                &[FontRun {
                    len: buffer.len(),
                    font_id,
                }],
                None,
            )
            .width
    }

    /// Returns the shaped layout width of an `em`.
    pub fn em_layout_width(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.layout_width(font_id, font_size, 'm')
    }

    /// Probe the line layout cache using a caller-provided content hash, without allocating.
    ///
    /// Returns `Some(layout)` if the layout is already cached in either the current frame
    /// or the previous frame. Returns `None` if it is not cached.
    ///
    /// Contract (caller enforced):
    /// - Same `text_hash` implies identical text content (collision risk accepted by caller).
    /// - `text_len` should be the UTF-8 byte length of the text (helps reduce accidental collisions).
    pub fn try_layout_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[TextRun],
        force_width: Option<Pixels>,
    ) -> Option<Arc<LineLayout>> {
        let mut last_run = None::<&TextRun>;
        let mut font_runs = self.font_runs_pool.lock().pop().unwrap_or_default();
        font_runs.clear();

        for run in runs.iter() {
            let decoration_changed = if let Some(last_run) = last_run
                && last_run.color == run.color
                && last_run.underline == run.underline
                && last_run.strikethrough == run.strikethrough
            // we do not consider differing background color relevant, as it does not affect glyphs
            // && last_run.background_color == run.background_color
            {
                false
            } else {
                last_run = Some(run);
                true
            };

            let font_id = self.resolve_font(&run.font);
            if let Some(font_run) = font_runs.last_mut()
                && font_id == font_run.font_id
                && !decoration_changed
            {
                font_run.len += run.len;
            } else {
                font_runs.push(FontRun {
                    len: run.len,
                    font_id,
                });
            }
        }

        let layout = self.line_layout_cache.try_layout_line_by_hash(
            text_hash,
            text_len,
            font_size,
            &font_runs,
            force_width,
        );

        self.font_runs_pool.lock().push(font_runs);

        layout
    }

    /// Layout the given line of text using a caller-provided content hash as the cache key.
    ///
    /// This enables cache hits without materializing a contiguous `SharedString` for the text.
    /// If the cache misses, `materialize_text` is invoked to produce the `SharedString` for shaping.
    ///
    /// Contract (caller enforced):
    /// - Same `text_hash` implies identical text content (collision risk accepted by caller).
    /// - `text_len` should be the UTF-8 byte length of the text (helps reduce accidental collisions).
    pub fn layout_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[TextRun],
        force_width: Option<Pixels>,
        materialize_text: impl FnOnce() -> SharedString,
    ) -> Arc<LineLayout> {
        let mut last_run = None::<&TextRun>;
        let mut font_runs = self.font_runs_pool.lock().pop().unwrap_or_default();
        font_runs.clear();

        for run in runs.iter() {
            let decoration_changed = if let Some(last_run) = last_run
                && last_run.color == run.color
                && last_run.underline == run.underline
                && last_run.strikethrough == run.strikethrough
            // we do not consider differing background color relevant, as it does not affect glyphs
            // && last_run.background_color == run.background_color
            {
                false
            } else {
                last_run = Some(run);
                true
            };

            let font_id = self.resolve_font(&run.font);
            if let Some(font_run) = font_runs.last_mut()
                && font_id == font_run.font_id
                && !decoration_changed
            {
                font_run.len += run.len;
            } else {
                font_runs.push(FontRun {
                    len: run.len,
                    font_id,
                });
            }
        }

        let layout = self.line_layout_cache.layout_line_by_hash(
            text_hash,
            text_len,
            font_size,
            &font_runs,
            force_width,
            materialize_text,
        );

        self.font_runs_pool.lock().push(font_runs);

        layout
    }
}

#[derive(Hash, Eq, PartialEq)]
struct FontIdWithSize {
    font_id: FontId,
    font_size: Pixels,
}

/// A handle into the text system, which can be used to compute the wrapped layout of text
pub struct LineWrapperHandle {
    wrapper: Option<LineWrapper>,
    text_system: Arc<TextSystem>,
}

impl Drop for LineWrapperHandle {
    fn drop(&mut self) {
        let mut state = self.text_system.wrapper_pool.lock();
        let wrapper = self.wrapper.take().unwrap();
        state
            .get_mut(&FontIdWithSize {
                font_id: wrapper.font_id,
                font_size: wrapper.font_size,
            })
            .unwrap()
            .push(wrapper);
    }
}

impl Deref for LineWrapperHandle {
    type Target = LineWrapper;

    fn deref(&self) -> &Self::Target {
        self.wrapper.as_ref().unwrap()
    }
}

impl DerefMut for LineWrapperHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.wrapper.as_mut().unwrap()
    }
}

/// The degree of blackness or stroke thickness of a font. This value ranges from 100.0 to 900.0,
/// with 400.0 as normal.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize, Add, Sub, FromStr)]
#[serde(transparent)]
pub struct FontWeight(pub f32);

impl Display for FontWeight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f32> for FontWeight {
    fn from(weight: f32) -> Self {
        FontWeight(weight)
    }
}

impl Default for FontWeight {
    #[inline]
    fn default() -> FontWeight {
        FontWeight::NORMAL
    }
}

impl Hash for FontWeight {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u32(u32::from_be_bytes(self.0.to_be_bytes()));
    }
}

impl Eq for FontWeight {}

impl FontWeight {
    /// Thin weight (100), the thinnest value.
    pub const THIN: FontWeight = FontWeight(100.0);
    /// Extra light weight (200).
    pub const EXTRA_LIGHT: FontWeight = FontWeight(200.0);
    /// Light weight (300).
    pub const LIGHT: FontWeight = FontWeight(300.0);
    /// Normal (400).
    pub const NORMAL: FontWeight = FontWeight(400.0);
    /// Medium weight (500, higher than normal).
    pub const MEDIUM: FontWeight = FontWeight(500.0);
    /// Semibold weight (600).
    pub const SEMIBOLD: FontWeight = FontWeight(600.0);
    /// Bold weight (700).
    pub const BOLD: FontWeight = FontWeight(700.0);
    /// Extra-bold weight (800).
    pub const EXTRA_BOLD: FontWeight = FontWeight(800.0);
    /// Black weight (900), the thickest value.
    pub const BLACK: FontWeight = FontWeight(900.0);

    /// All of the font weights, in order from thinnest to thickest.
    pub const ALL: [FontWeight; 9] = [
        Self::THIN,
        Self::EXTRA_LIGHT,
        Self::LIGHT,
        Self::NORMAL,
        Self::MEDIUM,
        Self::SEMIBOLD,
        Self::BOLD,
        Self::EXTRA_BOLD,
        Self::BLACK,
    ];
}

impl schemars::JsonSchema for FontWeight {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "FontWeight".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        use schemars::json_schema;
        json_schema!({
            "type": "number",
            "minimum": Self::THIN,
            "maximum": Self::BLACK,
            "default": Self::default(),
            "description": "Font weight value between 100 (thin) and 900 (black)"
        })
    }
}

/// Allows italic or oblique faces to be selected.
#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum FontStyle {
    /// A face that is neither italic not obliqued.
    #[default]
    Normal,
    /// A form that is generally cursive in nature.
    Italic,
    /// A typically-sloped version of the regular face.
    Oblique,
}

impl Display for FontStyle {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Debug::fmt(self, f)
    }
}

/// A styled run of text, for use in [`crate::TextLayout`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextRun {
    /// A number of utf8 bytes
    pub len: usize,
    /// The font to use for this run.
    pub font: Font,
    /// The color
    pub color: Hsla,
    /// The background color (if any)
    pub background_color: Option<Hsla>,
    /// The underline style (if any)
    pub underline: Option<UnderlineStyle>,
    /// The strikethrough style (if any)
    pub strikethrough: Option<StrikethroughStyle>,
}

/// A styled text run with heterogeneous shaping metrics.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RichTextRun {
    /// The number of UTF-8 bytes covered by this run.
    pub len: usize,
    /// The font requested for this run.
    pub font: Font,
    /// The font size used to shape and rasterize this run.
    pub font_size: Pixels,
    /// The minimum line height contributed by this run.
    pub minimum_line_height: Pixels,
    /// The offset from the shared baseline. Positive values move glyphs upward.
    pub baseline_shift: Pixels,
    /// The foreground color.
    pub color: Hsla,
    /// The background color, if any.
    pub background_color: Option<Hsla>,
    /// The underline style, if any.
    pub underline: Option<UnderlineStyle>,
    /// The strikethrough style, if any.
    pub strikethrough: Option<StrikethroughStyle>,
}

impl RichTextRun {
    /// Promote a legacy run into a run with explicit shaping metrics.
    pub fn from_text_run(run: TextRun, font_size: Pixels, minimum_line_height: Pixels) -> Self {
        Self {
            len: run.len,
            font: run.font,
            font_size,
            minimum_line_height,
            baseline_shift: Pixels::ZERO,
            color: run.color,
            background_color: run.background_color,
            underline: run.underline,
            strikethrough: run.strikethrough,
        }
    }
}

fn rich_decoration_runs(text: &str, runs: &[RichTextRun]) -> Result<SmallVec<[DecorationRun; 32]>> {
    let mut result = SmallVec::<[DecorationRun; 32]>::new();
    let mut covered = 0usize;
    for run in runs {
        covered = covered
            .checked_add(run.len)
            .ok_or_else(|| anyhow!("rich text decoration length overflow"))?;
        let len = u32::try_from(run.len)
            .map_err(|_| anyhow!("rich text decoration run exceeds u32::MAX bytes"))?;
        if len == 0 {
            continue;
        }
        if let Some(previous) = result.last_mut()
            && previous.color == run.color
            && previous.underline == run.underline
            && previous.strikethrough == run.strikethrough
            && previous.background_color == run.background_color
        {
            previous.len = previous
                .len
                .checked_add(len)
                .ok_or_else(|| anyhow!("rich text decoration length exceeds u32::MAX bytes"))?;
        } else {
            result.push(DecorationRun {
                len,
                color: run.color,
                background_color: run.background_color,
                underline: run.underline,
                strikethrough: run.strikethrough,
            });
        }
    }
    if covered != text.len() {
        return Err(anyhow!(
            "rich text decorations cover {covered} bytes but the text contains {} bytes",
            text.len()
        ));
    }
    Ok(result)
}

#[cfg(all(target_os = "macos", test))]
impl TextRun {
    fn with_len(&self, len: usize) -> Self {
        let mut this = self.clone();
        this.len = len;
        this
    }
}

/// An identifier for a specific glyph, as returned by [`WindowTextSystem::layout_line`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(C)]
pub struct GlyphId(pub u32);

/// Parameters for rendering a glyph, used as cache keys for raster bounds.
///
/// This struct identifies a specific glyph rendering configuration including
/// font, size, subpixel positioning, and scale factor. It's used to look up
/// cached raster bounds and sprite atlas entries.
#[derive(Clone, Debug, PartialEq)]
#[expect(missing_docs)]
pub struct RenderGlyphParams {
    pub font_id: FontId,
    pub glyph_id: GlyphId,
    pub font_size: Pixels,
    pub subpixel_variant: Point<u8>,
    pub scale_factor: f32,
    pub is_emoji: bool,
    pub subpixel_rendering: bool,
    pub dilation: u8,
}

impl Eq for RenderGlyphParams {}

impl Hash for RenderGlyphParams {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.font_id.0.hash(state);
        self.glyph_id.0.hash(state);
        self.font_size.0.to_bits().hash(state);
        self.subpixel_variant.hash(state);
        self.scale_factor.to_bits().hash(state);
        self.is_emoji.hash(state);
        self.subpixel_rendering.hash(state);
        self.dilation.hash(state);
    }
}

/// The configuration details for identifying a specific font.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Font {
    /// The font family name.
    ///
    /// The special name ".SystemUIFont" is used to identify the system UI font, which varies based on platform.
    pub family: SharedString,

    /// The font features to use.
    pub features: FontFeatures,

    /// The fallbacks fonts to use.
    pub fallbacks: Option<FontFallbacks>,

    /// The font weight.
    pub weight: FontWeight,

    /// The font style.
    pub style: FontStyle,
}

impl Default for Font {
    fn default() -> Self {
        font(".SystemUIFont")
    }
}

/// Get a [`Font`] for a given name.
pub fn font(family: impl Into<SharedString>) -> Font {
    Font {
        family: family.into(),
        features: FontFeatures::default(),
        weight: FontWeight::default(),
        style: FontStyle::default(),
        fallbacks: None,
    }
}

impl Font {
    /// Set this Font to be bold
    pub fn bold(mut self) -> Self {
        self.weight = FontWeight::BOLD;
        self
    }

    /// Set this Font to be italic
    pub fn italic(mut self) -> Self {
        self.style = FontStyle::Italic;
        self
    }
}

/// A struct for storing font metrics.
/// It is used to define the measurements of a typeface.
#[derive(Clone, Copy, Debug)]
pub struct FontMetrics {
    /// The number of font units that make up the "em square",
    /// a scalable grid for determining the size of a typeface.
    pub units_per_em: u32,

    /// The vertical distance from the baseline of the font to the top of the glyph covers.
    pub ascent: f32,

    /// The vertical distance from the baseline of the font to the bottom of the glyph covers.
    pub descent: f32,

    /// The recommended additional space to add between lines of type.
    pub line_gap: f32,

    /// The suggested position of the underline.
    pub underline_position: f32,

    /// The suggested thickness of the underline.
    pub underline_thickness: f32,

    /// The height of a capital letter measured from the baseline of the font.
    pub cap_height: f32,

    /// The height of a lowercase x.
    pub x_height: f32,

    /// The outer limits of the area that the font covers.
    /// Corresponds to the xMin / xMax / yMin / yMax values in the OpenType `head` table
    pub bounding_box: Bounds<f32>,
}

impl FontMetrics {
    /// Returns the vertical distance from the baseline of the font to the top of the glyph covers in pixels.
    pub fn ascent(&self, font_size: Pixels) -> Pixels {
        Pixels((self.ascent / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the vertical distance from the baseline of the font to the bottom of the glyph covers in pixels.
    pub fn descent(&self, font_size: Pixels) -> Pixels {
        Pixels((self.descent / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the recommended additional space to add between lines of type in pixels.
    pub fn line_gap(&self, font_size: Pixels) -> Pixels {
        Pixels((self.line_gap / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the suggested position of the underline in pixels.
    pub fn underline_position(&self, font_size: Pixels) -> Pixels {
        Pixels((self.underline_position / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the suggested thickness of the underline in pixels.
    pub fn underline_thickness(&self, font_size: Pixels) -> Pixels {
        Pixels((self.underline_thickness / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the height of a capital letter measured from the baseline of the font in pixels.
    pub fn cap_height(&self, font_size: Pixels) -> Pixels {
        Pixels((self.cap_height / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the height of a lowercase x in pixels.
    pub fn x_height(&self, font_size: Pixels) -> Pixels {
        Pixels((self.x_height / self.units_per_em as f32) * font_size.0)
    }

    /// Returns the outer limits of the area that the font covers in pixels.
    pub fn bounding_box(&self, font_size: Pixels) -> Bounds<Pixels> {
        (self.bounding_box / self.units_per_em as f32 * font_size.0).map(px)
    }
}

/// Maps well-known virtual font names to their concrete equivalents.
#[allow(unused)]
pub fn font_name_with_fallbacks<'a>(name: &'a str, system: &'a str) -> &'a str {
    // Note: the "Zed Plex" fonts were deprecated as we are not allowed to use "Plex"
    // in a derived font name. They are essentially indistinguishable from IBM Plex/Lilex,
    // and so retained here for backward compatibility.
    match name {
        ".SystemUIFont" => system,
        ".ZedSans" | "Zed Plex Sans" => "IBM Plex Sans",
        ".ZedMono" | "Zed Plex Mono" => "Lilex",
        _ => name,
    }
}

/// Like [`font_name_with_fallbacks`] but accepts and returns [`SharedString`] references.
#[allow(unused)]
pub fn font_name_with_fallbacks_shared<'a>(
    name: &'a SharedString,
    system: &'a SharedString,
) -> &'a SharedString {
    // Note: the "Zed Plex" fonts were deprecated as we are not allowed to use "Plex"
    // in a derived font name. They are essentially indistinguishable from IBM Plex/Lilex,
    // and so retained here for backward compatibility.
    match name.as_str() {
        ".SystemUIFont" => system,
        ".ZedSans" | "Zed Plex Sans" => const { &SharedString::new_static("IBM Plex Sans") },
        ".ZedMono" | "Zed Plex Mono" => const { &SharedString::new_static("Lilex") },
        _ => name,
    }
}

#[cfg(test)]
mod rich_line_tests {
    use super::*;
    use crate::{NoopTextSystem, black, red};

    fn run(len: usize, color: Hsla) -> RichTextRun {
        RichTextRun {
            len,
            font: font("test"),
            font_size: px(18.0),
            minimum_line_height: px(30.0),
            color,
            ..Default::default()
        }
    }

    fn text_system() -> WindowTextSystem {
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))))
    }

    #[test]
    fn paint_only_changes_reuse_rich_geometry() -> Result<()> {
        let text_system = text_system();
        let first = text_system.shape_rich_line("abc".into(), &[run(3, black())], None)?;
        text_system.finish_frame();
        let second = text_system.shape_rich_line("abc".into(), &[run(3, red())], None)?;

        assert!(Arc::ptr_eq(&first.layout, &second.layout));
        assert_eq!(first.layout.minimum_line_height, px(30.0));
        assert_ne!(
            first.paint_payload().runs()[0].color,
            second.paint_payload().runs()[0].color
        );
        Ok(())
    }

    #[test]
    fn rich_runs_reject_invalid_utf8_coverage_and_metrics() {
        let text_system = text_system();

        assert!(
            text_system
                .shape_rich_line("é".into(), &[run(1, black())], None)
                .is_err()
        );
        assert!(
            text_system
                .shape_rich_line("abc".into(), &[run(2, black())], None)
                .is_err()
        );
        let mut invalid = run(3, black());
        invalid.font_size = px(f32::NAN);
        assert!(
            text_system
                .shape_rich_line("abc".into(), &[invalid], None)
                .is_err()
        );
    }

    #[test]
    fn empty_rich_line_preserves_requested_metrics() -> Result<()> {
        let text_system = text_system();
        let shaped = text_system.shape_rich_line("".into(), &[run(0, black())], None)?;

        assert_eq!(shaped.layout.font_size, px(18.0));
        assert!(shaped.layout.ascent > Pixels::ZERO);
        assert!(shaped.layout.descent > Pixels::ZERO);
        assert_eq!(shaped.layout.minimum_line_height, px(30.0));
        assert!(shaped.layout.runs.is_empty());
        assert_eq!(
            shaped.layout.caret_stops(),
            &[CaretStop {
                index: 0,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: Pixels::ZERO,
            }]
        );
        Ok(())
    }

    #[test]
    fn empty_rich_line_uses_glyph_metrics_as_minimum_height_floor() -> Result<()> {
        let text_system = text_system();
        let mut empty_run = run(0, black());
        empty_run.minimum_line_height = Pixels::ZERO;
        let shaped = text_system.shape_rich_line("".into(), &[empty_run], None)?;

        assert!(shaped.layout.ascent > Pixels::ZERO);
        assert!(shaped.layout.descent > Pixels::ZERO);
        assert_eq!(
            shaped.layout.minimum_line_height,
            shaped.layout.ascent + shaped.layout.descent
        );
        Ok(())
    }

    #[test]
    fn legacy_caret_stops_are_lazy_and_use_single_endpoint_affinities() {
        let text_system = text_system();
        let layout = text_system.layout_line(
            "abc",
            px(18.0),
            &[TextRun {
                len: 3,
                font: font("test"),
                ..Default::default()
            }],
            None,
        );

        assert!(layout.caret_stops.is_empty());
        assert!(layout.generated_caret_stops.get().is_none());
        assert_eq!(
            layout.caret_stops_for_index(0),
            &[CaretStop {
                index: 0,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: Pixels::ZERO,
            }]
        );
        assert!(layout.generated_caret_stops.get().is_some());
        assert_eq!(layout.caret_stops_for_index(1).len(), 2);
        assert_eq!(layout.caret_stops_for_index(3).len(), 1);
        assert_eq!(
            layout.caret_stops_for_index(3)[0].affinity,
            CaretAffinity::Upstream
        );
    }

    #[test]
    fn baseline_shift_changes_rich_ascent_and_descent() -> Result<()> {
        let text_system = text_system();
        let neutral = text_system.layout_rich_line("a", &[run(1, black())], None)?;
        let mut upward_run = run(1, black());
        upward_run.baseline_shift = px(3.0);
        let upward = text_system.layout_rich_line("a", &[upward_run], None)?;
        let mut downward_run = run(1, black());
        downward_run.baseline_shift = px(-3.0);
        let downward = text_system.layout_rich_line("a", &[downward_run], None)?;

        assert!(upward.ascent > neutral.ascent);
        assert!(upward.descent < neutral.descent);
        assert!(downward.ascent < neutral.ascent);
        assert!(downward.descent > neutral.descent);
        Ok(())
    }

    #[test]
    fn noop_legacy_descent_keeps_upstream_sign_while_rich_is_positive() -> Result<()> {
        let backend = NoopTextSystem::new();
        let legacy = backend.layout_line("", px(18.0), &[]);
        let rich = text_system().layout_rich_line("", &[run(0, black())], None)?;

        assert!(legacy.descent < Pixels::ZERO);
        assert!(rich.descent > Pixels::ZERO);
        Ok(())
    }
}
