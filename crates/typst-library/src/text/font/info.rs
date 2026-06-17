use std::fmt::{self, Debug, Formatter};

use serde::{Deserialize, Serialize};
use skrifa::raw::tables::os2::SelectionFlags;
use skrifa::raw::{FileRef, TableProvider};
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider, Tag as SkrifaTag};

use super::find_exception;
use crate::text::{
    AxisValue, FontAxis, FontStretch, FontStyle, FontVariant, FontWeight, Tag,
};

/// Properties of a single font.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
pub struct FontInfo {
    /// The typographic font family this font is part of.
    pub family: String,
    /// Properties that distinguish this font from other fonts in the same
    /// family. For a variable font, this designates the default instance.
    pub variant: FontVariant,
    /// Properties of the font.
    pub flags: FontFlags,
    /// Variation axes this font supports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<FontAxis>,
    /// The unicode coverage of the font.
    pub coverage: Coverage,
}

bitflags::bitflags! {
    /// Bitflags describing characteristics of a font.
    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    #[derive(Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct FontFlags: u32 {
        /// All glyphs have the same width.
        const MONOSPACE = 1 << 0;
        /// Glyphs have short strokes at their stems.
        const SERIF = 1 << 1;
        /// Font face has a MATH table
        const MATH = 1 << 2;
        /// Font face has an fvar table
        const VARIABLE = 1 << 3;
    }
}

impl FontInfo {
    /// Compute metadata for font at the `index` of the given data.
    pub fn new(data: &[u8], index: u32) -> Option<Self> {
        let font = FontRef::from_index(data, index).ok()?;
        Self::from_skrifa(&font)
    }

    /// Compute metadata for all fonts in the given data.
    pub fn iter(data: &[u8]) -> impl Iterator<Item = FontInfo> + '_ {
        let count = match FileRef::new(data) {
            Ok(FileRef::Collection(collection)) => collection.len(),
            _ => 1,
        };
        (0..count).filter_map(move |index| Self::new(data, index))
    }

    /// Compute metadata for a single `skrifa` font.
    pub(super) fn from_skrifa(font: &FontRef) -> Option<Self> {
        let ps_name = find_name(font, StringId::POSTSCRIPT_NAME);
        let exception = ps_name.as_deref().and_then(find_exception);
        // We cannot use Name ID 16 "Typographic Family", because for some
        // fonts it groups together more than just Style / Weight / Stretch
        // variants (e.g. Display variants of Noto fonts) and then some
        // variants become inaccessible from Typst. And even though the
        // fsSelection bit WWS should help us decide whether that is the
        // case, it's wrong for some fonts (e.g. for certain variants of "Noto
        // Sans Display").
        //
        // So, instead we use Name ID 1 "Family" and trim many common
        // suffixes for which know that they just describe styling (e.g.
        // "ExtraBold").
        let family =
            exception.and_then(|c| c.family.map(str::to_string)).or_else(|| {
                let family = find_name(font, StringId::FAMILY_NAME)?;
                Some(typographic_family(&family).to_string())
            })?;

        let variant = {
            let style = exception.and_then(|c| c.style).unwrap_or_else(|| {
                let mut full = find_name(font, StringId::FULL_NAME).unwrap_or_default();
                full.make_ascii_lowercase();

                // Read the OS/2 fsSelection bits directly to mirror
                // `ttf_parser`'s `style()`/`is_oblique()`: the italic bit, plus
                // the oblique bit (only honored for OS/2 version >= 4). When the
                // OS/2 table is missing, both default to `false`, just like
                // `ttf_parser`. We deliberately avoid `skrifa`'s `attributes()`,
                // which additionally falls back to the `head` table's `macStyle`
                // bits and would flip the result for OS/2-less fonts.
                let (os2_italic, os2_oblique) = font
                    .os2()
                    .map(|os2| {
                        let flags = os2.fs_selection();
                        let italic = flags.contains(SelectionFlags::ITALIC);
                        let oblique =
                            os2.version() >= 4 && flags.contains(SelectionFlags::OBLIQUE);
                        (italic, oblique)
                    })
                    .unwrap_or((false, false));

                // Some fonts miss the relevant bits for italic or oblique, so
                // we also try to infer that from the full name.
                //
                // We do not check the italic angle (as `ttf.is_italic()` would)
                // because that leads to false positives for some oblique fonts.
                //
                // See <https://github.com/typst/typst/issues/7479>.
                let italic = os2_italic || full.contains("italic");
                let oblique =
                    os2_oblique || full.contains("oblique") || full.contains("slanted");

                match (italic, oblique) {
                    (false, false) => FontStyle::Normal,
                    (true, _) => FontStyle::Italic,
                    (_, true) => FontStyle::Oblique,
                }
            });

            let weight = exception.and_then(|c| c.weight).unwrap_or_else(|| {
                // Mirror `ttf_parser`'s `weight()`, which reads `usWeightClass`
                // and defaults to 400 when the OS/2 table is absent. We read the
                // raw class rather than `skrifa`'s `attributes().weight`, which
                // falls back to the `head` table's bold bit for OS/2-less fonts.
                let number = font.os2().map(|os2| os2.us_weight_class()).unwrap_or(400);
                FontWeight::from_number(number)
            });

            let stretch = exception.and_then(|c| c.stretch).unwrap_or_else(|| {
                // Mirror `ttf_parser`'s `width()`, which maps `usWidthClass`
                // values 1..=9 directly and collapses everything else
                // (including 0 and out-of-range values) to 5 (Normal) before
                // Typst ever sees it. We replicate that collapse here rather
                // than relying on `FontStretch::from_number`, which maps 0 and
                // values > 9 to the extreme stretches.
                let raw = font.os2().map(|os2| os2.us_width_class()).unwrap_or(5);
                let class = if (1..=9).contains(&raw) { raw } else { 5 };
                FontStretch::from_number(class)
            });

            FontVariant { style, weight, stretch }
        };

        // Determine the unicode coverage.
        let coverage = font.charmap().mappings().map(|(codepoint, _)| codepoint);

        let mut flags = FontFlags::empty();
        flags.set(
            FontFlags::MONOSPACE,
            font.post().map(|post| post.is_fixed_pitch() != 0).unwrap_or(false),
        );
        flags.set(FontFlags::MATH, font.table_data(SkrifaTag::new(b"MATH")).is_some());
        flags.set(FontFlags::VARIABLE, font.fvar().is_ok());

        // Determine whether this is a serif or sans-serif font. The PANOSE
        // classification lives in 10 bytes at offset 32 of the OS/2 table; a
        // first byte of 2 (Latin Text) with a serif-style byte in 2..=10
        // designates a serif font. This mirrors the previous raw OS/2 slice.
        if let Some(panose) = font.os2().ok().map(|os2| os2.panose_10())
            && matches!(panose, [2, 2..=10, ..])
        {
            flags.insert(FontFlags::SERIF);
        }

        let axes = font
            .axes()
            .iter()
            .map(|axis| FontAxis {
                tag: Tag::from_bytes(&axis.tag().into_bytes()),
                min: AxisValue(axis.min_value()),
                max: AxisValue(axis.max_value()),
                default: AxisValue(axis.default_value()),
            })
            .collect();

        Some(FontInfo {
            family,
            variant,
            axes,
            flags,
            coverage: Coverage::from_vec(coverage.collect()),
        })
    }

    /// Whether this is the macOS LastResort font. It can yield tofus with
    /// glyph ID != 0.
    pub fn is_last_resort(&self) -> bool {
        self.family == "LastResort"
    }
}

