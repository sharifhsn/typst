use std::cmp::Ordering;

use ecow::EcoString;
use typst_library::layout::{Abs, Point};
use typst_library::text::{FontStyle, TextItem};
use typst_library::visualize::Paint;

use crate::dom::{
    BulletKind, InlineMath, ParaBullet, Placeholder, RunLink, SlideShape, TextBox,
    TextChild, TextField, TextPara, TextRun, TextWrap,
};

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
    pub slide_number: bool,
}

/// One inline math item discovered by the frame walk.
#[derive(Clone)]
pub(crate) struct InlineMathSource {
    pub order: usize,
    pub baseline: Point,
    pub min: Point,
    pub max: Point,
    pub rot_60k: i32,
    pub omml: String,
    pub fallback: EcoString,
}

/// A clustered text box and the walk order of its first contributing item.
pub(crate) struct ClusteredText {
    pub order: usize,
    pub shape: SlideShape,
    max_sz_100pt: i32,
    title_eligible: bool,
}

/// Cluster `FrameItem::Text` entries and inline math by rotation and baseline.
pub(crate) fn cluster_text<'a>(
    text: Vec<TextSource<'a>>,
    math: Vec<InlineMathSource>,
) -> Vec<ClusteredText> {
    let mut emit = Vec::new();
    let mut items = text
        .into_iter()
        .filter(|source| !source.item.text.is_empty())
        .map(FlowItem::Text)
        .chain(math.into_iter().map(FlowItem::Math))
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        item_rot(a)
            .cmp(&item_rot(b))
            .then_with(|| cmp_abs(item_baseline(a).y, item_baseline(b).y))
            .then_with(|| cmp_abs(item_left_x(a), item_left_x(b)))
            .then_with(|| item_order(a).cmp(&item_order(b)))
    });

    let mut class_start = 0;
    while class_start < items.len() {
        let rot = item_rot(&items[class_start]);
        let class_end = items[class_start..]
            .iter()
            .position(|source| item_rot(source) != rot)
            .map_or(items.len(), |pos| class_start + pos);
        cluster_rotation_class(&items[class_start..class_end], &mut emit);
        class_start = class_end;
    }

    mark_placeholders(&mut emit);
    emit.sort_by_key(|cluster| cluster.order);
    emit
}

#[derive(Clone)]
enum FlowItem<'a> {
    Text(TextSource<'a>),
    Math(InlineMathSource),
}

fn cluster_rotation_class(items: &[FlowItem<'_>], emit: &mut Vec<ClusteredText>) {
    let mut lines = Vec::new();
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
            cmp_abs(item_left_x(a), item_left_x(b))
                .then_with(|| item_order(a).cmp(&item_order(b)))
        });

        let mut segment_start = 0;
        while segment_start < line.len() {
            let mut segment_end = segment_start + 1;
            while segment_end < line.len()
                && !is_column_gap(line[segment_end - 1], line[segment_end])
            {
                segment_end += 1;
            }

            if let Some(segment) = build_line_segment(&line[segment_start..segment_end]) {
                lines.push(segment);
            }
            segment_start = segment_end;
        }

        line_start = line_end;
    }

    emit.extend(build_clusters(lines));
}

fn same_baseline_line(a: &FlowItem<'_>, b: &FlowItem<'_>) -> bool {
    let max_size = item_scaled_size(a).max(item_scaled_size(b));
    (item_baseline(a).y - item_baseline(b).y).abs() <= max_size * 0.25
}

fn is_column_gap(a: &FlowItem<'_>, b: &FlowItem<'_>) -> bool {
    let gap = (item_left_x(b) - item_end_x(a)).max(Abs::zero());
    gap > item_scaled_size(a).max(item_scaled_size(b)) * 2.0
}

#[derive(Clone)]
struct LineSegment {
    order: usize,
    left: Abs,
    right: Abs,
    top: Abs,
    bottom: Abs,
    baseline_y: Abs,
    max_size: Abs,
    max_sz_100pt: i32,
    rot_60k: i32,
    children: Vec<TextChild>,
    rtl: bool,
    bullet: Option<LineBullet>,
}

#[derive(Clone)]
struct LineBullet {
    kind: LineBulletKind,
    marker_left: Abs,
    body_left: Abs,
    strip_chars: usize,
}

