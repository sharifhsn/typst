//! Builders for the standard auxiliary OPC parts that Microsoft Word always
//! writes but that carry no Typst-derived content of their own: the theme
//! (`word/theme/theme1.xml`), the font table (`word/fontTable.xml`) and the web
//! settings (`word/webSettings.xml`). Emitting them makes the package look like a
//! document Word itself produced (themes/colours available in the ribbon, font
//! substitution metadata present), rather than a minimal third-party export.

use crate::dom::TextDefaults;
use crate::xml::escape_attr;

const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// The body/heading font for the theme, falling back to Word's own default.
fn theme_font(defaults: &TextDefaults) -> &str {
    defaults.font.as_deref().unwrap_or("Calibri")
}

/// Builds `word/theme/theme1.xml`: the standard Office colour scheme + format
/// scheme, with the document's font as the major (heading) and minor (body)
/// typeface, so Word's Design ▸ Colours/Fonts/Themes gallery has something to act
/// on and new content picks up the document font.
pub fn build_theme(defaults: &TextDefaults, _pretty: bool) -> String {
    let font = escape_attr(theme_font(defaults));
    format!(
        r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="{A_NS}" name="Office Theme"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="{font}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="{font}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:lumMod val="110000"/><a:satMod val="105000"/><a:tint val="67000"/></a:schemeClr></a:gs><a:gs pos="50000"><a:schemeClr val="phClr"><a:lumMod val="105000"/><a:satMod val="103000"/><a:tint val="73000"/></a:schemeClr></a:gs><a:gs pos="100000"><a:schemeClr val="phClr"><a:lumMod val="105000"/><a:satMod val="109000"/><a:tint val="81000"/></a:schemeClr></a:gs></a:gsLst><a:lin ang="5400000" scaled="0"/></a:gradFill><a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:satMod val="103000"/><a:lumMod val="102000"/><a:tint val="94000"/></a:schemeClr></a:gs><a:gs pos="50000"><a:schemeClr val="phClr"><a:satMod val="110000"/><a:lumMod val="100000"/><a:shade val="100000"/></a:schemeClr></a:gs><a:gs pos="100000"><a:schemeClr val="phClr"><a:lumMod val="99000"/><a:satMod val="120000"/><a:shade val="78000"/></a:schemeClr></a:gs></a:gsLst><a:lin ang="5400000" scaled="0"/></a:gradFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln><a:ln w="12700" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln><a:ln w="19050" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst><a:outerShdw blurRad="57150" dist="19050" dir="5400000" rotWithShape="0"><a:srgbClr val="000000"><a:alpha val="63000"/></a:srgbClr></a:outerShdw></a:effectLst></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"><a:tint val="95000"/><a:satMod val="170000"/></a:schemeClr></a:solidFill><a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:tint val="93000"/><a:satMod val="150000"/><a:shade val="98000"/><a:lumMod val="102000"/></a:schemeClr></a:gs><a:gs pos="50000"><a:schemeClr val="phClr"><a:tint val="98000"/><a:satMod val="130000"/><a:shade val="90000"/><a:lumMod val="103000"/></a:schemeClr></a:gs><a:gs pos="100000"><a:schemeClr val="phClr"><a:shade val="63000"/><a:satMod val="120000"/></a:schemeClr></a:gs></a:gsLst><a:lin ang="5400000" scaled="0"/></a:gradFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"##
    )
}

/// Builds `word/fontTable.xml`: the list of fonts the document references, with
/// the generic-family / pitch / charset metadata Word uses to drive substitution
/// when a font is missing on the opening machine.
pub fn build_font_table(fonts: &[String], _pretty: bool) -> String {
    let mut s = String::from(crate::xml::XML_DECL);
    s.push_str(&format!("<w:fonts xmlns:w=\"{W_NS}\">"));
    for font in fonts {
        let f = escape_attr(font);
        // A serif/roman default is a safe generic family + variable pitch; Word
        // refines this from its own font metrics on open, so a sound default is
        // enough to avoid a missing-font prompt.
        s.push_str(&format!(
            "<w:font w:name=\"{f}\"><w:charset w:val=\"00\"/><w:family w:val=\"auto\"/>\
             <w:pitch w:val=\"variable\"/></w:font>"
        ));
    }
    s.push_str("</w:fonts>");
    s
}

/// Builds `word/webSettings.xml`: the (minimal) web/HTML round-trip settings Word
/// always includes. `optimizeForBrowser` is what a freshly-saved Word doc carries.
pub fn build_web_settings(_pretty: bool) -> String {
    format!(
        "{}<w:webSettings xmlns:w=\"{W_NS}\"><w:optimizeForBrowser/><w:allowPNG/></w:webSettings>",
        crate::xml::XML_DECL
    )
}
