use criterion::{Criterion, criterion_group, criterion_main};
use gpui::{FontRun, Platform, RichFontRun, font, px};
use gpui_macos::MacPlatform;
use std::hint::black_box;

fn bench_layout_rich_line(c: &mut Criterion) {
    let platform = MacPlatform::new(true);
    let system = platform.text_system();
    let font_id = system.font_id(&font("Helvetica")).unwrap();
    let text = "office affine AVATAR 0123456789; ".repeat(112);
    let legacy_runs = [FontRun {
        len: text.len(),
        font_id,
    }];
    let homogeneous_runs = [RichFontRun {
        len: text.len(),
        font_id,
        font_size: px(14.0),
        minimum_line_height: px(18.0),
        baseline_shift: px(0.0),
    }];
    let run_len = text.len() / 64;
    let many_runs = (0..64)
        .map(|index| RichFontRun {
            len: if index == 63 {
                text.len() - run_len * 63
            } else {
                run_len
            },
            font_id,
            font_size: px(14.0),
            minimum_line_height: px(18.0),
            baseline_shift: px((index % 3) as f32 - 1.0),
        })
        .collect::<Vec<_>>();

    let mut group = c.benchmark_group("layout_rich_line");
    group.bench_function("legacy", |b| {
        b.iter(|| system.layout_line(black_box(&text), px(14.0), &legacy_runs))
    });
    group.bench_function("homogeneous", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &homogeneous_runs)
                .unwrap()
        })
    });
    group.bench_function("64_baseline_runs", |b| {
        b.iter(|| {
            system
                .layout_rich_line(black_box(&text), &many_runs)
                .unwrap()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_layout_rich_line);
criterion_main!(benches);
