use crate::{
    FontId, GlyphId, Pixels, Point, ResolvedFontFace, Result, SharedString, Size, TextSystem,
    point, px,
};
use collections::FxHashMap;
use parking_lot::{Mutex, RwLock, RwLockUpgradableReadGuard};
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    ops::Range,
    sync::{Arc, OnceLock},
};

use super::LineWrapper;

/// Which logical side of a byte boundary a caret is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CaretAffinity {
    /// The caret is attached to the preceding logical cluster.
    Upstream,
    /// The caret is attached to the following logical cluster.
    Downstream,
}

/// The visual direction of the shaped run adjacent to a caret stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TextDirection {
    /// Left-to-right visual order.
    LeftToRight,
    /// Right-to-left visual order.
    RightToLeft,
}

/// One visual caret location for a UTF-8 byte boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretStop {
    /// The UTF-8 byte boundary in the original text.
    pub index: usize,
    /// The logical side of the boundary represented by this stop.
    pub affinity: CaretAffinity,
    /// The visual direction of the adjacent shaped run.
    pub direction: TextDirection,
    /// The x coordinate in line-local geometry.
    pub x: Pixels,
}

/// A laid out and styled line of text
#[derive(Default, Debug)]
pub struct LineLayout {
    /// The font size for this line
    pub font_size: Pixels,
    /// The width of the line
    pub width: Pixels,
    /// The ascent of the line
    pub ascent: Pixels,
    /// The descent of the line
    pub descent: Pixels,
    /// The minimum line height requested by heterogeneous runs.
    pub minimum_line_height: Pixels,
    /// The shaped runs that make up this line
    pub runs: Vec<ShapedRun>,
    /// Visual caret stops sorted by UTF-8 byte index and visual x coordinate.
    pub caret_stops: Vec<CaretStop>,
    /// Lazily generated compatibility stops for the legacy homogeneous layout path.
    ///
    /// Rich backends populate `caret_stops` with platform shaping data. Legacy layouts leave that
    /// vector empty so labels and paint-only text do not pay caret construction costs; the public
    /// query methods initialize this fallback on first use. The `Arc<Vec<_>>` keeps this
    /// doc-hidden lazy slot smaller than an inline second `Vec` payload.
    #[doc(hidden)]
    pub generated_caret_stops: OnceLock<Arc<Vec<CaretStop>>>,
    /// The length of the line in utf-8 bytes
    pub len: usize,
}

/// A run of text that has been shaped .
#[derive(Debug, Clone)]
pub struct ShapedRun {
    /// The font id for this run
    pub font_id: FontId,
    /// The font size used to shape and rasterize this run.
    pub font_size: Pixels,
    /// The offset from the common baseline. Positive values move glyphs upward.
    pub baseline_shift: Pixels,
    /// The concrete physical face selected by the platform backend.
    pub resolved_face: Option<ResolvedFontFace>,
    /// The glyphs that make up this run
    pub glyphs: Vec<ShapedGlyph>,
}

/// A single glyph, ready to paint.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    /// The ID for this glyph, as determined by the text system.
    pub id: GlyphId,

    /// The position of this glyph in its containing line.
    pub position: Point<Pixels>,

    /// The index of this glyph in the original text.
    pub index: usize,

    /// Whether this glyph is an emoji
    pub is_emoji: bool,
}

impl LineLayout {
    /// All visual caret stops in this layout.
    pub fn caret_stops(&self) -> &[CaretStop] {
        if self.caret_stops.is_empty() {
            self.generated_caret_stops
                .get_or_init(|| Arc::new(self.generate_legacy_caret_stops()))
                .as_slice()
        } else {
            &self.caret_stops
        }
    }

    /// The visual caret stops for one UTF-8 byte boundary.
    pub fn caret_stops_for_index(&self, index: usize) -> &[CaretStop] {
        let stops = self.caret_stops();
        let start = stops.partition_point(|stop| stop.index < index);
        let end = stops.partition_point(|stop| stop.index <= index);
        &stops[start..end]
    }

    /// Find the visual caret stop closest to a line-local x coordinate.
    pub fn closest_caret_for_x(&self, x: Pixels) -> Option<CaretStop> {
        self.caret_stops().iter().copied().min_by(|left, right| {
            (left.x - x)
                .abs()
                .as_f32()
                .total_cmp(&(right.x - x).abs().as_f32())
                .then_with(|| left.index.cmp(&right.index))
                .then_with(|| left.affinity.cmp(&right.affinity))
                .then_with(|| left.direction.cmp(&right.direction))
        })
    }

    /// Resolve an affinity- and direction-qualified caret to its line-local x coordinate.
    pub fn x_for_caret(
        &self,
        index: usize,
        affinity: CaretAffinity,
        direction: TextDirection,
    ) -> Option<Pixels> {
        self.caret_stops_for_index(index)
            .iter()
            .find(|stop| stop.affinity == affinity && stop.direction == direction)
            .map(|stop| stop.x)
    }

    pub(crate) fn normalize_caret_stops(&mut self) {
        self.caret_stops.sort_by(|left, right| {
            left.index
                .cmp(&right.index)
                .then_with(|| left.x.as_f32().total_cmp(&right.x.as_f32()))
                .then_with(|| left.affinity.cmp(&right.affinity))
                .then_with(|| left.direction.cmp(&right.direction))
        });
        self.caret_stops.dedup_by(|left, right| {
            left.index == right.index
                && left.x.as_f32().to_bits() == right.x.as_f32().to_bits()
                && left.affinity == right.affinity
                && left.direction == right.direction
        });
    }