#[derive(Clone)]
enum LineBulletKind {
    Char(EcoString),
    AutoNum { ty: &'static str, start_at: u32 },
}

struct BulletDetection {
    kind: LineBulletKind,
    strip_chars: usize,
}

fn build_line_segment(segment: &[&FlowItem<'_>]) -> Option<LineSegment> {
    let first = *segment.first()?;
    let max_size = segment
        .iter()
        .map(|source| item_scaled_size(source))
        .max()
        .unwrap_or(Abs::zero());
    let descent = segment
        .iter()
        .map(|source| item_descent(source))
        .max()
        .unwrap_or(Abs::zero());

    let left = segment
        .iter()
        .map(|source| item_left_x(source))
        .min()
        .unwrap_or(item_left_x(first));
    let right = segment
        .iter()
        .map(|source| item_end_x(source))
        .max()
        .unwrap_or(item_left_x(first));
    let top = segment
        .iter()
        .map(|source| item_top(source))
        .min()
        .unwrap_or_else(|| box_top(item_baseline(first).y, max_size));
    let bottom = segment
        .iter()
        .map(|source| item_bottom(source))
        .max()
        .unwrap_or_else(|| top + box_height(max_size, descent));

    let mut children = Vec::new();
    let spc_100pt = segment_tracking(segment);
    for (idx, source) in segment.iter().enumerate() {
        let props = run_props(source, spc_100pt);
        if let Some(prev) = idx.checked_sub(1).map(|idx| segment[idx]) {
            synthesize_gap(prev, source, &props, &mut children);
        }
        match source {
            FlowItem::Text(_) => push_or_merge_run(&mut children, props),
            FlowItem::Math(math) => children.push(TextChild::Math(InlineMath {
                omml: math.omml.clone(),
                fallback: props,
            })),
        }
    }

    if children.is_empty() {
        return None;
    }

    let rtl = segment.iter().any(|source| item_rtl(source));
    let order = segment
        .iter()
        .map(|source| item_order(source))
        .min()
        .unwrap_or(item_order(first));
    let max_sz_100pt = (max_size.to_pt() * 100.0).round() as i32;
    let bullet = detect_bullet(segment, left, max_size, &children);
    Some(LineSegment {
        order,
        left,
        right,
        top,
        bottom,
        baseline_y: item_baseline(first).y,
        max_size,
        max_sz_100pt,
        rot_60k: item_rot(first),
        children,
        rtl,
        bullet,
    })
}

fn build_clusters(mut lines: Vec<LineSegment>) -> Vec<ClusteredText> {
    lines.sort_by(|a, b| {
        cmp_abs(a.baseline_y, b.baseline_y)
            .then_with(|| cmp_abs(a.left, b.left))
            .then_with(|| a.order.cmp(&b.order))
    });

    let mut used = vec![false; lines.len()];
    let mut clusters = Vec::new();
    for idx in 0..lines.len() {
        if used[idx] {
            continue;
        }

        if lines[idx].bullet.is_some() {
            let group = collect_bullet_group(idx, &lines, &used);
            let bullet_count =
                group.iter().filter(|&&idx| lines[idx].bullet.is_some()).count();
            if bullet_count >= 2 {
                for &idx in &group {
                    used[idx] = true;
                }
                clusters.push(build_bullet_box(&group, &lines));
                continue;
            }
        }

        if lines[idx].bullet.is_none() {
            let group = collect_flow_group(idx, &lines, &used);
            if group.len() >= 2 {
                for &idx in &group {
                    used[idx] = true;
                }
                clusters.push(build_flow_box(&group, &lines));
                continue;
            }
        }

        used[idx] = true;
        clusters.push(build_single_line_box(&lines[idx]));
    }

    clusters
}

fn collect_flow_group(start: usize, lines: &[LineSegment], used: &[bool]) -> Vec<usize> {
    let mut group = vec![start];
    let mut leading = None;
    let mut body_left = lines[start].left;

    while let Some(next) = next_following_line(&group, lines, used, |group, candidate| {
        can_follow_flow(group, candidate, leading, body_left)
    }) {
        let prev = *group.last().unwrap();
        let gap = lines[next].baseline_y - lines[prev].baseline_y;
        if group.len() == 1
            && !same_left(lines[start].left, lines[next].left, lines[start].max_size)
        {
            body_left = lines[next].left;
        }
        leading.get_or_insert(gap);
        group.push(next);
    }

    group
}

fn collect_bullet_group(
    start: usize,
    lines: &[LineSegment],
    used: &[bool],
) -> Vec<usize> {
    let mut group = vec![start];
    let mut leading = None;

    while let Some(next) = next_following_line(&group, lines, used, |group, candidate| {
        can_follow_bullet(group, candidate, leading)
    }) {
        let prev = *group.last().unwrap();
        leading.get_or_insert(lines[next].baseline_y - lines[prev].baseline_y);
        group.push(next);
    }

    group
}

fn next_following_line<F>(
    group: &[usize],
    lines: &[LineSegment],
    used: &[bool],
    mut can_follow: F,
) -> Option<usize>
where
    F: FnMut(&[LineSegment], &LineSegment) -> bool,
{
    let current = *group.last()?;
    let current_y = lines[current].baseline_y;
    let grouped = |idx| group.contains(&idx);
    let group_lines = group.iter().map(|&idx| lines[idx].clone()).collect::<Vec<_>>();

    lines
        .iter()
        .enumerate()
        .filter(|(idx, line)| {
            !used[*idx]
                && !grouped(*idx)
                && line.baseline_y > current_y + lines[current].max_size * 0.35
        })
        .filter(|(_, line)| can_follow(&group_lines, line))
        .min_by(|(_, a), (_, b)| {
            cmp_abs(a.baseline_y, b.baseline_y)
                .then_with(|| cmp_abs(a.left, b.left))
                .then_with(|| a.order.cmp(&b.order))
        })
        .map(|(idx, _)| idx)
}

fn can_follow_flow(
    group: &[LineSegment],
    candidate: &LineSegment,
    leading: Option<Abs>,
    body_left: Abs,
) -> bool {
    if candidate.bullet.is_some() {
        return false;
    }

    let prev = group.last().unwrap();
    if !same_line_class(prev, candidate) {
        return false;
    }

    let gap = candidate.baseline_y - prev.baseline_y;
    if !normal_leading(gap, prev.max_size.max(candidate.max_size)) {
        return false;
    }
    if let Some(leading) = leading
        && !same_leading(gap, leading, prev.max_size.max(candidate.max_size))
    {
        return false;
    }

    if same_left(candidate.left, body_left, candidate.max_size) {
        return true;
    }

    group.len() == 1
        && plausible_first_line_indent(group[0].left, candidate.left, candidate.max_size)
}

fn can_follow_bullet(
    group: &[LineSegment],
    candidate: &LineSegment,
    leading: Option<Abs>,
) -> bool {
    let prev = group.last().unwrap();
    if prev.rot_60k != candidate.rot_60k || prev.rtl != candidate.rtl {
        return false;
    }

    let gap = candidate.baseline_y - prev.baseline_y;
    if !normal_leading(gap, prev.max_size.max(candidate.max_size)) {
        return false;
    }
    if let Some(leading) = leading
        && !same_leading(gap, leading, prev.max_size.max(candidate.max_size))
    {
        return false;
    }

    let first_bullet = group.iter().find_map(|line| line.bullet.as_ref());
    match (first_bullet, candidate.bullet.as_ref()) {
        (Some(first), Some(candidate_bullet)) => {
            same_bullet_family(&first.kind, &candidate_bullet.kind)
                && (candidate_bullet.marker_left - first.marker_left).abs()
                    <= candidate.max_size * 8.0
        }
        (Some(_), None) => {
            group.iter().rev().find_map(|line| line.bullet.as_ref()).is_some_and(
                |bullet| {
                    same_left(candidate.left, bullet.body_left, candidate.max_size)
                        && (prev.right - prev.left) >= candidate.max_size * 8.0
                },
            )
        }
        _ => false,
    }
}

fn build_flow_box(group: &[usize], lines: &[LineSegment]) -> ClusteredText {
    let selected = group.iter().map(|&idx| lines[idx].clone()).collect::<Vec<_>>();
    let (left, right, top, bottom) = bounds(&selected);
    let leading = measured_leading(&selected);
    let children = children_from_lines(&selected, right, false);
    let rtl = selected.iter().any(|line| line.rtl);
    let max_sz_100pt = selected.iter().map(|line| line.max_sz_100pt).max().unwrap_or(0);
    let order = selected.iter().map(|line| line.order).min().unwrap_or(0);

    ClusteredText {
        order,
        max_sz_100pt,
        title_eligible: true,
        shape: SlideShape::TextBox(TextBox {
            x_emu: emu(left),
            y_emu: emu(top),
            w_emu: extent_emu((right - left).max(Abs::pt(0.1))),
            h_emu: extent_emu((bottom - top).max(Abs::pt(0.1))),
            rot_60k: selected[0].rot_60k,
            wrap: TextWrap::Square,
            placeholder: None,
            paras: vec![TextPara {
                children,
                rtl,
                line_spacing_100pt: leading_100pt(leading),
                bullet: None,
            }],
        }),
    }
}

fn build_bullet_box(group: &[usize], lines: &[LineSegment]) -> ClusteredText {
    let selected = group.iter().map(|&idx| lines[idx].clone()).collect::<Vec<_>>();
    let (left, right, top, bottom) = bounds(&selected);
    let leading = measured_leading(&selected);
    let levels = bullet_levels(&selected);
    let mut paras = Vec::new();
    let mut current: Vec<LineSegment> = Vec::new();
    let mut current_bullet: Option<LineBullet> = None;

    for line in selected.iter().cloned() {
        if let Some(bullet) = line.bullet.clone() {
            if let Some(prev_bullet) = current_bullet.take() {
                paras.push(bullet_para(
                    &current,
                    &prev_bullet,
                    left,
                    right,
                    leading,
                    &levels,
                ));
                current.clear();
            }
            current_bullet = Some(bullet);
        }
        current.push(line);
    }

    if let Some(bullet) = current_bullet {
        paras.push(bullet_para(&current, &bullet, left, right, leading, &levels));
    }

    let max_sz_100pt = selected.iter().map(|line| line.max_sz_100pt).max().unwrap_or(0);
    let order = selected.iter().map(|line| line.order).min().unwrap_or(0);

    ClusteredText {
        order,
        max_sz_100pt,
        title_eligible: false,
        shape: SlideShape::TextBox(TextBox {
            x_emu: emu(left),
            y_emu: emu(top),
            w_emu: extent_emu((right - left).max(Abs::pt(0.1))),
            h_emu: extent_emu((bottom - top).max(Abs::pt(0.1))),
            rot_60k: selected[0].rot_60k,
            wrap: TextWrap::Square,
            placeholder: None,
            paras,
        }),
    }
}

fn build_single_line_box(line: &LineSegment) -> ClusteredText {
    ClusteredText {
        order: line.order,
        max_sz_100pt: line.max_sz_100pt,
        title_eligible: line.bullet.is_none(),
        shape: SlideShape::TextBox(TextBox {
            x_emu: emu(line.left),
            y_emu: emu(line.top),
            w_emu: extent_emu(((line.right - line.left) * 1.02).max(Abs::pt(0.1))),
            h_emu: extent_emu((line.bottom - line.top).max(Abs::pt(0.1))),
            rot_60k: line.rot_60k,
            wrap: TextWrap::None,
            placeholder: None,
            paras: vec![TextPara {
                children: line.children.clone(),
                rtl: line.rtl,
                line_spacing_100pt: None,
                bullet: None,
            }],
        }),
    }
}

fn bullet_para(
    lines: &[LineSegment],
    bullet: &LineBullet,
    box_left: Abs,
    box_right: Abs,
    leading: Option<Abs>,
    levels: &[Abs],
) -> TextPara {
    let lvl = levels
        .iter()
        .position(|left| same_left(*left, bullet.marker_left, lines[0].max_size))
        .unwrap_or(0)
        .min(8) as u8;
    TextPara {
        children: children_from_lines(lines, box_right, true),
        rtl: lines.iter().any(|line| line.rtl),
        line_spacing_100pt: leading_100pt(leading),
        bullet: Some(ParaBullet {
            lvl,
            mar_l_emu: emu((bullet.body_left - box_left).max(Abs::zero())),
            indent_emu: emu(bullet.marker_left - bullet.body_left),
            kind: bullet_kind(&bullet.kind),
        }),
    }
}

fn children_from_lines(
    lines: &[LineSegment],
    box_right: Abs,
    strip_first_bullet: bool,
) -> Vec<TextChild> {
    let mut children = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            let prev = &lines[idx - 1];
            let separator =
                if hard_line_break(prev, line, box_right) { "\n" } else { " " };
            push_separator(&mut children, line, separator);
        }

        let mut line_children = line.children.clone();
        if idx == 0
            && strip_first_bullet
            && let Some(bullet) = &line.bullet
        {
            strip_prefix_chars(&mut line_children, bullet.strip_chars);
        }
        for child in line_children {
            match child {
                TextChild::Run(run) => push_or_merge_run(&mut children, run),
                TextChild::Math(math) => children.push(TextChild::Math(math)),
            }
        }
    }
    children
}