/// Try to find and decode the name with the given id.
///
/// Prefers the English (or otherwise first) localized record. `skrifa`'s
/// `LocalizedString` natively decodes both UTF-16BE and Mac-Roman name records,
/// so no manual Mac-Roman fallback is needed.
pub(super) fn find_name(font: &FontRef, id: StringId) -> Option<String> {
    font.localized_strings(id)
        .english_or_first()
        .map(|string| string.to_string())
}

/// Trim style naming from a family name and fix bad names.
fn typographic_family(mut family: &str) -> &str {
    // Separators between names, modifiers and styles.
    const SEPARATORS: [char; 3] = [' ', '-', '_'];

    // Modifiers that can appear in combination with suffixes.
    const MODIFIERS: &[&str] =
        &["extra", "ext", "ex", "x", "semi", "sem", "sm", "demi", "dem", "ultra"];

    // Style suffixes.
    #[rustfmt::skip]
    const SUFFIXES: &[&str] = &[
        "normal", "italic", "oblique", "slanted",
        "thin", "th", "hairline", "light", "lt", "regular", "medium", "med",
        "md", "bold", "bd", "demi", "extb", "black", "blk", "bk", "heavy",
        "narrow", "condensed", "cond", "cn", "cd", "compressed", "expanded", "exp",
        "vf", "var", "variable",
    ];

    // Trim spacing and weird leading dots in Apple fonts.
    family = family.trim().trim_start_matches('.');

    // Lowercase the string so that the suffixes match case-insensitively.
    let lower = family.to_ascii_lowercase();
    let mut len = usize::MAX;
    let mut trimmed = lower.as_str();

    // Trim style suffixes repeatedly.
    while trimmed.len() < len {
        len = trimmed.len();

        // Find style suffix.
        let mut t = trimmed;
        let mut shortened = false;
        while let Some(s) = SUFFIXES.iter().find_map(|s| t.strip_suffix(s)) {
            shortened = true;
            t = s;
        }

        if !shortened {
            break;
        }

        // Strip optional separator.
        if let Some(s) = t.strip_suffix(SEPARATORS) {
            trimmed = s;
            t = s;
        }

        // Also allow an extra modifier, but apply it only if it is separated it
        // from the text before it (to prevent false positives).
        if let Some(t) = MODIFIERS.iter().find_map(|s| t.strip_suffix(s))
            && let Some(stripped) = t.strip_suffix(SEPARATORS)
        {
            trimmed = stripped;
        }
    }

    // Apply style suffix trimming.
    family = &family[..len];

    family
}