    fn generate_legacy_caret_stops(&self) -> Vec<CaretStop> {
        let mut boundaries = vec![(0usize, Pixels::ZERO), (self.len, self.width)];
        for run in &self.runs {
            boundaries.extend(
                run.glyphs
                    .iter()
                    .map(|glyph| (glyph.index, glyph.position.x)),
            );
        }
        boundaries.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.as_f32().total_cmp(&right.1.as_f32()))
        });
        boundaries.dedup_by(|left, right| left.0 == right.0);

        let mut stops = Vec::with_capacity(boundaries.len() * 2);
        for (index, x) in boundaries {
            if index > 0 {
                stops.push(CaretStop {
                    index,
                    affinity: CaretAffinity::Upstream,
                    direction: TextDirection::LeftToRight,
                    x,
                });
            }
            if index < self.len || self.len == 0 {
                stops.push(CaretStop {
                    index,
                    affinity: CaretAffinity::Downstream,
                    direction: TextDirection::LeftToRight,
                    x,
                });
            }
        }
        stops
    }

    /// The index for the character at the given x coordinate
    pub fn index_for_x(&self, x: Pixels) -> Option<usize> {
        if x >= self.width {
            None
        } else {
            for run in self.runs.iter().rev() {
                for glyph in run.glyphs.iter().rev() {
                    if glyph.position.x <= x {
                        return Some(glyph.index);
                    }
                }
            }
            Some(0)
        }
    }

    /// closest_index_for_x returns the character boundary closest to the given x coordinate
    /// (e.g. to handle aligning up/down arrow keys)
    pub fn closest_index_for_x(&self, x: Pixels) -> usize {
        let mut prev_index = 0;
        let mut prev_x = px(0.);

        for run in self.runs.iter() {
            for glyph in run.glyphs.iter() {
                if glyph.position.x >= x {
                    if glyph.position.x - x < x - prev_x {
                        return glyph.index;
                    } else {
                        return prev_index;
                    }
                }
                prev_index = glyph.index;
                prev_x = glyph.position.x;
            }
        }

        if self.len == 1 {
            if x > self.width / 2. {
                return 1;
            } else {
                return 0;
            }
        }

        self.len
    }

    /// The x position of the character at the given index
    pub fn x_for_index(&self, index: usize) -> Pixels {
        if !self.caret_stops.is_empty() {
            let stops = &self.caret_stops;
            let start = stops.partition_point(|stop| stop.index < index);
            let end = stops.partition_point(|stop| stop.index <= index);
            if let Some(stop) = stops[start..end]
                .iter()
                .find(|stop| stop.affinity == CaretAffinity::Downstream)
                .or_else(|| stops[start..end].first())
            {
                return stop.x;
            }
        }

        self.runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .filter(|glyph| glyph.index >= index)
            .min_by_key(|glyph| glyph.index)
            .map_or(self.width, |glyph| glyph.position.x)
    }

    /// The corresponding Font at the given index
    pub fn font_id_for_index(&self, index: usize) -> Option<FontId> {
        self.runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)))
            .filter(|(_, glyph)| glyph.index >= index)
            .min_by_key(|(_, glyph)| glyph.index)
            .map(|(font_id, _)| font_id)
    }

    fn compute_wrap_boundaries(
        &self,
        text: &str,
        wrap_width: Pixels,
        max_lines: Option<usize>,
    ) -> SmallVec<[WrapBoundary; 1]> {
        let mut boundaries = SmallVec::new();
        let mut first_non_whitespace_ix = None;
        let mut last_candidate_ix = None;
        let mut last_candidate_x = px(0.);
        let mut last_boundary = WrapBoundary {
            run_ix: 0,
            glyph_ix: 0,
        };
        let mut last_boundary_x = px(0.);
        let mut prev_ch = '\0';
        let mut glyphs = self
            .runs
            .iter()
            .enumerate()
            .flat_map(move |(run_ix, run)| {
                run.glyphs.iter().enumerate().map(move |(glyph_ix, glyph)| {
                    let character = text[glyph.index..].chars().next().unwrap();
                    (
                        WrapBoundary { run_ix, glyph_ix },
                        character,
                        glyph.position.x,
                    )
                })
            })
            .peekable();

        while let Some((boundary, ch, x)) = glyphs.next() {
            if ch == '\n' {
                continue;
            }

            // Here is very similar to `LineWrapper::wrap_line` to determine text wrapping,
            // but there are some differences, so we have to duplicate the code here.
            if LineWrapper::is_word_char(ch) {
                if prev_ch == ' ' && ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            } else {
                if ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            }

            if ch != ' ' && first_non_whitespace_ix.is_none() {
                first_non_whitespace_ix = Some(boundary);
            }

            let next_x = glyphs.peek().map_or(self.width, |(_, _, x)| *x);
            let width = next_x - last_boundary_x;

            if width > wrap_width && boundary > last_boundary {
                // When used line_clamp, we should limit the number of lines.
                if let Some(max_lines) = max_lines
                    && boundaries.len() >= max_lines.saturating_sub(1)
                {
                    break;
                }

                if let Some(last_candidate_ix) = last_candidate_ix.take() {
                    last_boundary = last_candidate_ix;
                    last_boundary_x = last_candidate_x;
                } else {
                    last_boundary = boundary;
                    last_boundary_x = x;
                }
                boundaries.push(last_boundary);
            }
            prev_ch = ch;
        }

        boundaries
    }
}