fn push_separator(children: &mut Vec<TextChild>, line: &LineSegment, separator: &str) {
    let Some(template) = last_run(children).or_else(|| first_run(&line.children)) else {
        return;
    };
    let mut run = template.clone();
    run.text = EcoString::from(separator);
    run.link = None;
    push_or_merge_run(children, run);
}

fn bounds(lines: &[LineSegment]) -> (Abs, Abs, Abs, Abs) {
    let left = lines.iter().map(|line| line.left).min().unwrap_or(Abs::zero());
    let right = lines.iter().map(|line| line.right).max().unwrap_or(left);
    let top = lines.iter().map(|line| line.top).min().unwrap_or(Abs::zero());
    let bottom = lines.iter().map(|line| line.bottom).max().unwrap_or(top);
    (left, right, top, bottom)
}

fn measured_leading(lines: &[LineSegment]) -> Option<Abs> {
    let mut gaps = lines
        .windows(2)
        .map(|pair| pair[1].baseline_y - pair[0].baseline_y)
        .filter(|gap| *gap > Abs::zero());
    let first = gaps.next()?;
    if gaps.all(|gap| same_leading(gap, first, lines[0].max_size)) {
        Some(first)
    } else {
        None
    }
}

/// The measured baseline-to-baseline pitch as an `a:spcPts` value (1/100 pt).
fn leading_100pt(leading: Option<Abs>) -> Option<i32> {
    let pitch = leading?.to_pt();
    if pitch <= 0.0 {
        return None;
    }
    // ST_TextSpacingPoint caps at 158400 (1584 pt).
    Some(((pitch * 100.0).round() as i32).clamp(100, 158_400))
}

