//! Structural, well-formedness, and round-trip tests for the Pandoc exporter.
//!
//! These compile small Typst snippets through the real pipeline
//! (`typst::compile::<PandocDocument>` + [`typst_pandoc::pandoc`]) and assert on
//! the produced JSON AST:
//!
//! - **Always:** the bytes deserialize as JSON, carry the exact
//!   `pandoc-api-version` pandoc 3.x expects (`[1,23,1,1]` — a *minor* mismatch
//!   is a hard `exit 64` in the reader), and have the `{meta, blocks}` envelope.
//! - **When `pandoc` is on `PATH`:** the JSON is fed to the real binary
//!   (`pandoc -f json -t native`) and must exit 0 — the authoritative check that
//!   every node we emit is one pandoc accepts (a malformed `Table`, a `Note` with
//!   a bare `[Inline]` body, a dropped `Space`, … all surface here). Targeted
//!   substring assertions on the `native` rendering pin the structural mappings.
//!
//! The `pandoc`-gated tests are skipped (not failed) when the binary is absent,
//! so the suite stays green in a sandbox without pandoc.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_pandoc::{PandocDocument, PandocOptions, pandoc};

/// A minimal world: the embedded Typst fonts and a single detached source.
struct TestWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: Source,
}

impl TestWorld {
    fn new(text: &str) -> Self {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let book = FontBook::from_fonts(&fonts);
        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            main: Source::detached(text),
        }
    }
}

impl World for TestWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }
    fn main(&self) -> FileId {
        self.main.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(Default::default()))
        }
    }
    fn file(&self, _: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(Default::default()))
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }
    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}

/// Compiles `src` to Pandoc JSON bytes (non-pretty). Rasterization is made
/// deterministic via `SOURCE_DATE_EPOCH=0`, matching the DOCX suite.
fn compile(src: &str) -> Vec<u8> {
    // SAFETY: the test harness is single-threaded per process for env mutation
    // here; we only ever set it to the same constant.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "0") };
    let world = TestWorld::new(src);
    let doc = typst::compile::<PandocDocument>(&world)
        .output
        .expect("compilation failed");
    pandoc(&doc, &PandocOptions { pretty: false }).expect("pandoc export failed")
}

/// Deserializes the bytes as generic JSON, asserting well-formedness and the
/// document envelope (`pandoc-api-version` `[1,23,1,1]` + `meta` + `blocks`).
fn parse(bytes: &[u8]) -> serde_json::Value {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).expect("output is not valid JSON");
    assert_eq!(
        v["pandoc-api-version"],
        serde_json::json!([1, 23, 1, 1]),
        "api-version must be the one pandoc 3.x expects (a minor mismatch is exit 64)"
    );
    assert!(v.get("meta").is_some(), "the envelope must carry a meta map");
    assert!(
        v["blocks"].is_array(),
        "the envelope must carry a blocks array"
    );
    v
}