/// A line of text that has been wrapped to fit a given width
#[derive(Default, Debug)]
pub struct WrappedLineLayout {
    /// The line layout, pre-wrapping.
    pub unwrapped_layout: Arc<LineLayout>,

    /// The boundaries at which the line was wrapped
    pub wrap_boundaries: SmallVec<[WrapBoundary; 1]>,

    /// The width of the line, if it was wrapped
    pub wrap_width: Option<Pixels>,
}

/// A boundary at which a line was wrapped
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WrapBoundary {
    /// The index in the run just before the line was wrapped
    pub run_ix: usize,
    /// The index of the glyph just before the line was wrapped
    pub glyph_ix: usize,
}

impl WrappedLineLayout {
    /// The length of the underlying text, in utf8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.unwrapped_layout.len
    }

    /// The width of this line, in pixels, whether or not it was wrapped.
    pub fn width(&self) -> Pixels {
        self.wrap_width
            .unwrap_or(Pixels::MAX)
            .min(self.unwrapped_layout.width)
    }

    /// The size of the whole wrapped text, for the given line_height.
    /// can span multiple lines if there are multiple wrap boundaries.
    pub fn size(&self, line_height: Pixels) -> Size<Pixels> {
        Size {
            width: self.width(),
            height: line_height * (self.wrap_boundaries.len() + 1),
        }
    }

    /// The ascent of a line in this layout
    pub fn ascent(&self) -> Pixels {
        self.unwrapped_layout.ascent
    }

    /// The descent of a line in this layout
    pub fn descent(&self) -> Pixels {
        self.unwrapped_layout.descent
    }

    /// The wrap boundaries in this layout
    pub fn wrap_boundaries(&self) -> &[WrapBoundary] {
        &self.wrap_boundaries
    }

    /// The font size of this layout
    pub fn font_size(&self) -> Pixels {
        self.unwrapped_layout.font_size
    }

    /// The runs in this layout, sans wrapping
    pub fn runs(&self) -> &[ShapedRun] {
        &self.unwrapped_layout.runs
    }

    /// The index corresponding to a given position in this layout for the given line height.
    ///
    /// See also [`Self::closest_index_for_position`].
    pub fn index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, false)
    }

    /// The closest index to a given position in this layout for the given line height.
    ///
    /// Closest means the character boundary closest to the given position.
    ///
    /// See also [`LineLayout::closest_index_for_x`].
    pub fn closest_index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, true)
    }

    fn _index_for_position(
        &self,
        mut position: Point<Pixels>,
        line_height: Pixels,
        closest: bool,
    ) -> Result<usize, usize> {
        let wrapped_line_ix = (position.y / line_height) as usize;

        let wrapped_line_start_index;
        let wrapped_line_start_x;
        if wrapped_line_ix > 0 {
            let Some(line_start_boundary) = self.wrap_boundaries.get(wrapped_line_ix - 1) else {
                return Err(0);
            };
            let run = &self.unwrapped_layout.runs[line_start_boundary.run_ix];
            let glyph = &run.glyphs[line_start_boundary.glyph_ix];
            wrapped_line_start_index = glyph.index;
            wrapped_line_start_x = glyph.position.x;
        } else {
            wrapped_line_start_index = 0;
            wrapped_line_start_x = Pixels::ZERO;
        };

        let wrapped_line_end_index;
        let wrapped_line_end_x;
        if wrapped_line_ix < self.wrap_boundaries.len() {
            let next_wrap_boundary_ix = wrapped_line_ix;
            let next_wrap_boundary = self.wrap_boundaries[next_wrap_boundary_ix];
            let run = &self.unwrapped_layout.runs[next_wrap_boundary.run_ix];
            let glyph = &run.glyphs[next_wrap_boundary.glyph_ix];
            wrapped_line_end_index = glyph.index;
            wrapped_line_end_x = glyph.position.x;
        } else {
            wrapped_line_end_index = self.unwrapped_layout.len;
            wrapped_line_end_x = self.unwrapped_layout.width;
        };

        let mut position_in_unwrapped_line = position;
        position_in_unwrapped_line.x += wrapped_line_start_x;
        if position_in_unwrapped_line.x < wrapped_line_start_x {
            Err(wrapped_line_start_index)
        } else if position_in_unwrapped_line.x >= wrapped_line_end_x {
            Err(wrapped_line_end_index)
        } else {
            if closest {
                Ok(self
                    .unwrapped_layout
                    .closest_index_for_x(position_in_unwrapped_line.x))
            } else {
                Ok(self
                    .unwrapped_layout
                    .index_for_x(position_in_unwrapped_line.x)
                    .unwrap())
            }
        }
    }

    /// Returns the pixel position for the given byte index.
    pub fn position_for_index(&self, index: usize, line_height: Pixels) -> Option<Point<Pixels>> {
        let mut line_start_ix = 0;
        let mut line_end_indices = self
            .wrap_boundaries
            .iter()
            .map(|wrap_boundary| {
                let run = &self.unwrapped_layout.runs[wrap_boundary.run_ix];
                let glyph = &run.glyphs[wrap_boundary.glyph_ix];
                glyph.index
            })
            .chain([self.len()])
            .enumerate();
        for (ix, line_end_ix) in line_end_indices {
            let line_y = ix as f32 * line_height;
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_start_ix = line_end_ix;
                continue;
            } else {
                let line_start_x = self.unwrapped_layout.x_for_index(line_start_ix);
                let x = self.unwrapped_layout.x_for_index(index) - line_start_x;
                return Some(point(x, line_y));
            }
        }

        None
    }
}

