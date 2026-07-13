//! Shared OOXML namespace, relationship, and content-type constants.

pub const RELATIONSHIPS: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
pub const CONTENT_TYPES: &str =
    "http://schemas.openxmlformats.org/package/2006/content-types";

pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub const A14: &str = "http://schemas.microsoft.com/office/drawing/2010/main";
pub const ADEC: &str = "http://schemas.microsoft.com/office/drawing/2017/decorative";
pub const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
pub const WP: &str =
    "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
pub const WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";
pub const WPG: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";
pub const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
pub const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
pub const WP14: &str =
    "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing";
pub const V: &str = "urn:schemas-microsoft-com:vml";
pub const O: &str = "urn:schemas-microsoft-com:office:office";
pub const B: &str = "http://schemas.openxmlformats.org/officeDocument/2006/bibliography";
pub const DS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/customXml";

pub const CP: &str =
    "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
pub const DC: &str = "http://purl.org/dc/elements/1.1/";
pub const DCTERMS: &str = "http://purl.org/dc/terms/";
pub const DCMITYPE: &str = "http://purl.org/dc/dcmitype/";
pub const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub const EXTENDED_PROPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
pub const DOC_PROPS_VTYPES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";
pub const CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";

pub mod rel {
    pub const OFFICE_DOCUMENT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub const CORE_PROPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
    pub const EXTENDED_PROPS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
    pub const CUSTOM_PROPERTIES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";
    pub const STYLES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    pub const NUMBERING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
    pub const FOOTNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    pub const ENDNOTES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
    pub const SETTINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
    pub const THEME: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
    pub const FONT_TABLE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable";
    pub const FONT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/font";
    pub const WEB_SETTINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/webSettings";
    pub const HEADER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    pub const FOOTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    pub const IMAGE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    pub const HYPERLINK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
    pub const SLIDE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
    pub const SLIDE_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
    pub const SLIDE_LAYOUT: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
    pub const NOTES_SLIDE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
    pub const NOTES_MASTER: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster";
    pub const PRES_PROPS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps";
    pub const VIEW_PROPS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps";
    pub const TABLE_STYLES: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles";
    pub const CUSTOM_XML: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
    pub const CUSTOM_XML_PROPS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps";
}

pub mod ct {
    pub const RELS: &str = "application/vnd.openxmlformats-package.relationships+xml";
    pub const CORE_PROPS: &str =
        "application/vnd.openxmlformats-package.core-properties+xml";
    pub const EXTENDED_PROPS: &str =
        "application/vnd.openxmlformats-officedocument.extended-properties+xml";
    pub const CUSTOM_PROPERTIES: &str =
        "application/vnd.openxmlformats-officedocument.custom-properties+xml";
    pub const THEME: &str = "application/vnd.openxmlformats-officedocument.theme+xml";

    pub const WORD_DOCUMENT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
    pub const WORD_STYLES: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
    pub const WORD_NUMBERING: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
    pub const WORD_FOOTNOTES: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
    pub const WORD_ENDNOTES: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml";
    pub const WORD_SETTINGS: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
    pub const WORD_HEADER: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
    pub const WORD_FOOTER: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml";
    pub const WORD_FONT_TABLE: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml";
    pub const OBFUSCATED_FONT: &str =
        "application/vnd.openxmlformats-officedocument.obfuscatedFont";
    pub const WORD_WEB_SETTINGS: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.webSettings+xml";

    pub const PRESENTATION: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
    pub const SLIDE: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
    pub const SLIDE_MASTER: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
    pub const SLIDE_LAYOUT: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml";
    pub const NOTES_SLIDE: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml";
    pub const NOTES_MASTER: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml";
    pub const PRES_PROPS: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.presProps+xml";
    pub const VIEW_PROPS: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml";
    pub const TABLE_STYLES: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml";

    pub const CUSTOM_XML_PROPS: &str =
        "application/vnd.openxmlformats-officedocument.customXmlProperties+xml";
}
