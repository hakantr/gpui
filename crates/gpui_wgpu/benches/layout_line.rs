// Host-only: Criterion's default Rayon path rejects wasm, so the wasm `--all-targets` scope
// compiles this target as an empty binary instead of pulling the host bench harness in.
#[cfg(not(target_family = "wasm"))]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(not(target_family = "wasm"))]
use gpui::{FontFallbacks, FontRun, PlatformTextSystem, RichFontRun, font, px};
#[cfg(not(target_family = "wasm"))]
use gpui_wgpu::CosmicTextSystem;
#[cfg(not(target_family = "wasm"))]
use std::borrow::Cow;
#[cfg(not(target_family = "wasm"))]
use std::hint::black_box;

#[cfg(not(target_family = "wasm"))]
const LILEX: &[u8] = include_bytes!("../../../assets/fonts/lilex/Lilex-Regular.ttf");
#[cfg(not(target_family = "wasm"))]
const IBM_PLEX: &[u8] =
    include_bytes!("../../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");

// ~4 000 chars of typical ASCII code text, as a single display line.
//
// `layout_line` is handed one line at a time, already split on `\n` by its
// callers, so the newlines are replaced rather than kept: leaving them in would
// make this measure the multi-paragraph path instead of the common one, since
// `\n` is itself a bidi paragraph separator.
#[cfg(not(target_family = "wasm"))]
fn code_text() -> String {
    concat!(
        "    fn compute_run_spans(\n",
        "        text: &str,\n",
        "        run_offset: usize,\n",
        "        run_len: usize,\n",
        "        primary: FontId,\n",
        "        fallback_chain: &[(FontId, SharedString)],\n",
        "        covers: &impl Fn(FontId, char) -> bool,\n",
        "    ) -> SmallVec<[RunSpan; 4]> {\n",
        "        let mut spans = SmallVec::new();\n",
        "        let run_end = run_offset + run_len;\n",
        "        if run_end <= run_offset { return spans; }\n",
        "        let run_text = &text[run_offset..run_end];\n",
        "        let mut span_start = run_offset;\n",
        "        let mut span_slot: Option<usize> = None;\n",
        "        for (ch_idx, ch) in run_text.char_indices() {\n",
        "            let abs = run_offset + ch_idx;\n",
        "            let next = pick_covering_slot(ch, span_slot, primary, fallback_chain, covers);\n",
        "            if next == span_slot { continue; }\n",
        "            if abs > span_start {\n",
        "                spans.push(RunSpan { start: span_start, end: abs, slot: span_slot });\n",
        "            }\n",
        "            span_start = abs;\n",
        "            span_slot = next;\n",
        "        }\n",
        "        spans\n",
        "    }\n",
    )
    .repeat(8) // ~3 800 chars
    .replace('\n', " ")
}

