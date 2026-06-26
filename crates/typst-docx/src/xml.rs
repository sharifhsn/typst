//! A well-formed-by-construction XML writer for OOXML parts, plus the OOXML
//! element/attribute name constants.

/// The XML declaration every OPC part must begin with.
pub const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";

/// Streaming XML writer: open/close tags balance by construction; text and
/// attribute values are escaped.
pub struct XmlWriter {
    buf: String,
    stack: Vec<&'static str>,
    /// Whether a start tag is currently open (awaiting attributes / children).
    open_tag: bool,
    pretty: bool,
    indent: usize,
}

impl XmlWriter {
    /// Starts a part with the XML declaration.
    pub fn new(pretty: bool) -> Self {
        let mut buf = String::with_capacity(1024);
        buf.push_str(XML_DECL);
        Self { buf, stack: Vec::new(), open_tag: false, pretty, indent: 0 }
    }

    /// Finishes the document, returning the serialized string.
    pub fn finish(self) -> String {
        debug_assert!(self.stack.is_empty(), "unbalanced XML elements: {:?}", self.stack);
        debug_assert!(!self.open_tag, "dangling open tag");
        self.buf
    }

    /// Closes a pending start tag's `>` if one is open.
    fn flush_open(&mut self) {
        if self.open_tag {
            self.buf.push('>');
            self.open_tag = false;
        }
    }

    /// Opens an element; attributes may be added via [`Self::attr`] until
    /// [`Self::start_children`] or [`Self::empty`] is called.
    pub fn open(&mut self, name: &'static str) -> &mut Self {
        self.flush_open();
        self.newline_indent();
        self.buf.push('<');
        self.buf.push_str(name);
        self.stack.push(name);
        self.open_tag = true;
        self.indent += 1;
        self
    }

    /// Adds an attribute (value XML-escaped).
    pub fn attr(&mut self, name: &str, value: &str) -> &mut Self {
        debug_assert!(self.open_tag, "attr called with no open start tag");
        self.buf.push(' ');
        self.buf.push_str(name);
        self.buf.push_str("=\"");
        escape_into(&mut self.buf, value, true);
        self.buf.push('"');
        self
    }

    /// Closes the open tag as a self-closing element `<name .../>`.
    pub fn empty(&mut self) {
        debug_assert!(self.open_tag, "empty called with no open start tag");
        self.buf.push_str("/>");
        self.open_tag = false;
        self.stack.pop();
        self.indent -= 1;
    }

    /// Closes the open start tag (`>`) so children/text may follow.
    pub fn start_children(&mut self) -> &mut Self {
        self.flush_open();
        self
    }

    /// Closes the most recently opened element (`</name>`).
    pub fn close(&mut self) {
        let name = self.stack.pop().expect("close with empty stack");
        self.indent -= 1;
        if self.open_tag {
            // No children were emitted; emit as self-closing.
            self.buf.push_str("/>");
            self.open_tag = false;
        } else {
            self.newline_indent();
            self.buf.push_str("</");
            self.buf.push_str(name);
            self.buf.push('>');
        }
    }

    /// Convenience: `<name>text</name>`.
    pub fn elem_text(&mut self, name: &'static str, text: &str) {
        self.open(name);
        self.start_children();
        self.text(text);
        self.close();
    }

    /// Convenience: `<name/>` with no children.
    pub fn leaf(&mut self, name: &'static str) {
        self.open(name);
        self.empty();
    }

    /// Raw text content (escaped).
    pub fn text(&mut self, s: &str) {
        self.flush_open();
        escape_into(&mut self.buf, s, false);
    }

    /// Append already-serialized child XML verbatim (e.g. an OMML fragment).
    pub fn raw(&mut self, xml: &str) {
        self.flush_open();
        self.buf.push_str(xml);
    }

    fn newline_indent(&mut self) {
        if self.pretty {
            self.buf.push('\n');
            for _ in 0..self.indent {
                self.buf.push_str("  ");
            }
        }
    }
}

/// Escapes `s` into `buf`, stripping XML-1.0-illegal control characters.
///
/// When `attr` is true, also escapes characters that matter inside attribute
/// values.
fn escape_into(buf: &mut String, s: &str, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '"' if attr => buf.push_str("&quot;"),
            '\'' if attr => buf.push_str("&apos;"),
            // Tab and newline are legal in XML 1.0 (and meaningful in attrs as
            // whitespace) but should not appear literally in `w:t`; callers
            // route them to `w:tab` / `w:br`. Keep them here for robustness.
            '\t' | '\n' | '\r' => buf.push(c),
            // Strip other XML-1.0-illegal control characters.
            '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' => {}
            _ => buf.push(c),
        }
    }
}

/// Escapes a standalone string (used by writers that build XML by hand).
pub fn escape(s: &str) -> String {
    let mut buf = String::with_capacity(s.len());
    escape_into(&mut buf, s, false);
    buf
}

/// Escapes a string for use in an attribute value.
pub fn escape_attr(s: &str) -> String {
    let mut buf = String::with_capacity(s.len());
    escape_into(&mut buf, s, true);
    buf
}

// ---------------------------------------------------------------------------
// OOXML element/attribute name constants.
// ---------------------------------------------------------------------------

pub const W_DOCUMENT: &str = "w:document";
pub const W_STYLES: &str = "w:styles";
pub const W_BODY: &str = "w:body";
pub const W_P: &str = "w:p";
pub const W_PPR: &str = "w:pPr";
pub const W_PSTYLE: &str = "w:pStyle";
pub const W_R: &str = "w:r";
pub const W_RPR: &str = "w:rPr";
pub const W_T: &str = "w:t";
pub const W_BR: &str = "w:br";
pub const W_TAB: &str = "w:tab";
pub const W_B: &str = "w:b";
pub const W_BCS: &str = "w:bCs";
pub const W_I: &str = "w:i";
pub const W_ICS: &str = "w:iCs";
pub const W_SMALLCAPS: &str = "w:smallCaps";
pub const W_STRIKE: &str = "w:strike";
pub const W_U: &str = "w:u";
pub const W_COLOR: &str = "w:color";
pub const W_SPACING: &str = "w:spacing";
pub const W_SZ: &str = "w:sz";
pub const W_SZCS: &str = "w:szCs";
pub const W_SHD: &str = "w:shd";
pub const W_RFONTS: &str = "w:rFonts";
pub const W_RSTYLE: &str = "w:rStyle";
pub const W_VERTALIGN: &str = "w:vertAlign";
pub const W_LANG: &str = "w:lang";
pub const W_VAL: &str = "w:val";
pub const W_SECTPR: &str = "w:sectPr";
pub const W_HYPERLINK: &str = "w:hyperlink";
pub const W_BOOKMARK_START: &str = "w:bookmarkStart";
pub const W_BOOKMARK_END: &str = "w:bookmarkEnd";
pub const W_FOOTNOTE_REF: &str = "w:footnoteReference";

pub const M_OMATH: &str = "m:oMath";
pub const M_OMATH_PARA: &str = "m:oMathPara";