pub(crate) struct LineLayoutCache {
    previous_frame: Mutex<FrameCache>,
    current_frame: RwLock<FrameCache>,
    text_system: Arc<TextSystem>,
}

#[derive(Default)]
struct FrameCache {
    lines: FxHashMap<Arc<CacheKey>, Arc<LineLayout>>,
    wrapped_lines: FxHashMap<Arc<CacheKey>, Arc<WrappedLineLayout>>,
    used_lines: Vec<Arc<CacheKey>>,
    used_wrapped_lines: Vec<Arc<CacheKey>>,

    // Content-addressable caches keyed by caller-provided text hash + layout params.
    // These allow cache hits without materializing a contiguous `SharedString`.
    //
    // IMPORTANT: To support allocation-free lookups, we store these maps using a key type
    // (`HashedCacheKeyRef`) that can be computed without building a contiguous `&str`/`SharedString`.
    // On miss, we allocate once and store under an owned `HashedCacheKey`.
    lines_by_hash: FxHashMap<Arc<HashedCacheKey>, Arc<LineLayout>>,
    wrapped_lines_by_hash: FxHashMap<Arc<HashedCacheKey>, Arc<WrappedLineLayout>>,
    used_lines_by_hash: Vec<Arc<HashedCacheKey>>,
    used_wrapped_lines_by_hash: Vec<Arc<HashedCacheKey>>,
    rich_lines: FxHashMap<Arc<RichCacheKey>, Arc<LineLayout>>,
    used_rich_lines: Vec<Arc<RichCacheKey>>,
}

#[derive(Clone, Default)]
pub(crate) struct LineLayoutIndex {
    lines_index: usize,
    wrapped_lines_index: usize,
    lines_by_hash_index: usize,
    wrapped_lines_by_hash_index: usize,
    rich_lines_index: usize,
}

impl LineLayoutCache {
    pub fn new(text_system: Arc<TextSystem>) -> Self {
        Self {
            previous_frame: Mutex::default(),
            current_frame: RwLock::default(),
            text_system,
        }
    }

    pub fn layout_index(&self) -> LineLayoutIndex {
        let frame = self.current_frame.read();
        LineLayoutIndex {
            lines_index: frame.used_lines.len(),
            wrapped_lines_index: frame.used_wrapped_lines.len(),
            lines_by_hash_index: frame.used_lines_by_hash.len(),
            wrapped_lines_by_hash_index: frame.used_wrapped_lines_by_hash.len(),
            rich_lines_index: frame.used_rich_lines.len(),
        }
    }

    pub fn reuse_layouts(&self, range: Range<LineLayoutIndex>) {
        let mut previous_frame = &mut *self.previous_frame.lock();
        let mut current_frame = &mut *self.current_frame.write();

        for key in &previous_frame.used_lines[range.start.lines_index..range.end.lines_index] {
            if let Some((key, line)) = previous_frame.lines.remove_entry(key) {
                current_frame.lines.insert(key, line);
            }
            current_frame.used_lines.push(key.clone());
        }

        for key in &previous_frame.used_wrapped_lines
            [range.start.wrapped_lines_index..range.end.wrapped_lines_index]
        {
            if let Some((key, line)) = previous_frame.wrapped_lines.remove_entry(key) {
                current_frame.wrapped_lines.insert(key, line);
            }
            current_frame.used_wrapped_lines.push(key.clone());
        }

        for key in &previous_frame.used_lines_by_hash
            [range.start.lines_by_hash_index..range.end.lines_by_hash_index]
        {
            if let Some((key, line)) = previous_frame.lines_by_hash.remove_entry(key) {
                current_frame.lines_by_hash.insert(key, line);
            }
            current_frame.used_lines_by_hash.push(key.clone());
        }

        for key in &previous_frame.used_wrapped_lines_by_hash
            [range.start.wrapped_lines_by_hash_index..range.end.wrapped_lines_by_hash_index]
        {
            if let Some((key, line)) = previous_frame.wrapped_lines_by_hash.remove_entry(key) {
                current_frame.wrapped_lines_by_hash.insert(key, line);
            }
            current_frame.used_wrapped_lines_by_hash.push(key.clone());
        }

        for key in &previous_frame.used_rich_lines
            [range.start.rich_lines_index..range.end.rich_lines_index]
        {
            if let Some((key, line)) = previous_frame.rich_lines.remove_entry(key) {
                current_frame.rich_lines.insert(key, line);
            }
            current_frame.used_rich_lines.push(key.clone());
        }
    }

