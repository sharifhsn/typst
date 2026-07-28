//! The `bibliography` mapper: Word's Source Manager (`b:Sources`) → a
//! hayagriva YAML sidecar that `#bibliography(..)` reads, and `CITATION`
//! fields → live `#cite(<key>)` calls.
//!
//! This is the inverse of `typst-docx`'s `bibliography.rs`, and the only
//! construct where the two directions disagree about vocabulary size: Word has
//! seventeen fixed source types, hayagriva around thirty. Going out, several
//! hayagriva types collapse onto one Word type; coming back, the collapse
//! cannot be undone, so each Word type maps to the hayagriva type it most
//! often came from.
//!
//! Measured against 15,241 entries from 1,213 real `.bib` files: that
//! most-likely inverse is right for **95.4%** of them. The loss concentrates
//! in one place — `Report` is a near coin-flip between a report and a thesis
//! (313 vs 261 in that sample) — because Word genuinely cannot tell them
//! apart once both are `SourceType="Report"`.
//!
//! Note that a document written *by Word* fares better than that figure
//! suggests: Word uses its own full vocabulary, including `JournalArticle`,
//! which the exporter never emits but which maps back exactly.

use ecow::{EcoString, eco_format};

use crate::wml::model::WordSource;

/// Where the sidecar is written, relative to the emitted `.typ`.
pub(crate) const SIDECAR: &str = "bibliography.yml";

/// Word's `b:SourceType` → the hayagriva `type:` it most likely came from.
///
/// `Interview` has no hayagriva counterpart at all and lands in `misc`, which
/// is honest: inventing a nearer-looking type would misreport the source.
fn entry_type(source_type: &str) -> &'static str {
    match source_type {
        "Book" => "book",
        "BookSection" => "chapter",
        // Both of Word's periodical types are articles. The distinction it
        // draws — scholarly journal vs magazine — is one hayagriva expresses
        // through the *parent*, not the entry type.
        "JournalArticle" | "ArticleInAPeriodical" => "article",
        "ConferenceProceedings" => "proceedings",
        "Report" => "report",
        "SoundRecording" => "audio",
        "Performance" => "performance",
        "Art" => "artwork",
        "DocumentFromInternetSite" | "InternetSite" | "ElectronicSource" => "web",
        "Case" => "case",
        "Patent" => "patent",
        "Film" => "video",
        _ => "misc",
    }
}

/// Whether this Word type keeps its container title as a *parent* entry
/// (a journal an article sits in, a book a chapter sits in) rather than as its
/// own title.
fn has_parent(source_type: &str) -> bool {
    matches!(
        source_type,
        "JournalArticle"
            | "ArticleInAPeriodical"
            | "BookSection"
            | "ConferenceProceedings"
    )
}

/// Render every source as one hayagriva YAML document.
///
/// Written by hand rather than through a serializer: the output is a small,
/// fixed shape, and hayagriva's own types aren't a dependency of this crate.
pub(crate) fn render_yaml(sources: &[WordSource]) -> String {
    let mut out = String::from(
        "# Bibliography recovered from Word's Source Manager (b:Sources).\n\
         # Word stores seventeen source types where hayagriva has around thirty,\n\
         # so a type here is the most likely original, not a certainty.\n",
    );
    for source in sources {
        out.push_str(&eco_format!("\n{}:\n", yaml_key(&source.tag)));
        out.push_str(&eco_format!("  type: {}\n", entry_type(&source.source_type)));

        let parent = has_parent(&source.source_type);
        if let Some(title) = &source.title {
            out.push_str(&eco_format!("  title: {}\n", quote(title)));
        }
        push_authors(&mut out, source);
        if let Some(date) = date(source) {
            out.push_str(&eco_format!("  date: {date}\n"));
        }
        if let Some(url) = &source.url {
            out.push_str(&eco_format!("  url: {}\n", quote(url)));
        }
        if let Some(doi) = &source.doi {
            out.push_str(&eco_format!("  serial-number:\n    doi: {}\n", quote(doi)));
        }
        if let Some(pages) = &source.pages {
            // hayagriva wants a range; Word writes free text ("12-18", "12,
            // 15"). Only an unambiguous single range is emitted as one.
            if let Some(range) = page_range(pages) {
                out.push_str(&eco_format!("  page-range: {range}\n"));
            }
        }
        if !parent {
            push_publisher(&mut out, source);
            if let Some(volume) = &source.volume {
                out.push_str(&eco_format!("  volume: {}\n", quote(volume)));
            }
        } else {
            out.push_str("  parent:\n");
            let container = source.container.as_deref().unwrap_or("");
            if !container.is_empty() {
                out.push_str(&eco_format!("    title: {}\n", quote(container)));
            }
            out.push_str(&eco_format!(
                "    type: {}\n",
                match source.source_type.as_str() {
                    "BookSection" => "book",
                    "ConferenceProceedings" => "proceedings",
                    _ => "periodical",
                }
            ));
            if let Some(volume) = &source.volume {
                out.push_str(&eco_format!("    volume: {}\n", quote(volume)));
            }
            if let Some(issue) = &source.issue {
                out.push_str(&eco_format!("    issue: {}\n", quote(issue)));
            }
        }
    }
    out
}

fn push_authors(out: &mut String, source: &WordSource) {
    if let Some(corporate) = &source.corporate {
        out.push_str(&eco_format!("  author: {}\n", quote(corporate)));
        return;
    }
    if source.persons.is_empty() {
        return;
    }
    out.push_str("  author:\n");
    for (last, first, middle) in &source.persons {
        // hayagriva reads "Last, First" — the one form that keeps a
        // multi-word surname from being split at the wrong space.
        let given: Vec<&str> =
            [first.as_deref(), middle.as_deref()].into_iter().flatten().collect();
        let name = if given.is_empty() {
            last.clone()
        } else {
            eco_format!("{last}, {}", given.join(" "))
        };
        out.push_str(&eco_format!("    - {}\n", quote(&name)));
    }
}