#[cfg(not(target_family = "wasm"))]
fn bench_layout_line(c: &mut Criterion) {
    let system = CosmicTextSystem::new_without_system_fonts("Lilex");
    system
        .add_fonts(vec![Cow::Borrowed(LILEX), Cow::Borrowed(IBM_PLEX)])
        .unwrap();

    let font_id_no_fallback = system.font_id(&font("Lilex")).unwrap();

    let font_id_with_fallback = {
        let mut f = font("Lilex");
        f.fallbacks = Some(FontFallbacks::from_fonts(vec!["IBM Plex Sans".to_string()]));
        system.font_id(&f).unwrap()
    };

    let text = code_text();
    let short_text = "Hello, world!";

    // Same text, but with a bidi paragraph separator (U+001C) and RTL content
    // forcing the per-paragraph shaping path.
    let text_mixed_direction = text.clone() + "\u{001c}\u{05d0}\u{05d1}";
    assert!(
        !text.contains('\n'),
        "fast-path corpus must contain no separator"
    );

    let runs_no_fallback = vec![FontRun {
        len: text.len(),
        font_id: font_id_no_fallback,
    }];
    let runs_with_fallback = vec![FontRun {
        len: text.len(),
        font_id: font_id_with_fallback,
    }];
    let runs_mixed_direction = vec![FontRun {
        len: text_mixed_direction.len(),
        font_id: font_id_no_fallback,
    }];
    let runs_short = [FontRun {
        len: short_text.len(),
        font_id: font_id_no_fallback,
    }];
    let quarter = text.len() / 4;
    let rich_runs = [
        RichFontRun {
            len: quarter,
            font_id: font_id_no_fallback,
            font_size: px(12.0),
            minimum_line_height: px(18.0),
            baseline_shift: px(0.0),
        },
        RichFontRun {
            len: quarter,
            font_id: font_id_no_fallback,
            font_size: px(14.0),
            minimum_line_height: px(20.0),
            baseline_shift: px(1.0),
        },
        RichFontRun {
            len: quarter,
            font_id: font_id_no_fallback,
            font_size: px(16.0),
            minimum_line_height: px(22.0),
            baseline_shift: px(-1.0),
        },
        RichFontRun {
            len: text.len() - quarter * 3,
            font_id: font_id_no_fallback,
            font_size: px(18.0),
            minimum_line_height: px(24.0),
            baseline_shift: px(0.0),
        },
    ];
    let rich_homogeneous = [RichFontRun {
        len: text.len(),
        font_id: font_id_no_fallback,
        font_size: px(14.0),
        minimum_line_height: px(14.0),
        baseline_shift: px(0.0),
    }];
    let rich_run_len = text.len() / 64;
    let rich_many_runs = (0..64)
        .map(|index| RichFontRun {
            len: if index == 63 {
                text.len() - rich_run_len * 63
            } else {
                rich_run_len
            },
            font_id: font_id_no_fallback,
            font_size: px(if index % 2 == 0 { 13.0 } else { 15.0 }),
            minimum_line_height: px(if index % 2 == 0 { 18.0 } else { 21.0 }),
            baseline_shift: px((index % 3) as f32 - 1.0),
        })
        .collect::<Vec<_>>();
    let rich_baseline_runs = (0..64)
        .map(|index| RichFontRun {
            len: if index == 63 {
                text.len() - rich_run_len * 63
            } else {
                rich_run_len
            },
            font_id: font_id_no_fallback,
            font_size: px(14.0),
            minimum_line_height: px(18.0),
            baseline_shift: px((index % 3) as f32 - 1.0),
        })
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("layout_line");

    group.bench_function("no_fallback", |b| {
        b.iter(|| system.layout_line(black_box(&text), px(14.0), &runs_no_fallback))
    });

    group.bench_function("with_fallback_ascii", |b| {
        b.iter(|| system.layout_line(black_box(&text), px(14.0), &runs_with_fallback))
    });

    group.bench_function("mixed_direction_paragraphs", |b| {
        b.iter(|| {
            system.layout_line(
                black_box(&text_mixed_direction),
                px(14.0),
                &runs_mixed_direction,
            )
        })
    });

    // cosmic-text's own benchmark matrix includes a small UI-sized string in addition to large
    // documents. Keep this deterministic with the bundled Lilex face; language/emoji corpora need
    // system-dependent fallback fonts and belong in cosmic-text's upstream suite.
    group.bench_function("legacy_short_ascii", |b| {
        b.iter(|| system.layout_line(black_box(short_text), px(14.0), &runs_short))
    });

    group.bench_function("rich_heterogeneous_metrics", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &rich_runs)
                .unwrap()
        })
    });

    group.bench_function("rich_homogeneous_metrics", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &rich_homogeneous)
                .unwrap()
        })
    });

    group.bench_function("rich_64_metric_runs", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &rich_many_runs)
                .unwrap()
        })
    });

    group.bench_function("rich_64_baseline_runs", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &rich_baseline_runs)
                .unwrap()
        })
    });

    group.finish();

    // Warm the physical-face cache before measuring its steady-state lookup cost.
    system.font_face(font_id_no_fallback).unwrap();
    c.bench_function("font_face/cached", |b| {
        b.iter(|| black_box(system.font_face(font_id_no_fallback).unwrap()))
    });
}

#[cfg(not(target_family = "wasm"))]
criterion_group!(benches, bench_layout_line);
#[cfg(not(target_family = "wasm"))]
criterion_main!(benches);

#[cfg(target_family = "wasm")]
fn main() {}
