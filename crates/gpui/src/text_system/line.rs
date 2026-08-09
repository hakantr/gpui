use crate::{
    App, Bounds, CaretAffinity, CaretStop, DevicePixels, Half, Hsla, LineLayout, Pixels, Point,
    RenderGlyphParams, Result, ShapedGlyph, ShapedRun, SharedString, StrikethroughStyle, TextAlign,
    TextDirection, UnderlineStyle, Window, WrapBoundary, WrappedLineLayout, black, fill, point, px,
    size,
};
use derive_more::{Deref, DerefMut};
use smallvec::SmallVec;
use std::sync::Arc;

/// Pre-computed glyph data for efficient painting without per-glyph cache lookups.
///
/// This is produced by `ShapedLine::compute_glyph_raster_data` during prepaint
/// and consumed by `ShapedLine::paint_with_raster_data` during paint.
#[derive(Clone, Debug)]
pub struct GlyphRasterData {
    /// The raster bounds for each glyph, in paint order.
    pub bounds: Vec<Bounds<DevicePixels>>,
    /// The render params for each glyph (needed for sprite atlas lookup).
    pub params: Vec<RenderGlyphParams>,
}

/// Set the text decoration for a run of text.
#[derive(Debug, Clone)]
pub struct DecorationRun {
    /// The length of the run in utf-8 bytes.
    pub len: u32,

    /// The color for this run
    pub color: Hsla,

    /// The background color for this run
    pub background_color: Option<Hsla>,

    /// The underline style for this run
    pub underline: Option<UnderlineStyle>,

    /// The strikethrough style for this run
    pub strikethrough: Option<StrikethroughStyle>,
}

/// Paint-only data that can be replaced without reshaping line geometry.
#[derive(Clone, Debug)]
pub struct LinePaint {
    len: usize,
    decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl LinePaint {
    /// Build a paint payload whose runs exactly cover `len` UTF-8 bytes.
    pub fn new(
        len: usize,
        decoration_runs: impl IntoIterator<Item = DecorationRun>,
    ) -> Result<Self> {
        let decoration_runs = decoration_runs
            .into_iter()
            .collect::<SmallVec<[DecorationRun; 32]>>();
        let mut covered = 0usize;
        for run in &decoration_runs {
            covered = covered
                .checked_add(run.len as usize)
                .ok_or_else(|| anyhow::anyhow!("line paint run length overflow"))?;
        }
        if covered != len {
            return Err(anyhow::anyhow!(
                "line paint covers {covered} bytes but the geometry contains {len} bytes"
            ));
        }
        Ok(Self {
            len,
            decoration_runs,
        })
    }

    /// The UTF-8 byte length covered by this paint payload.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this paint payload covers an empty line.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The paint-only runs in logical byte order.
    pub fn runs(&self) -> &[DecorationRun] {
        &self.decoration_runs
    }
}

/// A visual caret stop after applying a line placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedCaretStop {
    /// The UTF-8 byte boundary in the original text.
    pub index: usize,
    /// The logical side of the boundary represented by this stop.
    pub affinity: CaretAffinity,
    /// The visual direction of the adjacent shaped run.
    pub direction: TextDirection,
    /// The position in the same coordinate space as the placement origin.
    pub position: Point<Pixels>,
}

/// The shared origin, line-height, and alignment transform for geometry queries and paint.
#[derive(Clone, Debug)]
pub struct LinePlacement {
    layout: Arc<LineLayout>,
    origin: Point<Pixels>,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    content_origin: Point<Pixels>,
}

impl LinePlacement {
    fn new(
        layout: Arc<LineLayout>,
        origin: Point<Pixels>,
        requested_line_height: Pixels,
        align: TextAlign,
        align_width: Option<Pixels>,
    ) -> Self {
        let line_height = requested_line_height.max(layout.minimum_line_height);
        let content_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                Pixels::ZERO,
                &align,
                &layout,
                None,
            ),
            origin.y,
        );
        Self {
            layout,
            origin,
            line_height,
            align,
            align_width,
            content_origin,
        }
    }

    /// The requested outer origin.
    pub fn origin(&self) -> Point<Pixels> {
        self.origin
    }

    /// The resolved line height, including heterogeneous minimums.
    pub fn line_height(&self) -> Pixels {
        self.line_height
    }

    /// The aligned origin of line-local glyph geometry.
    pub fn content_origin(&self) -> Point<Pixels> {
        self.content_origin
    }

    /// Convert a line-local x coordinate into the placement coordinate space.
    pub fn viewport_x_for_local_x(&self, x: Pixels) -> Pixels {
        self.content_origin.x + x
    }

    /// Convert an x coordinate in the placement coordinate space into line-local geometry.
    pub fn local_x_for_viewport_x(&self, x: Pixels) -> Pixels {
        x - self.content_origin.x
    }

    /// Return all visual caret stops for one UTF-8 byte boundary in placement coordinates.
    pub fn caret_stops_for_index(&self, index: usize) -> SmallVec<[PlacedCaretStop; 2]> {
        self.layout
            .caret_stops_for_index(index)
            .iter()
            .map(|stop| self.place_caret(*stop))
            .collect()
    }

    /// Find the closest affinity-qualified caret to an x coordinate in placement space.
    pub fn caret_for_viewport_x(&self, x: Pixels) -> Option<PlacedCaretStop> {
        self.layout
            .closest_caret_for_x(self.local_x_for_viewport_x(x))
            .map(|stop| self.place_caret(stop))
    }

    /// Resolve a qualified caret to an x coordinate in placement space.
    pub fn viewport_x_for_caret(
        &self,
        index: usize,
        affinity: CaretAffinity,
        direction: TextDirection,
    ) -> Option<Pixels> {
        self.layout
            .x_for_caret(index, affinity, direction)
            .map(|x| self.viewport_x_for_local_x(x))
    }

    fn place_caret(&self, stop: CaretStop) -> PlacedCaretStop {
        PlacedCaretStop {
            index: stop.index,
            affinity: stop.affinity,
            direction: stop.direction,
            position: point(self.viewport_x_for_local_x(stop.x), self.content_origin.y),
        }
    }
}

