//! Mechanics for writing OMML (Office MathML) fragments.
//!
//! Everything here is *format* knowledge, shared by every exporter that emits
//! math: how an OMML fragment is spelled ([`Omml`]), which combining codepoint
//! OMML demands for an accent ([`to_combining`]), and which characters OMML
//! treats as n-ary operators ([`is_nary_operator`], [`is_integral_char`]).
//!
//! It deliberately holds no Typst-mapping policy. Nothing here knows what a
//! Typst element is; deciding *which* OMML construct a given Typst construct
//! becomes stays with the exporters.

/// Whether the character is a large (n-ary) operator that takes limits.
pub fn is_nary_operator(c: char) -> bool {
    is_integral_char(c)
        || matches!(
            c,
            '∑'     // n-ary summation U+2211
            | '∏'   // n-ary product U+220F
            | '∐'   // n-ary coproduct U+2210
            | '⋃'   // n-ary union U+22C3
            | '⋂'   // n-ary intersection U+22C2
            | '⋁'   // n-ary logical or U+22C1
            | '⋀'   // n-ary logical and U+22C0
            | '⨄'   // n-ary union with plus U+2A04
            | '⨃'   // n-ary union with dot U+2A03
            | '⨆'   // n-ary square union U+2A06
            | '⨅'   // n-ary square intersection U+2A05
            | '⨀'   // n-ary circled dot U+2A00
            | '⨁'   // n-ary circled plus U+2A01
            | '⨂'   // n-ary circled times U+2A02
            | '⫿' // n-ary triple vertical bar U+2AFF
        )
}

/// Whether the character is one of the integral signs (limits as sub/sup).
/// Mirrors `typst_library::math`'s private `is_integral_char`.
pub fn is_integral_char(c: char) -> bool {
    ('∫'..='∳').contains(&c) || ('⨋'..='⨜').contains(&c)
}

/// Maps a spacing accent character to its combining equivalent. Combining marks
/// (U+0300–U+036F, U+20D0–U+20FF) and characters with no spacing form pass
/// through unchanged.
pub fn to_combining(c: char) -> char {
    match c {
        // Spacing → combining for the common math accents.
        '`' => '\u{0300}',              // grave
        '´' => '\u{0301}',              // acute
        '^' => '\u{0302}',              // circumflex / hat
        '~' => '\u{0303}',              // tilde
        '¯' | '\u{02C9}' => '\u{0304}', // macron / bar (+ modifier macron)
        '\u{02D8}' => '\u{0306}',       // breve
        '\u{02D9}' => '\u{0307}',       // dot above
        '¨' => '\u{0308}',              // diaeresis / ddot
        '°' | '\u{02DA}' => '\u{030A}', // ring above
        '\u{02DD}' => '\u{030B}',       // double acute
        'ˇ' => '\u{030C}',              // caron / check
        '→' => '\u{20D7}',              // rightwards arrow → combining (vec)
        '←' => '\u{20D6}',              // leftwards arrow → combining
        '↔' => '\u{20E1}',              // left-right arrow → combining
        // Already a combining mark, or a dedicated accent codepoint: keep it.
        _ => c,
    }
}

// ===========================================================================
// OMML string builder (no XML declaration; balanced by construction).
// ===========================================================================

/// A minimal XML builder for OMML *fragments*.
///
/// We cannot reuse [`crate::xml::XmlWriter`] directly because its constructor
/// emits the `<?xml …?>` declaration, which is illegal inside a fragment that
/// gets `raw`-spliced into an enclosing part such as `document.xml` or a slide.
///
/// Escaping reuses [`crate::xml::escape`] and [`crate::xml::escape_attr`] so it
/// matches the rest of the package exactly (same control-char stripping, same
/// entity set).
///
/// `Default` is derived rather than hand-written: it produces exactly what
/// [`Omml::new`] does (an empty buffer, an empty stack, no open tag).
#[derive(Default)]
pub struct Omml {
    buf: String,
    stack: Vec<String>,
    /// Whether a start tag is currently open (awaiting attributes / children).
    open_tag: bool,
}

impl Omml {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            stack: Vec::new(),
            open_tag: false,
        }
    }

    pub fn into_string(self) -> String {
        debug_assert!(
            self.stack.is_empty(),
            "Omml: unbalanced elements: {:?}",
            self.stack
        );
        debug_assert!(!self.open_tag, "Omml: dangling open tag");
        self.buf
    }

    fn flush_open(&mut self) {
        if self.open_tag {
            self.buf.push('>');
            self.open_tag = false;
        }
    }

    /// Opens an element `<name`; attributes may follow until `children`/`empty`.
    pub fn open(&mut self, name: &str) -> &mut Self {
        self.flush_open();
        self.buf.push('<');
        self.buf.push_str(name);
        self.stack.push(name.to_string());
        self.open_tag = true;
        self
    }

    /// Adds an attribute (value escaped).
    pub fn attr(&mut self, name: &str, value: &str) -> &mut Self {
        debug_assert!(self.open_tag, "Omml::attr with no open tag");
        self.buf.push(' ');
        self.buf.push_str(name);
        self.buf.push_str("=\"");
        self.buf.push_str(&crate::xml::escape_attr(value));
        self.buf.push('"');
        self
    }

    /// Closes the start tag (`>`) so children/text may follow.
    pub fn children(&mut self) -> &mut Self {
        self.flush_open();
        self
    }

    /// Self-closes the currently open element `<name .../>`.
    pub fn empty(&mut self) {
        debug_assert!(self.open_tag, "Omml::empty with no open tag");
        self.buf.push_str("/>");
        self.open_tag = false;
        self.stack.pop();
    }

    /// A self-closing leaf element `<name/>`.
    pub fn leaf(&mut self, name: &str) {
        self.open(name).empty();
    }

    /// Emits escaped text content.
    pub fn text(&mut self, s: &str) {
        self.flush_open();
        self.buf.push_str(&crate::xml::escape(s));
    }

    /// Splices already-serialized child XML verbatim.
    pub fn raw(&mut self, xml: &str) {
        self.flush_open();
        self.buf.push_str(xml);
    }

    /// Closes the most recently opened element. If it had no children, it is
    /// emitted as `<name></name>` (some OMML readers dislike self-closing
    /// structural elements that were opened with `children()`).
    pub fn close(&mut self) {
        let name = self.stack.pop().expect("Omml: close with empty stack");
        self.flush_open();
        self.buf.push_str("</");
        self.buf.push_str(&name);
        self.buf.push('>');
    }

    /// Convenience: `<wrapper>{inner}</wrapper>` where `inner` is raw OMML.
    pub fn wrap_raw(&mut self, wrapper: &str, inner: &str) {
        self.open(wrapper).children();
        self.raw(inner);
        self.close();
    }
}
