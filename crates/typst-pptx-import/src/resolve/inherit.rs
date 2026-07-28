//! slide → layout → master placeholder inheritance.
//!
//! A placeholder shape on a slide usually states only its text. Its position,
//! size, font, size, colour and bullet style all come from the placeholder it
//! *matches* on the layout, and from that one's match on the master. Matching
//! is by `p:ph`'s `idx` when there is one and by `type` otherwise — the two
//! keys PowerPoint itself uses, in that order, because a layout with two body
//! placeholders distinguishes them by `idx` alone.

use crate::pml::model::*;

/// Everything a placeholder inherits, already flattened.
#[derive(Debug, Default)]
pub struct Inherited {
    pub xfrm: Option<Xfrm>,
    /// Defaults per outline level, level 0 first.
    pub levels: Vec<LevelStyle>,
    pub anchor: Option<EcoStringAlias>,
    pub insets: Option<Insets>,
}

pub type EcoStringAlias = ecow::EcoString;

/// Resolve one slide shape's inherited properties.
///
/// `None` when the shape is not a placeholder — a free-floating text box
/// inherits nothing, which is why decks that avoid placeholders survive a
/// naive importer and placeholder-based ones do not.
pub fn resolve(
    shape: &TextShape,
    layout: Option<&SlideLayout>,
    masters: &[SlideMaster],
) -> Option<Inherited> {
    let ph = shape.placeholder.as_ref()?;
    let mut out = Inherited::default();

    let master = layout.and_then(|l| l.master).and_then(|i| masters.get(i));

    // Layout first: it is the nearer ancestor, so what it states wins over the
    // master. `find_placeholder` walks groups too, since a layout may nest its
    // placeholders inside one.
    if let Some(layout) = layout
        && let Some(matched) = find_placeholder(&layout.shapes, ph)
    {
        out.xfrm = matched.xfrm;
        out.anchor = matched.anchor.clone();
        out.insets = Some(matched.insets);
    }
    if out.xfrm.is_none()
        && let Some(master) = master
        && let Some(matched) = find_placeholder(&master.shapes, ph)
    {
        out.xfrm = matched.xfrm;
        out.anchor = out.anchor.or_else(|| matched.anchor.clone());
        out.insets = out.insets.or(Some(matched.insets));
    }

    // Text defaults come from the master's `p:txStyles`, chosen by the
    // placeholder's *role*: a title placeholder takes titleStyle, body and
    // object take bodyStyle, everything else otherStyle.
    if let Some(master) = master {
        let styles = &master.text_styles;
        out.levels = if ph.kind.is_title() {
            styles.title.clone()
        } else if matches!(ph.kind, PhKind::Body | PhKind::Object | PhKind::Subtitle) {
            styles.body.clone()
        } else {
            styles.other.clone()
        };
    }

    // The layout's own placeholder sits *between* the slide and the master, so
    // its `a:lstStyle` overlays the master's defaults. This is the link that
    // carries "the title on this layout is right-aligned": read only the
    // master and every slide using the layout comes out left-aligned.
    if let Some(layout) = layout
        && let Some(matched) = find_placeholder(&layout.shapes, ph)
        && !matched.list_style.is_empty()
    {
        out.levels = overlay(&matched.list_style, &out.levels);
    }

    Some(out)
}

/// Layer one level-style list over another, level by level.
fn overlay(over: &[LevelStyle], base: &[LevelStyle]) -> Vec<LevelStyle> {
    let n = over.len().max(base.len());
    (0..n)
        .map(|i| {
            let empty = LevelStyle::default();
            let o = over.get(i).unwrap_or(&empty);
            let b = base.get(i).unwrap_or(&empty);
            LevelStyle {
                run: o.run.over(&b.run),
                para: o.para.over(&b.para),
            }
        })
        .collect()
}

