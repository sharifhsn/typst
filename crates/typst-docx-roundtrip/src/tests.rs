use std::collections::HashMap;
use std::io::{Cursor, Write};

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::*;

fn state(source: &str, word: &str) -> RoundtripState {
    let text = format!("before {source} after");
    let start = "before ".len();
    RoundtripState {
        export_id: "export".into(),
        main: "main.typ".into(),
        files: vec![BaselineFile {
            path: "main.typ".into(),
            sha256: sha256(text.as_bytes()),
            text,
        }],
        regions: vec![Region {
            id: "one".into(),
            file: "main.typ".into(),
            baseline_start: start,
            baseline_end: start + source.len(),
            source: source.into(),
            word_baseline: word.into(),
            kind: RegionKind("heading".into()),
        }],
    }
}

fn docx(controls: &[(&str, &str)]) -> Vec<u8> {
    let body = controls
        .iter()
        .map(|(tag, xml)| format!(r#"<w:sdt><w:sdtPr><w:tag w:val="{tag}"/></w:sdtPr><w:sdtContent><w:p>{xml}</w:p></w:sdtContent></w:sdt>"#))
        .collect::<String>();
    zip_xml(&format!(
        r#"<?xml version="1.0"?><w:document xmlns:w="urn:w"><w:body>{body}</w:body></w:document>"#
    ))
}

fn zip_xml(xml: &str) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut output);
        zip.start_file(DOCUMENT_XML, SimpleFileOptions::default()).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    output.into_inner()
}

fn current(text: &str) -> HashMap<String, String> {
    HashMap::from([("main.typ".into(), text.into())])
}

#[test]
fn state_json_is_full_and_verified() {
    let state = state("old", "old");
    assert_eq!(RoundtripState::from_json(&state.to_json().unwrap()).unwrap(), state);
    let mut bad = state;
    bad.files[0].text.push('!');
    assert!(matches!(bad.validate(), Err(Error::InvalidState(_))));
}

#[test]
fn validates_every_supported_region_kind_and_rejects_unknown_ones() {
    for kind in ["heading", "paragraph", "list_item", "table_cell"] {
        let mut candidate = state("old", "old");
        candidate.regions[0].kind = RegionKind(kind.into());
        candidate.validate().unwrap();
    }
    let mut unknown = state("old", "old");
    unknown.regions[0].kind = RegionKind("future".into());
    assert!(matches!(unknown.validate(), Err(Error::InvalidState(_))));
}

#[test]
fn table_cell_edits_remain_content_in_code_mode() {
    let mut state = state("[old]", "old");
    state.regions[0].kind = RegionKind("table_cell".into());
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new #[literal]".into())]),
    };
    let report = dry_run(&state, &edits, &current("before [old] after")).unwrap();
    assert_eq!(report.files[0].contents, r#"before [#("new #[literal]")] after"#);
}

#[test]
fn direct_edit_merges_and_escapes_utf8_markup() {
    let state = state("old", "old");
    let edits = parse_docx(
        &docx(&[(
            "typst:v1:export:one",
            "<w:r><w:t>été #[x] *_$&lt;@ `~ \\</w:t></w:r>",
        )]),
        &state,
    )
    .unwrap();
    let report = dry_run(&state, &edits, &current("before old after")).unwrap();
    assert_eq!(report.regions[0].status, RegionStatus::Ready);
    assert_eq!(report.files[0].contents, r#"before #("été #[x] *_$<@ `~ \\") after"#);
}

#[test]
fn unrelated_local_edit_remaps_exact_source_island() {
    let state = state("old", "old");
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new".into())]),
    };
    let report =
        dry_run(&state, &edits, &current("new prelude before old after")).unwrap();
    assert_eq!(report.files[0].contents, "new prelude before #(\"new\") after");
}

#[test]
fn overlapping_local_edit_conflicts() {
    let state = state("old", "old");
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new".into())]),
    };
    let report =
        dry_run(&state, &edits, &current("before locally-changed after")).unwrap();
    assert_eq!(
        report.regions[0].status,
        RegionStatus::Conflict(ConflictKind::LocalOverlap)
    );
    assert!(!report.can_apply());
}

#[test]
fn repeated_relocated_islands_conflict() {
    let state = state("old", "old");
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new".into())]),
    };
    let report = dry_run(&state, &edits, &current("shift old and old")).unwrap();
    assert_eq!(
        report.regions[0].status,
        RegionStatus::Conflict(ConflictKind::LocalOverlap)
    );
}

#[test]
fn missing_duplicate_and_foreign_tags_are_rejected() {
    let state = state("old", "old");
    assert!(matches!(parse_docx(&docx(&[]), &state), Err(Error::MissingControl(_))));
    let duplicate = docx(&[
        ("typst:v1:export:one", "<w:r><w:t>old</w:t></w:r>"),
        ("typst:v1:export:one", "<w:r><w:t>old</w:t></w:r>"),
    ]);
    assert!(matches!(parse_docx(&duplicate, &state), Err(Error::DuplicateControl(_))));
    let foreign = docx(&[("typst:v1:other:one", "<w:r><w:t>old</w:t></w:r>")]);
    assert!(matches!(parse_docx(&foreign, &state), Err(Error::ForeignControl(_))));
}

#[test]
fn tracked_changes_use_final_view() {
    let state = state("old", "old");
    let xml = "<w:r><w:t>kept </w:t></w:r><w:del><w:r><w:delText>deleted</w:delText><w:t>also deleted</w:t></w:r></w:del><w:moveFrom><w:r><w:t>moved away</w:t></w:r></w:moveFrom><w:ins><w:r><w:t>inserted</w:t></w:r></w:ins><w:moveTo><w:r><w:t> moved here</w:t></w:r></w:moveTo>";
    let edits = parse_docx(&docx(&[("typst:v1:export:one", xml)]), &state).unwrap();
    assert_eq!(edits.regions["one"], "kept inserted moved here");
}