/// A compactly encoded set of codepoints.
///
/// The set is represented by alternating specifications of how many codepoints
/// are not in the set and how many are in the set.
///
/// For example, for the set `{2, 3, 4, 9, 10, 11, 15, 18, 19}`, there are:
/// - 2 codepoints not inside (0, 1)
/// - 3 codepoints inside (2, 3, 4)
/// - 4 codepoints not inside (5, 6, 7, 8)
/// - 3 codepoints inside (9, 10, 11)
/// - 3 codepoints not inside (12, 13, 14)
/// - 1 codepoint inside (15)
/// - 2 codepoints not inside (16, 17)
/// - 2 codepoints inside (18, 19)
///
/// So the resulting encoding is `[2, 3, 4, 3, 3, 1, 2, 2]`.
#[derive(Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Coverage(Vec<u32>);

impl Coverage {
    /// Encode a vector of codepoints.
    pub fn from_vec(mut codepoints: Vec<u32>) -> Self {
        codepoints.sort();
        codepoints.dedup();

        let mut runs = Vec::new();
        let mut next = 0;

        for c in codepoints {
            if let Some(run) = runs.last_mut().filter(|_| c == next) {
                *run += 1;
            } else {
                runs.push(c - next);
                runs.push(1);
            }

            next = c + 1;
        }

        Self(runs)
    }

    /// Whether the codepoint is covered.
    pub fn contains(&self, c: u32) -> bool {
        let mut inside = false;
        let mut cursor = 0;

        for &run in &self.0 {
            if (cursor..cursor + run).contains(&c) {
                return inside;
            }
            cursor += run;
            inside = !inside;
        }

        false
    }

    /// Iterate over all covered codepoints.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let mut inside = false;
        let mut cursor = 0;
        self.0.iter().flat_map(move |run| {
            let range = if inside { cursor..cursor + run } else { 0..0 };
            inside = !inside;
            cursor += run;
            range
        })
    }
}

impl Debug for Coverage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.pad("Coverage(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_styles() {
        assert_eq!(typographic_family("Atma Light"), "Atma");
        assert_eq!(typographic_family("eras bold"), "eras");
        assert_eq!(typographic_family("footlight mt light"), "footlight mt");
        assert_eq!(typographic_family("times new roman"), "times new roman");
        assert_eq!(typographic_family("noto sans mono cond sembd"), "noto sans mono");
        assert_eq!(typographic_family("noto serif SEMCOND sembd"), "noto serif");
        assert_eq!(typographic_family("crimson text"), "crimson text");
        assert_eq!(typographic_family("footlight light"), "footlight");
        assert_eq!(typographic_family("Noto Sans"), "Noto Sans");
        assert_eq!(typographic_family("Noto Sans Light"), "Noto Sans");
        assert_eq!(typographic_family("Noto Sans Semicondensed Heavy"), "Noto Sans");
        assert_eq!(typographic_family("Familx"), "Familx");
        assert_eq!(typographic_family("Font Ultra"), "Font Ultra");
        assert_eq!(typographic_family("Font Ultra Bold"), "Font");
    }

    #[test]
    fn test_coverage() {
        #[track_caller]
        fn test(set: &[u32], runs: &[u32]) {
            let coverage = Coverage::from_vec(set.to_vec());
            assert_eq!(coverage.0, runs);

            let max = 5 + set.iter().copied().max().unwrap_or_default();
            for c in 0..max {
                assert_eq!(set.contains(&c), coverage.contains(c));
            }
        }

        test(&[], &[]);
        test(&[0], &[0, 1]);
        test(&[1], &[1, 1]);
        test(&[0, 1], &[0, 2]);
        test(&[0, 1, 3], &[0, 2, 1, 1]);
        test(
            // {2, 3, 4, 9, 10, 11, 15, 18, 19}
            &[18, 19, 2, 4, 9, 11, 15, 3, 3, 10],
            &[2, 3, 4, 3, 3, 1, 2, 2],
        )
    }

    #[test]
    fn test_coverage_iter() {
        let codepoints = vec![2, 3, 7, 8, 9, 14, 15, 19, 21];
        let coverage = Coverage::from_vec(codepoints.clone());
        assert_eq!(coverage.iter().collect::<Vec<_>>(), codepoints);
    }
}