    pub fn truncate_layouts(&self, index: LineLayoutIndex) {
        let mut current_frame = &mut *self.current_frame.write();
        current_frame.used_lines.truncate(index.lines_index);
        current_frame
            .used_wrapped_lines
            .truncate(index.wrapped_lines_index);
        current_frame
            .used_lines_by_hash
            .truncate(index.lines_by_hash_index);
        current_frame
            .used_wrapped_lines_by_hash
            .truncate(index.wrapped_lines_by_hash_index);
        current_frame
            .used_rich_lines
            .truncate(index.rich_lines_index);
    }

    pub fn finish_frame(&self) {
        let mut prev_frame = self.previous_frame.lock();
        let mut curr_frame = self.current_frame.write();
        std::mem::swap(&mut *prev_frame, &mut *curr_frame);
        curr_frame.lines.clear();
        curr_frame.wrapped_lines.clear();
        curr_frame.used_lines.clear();
        curr_frame.used_wrapped_lines.clear();

        curr_frame.lines_by_hash.clear();
        curr_frame.wrapped_lines_by_hash.clear();
        curr_frame.used_lines_by_hash.clear();
        curr_frame.used_wrapped_lines_by_hash.clear();
        curr_frame.rich_lines.clear();
        curr_frame.used_rich_lines.clear();
    }

    pub fn layout_wrapped_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> Arc<WrappedLineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width,
            force_width: None,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.upgradable_read();
        if let Some(layout) = current_frame.wrapped_lines.get(key) {
            return layout.clone();
        }