/// A line of text that has been shaped and decorated.
#[derive(Clone, Default, Debug, Deref, DerefMut)]
pub struct ShapedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<LineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl ShapedLine {
    /// The length of the line in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len
    }

    /// The width of the shaped line in pixels.
    ///
    /// This is the glyph advance width computed by the text shaping system and is useful for
    /// incrementally advancing a "pen" when painting multiple fragments on the same row.
    pub fn width(&self) -> Pixels {
        self.layout.width
    }

    /// Clone the immutable shaped geometry independently of its paint payload.
    pub fn geometry(&self) -> Arc<LineLayout> {
        self.layout.clone()
    }

    /// Clone the default paint payload produced during shaping.
    pub fn paint_payload(&self) -> LinePaint {
        LinePaint {
            len: self.layout.len,
            decoration_runs: self.decoration_runs.clone(),
        }
    }

    /// Resolve the transform shared by caret geometry and paint.
    pub fn place(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        align_width: Option<Pixels>,
    ) -> LinePlacement {
        LinePlacement::new(self.layout.clone(), origin, line_height, align, align_width)
    }

    /// Override the len, useful if you're rendering text a
    /// as text b (e.g. rendering invisibles).
    pub fn with_len(mut self, len: usize) -> Self {
        let layout = self.layout.as_ref();
        self.layout = Arc::new(LineLayout {
            font_size: layout.font_size,
            width: layout.width,
            ascent: layout.ascent,
            descent: layout.descent,
            minimum_line_height: layout.minimum_line_height,
            runs: layout.runs.clone(),
            caret_stops: layout
                .caret_stops()
                .iter()
                .map(|stop| {
                    let mut stop = *stop;
                    if stop.index == layout.len {
                        stop.index = len;
                    }
                    stop
                })
                .filter(|stop| stop.index <= len)
                .collect(),
            generated_caret_stops: Default::default(),
            len,
        });
        self
    }

    /// Paint the line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        align_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let placement = self.place(origin, line_height, align, align_width);
        paint_line(
            placement.origin,
            &self.layout,
            placement.line_height,
            placement.align,
            placement.align_width,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint immutable geometry with caller-provided paint-only data.
    pub fn paint_with(
        &self,
        placement: &LinePlacement,
        paint: &LinePaint,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        if !Arc::ptr_eq(&self.layout, &placement.layout) {
            return Err(anyhow::anyhow!(
                "line placement was created for different shaped geometry"
            ));
        }
        if paint.len != self.layout.len {
            return Err(anyhow::anyhow!(
                "line paint length does not match shaped geometry"
            ));
        }
        paint_line(
            placement.origin,
            &self.layout,
            placement.line_height,
            placement.align,
            placement.align_width,
            &paint.decoration_runs,
            &[],
            window,
            cx,
        )
    }

    /// Paint the background of the line to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        align_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let placement = self.place(origin, line_height, align, align_width);
        paint_line_background(
            placement.origin,
            &self.layout,
            placement.line_height,
            placement.align,
            placement.align_width,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint only backgrounds using caller-provided paint data and the shared placement.
    pub fn paint_background_with(
        &self,
        placement: &LinePlacement,
        paint: &LinePaint,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        if !Arc::ptr_eq(&self.layout, &placement.layout) {
            return Err(anyhow::anyhow!(
                "line placement was created for different shaped geometry"
            ));
        }
        if paint.len != self.layout.len {
            return Err(anyhow::anyhow!(
                "line paint length does not match shaped geometry"
            ));
        }
        paint_line_background(
            placement.origin,
            &self.layout,
            placement.line_height,
            placement.align,
            placement.align_width,
            &paint.decoration_runs,
            &[],
            window,
            cx,
        )
    }

    /// Split this shaped line at a byte index, returning `(prefix, suffix)`.
    ///
    /// - `prefix` contains glyphs for bytes `[0, byte_index)` and `suffix` contains glyphs for
    ///   bytes `[byte_index, len)`, regardless of the visual ordering of BiDi glyphs.
    /// - Each half is rebased to its own visual caret bounds. Their widths sum to the original
    ///   width for ordinary monotonic text; interleaved BiDi halves can have overlapping visual
    ///   bounds and therefore do not promise additive widths.
    /// - The index must be a shaped glyph-cluster boundary. Splitting inside a ligature or
    ///   combining cluster requires reshaping and cannot be represented by partitioning this
    ///   line's existing glyphs.
    /// - Decoration runs are partitioned at the boundary; a run that straddles it is
    ///   split into two with adjusted lengths.
    /// - `font_size`, `ascent`, and `descent` are copied to both halves.
    pub fn split_at(&self, byte_index: usize) -> (ShapedLine, ShapedLine) {
        assert!(
            byte_index <= self.text.len() && self.text.is_char_boundary(byte_index),
            "split index must be a UTF-8 boundary within the shaped line"
        );
        assert!(
            byte_index == 0
                || byte_index == self.text.len()
                || self
                    .layout
                    .runs
                    .iter()
                    .flat_map(|run| &run.glyphs)
                    .any(|glyph| glyph.index == byte_index),
            "split index must be a shaped glyph-cluster boundary; reshape the two halves to split inside a cluster"
        );

        let all_caret_stops = self.layout.caret_stops();
        let mut left_caret_stops = if byte_index == 0 {
            all_caret_stops
                .iter()
                .find(|stop| stop.index == 0 && stop.affinity == CaretAffinity::Downstream)
                .or_else(|| all_caret_stops.iter().find(|stop| stop.index == 0))
                .copied()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            all_caret_stops
                .iter()
                .copied()
                .filter(|stop| {
                    stop.index < byte_index
                        || (stop.index == byte_index && stop.affinity == CaretAffinity::Upstream)
                })
                .collect()
        };
        let mut right_caret_stops = if byte_index == self.layout.len {
            all_caret_stops
                .iter()
                .find(|stop| stop.index == byte_index && stop.affinity == CaretAffinity::Upstream)
                .or_else(|| all_caret_stops.iter().find(|stop| stop.index == byte_index))
                .copied()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            all_caret_stops
                .iter()
                .copied()
                .filter(|stop| {
                    stop.index > byte_index
                        || (stop.index == byte_index && stop.affinity == CaretAffinity::Downstream)
                })
                .collect()
        };

        // Some compatibility shapers expose only one unqualified stop at a boundary. Keep the
        // split total in that case by sharing the exact stop between both visual projections.
        if !left_caret_stops.iter().any(|stop| stop.index == byte_index) {
            left_caret_stops.extend(
                all_caret_stops
                    .iter()
                    .filter(|stop| stop.index == byte_index)
                    .copied(),
            );
        }
        if !right_caret_stops
            .iter()
            .any(|stop| stop.index == byte_index)
        {
            right_caret_stops.extend(
                all_caret_stops
                    .iter()
                    .filter(|stop| stop.index == byte_index)
                    .copied(),
            );
        }

        let caret_bounds = |stops: &[CaretStop], fallback: Pixels| {
            let Some((first, rest)) = stops.split_first() else {
                return (fallback, fallback);
            };
            rest.iter()
                .fold((first.x, first.x), |(min_x, max_x), stop| {
                    (min_x.min(stop.x), max_x.max(stop.x))
                })
        };
        let split_x = self.layout.x_for_index(byte_index);
        let (left_origin, left_end) = caret_bounds(&left_caret_stops, split_x);
        let (right_origin, right_end) = caret_bounds(&right_caret_stops, split_x);
        let left_width = left_end - left_origin;
        let right_width = right_end - right_origin;

        for stop in &mut left_caret_stops {
            stop.x -= left_origin;
        }
        for stop in &mut right_caret_stops {
            stop.index -= byte_index;
            stop.x -= right_origin;
        }

        // Select by logical index rather than partitioning the visually ordered glyph vector.
        // A single RTL run can contribute non-contiguous visual glyphs to both halves.
        let mut left_runs = Vec::new();
        let mut right_runs = Vec::new();

        for run in &self.layout.runs {
            let left_glyphs = run
                .glyphs
                .iter()
                .filter(|glyph| glyph.index < byte_index)
                .map(|glyph| ShapedGlyph {
                    id: glyph.id,
                    position: point(glyph.position.x - left_origin, glyph.position.y),
                    index: glyph.index,
                    is_emoji: glyph.is_emoji,
                })
                .collect::<Vec<_>>();
            if !left_glyphs.is_empty() {
                left_runs.push(ShapedRun {
                    font_id: run.font_id,
                    font_size: run.font_size,
                    baseline_shift: run.baseline_shift,
                    resolved_face: run.resolved_face.clone(),
                    glyphs: left_glyphs,
                });
            }

            let right_glyphs = run
                .glyphs
                .iter()
                .filter(|glyph| glyph.index >= byte_index)
                .map(|glyph| ShapedGlyph {
                    id: glyph.id,
                    position: point(glyph.position.x - right_origin, glyph.position.y),
                    index: glyph.index - byte_index,
                    is_emoji: glyph.is_emoji,
                })
                .collect::<Vec<_>>();
            if !right_glyphs.is_empty() {
                right_runs.push(ShapedRun {
                    font_id: run.font_id,
                    font_size: run.font_size,
                    baseline_shift: run.baseline_shift,
                    resolved_face: run.resolved_face.clone(),
                    glyphs: right_glyphs,
                });
            }
        }

        // Partition decoration runs. A run straddling the boundary is split into two.
        let mut left_decorations = SmallVec::new();
        let mut right_decorations = SmallVec::new();
        let mut decoration_offset = 0u32;
        let split_point = u32::try_from(byte_index)
            .expect("split index must fit the u32 decoration-run coordinate space");

        for decoration in &self.decoration_runs {
            let run_end = decoration_offset
                .checked_add(decoration.len)
                .expect("decoration-run coverage must fit u32");

            if run_end <= split_point {
                left_decorations.push(decoration.clone());
            } else if decoration_offset >= split_point {
                right_decorations.push(decoration.clone());
            } else {
                let left_len = split_point - decoration_offset;
                let right_len = run_end - split_point;
                left_decorations.push(DecorationRun {
                    len: left_len,
                    color: decoration.color,
                    background_color: decoration.background_color,
                    underline: decoration.underline,
                    strikethrough: decoration.strikethrough,
                });
                right_decorations.push(DecorationRun {
                    len: right_len,
                    color: decoration.color,
                    background_color: decoration.background_color,
                    underline: decoration.underline,
                    strikethrough: decoration.strikethrough,
                });
            }

            decoration_offset = run_end;
        }

        // Split text
        let left_text = if byte_index == self.text.len() {
            self.text.clone()
        } else {
            SharedString::new(&self.text[..byte_index])
        };
        let right_text = if byte_index == 0 {
            self.text.clone()
        } else {
            SharedString::new(&self.text[byte_index..])
        };

        let left = ShapedLine {
            layout: Arc::new(LineLayout {
                font_size: self.layout.font_size,
                width: left_width,
                ascent: self.layout.ascent,
                descent: self.layout.descent,
                minimum_line_height: self.layout.minimum_line_height,
                runs: left_runs,
                caret_stops: left_caret_stops,
                generated_caret_stops: Default::default(),
                len: byte_index,
            }),
            text: left_text,
            decoration_runs: left_decorations,
        };

        let right = ShapedLine {
            layout: Arc::new(LineLayout {
                font_size: self.layout.font_size,
                width: right_width,
                ascent: self.layout.ascent,
                descent: self.layout.descent,
                minimum_line_height: self.layout.minimum_line_height,
                runs: right_runs,
                caret_stops: right_caret_stops,
                generated_caret_stops: Default::default(),
                len: self.layout.len - byte_index,
            }),
            text: right_text,
            decoration_runs: right_decorations,
        };

        (left, right)
    }
}

/// A line of text that has been shaped, decorated, and wrapped by the text layout system.
#[derive(Default, Debug, Deref, DerefMut)]
pub struct WrappedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<WrappedLineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: Vec<DecorationRun>,
}