fn hard_line_break(prev: &LineSegment, next: &LineSegment, box_right: Abs) -> bool {
    same_left(prev.left, next.left, next.max_size)
        && box_right - prev.right > prev.max_size.max(next.max_size) * 2.0
}

fn same_line_class(a: &LineSegment, b: &LineSegment) -> bool {
    if a.rot_60k != b.rot_60k || a.rtl != b.rtl {
        return false;
    }
    let diff = (a.max_sz_100pt - b.max_sz_100pt).abs();
    diff <= 150.max(a.max_sz_100pt.max(b.max_sz_100pt) / 10)
}

fn normal_leading(gap: Abs, size: Abs) -> bool {
    gap >= size * 0.65 && gap <= size * 1.8
}

fn same_leading(a: Abs, b: Abs, size: Abs) -> bool {
    (a - b).abs() <= (size * 0.20).max(Abs::pt(1.0))
}

fn same_left(a: Abs, b: Abs, size: Abs) -> bool {
    (a - b).abs() <= (size * 0.25).max(Abs::pt(1.0))
}

fn plausible_first_line_indent(first: Abs, second: Abs, size: Abs) -> bool {
    let diff = (first - second).abs();
    diff >= size * 0.5 && diff <= size * 4.0
}

fn bullet_levels(lines: &[LineSegment]) -> Vec<Abs> {
    let mut levels = Vec::new();
    for bullet in lines.iter().filter_map(|line| line.bullet.as_ref()) {
        if !levels
            .iter()
            .any(|left| same_left(*left, bullet.marker_left, lines[0].max_size))
        {
            levels.push(bullet.marker_left);
        }
    }
    levels.sort_by(|a, b| cmp_abs(*a, *b));
    levels.truncate(9);
    levels
}