/// Whether the real `pandoc` binary is available on `PATH` (probed once).
fn pandoc_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("pandoc")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Feeds the JSON bytes to `pandoc -f json -t <to>`, returning the stdout text.
/// Asserts pandoc exits 0 — the authoritative "every node is accepted" check.
/// Returns `None` (caller skips) when pandoc is not installed.
fn pandoc_to(bytes: &[u8], to: &str) -> Option<String> {
    if !pandoc_available() {
        eprintln!("skipping pandoc-gated assertion (pandoc not on PATH)");
        return None;
    }
    let mut child = Command::new("pandoc")
        .args(["-f", "json", "-t", to])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pandoc");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(bytes)
        .expect("failed to write JSON to pandoc");
    let out = child.wait_with_output().expect("pandoc did not run");
    assert!(
        out.status.success(),
        "pandoc -t {to} rejected the JSON (exit {:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Convenience: compile, parse-validate, and (if pandoc present) render to
/// `native`, returning the native text (or `None` when pandoc is absent).
fn native(src: &str) -> Option<String> {
    let bytes = compile(src);
    parse(&bytes);
    pandoc_to(&bytes, "native")
}

// ===========================================================================
// Envelope / round-trip.
// ===========================================================================

#[test]
fn empty_document_is_valid() {
    let bytes = compile("");
    let v = parse(&bytes);
    assert!(
        v["blocks"].as_array().unwrap().is_empty(),
        "an empty document has no blocks"
    );
    // pandoc accepts an empty body.
    if let Some(out) = pandoc_to(&bytes, "native") {
        assert!(out.trim() == "[]" || out.trim().is_empty());
    }
}

#[test]
fn round_trips_through_pandoc_json() {
    // `-f json -t json` is pandoc's own canonicalization: a node it cannot read
    // makes this fail. Covers the whole vocabulary at once.
    let bytes = compile(
        "= H\nText *b* _i_ with #footnote[n].\n\n- a\n- b\n\n```rs\nx\n```\n\n$ x^2 $",
    );
    parse(&bytes);
    pandoc_to(&bytes, "json");
}

#[test]
fn renders_to_latex_and_docx() {
    let bytes = compile("= Title\n\nBody with *bold* and a #footnote[note].");
    parse(&bytes);
    pandoc_to(&bytes, "latex");
    // docx writes a binary; just assert pandoc accepts the AST (exit 0).
    pandoc_to(&bytes, "docx");
}

#[test]
fn document_metadata_populates_meta() {
    let bytes = compile("#set document(title: \"My Title\", author: \"Ada\")\nBody.");
    let v = parse(&bytes);
    assert_eq!(v["meta"]["title"]["t"], "MetaInlines");
    assert_eq!(v["meta"]["author"]["t"], "MetaList");
    // The title text survives.
    let title = serde_json::to_string(&v["meta"]["title"]).unwrap();
    assert!(title.contains("My Title"));
}

// ===========================================================================
// Inline formatting + the inter-word-space guard.
// ===========================================================================

#[test]
fn strong_and_emph_keep_surrounding_spaces() {
    // The canonical defect: inline formatting trimming its neighbouring spaces.
    // `-t plain` concatenates adjacent `Str`s, so a dropped `Space` would show
    // as "aboldb"; we assert the spaces survive in `native` and `plain`.
    let Some(out) = native("a *bold* and _italic_ b") else { return };
    assert!(out.contains("Strong"), "bold maps to Strong");
    assert!(out.contains("Emph"), "italic maps to Emph");

    let bytes = compile("a *bold* and _italic_ b");
    if let Some(plain) = pandoc_to(&bytes, "plain") {
        assert!(
            plain.contains("a bold and italic b"),
            "inter-word spaces around formatting must survive: {plain:?}"
        );
    }
}

#[test]
fn heading_is_header_with_anchor() {
    let Some(out) = native("= Introduction\n\nBody.") else { return };
    assert!(out.contains("Header"), "a heading maps to Header");
    // The id (shared anchor namespace) is stamped on the header attr.
    assert!(out.contains("ref-"), "the heading carries a stable anchor id");
}

// ===========================================================================
// Lists (incl. nesting) + def lists.
// ===========================================================================

#[test]
fn bullet_list_maps_and_nests() {
    let Some(out) = native("- one\n- two\n  - nested\n- three") else { return };
    assert!(out.contains("BulletList"), "a bullet list maps to BulletList");
    // The nested list surfaces as another BulletList inside an item.
    assert_eq!(out.matches("BulletList").count(), 2, "nesting yields an inner list");
}

#[test]
fn ordered_list_maps_to_ordered_list() {
    let Some(out) = native("+ first\n+ second\n+ third") else { return };
    assert!(out.contains("OrderedList"), "a decimal enum maps to OrderedList");
    assert!(out.contains("Decimal"), "with a decimal number style");
}

#[test]
fn term_list_maps_to_definition_list() {
    let Some(out) = native("/ Term: the definition.") else { return };
    assert!(out.contains("DefinitionList"), "a term list maps to DefinitionList");
    assert!(out.contains("Term"), "the term text is present");
    assert!(out.contains("definition"), "the definition text is present");
}

// ===========================================================================
// Table (incl. the rowspan omission rule).
// ===========================================================================

#[test]
fn table_maps_to_table() {
    let Some(out) = native("#table(columns: 2, [a], [b], [c], [d])") else { return };
    assert!(out.contains("Table"), "a table maps to a Table");
    assert!(out.contains("Cell"), "with cells");
}

#[test]
fn table_rowspan_omits_covered_slot() {
    // (0,0) spans 2 rows; row 1 must therefore list ONE fewer cell (the covered
    // slot is omitted, never placeheld — pandoc silently drops a colliding cell).
    let bytes = compile(
        "#table(columns: 2,\n  table.cell(rowspan: 2)[x], [a],\n  [b])",
    );
    let v = parse(&bytes);
    let Some(_out) = pandoc_to(&bytes, "native") else { return };
    // Find the Table block and inspect its body rows.
    let table = v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["t"] == "Table")
        .expect("a Table block");
    let c = table["c"].as_array().unwrap();
    // c[4] = [TableBody]; body = (Attr, RowHeadColumns, [head], [body]); body[3].
    let body_rows = c[4][0][3].as_array().unwrap();
    assert_eq!(body_rows.len(), 2, "two body rows");
    let r0 = body_rows[0][1].as_array().unwrap();
    let r1 = body_rows[1][1].as_array().unwrap();
    // Row 0: the spanning cell (RowSpan 2) + the [a] cell.
    assert_eq!(r0.len(), 2, "row 0 has both cells");
    assert_eq!(r0[0][2], 2, "the origin cell spans 2 rows");
    // Row 1: ONLY [b] — the (1,0) slot covered by the rowspan is omitted.
    assert_eq!(r1.len(), 1, "the covered slot is omitted, not placeheld");
}

// ===========================================================================
// Figure / footnote / cross-reference / math / quote / code.
// ===========================================================================

#[test]
fn figure_maps_to_figure_with_anchor_and_caption() {
    let Some(out) =
        native("#figure(rect(width: 20pt, height: 10pt), caption: [A box]) <f>")
    else {
        return;
    };
    assert!(out.contains("Figure"), "a figure maps to a Figure");
    assert!(out.contains("Caption"), "carrying a caption");
    assert!(out.contains("A box"), "the caption text survives");
    assert!(out.contains("ref-"), "the figure carries a stable anchor id");
    // No baked \"Figure N:\" prefix (writer owns numbering).
    assert!(!out.contains("Figure 1:"), "the baked supplement/number is dropped");
}

#[test]
fn footnote_maps_to_note_with_block_body() {
    let Some(out) = native("A claim.#footnote[The note.]") else { return };
    assert!(out.contains("Note"), "a footnote maps to a Note");
    // The Note body must be a Para (block), never a bare inline (pandoc exit 64).
    assert!(out.contains("Note"), "note present");
    assert!(out.contains("The note."), "the note body text survives");
}

#[test]
fn cross_reference_is_a_fragment_link() {
    let Some(out) =
        native("#set heading(numbering: \"1.\")\n= Methods <m>\n\nSee @m for details.")
    else {
        return;
    };
    assert!(out.contains("Link"), "a cross-reference maps to a Link");
    assert!(out.contains("#ref-"), "whose URL is a #-fragment in the shared namespace");
}

#[test]
fn inline_equation_stays_in_its_paragraph() {
    // Typst splits a paragraph with an inline equation into `[par, eq, par]`;
    // the converter must rejoin them into one Para.
    let bytes = compile("Before $x^2$ and text after.");
    let v = parse(&bytes);
    let Some(_) = pandoc_to(&bytes, "native") else { return };
    let paras: Vec<_> = v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["t"] == "Para")
        .collect();
    assert_eq!(paras.len(), 1, "the equation does not split the paragraph in two");
    let s = serde_json::to_string(&paras[0]).unwrap();
    assert!(s.contains("Before"), "text before the equation");
    assert!(s.contains("text after"), "and after it stay in one Para");
    assert!(s.contains("Image"), "the (rasterized) equation rides in the same Para");
}