        let previous_frame_entry = self.previous_frame.lock().wrapped_lines.remove_entry(key);
        if let Some((key, layout)) = previous_frame_entry {
            let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key);
            layout
        } else {
            drop(current_frame);
            let text = SharedString::from(text);
            let unwrapped_layout = self.layout_line::<&SharedString>(&text, font_size, runs, None);
            let wrap_boundaries = if let Some(wrap_width) = wrap_width {
                unwrapped_layout.compute_wrap_boundaries(text.as_ref(), wrap_width, max_lines)
            } else {
                SmallVec::new()
            };
            let layout = Arc::new(WrappedLineLayout {
                unwrapped_layout,
                wrap_boundaries,
                wrap_width,
            });
            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width,
                force_width: None,
            });

            let mut current_frame = self.current_frame.write();
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key);

            layout
        }
    }

    pub fn layout_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width: None,
            force_width,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.upgradable_read();
        if let Some(layout) = current_frame.lines.get(key) {
            return layout.clone();
        }

        let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
        if let Some((key, layout)) = self.previous_frame.lock().lines.remove_entry(key) {
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        } else {
            let text = SharedString::from(text);
            let mut layout = self
                .text_system
                .platform_text_system
                .layout_line(&text, font_size, runs);

            self.text_system.finalize_line_layout(&mut layout, false);

            if let Some(force_width) = force_width {
                apply_force_width_to_layout(&mut layout, force_width);
            }

            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width: None,
                force_width,
            });
            let layout = Arc::new(layout);
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        }
    }

    pub fn layout_rich_line<Text>(
        &self,
        text: Text,
        runs: &[RichFontRun],
        force_width: Option<Pixels>,
    ) -> Result<Arc<LineLayout>>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let text_ref = text.as_ref();
        let key = &RichCacheKeyRef {
            text: text_ref,
            runs,
            force_width,
        } as &dyn AsRichCacheKeyRef;
        let current_frame = self.current_frame.upgradable_read();
        if let Some(layout) = current_frame.rich_lines.get(key) {
            return Ok(layout.clone());
        }

        let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
        if let Some((key, layout)) = self.previous_frame.lock().rich_lines.remove_entry(key) {
            current_frame.rich_lines.insert(key.clone(), layout.clone());
            current_frame.used_rich_lines.push(key);
            return Ok(layout);
        }

        let text = SharedString::from(text);
        let mut layout = if text.is_empty() {
            self.empty_rich_line_layout(runs.first())
        } else {
            self.text_system
                .platform_text_system
                .layout_rich_line(&text, runs)?
        };
        layout.minimum_line_height = runs
            .iter()
            .map(|run| run.minimum_line_height)
            .max()
            .unwrap_or(Pixels::ZERO);
        self.text_system.finalize_line_layout(&mut layout, true);

        if let Some(force_width) = force_width {
            apply_force_width_to_layout(&mut layout, force_width);
        }

        let key = Arc::new(RichCacheKey {
            text,
            runs: SmallVec::from(runs),
            force_width,
        });
        let layout = Arc::new(layout);
        current_frame.rich_lines.insert(key.clone(), layout.clone());
        current_frame.used_rich_lines.push(key);
        Ok(layout)
    }

    fn empty_rich_line_layout(&self, run: Option<&RichFontRun>) -> LineLayout {
        let Some(run) = run else {
            return LineLayout {
                caret_stops: vec![CaretStop {
                    index: 0,
                    affinity: CaretAffinity::Downstream,
                    direction: TextDirection::LeftToRight,
                    x: Pixels::ZERO,
                }],
                ..Default::default()
            };
        };

        let (ascent, descent) = self.text_system.read_metrics(run.font_id, |metrics| {
            let scale = run.font_size.as_f32() / metrics.units_per_em as f32;
            (
                (px(metrics.ascent * scale) + run.baseline_shift).max(Pixels::ZERO),
                (px(-metrics.descent * scale) - run.baseline_shift).max(Pixels::ZERO),
            )
        });
        LineLayout {
            font_size: run.font_size,
            width: Pixels::ZERO,
            ascent,
            descent,
            minimum_line_height: run.minimum_line_height,
            runs: Vec::new(),
            caret_stops: vec![CaretStop {
                index: 0,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: Pixels::ZERO,
            }],
            generated_caret_stops: OnceLock::new(),
            len: 0,
        }
    }

    /// Try to retrieve a previously-shaped line layout using a caller-provided content hash.
    ///
    /// This is a *non-allocating* cache probe: it does not materialize any text. If the layout
    /// is not already cached in either the current frame or previous frame, returns `None`.
    ///
    /// Contract (caller enforced):
    /// - Same `text_hash` implies identical text content (collision risk accepted by caller).
    /// - `text_len` should be the UTF-8 byte length of the text (helps reduce accidental collisions).
    pub fn try_layout_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Option<Arc<LineLayout>> {
        let key_ref = HashedCacheKeyRef {
            text_hash,
            text_len,
            font_size,
            runs,
            wrap_width: None,
            force_width,
        };

        let current_frame = self.current_frame.read();
        if let Some((_, layout)) = current_frame.lines_by_hash.iter().find(|(key, _)| {
            HashedCacheKeyRef {
                text_hash: key.text_hash,
                text_len: key.text_len,
                font_size: key.font_size,
                runs: key.runs.as_slice(),
                wrap_width: key.wrap_width,
                force_width: key.force_width,
            } == key_ref
        }) {
            return Some(layout.clone());
        }

        let previous_frame = self.previous_frame.lock();
        if let Some((_, layout)) = previous_frame.lines_by_hash.iter().find(|(key, _)| {
            HashedCacheKeyRef {
                text_hash: key.text_hash,
                text_len: key.text_len,
                font_size: key.font_size,
                runs: key.runs.as_slice(),
                wrap_width: key.wrap_width,
                force_width: key.force_width,
            } == key_ref
        }) {
            return Some(layout.clone());
        }

        None
    }

    /// Layout a line of text using a caller-provided content hash as the cache key.
    ///
    /// This enables cache hits without materializing a contiguous `SharedString` for `text`.
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
        runs: &[FontRun],
        force_width: Option<Pixels>,
        materialize_text: impl FnOnce() -> SharedString,
    ) -> Arc<LineLayout> {
        let key_ref = HashedCacheKeyRef {
            text_hash,
            text_len,
            font_size,
            runs,
            wrap_width: None,
            force_width,
        };

        // Fast path: already cached (no allocation).
        let current_frame = self.current_frame.upgradable_read();
        if let Some((_, layout)) = current_frame.lines_by_hash.iter().find(|(key, _)| {
            HashedCacheKeyRef {
                text_hash: key.text_hash,
                text_len: key.text_len,
                font_size: key.font_size,
                runs: key.runs.as_slice(),
                wrap_width: key.wrap_width,
                force_width: key.force_width,
            } == key_ref
        }) {
            return layout.clone();
        }

        let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);

        // Try to reuse from previous frame without allocating; do a linear scan to find a matching key.
        // (We avoid `drain()` here because it would eagerly move all entries.)
        let mut previous_frame = self.previous_frame.lock();
        if let Some(existing_key) = previous_frame
            .used_lines_by_hash
            .iter()
            .find(|key| {
                HashedCacheKeyRef {
                    text_hash: key.text_hash,
                    text_len: key.text_len,
                    font_size: key.font_size,
                    runs: key.runs.as_slice(),
                    wrap_width: key.wrap_width,
                    force_width: key.force_width,
                } == key_ref
            })
            .cloned()
        {
            if let Some((key, layout)) = previous_frame.lines_by_hash.remove_entry(&existing_key) {
                current_frame
                    .lines_by_hash
                    .insert(key.clone(), layout.clone());
                current_frame.used_lines_by_hash.push(key);
                return layout;
            }
        }

        let text = materialize_text();
        let mut layout = self
            .text_system
            .platform_text_system
            .layout_line(&text, font_size, runs);

        self.text_system.finalize_line_layout(&mut layout, false);

        if let Some(force_width) = force_width {
            apply_force_width_to_layout(&mut layout, force_width);
        }

        let key = Arc::new(HashedCacheKey {
            text_hash,
            text_len,
            font_size,
            runs: SmallVec::from(runs),
            wrap_width: None,
            force_width,
        });
        let layout = Arc::new(layout);
        current_frame
            .lines_by_hash
            .insert(key.clone(), layout.clone());
        current_frame.used_lines_by_hash.push(key);
        layout
    }
}