fn same_bullet_family(a: &LineBulletKind, b: &LineBulletKind) -> bool {
    match (a, b) {
        (LineBulletKind::Char(a), LineBulletKind::Char(b)) => a == b,
        (
            LineBulletKind::AutoNum { ty: a, .. },
            LineBulletKind::AutoNum { ty: b, .. },
        ) => a == b,
        _ => false,
    }
}

fn bullet_kind(kind: &LineBulletKind) -> BulletKind {
    match kind {
        LineBulletKind::Char(ch) => BulletKind::Char(ch.clone()),
        LineBulletKind::AutoNum { ty, start_at } => {
            BulletKind::AutoNum { ty, start_at: *start_at }
        }
    }
}

fn detect_bullet(
    segment: &[&FlowItem<'_>],
    left: Abs,
    max_size: Abs,
    children: &[TextChild],
) -> Option<LineBullet> {
    let text = children
        .iter()
        .map(|child| match child {
            TextChild::Run(run) => run.text.as_str(),
            TextChild::Math(math) => math.fallback.text.as_str(),
        })
        .collect::<String>();
    let detection = parse_bullet_prefix(&text)?;
    let body_left =
        measured_bullet_body_left(segment).unwrap_or_else(|| left + max_size * 1.8);
    Some(LineBullet {
        kind: detection.kind,
        marker_left: left,
        body_left,
        strip_chars: detection.strip_chars,
    })
}

fn parse_bullet_prefix(text: &str) -> Option<BulletDetection> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut idx = 0;
    while chars.get(idx).is_some_and(|ch| ch.is_whitespace()) {
        idx += 1;
    }
    let leading = idx;
    let ch = *chars.get(idx)?;

    let kind = match ch {
        '•' | '‣' | '–' | '-' => {
            idx += 1;
            LineBulletKind::Char(EcoString::from(ch.to_string()))
        }
        ch if ch.is_ascii_digit() => {
            let start = idx;
            while chars.get(idx).is_some_and(|ch| ch.is_ascii_digit()) {
                idx += 1;
            }
            let marker = *chars.get(idx)?;
            let ty = match marker {
                '.' => "arabicPeriod",
                ')' => "arabicParenR",
                _ => return None,
            };
            let number = chars[start..idx].iter().collect::<String>().parse().ok()?;
            idx += 1;
            LineBulletKind::AutoNum { ty, start_at: number }
        }
        ch if ch.is_ascii_alphabetic() => {
            let marker = *chars.get(idx + 1)?;
            let ty = match (ch.is_ascii_uppercase(), marker) {
                (false, ')') => "alphaLcParenR",
                (true, ')') => "alphaUcParenR",
                (false, '.') => "alphaLcPeriod",
                (true, '.') => "alphaUcPeriod",
                _ => return None,
            };
            let lower = ch.to_ascii_lowercase() as u32;
            let start_at = lower.checked_sub('a' as u32)? + 1;
            idx += 2;
            LineBulletKind::AutoNum { ty, start_at }
        }
        _ => return None,
    };

    let marker_end = idx;
    while chars.get(idx).is_some_and(|ch| ch.is_whitespace()) {
        idx += 1;
    }
    (idx > marker_end && idx < chars.len())
        .then_some(BulletDetection { kind, strip_chars: leading + (idx - leading) })
}