impl WrappedLine {
    /// The length of the underlying, unwrapped layout, in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len()
    }

    /// Paint this line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint the background of line of text to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line_background(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }
}

/// Resolve paint-only data by logical UTF-8 byte index. Shaped glyphs are
/// visited in visual order, which is not monotonic in mixed BiDi text, so a
/// forward-only decoration iterator would apply the wrong run after an RTL
/// visual segment jumps back to a smaller logical index.
struct DecorationRunLookup<'a> {
    runs: &'a [DecorationRun],
    ends: SmallVec<[usize; 32]>,
    last_run_ix: Option<usize>,
}

impl<'a> DecorationRunLookup<'a> {
    fn new(runs: &'a [DecorationRun]) -> Self {
        let mut end = 0usize;
        let ends = runs
            .iter()
            .map(|run| {
                end = end.saturating_add(run.len as usize);
                end
            })
            .collect();
        Self {
            runs,
            ends,
            last_run_ix: None,
        }
    }

    fn run_for_index(&mut self, index: usize) -> Option<(usize, &'a DecorationRun)> {
        if let Some(run_ix) = self.last_run_ix {
            let start = run_ix
                .checked_sub(1)
                .map_or(0, |previous| self.ends[previous]);
            if start <= index && index < self.ends[run_ix] {
                return Some((run_ix, &self.runs[run_ix]));
            }

            // Ordinary LTR traversal crosses only one boundary at a time. RTL visual runs can
            // move to the previous logical decoration; keep that adjacent transition cheap too.
            let neighbor_ix = if index >= self.ends[run_ix] {
                run_ix.checked_add(1)
            } else {
                run_ix.checked_sub(1)
            };
            if let Some(neighbor_ix) = neighbor_ix.filter(|ix| *ix < self.runs.len()) {
                let neighbor_start = neighbor_ix
                    .checked_sub(1)
                    .map_or(0, |previous| self.ends[previous]);
                if neighbor_start <= index && index < self.ends[neighbor_ix] {
                    self.last_run_ix = Some(neighbor_ix);
                    return Some((neighbor_ix, &self.runs[neighbor_ix]));
                }
            }
        }

        let run_ix = self.ends.partition_point(|end| *end <= index);
        let run = self.runs.get(run_ix)?;
        self.last_run_ix = Some(run_ix);
        Some((run_ix, run))
    }
}