// Combining marks (e.g. Thai vowel signs, Arabic diacritics) are shaped by
// HarfBuzz at the same x position as their base character. The force-width
// loop must not advance the cell counter for these zero-advance glyphs,
// otherwise they get displaced into the next cell. We detect them by checking
// whether shaped x has advanced by at least half a cell beyond the last base.
fn apply_force_width_to_layout(layout: &mut LineLayout, force_width: Pixels) {
    let mut glyph_pos: usize = 0;
    // NEG_INFINITY ensures the first glyph is always classified as a base.
    let mut last_base_shaped_x = px(f32::NEG_INFINITY);
    let mut last_base_actual_x = px(0.);
    let mut caret_x_anchors = Vec::<(f32, f32)>::new();

    for run in layout.runs.iter_mut() {
        for glyph in run.glyphs.iter_mut() {
            let shaped_x = glyph.position.x;

            if shaped_x > last_base_shaped_x + force_width * 0.5 {
                let forced_x = glyph_pos * force_width;
                if (shaped_x - forced_x).abs() > px(1.) {
                    glyph.position.x = forced_x;
                }
                last_base_shaped_x = shaped_x;
                last_base_actual_x = glyph.position.x;
                glyph_pos += 1;
            } else {
                glyph.position.x = last_base_actual_x + (shaped_x - last_base_shaped_x);
            }
            caret_x_anchors.push((shaped_x.as_f32(), glyph.position.x.as_f32()));
        }
    }

    if !layout.caret_stops.is_empty() {
        let width = layout.width.as_f32();
        caret_x_anchors.sort_by(|left, right| left.0.total_cmp(&right.0));
        caret_x_anchors.dedup_by(|left, right| left.0.to_bits() == right.0.to_bits());
        if let Some((_, mapped)) = caret_x_anchors
            .iter_mut()
            .find(|(shaped, _)| shaped.to_bits() == width.to_bits())
        {
            *mapped = width;
        } else {
            caret_x_anchors.push((width, width));
            caret_x_anchors.sort_by(|left, right| left.0.total_cmp(&right.0));
        }

        for stop in &mut layout.caret_stops {
            stop.x = px(remap_caret_x(stop.x.as_f32(), &caret_x_anchors));
        }
        layout.normalize_caret_stops();
    }
}

fn remap_caret_x(x: f32, anchors: &[(f32, f32)]) -> f32 {
    let next = anchors.partition_point(|(source, _)| *source < x);
    if let Some((source, target)) = anchors.get(next)
        && source.to_bits() == x.to_bits()
    {
        return *target;
    }

    match (
        next.checked_sub(1).and_then(|index| anchors.get(index)),
        anchors.get(next),
    ) {
        (Some((left_source, left_target)), Some((right_source, right_target)))
            if right_source > left_source =>
        {
            let ratio = (x - left_source) / (right_source - left_source);
            left_target + ratio * (right_target - left_target)
        }
        (Some((source, target)), _) | (_, Some((source, target))) => target + (x - source),
        (None, None) => x,
    }
}

/// A run of text with a single font.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[expect(missing_docs)]
pub struct FontRun {
    pub len: usize,
    pub font_id: FontId,
}

/// A font run carrying heterogeneous shaping metrics.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[expect(missing_docs)]
pub struct RichFontRun {
    pub len: usize,
    pub font_id: FontId,
    pub font_size: Pixels,
    pub minimum_line_height: Pixels,
    pub baseline_shift: Pixels,
}

trait AsCacheKeyRef {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_>;
}

trait AsRichCacheKeyRef {
    fn as_rich_cache_key_ref(&self) -> RichCacheKeyRef<'_>;
}

#[derive(Clone, Debug)]
struct RichCacheKey {
    text: SharedString,
    runs: SmallVec<[RichFontRun; 1]>,
    force_width: Option<Pixels>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct RichCacheKeyRef<'a> {
    text: &'a str,
    runs: &'a [RichFontRun],
    force_width: Option<Pixels>,
}

#[derive(Clone, Debug, Eq)]
struct CacheKey {
    text: SharedString,
    font_size: Pixels,
    runs: SmallVec<[FontRun; 1]>,
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct CacheKeyRef<'a> {
    text: &'a str,
    font_size: Pixels,
    runs: &'a [FontRun],
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

#[derive(Clone, Debug)]
struct HashedCacheKey {
    text_hash: u64,
    text_len: usize,
    font_size: Pixels,
    runs: SmallVec<[FontRun; 1]>,
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

#[derive(Copy, Clone)]
struct HashedCacheKeyRef<'a> {
    text_hash: u64,
    text_len: usize,
    font_size: Pixels,
    runs: &'a [FontRun],
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

impl PartialEq for dyn AsCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsCacheKeyRef) -> bool {
        self.as_cache_key_ref() == other.as_cache_key_ref()
    }
}

impl PartialEq for dyn AsRichCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsRichCacheKeyRef) -> bool {
        self.as_rich_cache_key_ref() == other.as_rich_cache_key_ref()
    }
}

impl Eq for dyn AsRichCacheKeyRef + '_ {}

impl Hash for dyn AsRichCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_rich_cache_key_ref().hash(state)
    }
}

impl AsRichCacheKeyRef for RichCacheKey {
    fn as_rich_cache_key_ref(&self) -> RichCacheKeyRef<'_> {
        RichCacheKeyRef {
            text: &self.text,
            runs: self.runs.as_slice(),
            force_width: self.force_width,
        }
    }
}

impl PartialEq for RichCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_rich_cache_key_ref()
            .eq(&other.as_rich_cache_key_ref())
    }
}

impl Eq for RichCacheKey {}

impl Hash for RichCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_rich_cache_key_ref().hash(state)
    }
}

impl<'a> Borrow<dyn AsRichCacheKeyRef + 'a> for Arc<RichCacheKey> {
    fn borrow(&self) -> &(dyn AsRichCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsRichCacheKeyRef
    }
}

impl AsRichCacheKeyRef for RichCacheKeyRef<'_> {
    fn as_rich_cache_key_ref(&self) -> RichCacheKeyRef<'_> {
        *self
    }
}