#[test]
fn structural_paragraph_change_is_rejected() {
    let state = state("old", "old");
    let xml = r#"<?xml version="1.0"?><w:document xmlns:w="urn:w"><w:body><w:sdt><w:sdtPr><w:tag w:val="typst:v1:export:one"/></w:sdtPr><w:sdtContent><w:p/><w:p/></w:sdtContent></w:sdt></w:body></w:document>"#;
    assert!(matches!(parse_docx(&zip_xml(xml), &state), Err(Error::StructuralEdit(_))));
}

#[test]
fn structural_inline_break_is_rejected() {
    let state = state("old", "old");
    let xml = "<w:r><w:t>first</w:t><w:br/><w:t>second</w:t></w:r>";
    assert!(matches!(
        parse_docx(&docx(&[("typst:v1:export:one", xml)]), &state),
        Err(Error::StructuralEdit(_))
    ));
}

#[test]
fn deleted_paragraph_mark_is_rejected() {
    let state = state("old", "old");
    let xml = concat!(
        "<w:pPr><w:rPr><w:del w:id=\"1\"/></w:rPr></w:pPr>",
        "<w:del><w:r><w:delText>old</w:delText></w:r></w:del>"
    );
    assert!(matches!(
        parse_docx(&docx(&[("typst:v1:export:one", xml)]), &state),
        Err(Error::StructuralEdit(_))
    ));
}

#[test]
fn unsupported_visible_word_nodes_are_rejected() {
    let state = state("old", "old");
    for xml in [
        "<w:r><w:sym w:char=\"F041\"/></w:r>",
        "<w:fldSimple w:instr=\"DATE\"><w:r><w:t>today</w:t></w:r></w:fldSimple>",
        "<w:r><w:noBreakHyphen/></w:r>",
    ] {
        assert!(matches!(
            parse_docx(&docx(&[("typst:v1:export:one", xml)]), &state),
            Err(Error::StructuralEdit(_))
        ));
    }
}

#[test]
fn malformed_xml_and_forbidden_doctype_are_rejected() {
    let state = state("old", "old");
    assert!(matches!(parse_docx(&zip_xml("<broken>"), &state), Err(Error::Xml(_))));
    assert!(matches!(
        parse_docx(&zip_xml("<!DOCTYPE x><x/>"), &state),
        Err(Error::UnsafeDocx(_))
    ));
}

#[test]
fn oversized_xml_is_rejected_before_parsing() {
    let state = state("old", "old");
    let xml = "x".repeat(MAX_XML_BYTES as usize + 1);
    assert!(matches!(parse_docx(&zip_xml(&xml), &state), Err(Error::UnsafeDocx(_))));
}

#[test]
fn apply_is_explicit_atomic_and_detects_stale_sources() {
    let state = state("old", "old");
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new".into())]),
    };
    let report = dry_run(&state, &edits, &current("before old after")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("main.typ"), "before old after").unwrap();
    apply_atomic(temp.path(), &report).unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join("main.typ")).unwrap(),
        "before #(\"new\") after"
    );

    fs::write(temp.path().join("main.typ"), "changed again").unwrap();
    assert!(matches!(apply_atomic(temp.path(), &report), Err(Error::SourceChanged(_))));
}

#[test]
fn multi_file_plan_applies_transactionally_and_preflights_every_source() {
    let report = MergeReport {
        regions: vec![RegionReport {
            id: "one".into(),
            file: "a.typ".into(),
            baseline: "a".into(),
            current: Some("a".into()),
            word: "b".into(),
            status: RegionStatus::Ready,
        }],
        files: vec![
            PlannedFile {
                path: "a.typ".into(),
                expected_sha256: sha256(b"a"),
                contents: "b".into(),
            },
            PlannedFile {
                path: "b.typ".into(),
                expected_sha256: sha256(b"a"),
                contents: "b".into(),
            },
        ],
    };
    assert!(report.can_apply());
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("a.typ"), "a").unwrap();
    fs::write(temp.path().join("b.typ"), "a").unwrap();
    apply_atomic(temp.path(), &report).unwrap();
    assert_eq!(fs::read_to_string(temp.path().join("a.typ")).unwrap(), "b");
    assert_eq!(fs::read_to_string(temp.path().join("b.typ")).unwrap(), "b");

    fs::write(temp.path().join("a.typ"), "a").unwrap();
    fs::write(temp.path().join("b.typ"), "stale").unwrap();
    assert!(matches!(
        apply_atomic(temp.path(), &report),
        Err(Error::SourceChanged(path)) if path == "b.typ"
    ));
    assert_eq!(fs::read_to_string(temp.path().join("a.typ")).unwrap(), "a");
    assert_eq!(fs::read_to_string(temp.path().join("b.typ")).unwrap(), "stale");
}

#[cfg(unix)]
#[test]
fn symbolic_link_source_is_rejected() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("target.typ"), "before old after").unwrap();
    symlink("target.typ", temp.path().join("main.typ")).unwrap();
    let state = state("old", "old");
    let edits = WordEdits {
        regions: HashMap::from([("one".into(), "new".into())]),
    };
    let report = dry_run(&state, &edits, &current("before old after")).unwrap();
    assert!(matches!(apply_atomic(temp.path(), &report), Err(Error::InvalidState(_))));
    assert_eq!(
        fs::read_to_string(temp.path().join("target.typ")).unwrap(),
        "before old after"
    );
}
