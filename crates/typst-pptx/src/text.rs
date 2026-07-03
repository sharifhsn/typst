use std::cmp::Ordering;

use ecow::EcoString;
use typst_library::layout::{Abs, Point};
use typst_library::text::{FontStyle, TextItem};
use typst_library::visualize::Paint;

use crate::dom::{RunLink, SlideShape, TextBox, TextPara, TextRun};

/// Cloneable link target used before lowering into the frozen DOM.
#[derive(Clone)]
pub(crate) enum LinkTarget {
    Url(EcoString),
    Slide(usize),
}

/// One text item discovered by the frame walk.
#[derive(Clone)]
pub(crate) struct TextSource<'a> {
    pub order: usize,
    pub baseline: Point,
    pub item: &'a TextItem,
    pub rot_60k: i32,
    pub scale: f64,
    pub link: Option<LinkTarget>,
}

/// A clustered text box and the walk order of its first contributing item.
pub(crate) struct ClusteredText {
    pub order: usize,
    pub shape: SlideShape,
}

/// Cluster `FrameItem::Text` entries by rotation and baseline.
pub(crate) fn cluster_text(items: Vec<TextSource<'_>>) -> Vec<ClusteredText> {
    let mut emit = Vec::new();
    let mut items = items
        .into_iter()
        .filter(|source| !source.item.text.is_empty())
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        a.rot_60k
            .cmp(&b.rot_60k)
            .then_with(|| cmp_abs(a.baseline.y, b.baseline.y))
            .then_with(|| cmp_abs(a.baseline.x, b.baseline.x))
            .then_with(|| a.order.cmp(&b.order))
    });

    let mut class_start = 0;
    while class_start < items.len() {
        let rot = items[class_start].rot_60k;
        let class_end = items[class_start..]
            .iter()
            .position(|source| source.rot_60k != rot)
            .map_or(items.len(), |pos| class_start + pos);
        cluster_rotation_class(&items[class_start..class_end], &mut emit);
        class_start = class_end;
    }

    emit.sort_by_key(|cluster| cluster.order);
    emit
}

fn cluster_rotation_class(items: &[TextSource<'_>], emit: &mut Vec<ClusteredText>) {
    let mut line_start = 0;
    while line_start < items.len() {
        let mut line_end = line_start + 1;
        while line_end < items.len()
            && same_baseline_line(&items[line_end - 1], &items[line_end])
        {
            line_end += 1;
        }

        let mut line = items[line_start..line_end].iter().collect::<Vec<_>>();
        line.sort_by(|a, b| {
            cmp_abs(a.baseline.x, b.baseline.x).then_with(|| a.order.cmp(&b.order))
        });

        let mut segment_start = 0;
        while segment_start < line.len() {
            let mut segment_end = segment_start + 1;
            while segment_end < line.len()
                && !is_column_gap(line[segment_end - 1], line[segment_end])
            {
                segment_end += 1;
            }

            if let Some(cluster) = build_segment(&line[segment_start..segment_end]) {
                emit.push(cluster);
            }
            segment_start = segment_end;
        }

        line_start = line_end;
    }
}

fn same_baseline_line(a: &TextSource<'_>, b: &TextSource<'_>) -> bool {
    let max_size = scaled_size(a).max(scaled_size(b));
    (a.baseline.y - b.baseline.y).abs() <= max_size * 0.25
}

fn is_column_gap(a: &TextSource<'_>, b: &TextSource<'_>) -> bool {
    let gap = (b.baseline.x - item_end_x(a)).max(Abs::zero());
    gap > scaled_size(a).max(scaled_size(b)) * 2.0
}

fn build_segment(segment: &[&TextSource<'_>]) -> Option<ClusteredText> {
    let first = *segment.first()?;
    let max_size = segment
        .iter()
        .map(|source| scaled_size(source))
        .max()
        .unwrap_or(Abs::zero());
    let descent = segment
        .iter()
        .map(|source| (-source.item.font.metrics().descender).at(scaled_size(source)))
        .max()
        .unwrap_or(Abs::zero());

    let left = segment
        .iter()
        .map(|source| source.baseline.x)
        .min()
        .unwrap_or(first.baseline.x);
    let right = segment
        .iter()
        .map(|source| item_end_x(source))
        .max()
        .unwrap_or(first.baseline.x);
    let width = ((right - left) * 1.02).max(Abs::pt(0.1));
    let height = box_height(max_size, descent);
    let top = box_top(first.baseline.y, max_size);

    let mut runs = Vec::new();
    let spc_100pt = segment_tracking(segment);
    for (idx, source) in segment.iter().enumerate() {
        let props = run_props(source, spc_100pt);
        if let Some(prev) = idx.checked_sub(1).map(|idx| segment[idx]) {
            synthesize_gap(prev, source, &props, &mut runs);
        }
        push_or_merge_run(&mut runs, props);
    }

    if runs.is_empty() {
        return None;
    }

    let rtl = segment
        .iter()
        .any(|source| matches!(source.item.lang.dir(), typst_library::layout::Dir::RTL));
    let order = segment.iter().map(|source| source.order).min().unwrap_or(first.order);
    Some(ClusteredText {
        order,
        shape: SlideShape::TextBox(TextBox {
            x_emu: emu(left),
            y_emu: emu(top),
            w_emu: extent_emu(width),
            h_emu: extent_emu(height),
            rot_60k: first.rot_60k,
            paras: vec![TextPara { runs, rtl }],
        }),
    })
}

fn run_props(source: &TextSource<'_>, spc_100pt: Option<i32>) -> TextRun {
    let variant = source.item.font.font().info().variant;
    TextRun {
        text: source.item.text.clone(),
        family: EcoString::from(source.item.font.font().info().family.as_str()),
        sz_100pt: (scaled_size(source).to_pt() * 100.0).round() as i32,
        b: variant.weight.to_number() >= 600,
        i: matches!(variant.style, FontStyle::Italic | FontStyle::Oblique),
        color: text_color(&source.item.fill),
        spc_100pt,
        link: source.link.as_ref().map(|link| match link {
            LinkTarget::Url(url) => RunLink::Url(url.clone()),
            LinkTarget::Slide(slide) => RunLink::Slide(*slide),
        }),
    }
}

fn push_or_merge_run(runs: &mut Vec<TextRun>, run: TextRun) {
    if let Some(last) = runs.last_mut()
        && compatible_run(last, &run)
    {
        last.text.push_str(&run.text);
        return;
    }
    runs.push(run);
}

fn synthesize_gap(
    prev: &TextSource<'_>,
    source: &TextSource<'_>,
    props: &TextRun,
    runs: &mut Vec<TextRun>,
) {
    let gap = (source.baseline.x - item_end_x(prev)).max(Abs::zero());
    let size = scaled_size(prev).max(scaled_size(source));
    if gap < size * 0.15 {
        return;
    }

    let count = if gap <= size * 0.7 {
        1
    } else {
        ((gap.to_pt() / (0.25 * size.to_pt())).round() as usize).max(1)
    };
    push_or_merge_run(
        runs,
        TextRun {
            text: EcoString::from(" ".repeat(count)),
            family: props.family.clone(),
            sz_100pt: props.sz_100pt,
            b: props.b,
            i: props.i,
            color: props.color,
            spc_100pt: props.spc_100pt,
            link: None,
        },
    );
}

fn compatible_run(a: &TextRun, b: &TextRun) -> bool {
    a.family == b.family
        && a.sz_100pt == b.sz_100pt
        && a.b == b.b
        && a.i == b.i
        && a.color == b.color
        && a.spc_100pt == b.spc_100pt
        && same_link(&a.link, &b.link)
}

fn same_link(a: &Option<RunLink>, b: &Option<RunLink>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(RunLink::Url(a)), Some(RunLink::Url(b))) => a == b,
        (Some(RunLink::Slide(a)), Some(RunLink::Slide(b))) => a == b,
        _ => false,
    }
}