/// The placeholder in `shapes` that `want` inherits from.
///
/// Two-pass on purpose. An exact `idx` match is authoritative; only when there
/// is none does a type match apply, and a title on a layout is spelled
/// `title` or `ctrTitle` interchangeably.
fn find_placeholder<'a>(
    shapes: &'a [Shape],
    want: &Placeholder,
) -> Option<&'a TextShape> {
    if want.idx.is_some()
        && let Some(found) = walk(shapes, &|ph| ph.idx == want.idx && ph.idx.is_some())
    {
        return Some(found);
    }
    walk(shapes, &|ph| {
        ph.kind == want.kind || (ph.kind.is_title() && want.kind.is_title())
    })
}

/// Depth-first search through the shape tree, groups included.
///
/// `pred` is a `&dyn Fn` rather than a generic: this recurses, and a generic
/// recursive function monomorphizes into an unbounded family of copies.
fn walk<'a>(
    shapes: &'a [Shape],
    pred: &dyn Fn(&Placeholder) -> bool,
) -> Option<&'a TextShape> {
    for shape in shapes {
        match shape {
            Shape::Text(text) => {
                if let Some(ph) = &text.placeholder
                    && pred(ph)
                {
                    return Some(text);
                }
            }
            Shape::Group(group) => {
                if let Some(found) = walk(&group.shapes, pred) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// The run properties a paragraph at `level` should start from.
pub fn level_run_props(inherited: &Inherited, level: u8) -> RunProps {
    inherited
        .levels
        .get(level as usize)
        .map(|l| l.run.clone())
        .unwrap_or_default()
}

/// The paragraph properties a paragraph at `level` should start from.
pub fn level_para_props(inherited: &Inherited, level: u8) -> ParaProps {
    inherited
        .levels
        .get(level as usize)
        .map(|l| l.para.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph_shape(kind: PhKind, idx: Option<u32>, x: i64) -> Shape {
        Shape::Text(TextShape {
            placeholder: Some(Placeholder { kind, idx }),
            xfrm: Some(Xfrm { x, y: 0, cx: 100, cy: 100, ..Xfrm::default() }),
            ..TextShape::default()
        })
    }

    #[test]
    fn an_idx_match_beats_a_type_match() {
        // Two body placeholders: only `idx` tells them apart, and picking the
        // wrong one puts the text in the other half of the slide.
        let layout = SlideLayout {
            shapes: vec![
                ph_shape(PhKind::Body, Some(1), 10),
                ph_shape(PhKind::Body, Some(2), 900),
            ],
            master: None,
            ..SlideLayout::default()
        };
        let slide_shape = TextShape {
            placeholder: Some(Placeholder { kind: PhKind::Body, idx: Some(2) }),
            ..TextShape::default()
        };
        let resolved = resolve(&slide_shape, Some(&layout), &[]).unwrap();
        assert_eq!(resolved.xfrm.unwrap().x, 900);
    }

    #[test]
    fn a_title_matches_a_centred_title_placeholder() {
        // PowerPoint uses `ctrTitle` on the title layout and `title`
        // elsewhere; a slide saying one must still find the other.
        let layout = SlideLayout {
            shapes: vec![ph_shape(PhKind::CtrTitle, None, 42)],
            ..SlideLayout::default()
        };
        let slide_shape = TextShape {
            placeholder: Some(Placeholder { kind: PhKind::Title, idx: None }),
            ..TextShape::default()
        };
        let resolved = resolve(&slide_shape, Some(&layout), &[]).unwrap();
        assert_eq!(resolved.xfrm.unwrap().x, 42);
    }

    #[test]
    fn a_placeholder_nested_in_a_group_is_still_found() {
        let layout = SlideLayout {
            shapes: vec![Shape::Group(Group {
                shapes: vec![ph_shape(PhKind::Body, None, 77)],
                ..Group::default()
            })],
            ..SlideLayout::default()
        };
        let slide_shape = TextShape {
            placeholder: Some(Placeholder { kind: PhKind::Body, idx: None }),
            ..TextShape::default()
        };
        let resolved = resolve(&slide_shape, Some(&layout), &[]).unwrap();
        assert_eq!(resolved.xfrm.unwrap().x, 77);
    }

    #[test]
    fn a_non_placeholder_inherits_nothing() {
        assert!(resolve(&TextShape::default(), None, &[]).is_none());
    }
}