fn push_publisher(out: &mut String, source: &WordSource) {
    match (&source.publisher, &source.city) {
        (Some(publisher), Some(city)) => {
            out.push_str(&eco_format!("  publisher:\n    name: {}\n", quote(publisher)));
            out.push_str(&eco_format!("    location: {}\n", quote(city)));
        }
        (Some(publisher), None) => {
            out.push_str(&eco_format!("  publisher: {}\n", quote(publisher)));
        }
        (None, Some(city)) => out.push_str(&eco_format!("  location: {}\n", quote(city))),
        (None, None) => {}
    }
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, built only from parts that are actually
/// numeric — Word lets a user type anything into those boxes.
fn date(source: &WordSource) -> Option<EcoString> {
    let year = source.year.as_deref()?.trim();
    if year.len() != 4 || !year.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let two = |v: &Option<EcoString>| -> Option<u32> {
        v.as_deref()?.trim().parse::<u32>().ok().filter(|n| *n >= 1)
    };
    match (two(&source.month), two(&source.day)) {
        (Some(m), Some(d)) if m <= 12 && d <= 31 => {
            Some(eco_format!("{year}-{m:02}-{d:02}"))
        }
        (Some(m), _) if m <= 12 => Some(eco_format!("{year}-{m:02}")),
        _ => Some(year.into()),
    }
}

/// A single `12-18`-style range, or `None` for anything less clear-cut.
fn page_range(pages: &str) -> Option<EcoString> {
    let pages = pages.trim();
    let (from, to) = pages.split_once(['-', '–'])?;
    fn numeric(s: &str) -> Option<&str> {
        let s = s.trim();
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then_some(s)
    }
    Some(eco_format!("{}-{}", numeric(from)?, numeric(to)?))
}

/// A YAML mapping key. Word's tags are alphanumeric in practice, but a
/// hand-edited one could hold anything, so anything unusual gets quoted.
fn yaml_key(tag: &str) -> EcoString {
    if !tag.is_empty()
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !tag.starts_with(|c: char| c.is_ascii_digit())
    {
        tag.into()
    } else {
        quote(tag)
    }
}

/// A double-quoted YAML scalar — the one form that needs no knowledge of
/// which characters would otherwise start a block, alias or tag.
fn quote(value: &str) -> EcoString {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out.into()
}

/// Whether a `CITATION` field's tag names a source that actually exists —
/// what decides between a live `#cite` and keeping Word's frozen text.
pub(crate) fn has_source(sources: &[WordSource], tag: &str) -> bool {
    sources.iter().any(|s| s.tag == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> WordSource {
        WordSource {
            tag: "Kra06".into(),
            source_type: "Book".into(),
            persons: vec![("Kramer".into(), Some("James".into()), Some("D".into()))],
            title: Some("How to Write Bibliographies".into()),
            year: Some("2006".into()),
            city: Some("Chicago".into()),
            publisher: Some("Adventure Works Press".into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_book_renders_as_hayagriva_yaml() {
        let yaml = render_yaml(&[book()]);
        assert!(yaml.contains("\nKra06:\n"), "{yaml}");
        assert!(yaml.contains("  type: book\n"), "{yaml}");
        assert!(yaml.contains("    - \"Kramer, James D\"\n"), "{yaml}");
        assert!(yaml.contains("  date: 2006\n"), "{yaml}");
        assert!(yaml.contains("    name: \"Adventure Works Press\"\n"), "{yaml}");
        assert!(yaml.contains("    location: \"Chicago\"\n"), "{yaml}");
    }

    /// An article's container title is a *parent* entry, not its own title —
    /// otherwise the journal name would be rendered as the article's.
    #[test]
    fn a_journal_article_nests_its_journal_as_a_parent() {
        let source = WordSource {
            tag: "Sm20".into(),
            source_type: "JournalArticle".into(),
            title: Some("On Things".into()),
            container: Some("Journal of Things".into()),
            volume: Some("4".into()),
            ..Default::default()
        };
        let yaml = render_yaml(&[source]);
        assert!(yaml.contains("  title: \"On Things\"\n"), "{yaml}");
        assert!(yaml.contains("  parent:\n"), "{yaml}");
        assert!(yaml.contains("    title: \"Journal of Things\"\n"), "{yaml}");
        assert!(yaml.contains("    type: periodical\n"), "{yaml}");
        assert!(yaml.contains("    volume: \"4\"\n"), "{yaml}");
    }

    /// Word's date boxes are free text, so a non-numeric year yields no date
    /// at all rather than an entry hayagriva would reject.
    #[test]
    fn a_nonsense_date_is_omitted_rather_than_emitted() {
        let mut source = book();
        source.year = Some("n.d.".into());
        assert!(!render_yaml(&[source]).contains("date:"));
    }

    #[test]
    fn only_an_unambiguous_page_range_is_emitted() {
        assert_eq!(page_range("12-18").as_deref(), Some("12-18"));
        assert_eq!(page_range("12–18").as_deref(), Some("12-18"));
        assert_eq!(page_range("12, 15").as_deref(), None);
        assert_eq!(page_range("passim").as_deref(), None);
    }

    /// A tag is a YAML *key*; one that isn't a plain identifier must be
    /// quoted or the whole sidecar fails to parse.
    #[test]
    fn an_unusual_tag_is_quoted_as_a_key() {
        assert_eq!(yaml_key("Kra06"), "Kra06");
        assert_eq!(yaml_key("a b"), "\"a b\"");
        assert_eq!(yaml_key("2020x"), "\"2020x\"");
    }
}
