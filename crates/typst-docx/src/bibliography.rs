//! Maps hayagriva bibliography entries onto Word's native `b:Source` schema,
//! the `customXml/item1.xml` part backing References → Manage Sources (see
//! `encode.rs` for the part/relationship wiring).
//!
//! This is a best-effort, necessarily lossy translation: Word's schema has a
//! fixed, coarse field set and a 17-value `SourceType` enum, while hayagriva
//! entries carry a much richer `EntryType` and structured metadata. The
//! visible body text keeps the fully-realized, formatted citation output;
//! this module only feeds Word's Source Manager UI/tooling with real,
//! correctly-typed (if imperfect) sources — it does not attempt round-trip
//! fidelity, which is why the lossless BibLaTeX sidecar (`build_typst_bibliography`
//! in `encode.rs`) is kept alongside it for external tools.

use ecow::{EcoString, eco_format};
use hayagriva::Entry;
use hayagriva::types::{EntryType, Person};
use typst_library::foundations::Label;

/// One entry mapped onto Word's `b:Source` field set.
pub(crate) struct WordSource {
    /// Free-form citation key shown in Source Manager's "Tag" column.
    pub tag: EcoString,
    /// A deterministic (not random) GUID, so repeated exports of the same
    /// document are byte-identical.
    pub guid: EcoString,
    /// One of Word's 17 `SourceType` enum values.
    pub source_type: &'static str,
    pub author: WordAuthor,
    pub title: Option<EcoString>,
    pub year: Option<EcoString>,
    pub publisher: Option<EcoString>,
    pub city: Option<EcoString>,
    pub journal_name: Option<EcoString>,
    pub volume: Option<EcoString>,
    pub issue: Option<EcoString>,
    pub pages: Option<EcoString>,
    pub url: Option<EcoString>,
}

/// Word represents an author either as a list of structured persons or as a
/// single corporate/organizational name — never both.
pub(crate) enum WordAuthor {
    Persons(Vec<WordPerson>),
    Corporate(EcoString),
    None,
}

pub(crate) struct WordPerson {
    pub last: EcoString,
    pub first: Option<EcoString>,
}

/// Maps every bibliography entry to a `WordSource`, in the given order.
pub(crate) fn map_entries(entries: &[(Label, Entry)]) -> Vec<WordSource> {
    entries.iter().map(|(label, entry)| map_entry(*label, entry)).collect()
}

fn map_entry(label: Label, entry: &Entry) -> WordSource {
    let tag: EcoString = label.resolve().as_str().into();
    let guid = guid_from_key(&tag);
    let source_type = map_source_type(entry.entry_type());
    let author = map_author(entry);
    let title = entry.title().map(|t| t.value.to_str().into());
    let year = entry.date().map(|d| eco_format!("{}", d.year));
    let publisher =
        entry.publisher().and_then(|p| p.name()).map(|n| n.value.to_str().into());
    let city =
        entry.publisher().and_then(|p| p.location()).map(|l| l.value.to_str().into());
    // Word's `JournalName` is only meaningful for periodical-shaped entries;
    // for everything else, a parent title (if any) is typically the
    // containing book/proceedings, not a journal.
    let journal_name = matches!(
        entry.entry_type(),
        EntryType::Article | EntryType::Periodical | EntryType::Newspaper
    )
    .then(|| entry.parents().first().and_then(|p| p.title()))
    .flatten()
    .map(|t| t.value.to_str().into());
    let volume = entry.volume().map(|v| eco_format!("{v}"));
    let issue = entry.issue().map(|i| eco_format!("{i}"));
    let pages = entry.page_range().map(|p| eco_format!("{p}"));
    let url = entry.url().map(|u| eco_format!("{u}"));

    WordSource {
        tag,
        guid,
        source_type,
        author,
        title,
        year,
        publisher,
        city,
        journal_name,
        volume,
        issue,
        pages,
        url,
    }
}

/// Derives a deterministic package-level GUID for the `itemProps1.xml`
/// datastore item, from every source's tag — distinct from any individual
/// source's own GUID, but stable across repeated exports of the same
/// document.
pub(crate) fn package_guid(sources: &[WordSource]) -> EcoString {
    let mut key = EcoString::new();
    for src in sources {
        key.push_str(&src.tag);
        key.push('\u{0}');
    }
    guid_from_key(&key)
}

/// Derives a stable, non-random GUID from the citation key, so exporting the
/// same document twice produces byte-identical output.
fn guid_from_key(key: &str) -> EcoString {
    typst_ooxml_core::xml::guid_from_hash(typst_utils::hash128(key)).into()
}

/// Best-effort mapping from hayagriva's ~29-value `EntryType` onto Word's
/// fixed 17-value `SourceType` enum. Neither schema enforces conditional
/// per-type field requirements, so an approximate match is always safe to
/// emit — worst case, Word shows a source under a slightly-off category.
fn map_source_type(ty: &EntryType) -> &'static str {
    match ty {
        EntryType::Article | EntryType::Periodical | EntryType::Newspaper => {
            "ArticleInAPeriodical"
        }
        EntryType::Chapter | EntryType::Anthos => "BookSection",
        EntryType::Report | EntryType::Thesis => "Report",
        EntryType::Web | EntryType::Post | EntryType::Thread => {
            "DocumentFromInternetSite"
        }
        EntryType::Blog => "InternetSite",
        EntryType::Scene | EntryType::Performance => "Performance",
        EntryType::Artwork | EntryType::Exhibition => "Art",
        EntryType::Patent => "Patent",
        EntryType::Case => "Case",
        EntryType::Video => "Film",
        EntryType::Audio => "SoundRecording",
        EntryType::Proceedings | EntryType::Conference => "ConferenceProceedings",
        EntryType::Book | EntryType::Anthology | EntryType::Reference => "Book",
        EntryType::Repository => "ElectronicSource",
        _ => "Misc",
    }
}

/// Splits author names into Word's structured-person list, or falls back to
/// a single corporate name when there are no persons but an organization is
/// recorded (the two are mutually exclusive in Word's schema).
fn map_author(entry: &Entry) -> WordAuthor {
    if let Some(persons) = entry.authors()
        && !persons.is_empty()
    {
        return WordAuthor::Persons(persons.iter().map(map_person).collect());
    }
    if let Some(org) = entry.organization() {
        return WordAuthor::Corporate(org.value.to_str().into());
    }
    WordAuthor::None
}

fn map_person(person: &Person) -> WordPerson {
    WordPerson {
        last: person.name.as_str().into(),
        first: person.given_name.as_deref().map(Into::into),
    }
}