impl PartialEq for HashedCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.text_hash == other.text_hash
            && self.text_len == other.text_len
            && self.font_size == other.font_size
            && self.runs.as_slice() == other.runs.as_slice()
            && self.wrap_width == other.wrap_width
            && self.force_width == other.force_width
    }
}

impl Eq for HashedCacheKey {}

impl Hash for HashedCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text_hash.hash(state);
        self.text_len.hash(state);
        self.font_size.hash(state);
        self.runs.as_slice().hash(state);
        self.wrap_width.hash(state);
        self.force_width.hash(state);
    }
}

impl PartialEq for HashedCacheKeyRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.text_hash == other.text_hash
            && self.text_len == other.text_len
            && self.font_size == other.font_size
            && self.runs == other.runs
            && self.wrap_width == other.wrap_width
            && self.force_width == other.force_width
    }
}

impl Eq for HashedCacheKeyRef<'_> {}

impl Hash for HashedCacheKeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text_hash.hash(state);
        self.text_len.hash(state);
        self.font_size.hash(state);
        self.runs.hash(state);
        self.wrap_width.hash(state);
        self.force_width.hash(state);
    }
}

impl Eq for dyn AsCacheKeyRef + '_ {}

impl Hash for dyn AsCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state)
    }
}

impl AsCacheKeyRef for CacheKey {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        CacheKeyRef {
            text: &self.text,
            font_size: self.font_size,
            runs: self.runs.as_slice(),
            wrap_width: self.wrap_width,
            force_width: self.force_width,
        }
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_cache_key_ref().eq(&other.as_cache_key_ref())
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state);
    }
}

impl<'a> Borrow<dyn AsCacheKeyRef + 'a> for Arc<CacheKey> {
    fn borrow(&self) -> &(dyn AsCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsCacheKeyRef
    }
}

impl AsCacheKeyRef for CacheKeyRef<'_> {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphId;

    fn glyph_at(x: f32, index: usize) -> ShapedGlyph {
        ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), px(0.)),
            index,
            is_emoji: false,
        }
    }

    fn make_layout(glyphs: Vec<ShapedGlyph>) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(100.),
            ascent: px(12.),
            descent: px(4.),
            minimum_line_height: Pixels::ZERO,
            runs: vec![ShapedRun {
                font_id: FontId(0),
                font_size: px(16.),
                baseline_shift: Pixels::ZERO,
                resolved_face: None,
                glyphs,
            }],
            caret_stops: Vec::new(),
            generated_caret_stops: OnceLock::new(),
            len: 0,
        }
    }

    fn glyph_x_positions(layout: &LineLayout) -> Vec<f32> {
        layout.runs[0]
            .glyphs
            .iter()
            .map(|g| f32::from(g.position.x))
            .collect()
    }

    #[test]
    fn test_force_width_latin_unchanged() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(8., 1), glyph_at(16., 2)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_marks_not_advanced() {
        let cell_width = px(8.);
        // Simulates Thai "กี" — base consonant at x=0, combining vowel also at x=0
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // ี (combining mark, same x)
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0.]);
    }

    #[test]
    fn test_force_width_base_after_combining_mark() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3), glyph_at(8., 6)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 8.]);
    }

    #[test]
    fn test_force_width_multiple_combining_marks() {
        let cell_width = px(8.);
        // Simulates "ก้" — base + vowel + tone mark (two combining marks stacked)
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // vowel (combining)
            glyph_at(0., 6), // tone mark (combining)
            glyph_at(8., 9), // next base
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 0., 8.]);
    }

    #[test]
    fn test_force_width_corrects_drifted_base_positions() {
        let cell_width = px(8.);
        // Font metrics don't perfectly match cell grid — glyphs drift >1px from cell boundary
        let mut layout = make_layout(vec![
            glyph_at(0.5, 0),  // within 1px tolerance, kept as-is
            glyph_at(10.2, 1), // >1px off from 8.0, corrected
            glyph_at(19.8, 2), // >1px off from 16.0, corrected
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 8., 16.]);
    }

    #[test]
    fn test_force_width_remaps_caret_geometry_with_glyphs() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(10.2, 1), glyph_at(19.8, 2)]);
        layout.caret_stops = vec![
            CaretStop {
                index: 1,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: px(10.2),
            },
            CaretStop {
                index: 2,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: px(19.8),
            },
            CaretStop {
                index: 3,
                affinity: CaretAffinity::Upstream,
                direction: TextDirection::LeftToRight,
                x: layout.width,
            },
        ];

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(layout.caret_stops()[0].x, px(8.));
        assert_eq!(layout.caret_stops()[1].x, px(16.));
        assert_eq!(layout.caret_stops()[2].x, layout.width);
    }

    #[test]
    fn test_force_width_combining_mark_after_within_tolerance_base() {
        let cell_width = px(8.);
        // Base glyph is within 1px of grid so it keeps its shaped position.
        // The combining mark must align to the base's actual position, not the grid slot.
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(0.5, 3)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 0.5]);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn rich_line_metadata_stays_compact() {
        assert_eq!(std::mem::size_of::<ResolvedFontFace>(), 8);
        assert_eq!(std::mem::size_of::<ShapedRun>(), 48);
        assert_eq!(std::mem::size_of::<LineLayout>(), 96);
    }
}