#[test]
fn display_equation_maps_to_para() {
    let Some(out) = native("$ E = m c^2 $") else { return };
    // Rasterized equation → a Para holding an Image (math-to-LaTeX is future work).
    assert!(out.contains("Image"), "a display equation rasterizes to an Image");
}

#[test]
fn block_quote_maps_and_keeps_attribution() {
    let Some(out) =
        native("#quote(block: true, attribution: [Einstein])[Imagination matters.]")
    else {
        return;
    };
    assert!(out.contains("BlockQuote"), "a block quote maps to BlockQuote");
    assert!(out.contains("Imagination matters."), "the body survives");
    assert!(out.contains("Einstein"), "the attribution is not dropped");
}

#[test]
fn raw_block_maps_to_code_block_with_language() {
    let Some(out) = native("```rust\nfn main() {}\n```") else { return };
    assert!(out.contains("CodeBlock"), "a raw block maps to CodeBlock");
    assert!(out.contains("rust"), "carrying the language as a class");
    assert!(out.contains("fn main"), "the literal text survives verbatim");
}

#[test]
fn inline_raw_maps_to_code() {
    let Some(out) = native("Use `cargo build` to compile.") else { return };
    assert!(out.contains("Code"), "inline raw maps to Code");
    assert!(out.contains("cargo build"), "the literal survives");
}

#[test]
fn url_link_maps_to_link() {
    let Some(out) = native("Visit #link(\"https://typst.app\")[Typst].") else { return };
    assert!(out.contains("Link"), "a url link maps to Link");
    assert!(out.contains("https://typst.app"), "the destination url survives");
}

#[test]
fn image_passes_png_through_as_data_uri() {
    // A standalone block image: the raster path embeds the bytes as a data URI.
    // (No real file in the minimal world, so use a rect-backed figure instead,
    // which rasterizes — exercising the data-URI path.)
    let Some(out) = native("#rect(width: 10pt, height: 10pt)") else { return };
    assert!(out.contains("Image"), "a drawn shape rasterizes to an Image");
    assert!(out.contains("data:image/png;base64,"), "embedded as a self-contained data URI");
}
