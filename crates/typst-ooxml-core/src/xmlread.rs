//! Local-name XML read helpers shared by the OOXML importers.
//!
//! OOXML is matched by *local* name throughout: namespace prefixes are a red
//! herring across real-world producers, and the local names an importer reads
//! do not collide across namespaces on the same element. The one exception is
//! `r:id`/`r:embed`/`r:link`, whose local names *do* clash with unrelated
//! attributes elsewhere — [`attr_ns`] exists for exactly those.
//!
//! These are pure mechanics: they say nothing about how any element maps to
//! Typst. Every format decision stays in the importer crates.

use roxmltree::Node;

/// An element's local name (the tag name with any namespace prefix dropped).
pub fn local<'i>(node: Node<'_, 'i>) -> &'i str {
    node.tag_name().name()
}

/// Whether `node` is an element with local name `name`.
pub fn is_el(node: Node, name: &str) -> bool {
    node.is_element() && local(node) == name
}

/// The first element child of `node` with local name `name`.
pub fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| is_el(*n, name))
}

/// Every element child of `node` with local name `name`, in document order.
pub fn children<'a, 'i>(
    node: Node<'a, 'i>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |n| is_el(*n, name))
}

/// An attribute of `node` by local name only (namespace prefix ignored).
pub fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// A namespace-scoped attribute lookup, for `r:id`/`r:embed`/`r:link` — these
/// share a local name with unrelated attributes in other namespaces, so a
/// plain [`attr`] lookup isn't safe for them.
pub fn attr_ns<'a>(
    node: Node<'a, '_>,
    namespace: &str,
    name: &str,
) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace() == Some(namespace) && a.name() == name)
        .map(|a| a.value())
}