fn segment_tracking(segment: &[&TextSource<'_>]) -> Option<i32> {
    if segment.len() < 2 {
        return None;
    }

    let left = segment.first()?.baseline.x;
    let right = item_end_x(segment.last()?);
    let typst_segment_width = right - left;
    let item_width = segment.iter().map(|source| scaled_width(source)).sum::<Abs>();
    let char_count = segment
        .iter()
        .map(|source| source.item.text.chars().count())
        .sum::<usize>();
    if char_count == 0 {
        return None;
    }

    let correction = (typst_segment_width - item_width).to_pt() / char_count as f64;
    if correction.abs() > 1.5 || correction.abs() < 0.01 {
        None
    } else {
        Some((correction * 100.0).round() as i32)
    }
}

/// A representative solid color for a text run.
///
/// A DrawingML text run can only carry a solid color, so a gradient or tiling
/// text fill is approximated rather than dropped — keeping the text visible
/// (the earlier behavior discarded the whole cluster on any non-solid glyph).
/// The first gradient stop reads closest to the intended look for the common
/// gradient-title case.
fn text_color(fill: &Paint) -> [u8; 4] {
    match fill {
        Paint::Solid(color) => crate::shape::srgb_bytes(color),
        Paint::Gradient(gradient) => gradient
            .stops_ref()
            .first()
            .map(|(color, _)| crate::shape::srgb_bytes(color))
            .unwrap_or([0, 0, 0, 255]),
        Paint::Tiling(_) => [0, 0, 0, 255],
    }
}

fn item_end_x(source: &TextSource<'_>) -> Abs {
    source.baseline.x + scaled_width(source)
}

fn scaled_width(source: &TextSource<'_>) -> Abs {
    source.item.width() * source.scale
}

fn scaled_size(source: &TextSource<'_>) -> Abs {
    source.item.size * source.scale.abs()
}

/// Measured baseline rule: first baseline sits at box top + max font size.
pub(crate) fn box_top(baseline: Abs, max_size: Abs) -> Abs {
    baseline - max_size
}

/// Box height uses max size plus descender; position never uses ascent.
pub(crate) fn box_height(max_size: Abs, descent: Abs) -> Abs {
    (max_size + descent).max(Abs::pt(0.1))
}

pub(crate) fn emu(abs: Abs) -> i64 {
    let value = abs.to_pt() * 12700.0;
    if value.is_finite() {
        value.clamp(i64::MIN as f64, i64::MAX as f64) as i64
    } else if value.is_sign_negative() {
        i64::MIN
    } else {
        i64::MAX
    }
}

pub(crate) fn extent_emu(abs: Abs) -> i64 {
    emu(abs).max(1)
}

fn cmp_abs(a: Abs, b: Abs) -> Ordering {
    a.to_raw().partial_cmp(&b.to_raw()).unwrap_or(Ordering::Equal)
}
