//! Repair-sensitive WordprocessingML sequence validation.
//!
//! This is deliberately narrower than full ECMA-376/XSD validation. It guards
//! the child-order invariants most likely to make Word repair a document while
//! keeping Word-specific policy out of the shared OPC package implementation.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use roxmltree::Node;
use typst_ooxml_core::ns;

use crate::package::Package;

/// A WordprocessingML sequence violation in a finalized package part.
#[derive(Debug, Eq, PartialEq)]
pub struct SchemaError {
    pub part_name: String,
    pub element: String,
    pub message: String,
}

impl Display for SchemaError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} in `{}`: {}", self.element, self.part_name, self.message)
    }
}

impl Error for SchemaError {}

/// Validates the repair-sensitive child sequences in all accumulated XML parts.
pub fn validate_package(package: &Package) -> Result<(), SchemaError> {
    for (part_name, xml) in package.xml_parts() {
        let document = roxmltree::Document::parse(xml).map_err(|err| SchemaError {
            part_name: part_name.into(),
            element: "XML document".into(),
            message: err.to_string(),
        })?;
        for node in document.descendants().filter(Node::is_element) {
            validate_node(part_name, node)?;
        }
    }
    Ok(())
}

fn validate_node(part_name: &str, node: Node<'_, '_>) -> Result<(), SchemaError> {
    let ns = node.tag_name().namespace();
    let name = node.tag_name().name();
    if ns == Some(ns::W) {
        match name {
            "p" => property_first(part_name, node, "pPr"),
            "r" => property_first(part_name, node, "rPr"),
            "tbl" => validate_table(part_name, node),
            "tr" => property_first(part_name, node, "trPr"),
            "tc" => validate_cell(part_name, node),
            "body" => terminal_unique(part_name, node, "sectPr"),
            _ => Ok(()),
        }
    } else if ns == Some(ns::MC) && name == "AlternateContent" {
        validate_alternate_content(part_name, node)
    } else {
        Ok(())
    }
}

fn element_children<'a, 'input>(node: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    node.children().filter(Node::is_element).collect()
}

fn is_w(node: Node<'_, '_>, local: &str) -> bool {
    node.tag_name().namespace() == Some(ns::W) && node.tag_name().name() == local
}

fn fail(part_name: &str, node: Node<'_, '_>, message: impl Into<String>) -> SchemaError {
    SchemaError {
        part_name: part_name.into(),
        element: format!("w:{}", node.tag_name().name()),
        message: message.into(),
    }
}

fn property_first(
    part_name: &str,
    node: Node<'_, '_>,
    property: &str,
) -> Result<(), SchemaError> {
    let children = element_children(node);
    let positions = children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| is_w(*child, property).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err(fail(
            part_name,
            node,
            format!("contains more than one w:{property}"),
        ));
    }
    if positions.first().is_some_and(|position| *position != 0) {
        return Err(fail(
            part_name,
            node,
            format!("w:{property} must be the first element child"),
        ));
    }
    Ok(())
}

fn terminal_unique(
    part_name: &str,
    node: Node<'_, '_>,
    terminal: &str,
) -> Result<(), SchemaError> {
    let children = element_children(node);
    let positions = children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| is_w(*child, terminal).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err(fail(
            part_name,
            node,
            format!("contains more than one w:{terminal}"),
        ));
    }
    if positions
        .first()
        .is_some_and(|position| *position + 1 != children.len())
    {
        return Err(fail(
            part_name,
            node,
            format!("w:{terminal} must be the final element child"),
        ));
    }
    Ok(())
}

fn validate_table(part_name: &str, node: Node<'_, '_>) -> Result<(), SchemaError> {
    property_first(part_name, node, "tblPr")?;
    let mut saw_grid = false;
    let mut saw_row = false;
    for child in element_children(node) {
        if is_w(child, "tblGrid") {
            if saw_grid {
                return Err(fail(part_name, node, "contains more than one w:tblGrid"));
            }
            if saw_row {
                return Err(fail(part_name, node, "w:tblGrid must precede table rows"));
            }
            saw_grid = true;
        } else if is_w(child, "tr") {
            saw_row = true;
        }
    }
    Ok(())
}