fn measured_bullet_body_left(segment: &[&FlowItem<'_>]) -> Option<Abs> {
    let mut saw_marker = false;
    for source in segment {
        let trimmed = match *source {
            FlowItem::Text(source) => source.item.text.trim(),
            FlowItem::Math(math) => math.fallback.trim(),
        };
        if trimmed.is_empty() {
            continue;
        }
        if !saw_marker {
            if marker_only(trimmed) {
                saw_marker = true;
                continue;
            }
            return None;
        }
        return Some(item_left_x(source));
    }
    None
}

fn marker_only(text: &str) -> bool {
    matches!(text, "•" | "‣" | "–" | "-")
        || text.strip_suffix('.').or_else(|| text.strip_suffix(')')).is_some_and(
            |prefix| {
                !prefix.is_empty()
                    && (prefix.chars().all(|ch| ch.is_ascii_digit())
                        || (prefix.len() == 1
                            && prefix.chars().all(|ch| ch.is_ascii_alphabetic())))
            },
        )
}

fn strip_prefix_chars(children: &mut Vec<TextChild>, mut count: usize) {
    for child in children.iter_mut() {
        if count == 0 {
            break;
        }
        let TextChild::Run(run) = child else {
            continue;
        };
        let len = run.text.chars().count();
        if count >= len {
            run.text.clear();
            count -= len;
        } else {
            run.text = run.text.chars().skip(count).collect::<String>().into();
            break;
        }
    }
    children.retain(|child| match child {
        TextChild::Run(run) => !run.text.is_empty(),
        TextChild::Math(_) => true,
    });
}

fn mark_placeholders(clusters: &mut [ClusteredText]) {
    let title = mark_title_placeholder(clusters);
    mark_slide_number_placeholders(clusters);
    mark_body_placeholder(clusters, title);
}

fn mark_title_placeholder(clusters: &mut [ClusteredText]) -> Option<usize> {
    let mut sizes = clusters
        .iter()
        .filter(|cluster| cluster.title_eligible)
        .map(|cluster| cluster.max_sz_100pt)
        .collect::<Vec<_>>();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let &largest = sizes.first()?;
    if largest < 1_400 {
        return None;
    }
    if let Some(&second) = sizes.get(1)
        && largest < second + 200
        && largest * 100 < second * 115
    {
        return None;
    }

    let (idx, _) = clusters
        .iter()
        .enumerate()
        .filter(|(_, cluster)| cluster.title_eligible && cluster.max_sz_100pt == largest)
        .min_by(|(_, a), (_, b)| match (&a.shape, &b.shape) {
            (SlideShape::TextBox(a), SlideShape::TextBox(b)) => {
                a.y_emu.cmp(&b.y_emu).then_with(|| a.x_emu.cmp(&b.x_emu))
            }
            _ => Ordering::Equal,
        })?;

    if let SlideShape::TextBox(text) = &mut clusters[idx].shape {
        text.placeholder = Some(Placeholder::Title);
        text.wrap = TextWrap::Square;
        Some(idx)
    } else {
        None
    }
}

fn mark_slide_number_placeholders(clusters: &mut [ClusteredText]) {
    for cluster in clusters {
        let SlideShape::TextBox(text) = &mut cluster.shape else {
            continue;
        };
        if text.placeholder.is_none() && text_box_is_slide_number(text) {
            text.placeholder = Some(Placeholder::SlideNumber);
            text.wrap = TextWrap::Square;
        }
    }
}

fn mark_body_placeholder(clusters: &mut [ClusteredText], title_idx: Option<usize>) {
    let Some(title_idx) = title_idx else { return };
    let SlideShape::TextBox(title) = &clusters[title_idx].shape else { return };
    let title_bottom = title.y_emu + title.h_emu / 2;

    let mut candidates = clusters
        .iter()
        .enumerate()
        .filter_map(|(idx, cluster)| {
            if idx == title_idx {
                return None;
            }
            let SlideShape::TextBox(text) = &cluster.shape else { return None };
            if text.placeholder.is_some()
                || text.rot_60k != 0
                || text.y_emu < title_bottom
                || !body_placeholder_text_candidate(text)
            {
                return None;
            }
            let area = i128::from(text.w_emu.max(1)) * i128::from(text.h_emu.max(1));
            Some((idx, area, cluster.order))
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return;
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    let selected = match candidates.as_slice() {
        [(idx, _, _)] => Some(*idx),
        [(idx, area, _), (_, next_area, _), ..] if *area >= *next_area * 2 => Some(*idx),
        _ => None,
    };

    if let Some(idx) = selected
        && let SlideShape::TextBox(text) = &mut clusters[idx].shape
    {
        text.placeholder = Some(Placeholder::Body);
        text.wrap = TextWrap::Square;
    }
}

fn text_box_is_slide_number(text: &TextBox) -> bool {
    let mut has_field = false;
    for para in &text.paras {
        for child in &para.children {
            match child {
                TextChild::Run(run) => {
                    if matches!(run.field, Some(TextField::SlideNumber)) {
                        has_field = true;
                    } else if !run.text.trim().is_empty() {
                        return false;
                    }
                }
                TextChild::Math(_) => return false,
            }
        }
    }
    has_field
}

fn body_placeholder_text_candidate(text: &TextBox) -> bool {
    let mut chars = 0usize;
    let mut saw_literal = false;
    for para in &text.paras {
        for child in &para.children {
            match child {
                TextChild::Run(run) => {
                    if run.field.is_some() {
                        continue;
                    }
                    let trimmed = run.text.trim();
                    chars += trimmed.chars().count();
                    saw_literal |= !trimmed.is_empty();
                }
                TextChild::Math(_) => saw_literal = true,
            }
        }
    }
    saw_literal && chars >= 2
}

fn run_props(source: &FlowItem<'_>, spc_100pt: Option<i32>) -> TextRun {
    match source {
        FlowItem::Text(source) => text_run_props(source, spc_100pt),
        FlowItem::Math(math) => math_fallback_run(math, spc_100pt),
    }
}

fn text_run_props(source: &TextSource<'_>, spc_100pt: Option<i32>) -> TextRun {
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
        field: source.slide_number.then_some(TextField::SlideNumber),
    }
}

fn math_fallback_run(math: &InlineMathSource, spc_100pt: Option<i32>) -> TextRun {
    TextRun {
        text: math.fallback.clone(),
        family: EcoString::from("New Computer Modern Math"),
        sz_100pt: (inline_math_size(math).to_pt() * 100.0).round() as i32,
        b: false,
        i: false,
        color: [0, 0, 0, 255],
        spc_100pt,
        link: None,
        field: None,
    }
}

fn push_or_merge_run(children: &mut Vec<TextChild>, run: TextRun) {
    if let Some(TextChild::Run(last)) = children.last_mut()
        && compatible_run(last, &run)
    {
        last.text.push_str(&run.text);
        return;
    }
    children.push(TextChild::Run(run));
}

fn last_run(children: &[TextChild]) -> Option<&TextRun> {
    children.iter().rev().find_map(|child| match child {
        TextChild::Run(run) => Some(run),
        TextChild::Math(_) => None,
    })
}

fn first_run(children: &[TextChild]) -> Option<&TextRun> {
    children.iter().find_map(|child| match child {
        TextChild::Run(run) => Some(run),
        TextChild::Math(_) => None,
    })
}

fn synthesize_gap(
    prev: &FlowItem<'_>,
    source: &FlowItem<'_>,
    props: &TextRun,
    children: &mut Vec<TextChild>,
) {
    let gap = (item_left_x(source) - item_end_x(prev)).max(Abs::zero());
    let size = item_scaled_size(prev).max(item_scaled_size(source));
    if gap < size * 0.15 {
        return;
    }

    let count = if gap <= size * 0.7 {
        1
    } else {
        ((gap.to_pt() / (0.25 * size.to_pt())).round() as usize).max(1)
    };
    push_or_merge_run(
        children,
        TextRun {
            text: EcoString::from(" ".repeat(count)),
            family: props.family.clone(),
            sz_100pt: props.sz_100pt,
            b: props.b,
            i: props.i,
            color: props.color,
            spc_100pt: props.spc_100pt,
            link: None,
            field: None,
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
        && a.field == b.field
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

fn segment_tracking(segment: &[&FlowItem<'_>]) -> Option<i32> {
    if segment.len() < 2 {
        return None;
    }

    let mut text_items = Vec::new();
    for source in segment {
        match *source {
            FlowItem::Text(text) => text_items.push(text),
            FlowItem::Math(_) => return None,
        }
    }
    if text_items.len() < 2 {
        return None;
    }

    let left = text_items.first()?.baseline.x;
    let right = text_item_end_x(text_items.last()?);
    let typst_segment_width = right - left;
    let item_width = text_items.iter().map(|source| scaled_width(source)).sum::<Abs>();
    let char_count = text_items
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

fn item_order(source: &FlowItem<'_>) -> usize {
    match source {
        FlowItem::Text(source) => source.order,
        FlowItem::Math(math) => math.order,
    }
}

fn item_baseline(source: &FlowItem<'_>) -> Point {
    match source {
        FlowItem::Text(source) => source.baseline,
        FlowItem::Math(math) => math.baseline,
    }
}

fn item_rot(source: &FlowItem<'_>) -> i32 {
    match source {
        FlowItem::Text(source) => source.rot_60k,
        FlowItem::Math(math) => math.rot_60k,
    }
}

fn item_left_x(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => source.baseline.x,
        FlowItem::Math(math) => math.min.x,
    }
}

fn item_end_x(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => text_item_end_x(source),
        FlowItem::Math(math) => math.max.x,
    }
}

fn item_top(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => box_top(source.baseline.y, scaled_size(source)),
        FlowItem::Math(math) => math.min.y,
    }
}

fn item_bottom(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => {
            source.baseline.y
                + (-source.item.font.metrics().descender).at(scaled_size(source))
        }
        FlowItem::Math(math) => math.max.y,
    }
}

fn item_descent(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => {
            (-source.item.font.metrics().descender).at(scaled_size(source))
        }
        FlowItem::Math(math) => (math.max.y - math.baseline.y).max(Abs::zero()),
    }
}

fn item_scaled_size(source: &FlowItem<'_>) -> Abs {
    match source {
        FlowItem::Text(source) => scaled_size(source),
        FlowItem::Math(math) => inline_math_size(math),
    }
}

fn item_rtl(source: &FlowItem<'_>) -> bool {
    match source {
        FlowItem::Text(source) => {
            matches!(source.item.lang.dir(), typst_library::layout::Dir::RTL)
        }
        FlowItem::Math(_) => false,
    }
}

fn inline_math_size(math: &InlineMathSource) -> Abs {
    (math.max.y - math.min.y).max(Abs::pt(0.1))
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

fn text_item_end_x(source: &TextSource<'_>) -> Abs {
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