fn paint_line(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let line_bounds = Bounds::new(
        origin,
        size(
            layout.width,
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        let padding_top = (line_height - layout.ascent - layout.descent) / 2.;
        let baseline_offset = point(px(0.), padding_top + layout.ascent);
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut current_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
        let mut current_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
        let mut decoration_lookup = DecorationRunLookup::new(decoration_runs);
        let mut color = black();
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        let mut first_glyph_x = origin.x;
        for (run_ix, run) in layout.runs.iter().enumerate() {
            let (run_bounds, run_ascent, run_descent) =
                text_system.read_metrics(run.font_id, |metrics| {
                    (
                        metrics.bounding_box(run.font_size).size,
                        metrics.ascent(run.font_size),
                        -metrics.descent(run.font_size),
                    )
                });
            max_glyph_size = run_bounds;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;
                if glyph_ix == 0 && run_ix == 0 {
                    first_glyph_x = glyph_origin.x;
                }
                let glyph_decoration = decoration_lookup.run_for_index(glyph.index);
                // Resolve inherited colors before comparing segment identity. Upstream compares
                // the raw optional-color payload with the active resolved style, which splits
                // visually identical inherited decorations at every logical run boundary.
                let target_underline = glyph_decoration.and_then(|(_, style_run)| {
                    style_run.underline.map(|run_underline| UnderlineStyle {
                        color: Some(run_underline.color.unwrap_or(style_run.color)),
                        thickness: run_underline.thickness,
                        wavy: run_underline.wavy,
                    })
                });
                let target_strikethrough = glyph_decoration.and_then(|(_, style_run)| {
                    style_run
                        .strikethrough
                        .map(|run_strikethrough| StrikethroughStyle {
                            color: Some(run_strikethrough.color.unwrap_or(style_run.color)),
                            thickness: run_strikethrough.thickness,
                        })
                });

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((underline_origin, underline_style)) = current_underline.as_mut() {
                        if glyph_origin.x == underline_origin.x {
                            underline_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_underline(
                            *underline_origin,
                            glyph_origin.x - underline_origin.x,
                            underline_style,
                        );
                        if Some(*underline_style) == target_underline {
                            underline_origin.x = origin.x;
                            underline_origin.y += line_height;
                        } else {
                            current_underline = None;
                        }
                    }
                    if let Some((strikethrough_origin, strikethrough_style)) =
                        current_strikethrough.as_mut()
                    {
                        if glyph_origin.x == strikethrough_origin.x {
                            strikethrough_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_strikethrough(
                            *strikethrough_origin,
                            glyph_origin.x - strikethrough_origin.x,
                            strikethrough_style,
                        );
                        if Some(*strikethrough_style) == target_strikethrough {
                            strikethrough_origin.x = origin.x;
                            strikethrough_origin.y += line_height;
                        } else {
                            current_strikethrough = None;
                        }
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                }
                prev_glyph_position = glyph.position;

                let mut finished_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
                let mut finished_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
                let underline_y =
                    glyph_origin.y + baseline_offset.y - run.baseline_shift + (run_descent * 0.618);
                if current_underline.as_ref().map(|(_, style)| *style) != target_underline {
                    finished_underline = current_underline.take();
                    if let Some(style) = target_underline {
                        current_underline = Some((point(glyph_origin.x, underline_y), style));
                    }
                } else if let Some((origin, _)) = current_underline.as_mut() {
                    // A continuous underline uses the deepest participating run metric instead of
                    // producing a visible staircase at shaped-run boundaries.
                    origin.y = origin.y.max(underline_y);
                }

                // Deliberately anchor the decoration to the physical run baseline. Unlike the
                // upstream formula, requested line-height padding does not move it through the
                // glyph box; the exact divergence is recorded in SAPMALAR.md.
                let strikethrough_y =
                    glyph_origin.y + baseline_offset.y - run.baseline_shift - (run_ascent * 0.25);
                if current_strikethrough.as_ref().map(|(_, style)| *style) != target_strikethrough {
                    finished_strikethrough = current_strikethrough.take();
                    if let Some(style) = target_strikethrough {
                        current_strikethrough =
                            Some((point(glyph_origin.x, strikethrough_y), style));
                    }
                } else if let Some((origin, _)) = current_strikethrough.as_mut() {
                    origin.y = origin.y.min(strikethrough_y);
                }
                if let Some((_, style_run)) = glyph_decoration {
                    color = style_run.color;
                }

                if let Some((mut underline_origin, underline_style)) = finished_underline {
                    if underline_origin.x == glyph_origin.x {
                        underline_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_underline(
                        underline_origin,
                        glyph_origin.x - underline_origin.x,
                        &underline_style,
                    );
                }

                if let Some((mut strikethrough_origin, strikethrough_style)) =
                    finished_strikethrough
                {
                    if strikethrough_origin.x == glyph_origin.x {
                        strikethrough_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_strikethrough(
                        strikethrough_origin,
                        glyph_origin.x - strikethrough_origin.x,
                        &strikethrough_style,
                    );
                }

                let max_glyph_bounds = Bounds {
                    origin: glyph_origin,
                    size: max_glyph_size,
                };

                let content_mask = window.content_mask();
                if max_glyph_bounds.intersects(&content_mask.bounds) {
                    let vertical_offset = point(px(0.0), glyph.position.y - run.baseline_shift);
                    if glyph.is_emoji {
                        window.paint_emoji(
                            glyph_origin + baseline_offset + vertical_offset,
                            run.font_id,
                            glyph.id,
                            run.font_size,
                        )?;
                    } else {
                        window.paint_glyph(
                            glyph_origin + baseline_offset + vertical_offset,
                            run.font_id,
                            glyph.id,
                            run.font_size,
                            color,
                        )?;
                    }
                }
            }
        }

        let mut last_line_end_x = first_glyph_x + layout.width;
        if let Some(boundary) = wrap_boundaries.last() {
            let run = &layout.runs[boundary.run_ix];
            let glyph = &run.glyphs[boundary.glyph_ix];
            last_line_end_x -= glyph.position.x;
        }

        if let Some((mut underline_start, underline_style)) = current_underline.take() {
            if last_line_end_x == underline_start.x {
                underline_start.x -= max_glyph_size.width.half()
            };
            window.paint_underline(
                underline_start,
                last_line_end_x - underline_start.x,
                &underline_style,
            );
        }

        if let Some((mut strikethrough_start, strikethrough_style)) = current_strikethrough.take() {
            if last_line_end_x == strikethrough_start.x {
                strikethrough_start.x -= max_glyph_size.width.half()
            };
            window.paint_strikethrough(
                strikethrough_start,
                last_line_end_x - strikethrough_start.x,
                &strikethrough_style,
            );
        }

        Ok(())
    })
}

fn paint_line_background(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let line_bounds = Bounds::new(
        origin,
        size(
            layout.width,
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut current_background: Option<(Point<Pixels>, Hsla)> = None;
        let mut decoration_lookup = DecorationRunLookup::new(decoration_runs);
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        for (run_ix, run) in layout.runs.iter().enumerate() {
            max_glyph_size = text_system.bounding_box(run.font_id, run.font_size).size;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;
                let glyph_decoration = decoration_lookup.run_for_index(glyph.index);
                let target_background =
                    glyph_decoration.and_then(|(_, style_run)| style_run.background_color);

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((background_origin, background_color)) = current_background.as_mut()
                    {
                        if glyph_origin.x == background_origin.x {
                            background_origin.x -= max_glyph_size.width.half()
                        }
                        window.paint_quad(fill(
                            Bounds {
                                origin: *background_origin,
                                size: size(glyph_origin.x - background_origin.x, line_height),
                            },
                            *background_color,
                        ));
                        if Some(*background_color) == target_background {
                            background_origin.x = origin.x;
                            background_origin.y += line_height;
                        } else {
                            current_background = None;
                        }
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                }
                prev_glyph_position = glyph.position;

                let mut finished_background: Option<(Point<Pixels>, Hsla)> = None;
                if current_background.as_ref().map(|(_, color)| *color) != target_background {
                    finished_background = current_background.take();
                    if let Some(background) = target_background {
                        current_background =
                            Some((point(glyph_origin.x, glyph_origin.y), background));
                    }
                }

                if let Some((mut background_origin, background_color)) = finished_background {
                    let mut width = glyph_origin.x - background_origin.x;
                    if background_origin.x == glyph_origin.x {
                        background_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_quad(fill(
                        Bounds {
                            origin: background_origin,
                            size: size(width, line_height),
                        },
                        background_color,
                    ));
                }
            }
        }

        let mut last_line_end_x = origin.x + layout.width;
        if let Some(boundary) = wrap_boundaries.last() {
            let run = &layout.runs[boundary.run_ix];
            let glyph = &run.glyphs[boundary.glyph_ix];
            last_line_end_x -= glyph.position.x;
        }

        if let Some((mut background_origin, background_color)) = current_background.take() {
            if last_line_end_x == background_origin.x {
                background_origin.x -= max_glyph_size.width.half()
            };
            window.paint_quad(fill(
                Bounds {
                    origin: background_origin,
                    size: size(last_line_end_x - background_origin.x, line_height),
                },
                background_color,
            ));
        }

        Ok(())
    })
}

fn aligned_origin_x(
    origin: Point<Pixels>,
    align_width: Pixels,
    last_glyph_x: Pixels,
    align: &TextAlign,
    layout: &LineLayout,
    wrap_boundary: Option<&&WrapBoundary>,
) -> Pixels {
    let end_of_line = if let Some(WrapBoundary { run_ix, glyph_ix }) = wrap_boundary {
        layout.runs[*run_ix].glyphs[*glyph_ix].position.x
    } else {
        layout.width
    };

    let line_width = end_of_line - last_glyph_x;

    match align {
        TextAlign::Left => origin.x,
        TextAlign::Center => (origin.x * 2.0 + align_width - line_width) / 2.0,
        TextAlign::Right => origin.x + align_width - line_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontId, GlyphId};

    /// Helper: build a ShapedLine from glyph descriptors without the platform text system.
    /// Each glyph is described as (byte_index, x_position).
    fn make_shaped_line(
        text: &str,
        glyphs: &[(usize, f32)],
        width: f32,
        decorations: &[DecorationRun],
    ) -> ShapedLine {
        let shaped_glyphs: Vec<ShapedGlyph> = glyphs
            .iter()
            .map(|&(index, x)| ShapedGlyph {
                id: GlyphId(0),
                position: point(px(x), px(0.0)),
                index,
                is_emoji: false,
            })
            .collect();
        let mut caret_stops = glyphs
            .iter()
            .map(|&(index, x)| CaretStop {
                index,
                affinity: CaretAffinity::Downstream,
                direction: TextDirection::LeftToRight,
                x: px(x),
            })
            .collect::<Vec<_>>();
        caret_stops.push(CaretStop {
            index: text.len(),
            affinity: CaretAffinity::Upstream,
            direction: TextDirection::LeftToRight,
            x: px(width),
        });

        ShapedLine {
            layout: Arc::new(LineLayout {
                font_size: px(16.0),
                width: px(width),
                ascent: px(12.0),
                descent: px(4.0),
                minimum_line_height: Pixels::ZERO,
                runs: vec![ShapedRun {
                    font_id: FontId(0),
                    font_size: px(16.0),
                    baseline_shift: Pixels::ZERO,
                    resolved_face: None,
                    glyphs: shaped_glyphs,
                }],
                caret_stops,
                generated_caret_stops: Default::default(),
                len: text.len(),
            }),
            text: SharedString::new(text),
            decoration_runs: SmallVec::from(decorations.to_vec()),
        }
    }

    #[test]
    fn test_split_at_invariants() {
        // Split "abcdef" at every possible byte index and verify structural invariants.
        let line = make_shaped_line(
            "abcdef",
            &[
                (0, 0.0),
                (1, 10.0),
                (2, 20.0),
                (3, 30.0),
                (4, 40.0),
                (5, 50.0),
            ],
            60.0,
            &[],
        );

        for i in 0..=6 {
            let (left, right) = line.split_at(i);

            assert_eq!(
                left.width() + right.width(),
                line.width(),
                "widths must sum at split={i}"
            );
            assert_eq!(
                left.len() + right.len(),
                line.len(),
                "lengths must sum at split={i}"
            );
            assert_eq!(
                format!("{}{}", left.text.as_ref(), right.text.as_ref()),
                "abcdef",
                "text must concatenate at split={i}"
            );
            assert_eq!(left.font_size, line.font_size, "font_size at split={i}");
            assert_eq!(right.ascent, line.ascent, "ascent at split={i}");
            assert_eq!(right.descent, line.descent, "descent at split={i}");
        }

        // Edge: split at 0 produces no left runs, full content on right
        let (left, right) = line.split_at(0);
        assert_eq!(left.runs.len(), 0);
        assert_eq!(right.runs[0].glyphs.len(), 6);

        // Edge: split at end produces full content on left, no right runs
        let (left, right) = line.split_at(6);
        assert_eq!(left.runs[0].glyphs.len(), 6);
        assert_eq!(right.runs.len(), 0);
    }

    #[test]
    #[should_panic(expected = "split index must be a shaped glyph-cluster boundary")]
    fn split_at_rejects_a_ligature_internal_boundary() {
        let line = make_shaped_line(
            "office",
            &[(0, 0.0), (1, 10.0), (4, 30.0), (5, 40.0)],
            50.0,
            &[],
        );

        // Byte 2 lies inside the synthetic `ffi` cluster that starts at byte 1 and whose next
        // glyph starts at byte 4. Partitioning the existing glyph vector cannot shape either
        // half correctly.
        let _ = line.split_at(2);
    }

    #[test]
    fn test_split_at_glyph_rebasing() {
        // Two font runs (simulating a font fallback boundary at byte 3):
        //   run A (FontId 0): glyphs at bytes 0,1,2  positions 0,10,20
        //   run B (FontId 1): glyphs at bytes 3,4,5  positions 30,40,50
        // Successive splits simulate the incremental splitting done during wrap.
        let line = ShapedLine {
            layout: Arc::new(LineLayout {
                font_size: px(16.0),
                width: px(60.0),
                ascent: px(12.0),
                descent: px(4.0),
                minimum_line_height: Pixels::ZERO,
                runs: vec![
                    ShapedRun {
                        font_id: FontId(0),
                        font_size: px(16.0),
                        baseline_shift: Pixels::ZERO,
                        resolved_face: None,
                        glyphs: vec![
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(0.0), px(0.0)),
                                index: 0,
                                is_emoji: false,
                            },
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(10.0), px(0.0)),
                                index: 1,
                                is_emoji: false,
                            },
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(20.0), px(0.0)),
                                index: 2,
                                is_emoji: false,
                            },
                        ],
                    },
                    ShapedRun {
                        font_id: FontId(1),
                        font_size: px(16.0),
                        baseline_shift: Pixels::ZERO,
                        resolved_face: None,
                        glyphs: vec![
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(30.0), px(0.0)),
                                index: 3,
                                is_emoji: false,
                            },
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(40.0), px(0.0)),
                                index: 4,
                                is_emoji: false,
                            },
                            ShapedGlyph {
                                id: GlyphId(0),
                                position: point(px(50.0), px(0.0)),
                                index: 5,
                                is_emoji: false,
                            },
                        ],
                    },
                ],
                caret_stops: Vec::new(),
                generated_caret_stops: Default::default(),
                len: 6,
            }),
            text: "abcdef".into(),
            decoration_runs: SmallVec::new(),
        };

        // First split at byte 2 — mid-run in run A
        let (first, remainder) = line.split_at(2);
        assert_eq!(first.text.as_ref(), "ab");
        assert_eq!(first.runs.len(), 1);
        assert_eq!(first.runs[0].font_id, FontId(0));

        // Remainder "cdef" should have two runs: tail of A (1 glyph) + all of B (3 glyphs)
        assert_eq!(remainder.text.as_ref(), "cdef");
        assert_eq!(remainder.runs.len(), 2);
        assert_eq!(remainder.runs[0].font_id, FontId(0));
        assert_eq!(remainder.runs[0].glyphs.len(), 1);
        assert_eq!(remainder.runs[0].glyphs[0].index, 0);
        assert_eq!(remainder.runs[0].glyphs[0].position.x, px(0.0));
        assert_eq!(remainder.runs[1].font_id, FontId(1));
        assert_eq!(remainder.runs[1].glyphs[0].index, 1);
        assert_eq!(remainder.runs[1].glyphs[0].position.x, px(10.0));

        // Second split at byte 2 within remainder — crosses the run boundary
        let (second, final_part) = remainder.split_at(2);
        assert_eq!(second.text.as_ref(), "cd");
        assert_eq!(final_part.text.as_ref(), "ef");
        assert_eq!(final_part.runs[0].glyphs[0].index, 0);
        assert_eq!(final_part.runs[0].glyphs[0].position.x, px(0.0));

        // Widths must sum across all three pieces
        assert_eq!(
            first.width() + second.width() + final_part.width(),
            line.width()
        );
    }

    #[test]
    fn split_at_handles_descending_bidi_glyph_indices() {
        let text = "abc אבג xyz";
        let glyphs = [
            (0, 0.0),
            (1, 1.0),
            (2, 2.0),
            (3, 3.0),
            // The RTL run is visually ordered but its logical byte indices descend.
            (8, 4.0),
            (6, 5.0),
            (4, 6.0),
            (10, 7.0),
            (11, 8.0),
            (12, 9.0),
            (13, 10.0),
        ]
        .into_iter()
        .map(|(index, x)| ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), Pixels::ZERO),
            index,
            is_emoji: false,
        })
        .collect();
        let upstream = CaretAffinity::Upstream;
        let downstream = CaretAffinity::Downstream;
        let ltr = TextDirection::LeftToRight;
        let rtl = TextDirection::RightToLeft;
        let caret_stops = [
            (0, downstream, ltr, 0.0),
            (1, downstream, ltr, 1.0),
            (2, downstream, ltr, 2.0),
            (3, downstream, ltr, 3.0),
            (4, downstream, rtl, 7.0),
            (6, upstream, rtl, 5.0),
            (6, downstream, rtl, 5.0),
            (8, upstream, rtl, 4.0),
            (10, upstream, rtl, 4.0),
            (10, downstream, ltr, 7.0),
            (11, downstream, ltr, 8.0),
            (12, downstream, ltr, 9.0),
            (13, downstream, ltr, 10.0),
            (14, upstream, ltr, 11.0),
        ]
        .into_iter()
        .map(|(index, affinity, direction, x)| CaretStop {
            index,
            affinity,
            direction,
            x: px(x),
        })
        .collect();
        let line = ShapedLine {
            layout: Arc::new(LineLayout {
                font_size: px(16.0),
                width: px(11.0),
                ascent: px(12.0),
                descent: px(4.0),
                minimum_line_height: Pixels::ZERO,
                runs: vec![ShapedRun {
                    font_id: FontId(0),
                    font_size: px(16.0),
                    baseline_shift: Pixels::ZERO,
                    resolved_face: None,
                    glyphs,
                }],
                caret_stops,
                generated_caret_stops: Default::default(),
                len: text.len(),
            }),
            text: text.into(),
            decoration_runs: SmallVec::new(),
        };

        assert_eq!(line.layout.x_for_index(6), px(5.0));
        let (left, right) = line.split_at(6);

        assert_eq!(left.text.as_ref(), "abc א");
        assert_eq!(right.text.as_ref(), "בג xyz");
        assert_eq!(
            right.runs[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.index)
                .collect::<Vec<_>>(),
            vec![2, 0, 4, 5, 6, 7]
        );
        assert!(
            right.runs[0]
                .glyphs
                .iter()
                .all(|glyph| glyph.position.x >= Pixels::ZERO)
        );
        assert!(
            right
                .caret_stops()
                .iter()
                .all(|stop| stop.index <= right.len())
        );
    }

    #[test]
    fn placement_and_paint_payload_reuse_the_same_geometry() {
        let line = make_shaped_line("ab", &[(0, 0.0), (1, 10.0)], 20.0, &[]);
        let geometry = line.geometry();
        let placement = line.place(
            point(px(10.0), px(20.0)),
            px(18.0),
            TextAlign::Right,
            Some(px(100.0)),
        );

        assert_eq!(placement.content_origin(), point(px(90.0), px(20.0)));
        assert_eq!(
            placement.viewport_x_for_caret(
                1,
                CaretAffinity::Downstream,
                TextDirection::LeftToRight
            ),
            Some(px(100.0))
        );

        let first_paint = LinePaint::new(
            2,
            [DecorationRun {
                len: 2,
                color: black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
        )
        .expect("valid paint coverage");
        let second_paint = LinePaint::new(
            2,
            [
                DecorationRun {
                    len: 1,
                    color: black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                },
                DecorationRun {
                    len: 1,
                    color: crate::red(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                },
            ],
        )
        .expect("valid replacement paint coverage");

        assert_eq!(first_paint.len(), second_paint.len());
        assert!(Arc::ptr_eq(&geometry, &placement.layout));
        assert!(Arc::ptr_eq(&geometry, &line.geometry()));
        assert!(LinePaint::new(2, Vec::<DecorationRun>::new()).is_err());
    }

    #[test]
    fn decoration_lookup_follows_logical_indices_in_bidi_visual_order() {
        let first = crate::red();
        let second = crate::blue();
        let decorations = [
            DecorationRun {
                len: 3,
                color: first,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            DecorationRun {
                len: 3,
                color: second,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
        ];
        let mut lookup = DecorationRunLookup::new(&decorations);

        let visual_order = [0, 1, 5, 4, 3, 2];
        let colors = visual_order
            .into_iter()
            .map(|index| lookup.run_for_index(index).unwrap().1.color)
            .collect::<Vec<_>>();

        assert_eq!(colors, [first, first, second, second, second, first]);
        assert!(lookup.run_for_index(6).is_none());
    }

    #[test]
    fn test_split_at_decorations() {
        // Three decoration runs: red [0..2), green [2..5), blue [5..6).
        // Split at byte 3 — red goes entirely left, green straddles, blue goes entirely right.
        let red = Hsla {
            h: 0.0,
            s: 1.0,
            l: 0.5,
            a: 1.0,
        };
        let green = Hsla {
            h: 0.3,
            s: 1.0,
            l: 0.5,
            a: 1.0,
        };
        let blue = Hsla {
            h: 0.6,
            s: 1.0,
            l: 0.5,
            a: 1.0,
        };

        let line = make_shaped_line(
            "abcdef",
            &[
                (0, 0.0),
                (1, 10.0),
                (2, 20.0),
                (3, 30.0),
                (4, 40.0),
                (5, 50.0),
            ],
            60.0,
            &[
                DecorationRun {
                    len: 2,
                    color: red,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                },
                DecorationRun {
                    len: 3,
                    color: green,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                },
                DecorationRun {
                    len: 1,
                    color: blue,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                },
            ],
        );

        let (left, right) = line.split_at(3);

        // Left: red(2) + green(1) — green straddled, left portion has len 1
        assert_eq!(left.decoration_runs.len(), 2);
        assert_eq!(left.decoration_runs[0].len, 2);
        assert_eq!(left.decoration_runs[0].color, red);
        assert_eq!(left.decoration_runs[1].len, 1);
        assert_eq!(left.decoration_runs[1].color, green);

        // Right: green(2) + blue(1) — green straddled, right portion has len 2
        assert_eq!(right.decoration_runs.len(), 2);
        assert_eq!(right.decoration_runs[0].len, 2);
        assert_eq!(right.decoration_runs[0].color, green);
        assert_eq!(right.decoration_runs[1].len, 1);
        assert_eq!(right.decoration_runs[1].color, blue);
    }
}