fn validate_cell(part_name: &str, node: Node<'_, '_>) -> Result<(), SchemaError> {
    property_first(part_name, node, "tcPr")?;
    let children = element_children(node);
    if !children.last().is_some_and(|child| is_w(*child, "p")) {
        return Err(fail(
            part_name,
            node,
            "must end in a paragraph so Word has an editable cell terminator",
        ));
    }
    Ok(())
}

fn validate_alternate_content(
    part_name: &str,
    node: Node<'_, '_>,
) -> Result<(), SchemaError> {
    let children = element_children(node);
    let mut choices = 0;
    let mut saw_fallback = false;
    let mut fallback_index = None;
    for (index, child) in children.iter().copied().enumerate() {
        let is_mc = child.tag_name().namespace() == Some(ns::MC);
        match (is_mc, child.tag_name().name()) {
            (true, "Choice") if !saw_fallback => choices += 1,
            (true, "Choice") => {
                return Err(fail(part_name, node, "mc:Choice cannot follow mc:Fallback"));
            }
            (true, "Fallback") if !saw_fallback => {
                saw_fallback = true;
                fallback_index = Some(index);
            }
            (true, "Fallback") => {
                return Err(fail(part_name, node, "contains more than one mc:Fallback"));
            }
            _ => {
                return Err(fail(
                    part_name,
                    node,
                    format!(
                        "contains invalid child {}:{}",
                        child.tag_name().namespace().unwrap_or("none"),
                        child.tag_name().name()
                    ),
                ));
            }
        }
    }
    if choices == 0 {
        return Err(fail(part_name, node, "must contain at least one mc:Choice"));
    }
    if fallback_index.is_some_and(|index| index + 1 != children.len()) {
        return Err(fail(part_name, node, "mc:Fallback must be the final element child"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::DOCX_PACKAGE_OPTIONS;

    fn validate(xml: &str) -> Result<(), SchemaError> {
        let mut package = Package::new(DOCX_PACKAGE_OPTIONS);
        package.add_xml("word/document.xml", "application/xml", xml.into());
        validate_package(&package)
    }

    #[test]
    fn accepts_repair_safe_sequences() {
        validate(&format!(
            r#"<w:document xmlns:w="{}" xmlns:mc="{}"><w:body><w:tbl><w:tblPr/><w:tblGrid/><w:tr><w:trPr/><w:tc><w:tcPr/><w:p><w:pPr/><w:r><w:rPr/><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>"#,
            ns::W,
            ns::MC
        ))
        .unwrap();
    }

    #[test]
    fn rejects_late_paragraph_properties() {
        let err = validate(&format!(
            r#"<w:document xmlns:w="{}"><w:body><w:p><w:r/><w:pPr/></w:p></w:body></w:document>"#,
            ns::W
        ))
        .unwrap_err();
        assert!(err.message.contains("first element child"));
    }

    #[test]
    fn rejects_grid_after_row() {
        let err = validate(&format!(
            r#"<w:document xmlns:w="{}"><w:body><w:tbl><w:tr/><w:tblGrid/></w:tbl><w:p/></w:body></w:document>"#,
            ns::W
        ))
        .unwrap_err();
        assert!(err.message.contains("must precede table rows"));
    }

    #[test]
    fn rejects_choice_after_fallback() {
        let err = validate(&format!(
            r#"<mc:AlternateContent xmlns:mc="{}"><mc:Fallback/><mc:Choice Requires="wps"/></mc:AlternateContent>"#,
            ns::MC
        ))
        .unwrap_err();
        assert!(err.message.contains("cannot follow"));
    }

    #[test]
    fn rejects_alternate_content_without_choice() {
        let err = validate(&format!(
            r#"<mc:AlternateContent xmlns:mc="{}"><mc:Fallback/></mc:AlternateContent>"#,
            ns::MC
        ))
        .unwrap_err();
        assert!(err.message.contains("at least one mc:Choice"));
    }

    #[test]
    fn rejects_cell_without_terminal_paragraph() {
        let err =
            validate(&format!(r#"<w:tc xmlns:w="{}"><w:p/><w:tbl/></w:tc>"#, ns::W))
                .unwrap_err();
        assert!(err.message.contains("must end in a paragraph"));
    }
}
