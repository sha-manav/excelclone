//! Reading and writing the slice of `xl/styles.xml` that Gridline models.
//!
//! xlsx keeps formatting in a normalized side table: `<cellXfs>` holds one
//! `<xf>` record per distinct cell format, and each `<xf>` points at indices
//! into `<fonts>`, `<fills>`, `<borders>` and `<numFmts>`. A cell carries only
//! an `s` attribute — the index of its `<xf>`.
//!
//! We model a deliberate subset: bold, italic, font colour, solid fill,
//! border edges, number format and horizontal alignment. Everything else in
//! that file — themes, cell styles, differential formats, font typefaces we
//! did not change — has to survive a round trip untouched, which shapes the
//! whole design here:
//!
//! * **Import** derives a [`CellFormat`] per `<xf>` and remembers the original
//!   index alongside it.
//! * **Export** writes the *original* index back for any cell whose format the
//!   user did not change, so an untouched workbook is byte-identical.
//! * Only when a cell's format actually differs is a new `<xf>` appended.
//!   Appending is safe in a way that editing is not: existing indices keep
//!   pointing at the same records, so every part of the file we did not touch
//!   stays correct.
//!
//! A synthesized font copies the original's typeface, size, underline and
//! scheme children verbatim, so bolding an 8pt Garamond cell yields bold 8pt
//! Garamond rather than bold 11pt Calibri.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;

use super::IoError;
use crate::format::{Borders, CellFormat, HAlign};

/// Number format codes Excel knows by id without writing them down. Only the
/// ids that can appear on a cell we model are listed; the rest fall through to
/// General, which is display-only and therefore safe to get wrong.
const BUILTIN_NUM_FMTS: &[(u32, &str)] = &[
    (1, "0"),
    (2, "0.00"),
    (3, "#,##0"),
    (4, "#,##0.00"),
    (9, "0%"),
    (10, "0.00%"),
    (11, "0.00E+00"),
    (12, "# ?/?"),
    (13, "# ??/??"),
    (14, "mm-dd-yy"),
    (15, "d-mmm-yy"),
    (16, "d-mmm"),
    (17, "mmm-yy"),
    (18, "h:mm AM/PM"),
    (19, "h:mm:ss AM/PM"),
    (20, "h:mm"),
    (21, "h:mm:ss"),
    (22, "m/d/yy h:mm"),
    (37, "#,##0 ;(#,##0)"),
    (38, "#,##0 ;[Red](#,##0)"),
    (39, "#,##0.00;(#,##0.00)"),
    (40, "#,##0.00;[Red](#,##0.00)"),
    (45, "mm:ss"),
    (46, "[h]:mm:ss"),
    (47, "mmss.0"),
    (48, "##0.0E+0"),
    (49, "@"),
];

/// The first id available for a format code Excel does not know by number.
const FIRST_CUSTOM_NUM_FMT: u32 = 164;

/// One `<xf>` record from `<cellXfs>`, reduced to what we need.
#[derive(Debug, Clone, Default)]
struct Xf {
    num_fmt_id: u32,
    font_id: usize,
    fill_id: usize,
    border_id: usize,
    align: Option<HAlign>,
}

/// The parts of `xl/styles.xml` we read, plus the raw font records we need in
/// order to synthesize new ones without losing the typeface.
#[derive(Debug, Clone, Default)]
pub(crate) struct StyleSheet {
    /// The `CellFormat` each `<xf>` index resolves to.
    formats: Vec<CellFormat>,
    /// Children of each `<font>` that we do not model but must carry over
    /// (`sz`, `name`, `family`, `scheme`, `u`), in document order.
    font_carry: Vec<String>,
    /// Which font each `<xf>` uses, so a synthesized xf can start from it.
    xf_font: Vec<usize>,
    counts: Counts,
    custom_num_fmts: BTreeMap<u32, String>,
}

#[derive(Debug, Clone, Default)]
struct Counts {
    fonts: usize,
    fills: usize,
    borders: usize,
    xfs: usize,
}

impl StyleSheet {
    /// The format an `s` attribute resolves to, or the default when the index
    /// is missing or out of range (which Excel treats as "no formatting").
    pub(crate) fn format_for(&self, s: Option<&str>) -> CellFormat {
        let Some(i) = s.and_then(|v| v.parse::<usize>().ok()) else {
            return CellFormat::default();
        };
        self.formats.get(i).cloned().unwrap_or_default()
    }

    pub(crate) fn parse(xml: &[u8]) -> Result<StyleSheet, IoError> {
        let mut reader = XmlReader::from_reader(xml);
        let mut out = StyleSheet::default();

        // Which collection we are inside. `<xf>` appears in both
        // `<cellStyleXfs>` and `<cellXfs>`, and only the latter is what a
        // cell's `s` indexes into — conflating them shifts every style by the
        // number of named styles in the file.
        let mut section: Option<Section> = None;
        let mut font = FontAccum::default();
        let mut fills: Vec<Option<String>> = Vec::new();
        let mut borders: Vec<Borders> = Vec::new();
        let mut fonts: Vec<(bool, bool, Option<String>)> = Vec::new();
        let mut xfs: Vec<Xf> = Vec::new();
        let mut fill: Option<String> = None;
        let mut in_pattern = false;
        let mut border = Borders::default();
        let mut cur_xf: Option<Xf> = None;

        loop {
            let event = reader.read_event().map_err(IoError::from)?;
            let (e, empty) = match &event {
                Event::Start(e) => (e, false),
                Event::Empty(e) => (e, true),
                Event::End(e) => {
                    match e.name().local_name().as_ref() {
                        b"fonts" | b"fills" | b"borders" | b"cellXfs" | b"cellStyleXfs"
                        | b"numFmts" => section = None,
                        b"font" if section == Some(Section::Fonts) => {
                            fonts.push((font.bold, font.italic, font.color.take()));
                            out.font_carry.push(std::mem::take(&mut font.carry));
                            font = FontAccum::default();
                        }
                        b"fill" if section == Some(Section::Fills) => {
                            fills.push(fill.take());
                            in_pattern = false;
                        }
                        b"border" if section == Some(Section::Borders) => {
                            borders.push(std::mem::take(&mut border));
                        }
                        b"xf" if section == Some(Section::CellXfs) => {
                            if let Some(x) = cur_xf.take() {
                                xfs.push(x);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                Event::Eof => break,
                _ => continue,
            };

            let name = e.name().local_name().as_ref().to_vec();
            match name.as_slice() {
                b"fonts" => section = Some(Section::Fonts),
                b"fills" => section = Some(Section::Fills),
                b"borders" => section = Some(Section::Borders),
                b"cellXfs" => section = Some(Section::CellXfs),
                b"cellStyleXfs" => section = Some(Section::CellStyleXfs),
                b"numFmts" => section = Some(Section::NumFmts),
                b"numFmt" => {
                    let id = attr(e, b"numFmtId").and_then(|v| v.parse::<u32>().ok());
                    let code = attr(e, b"formatCode");
                    if let (Some(id), Some(code)) = (id, code) {
                        out.custom_num_fmts.insert(id, code);
                    }
                }
                b"font" if section == Some(Section::Fonts) => {
                    if empty {
                        fonts.push((false, false, None));
                        out.font_carry.push(String::new());
                    }
                }
                b"b" if section == Some(Section::Fonts) => font.bold = bool_attr(e),
                b"i" if section == Some(Section::Fonts) => font.italic = bool_attr(e),
                b"color" if section == Some(Section::Fonts) => font.color = rgb_attr(e),
                b"sz" | b"name" | b"family" | b"scheme" | b"u" | b"charset" | b"vertAlign"
                    if section == Some(Section::Fonts) =>
                {
                    font.carry.push_str(&raw_empty_element(&name, e));
                }
                b"fill" if section == Some(Section::Fills) => {
                    if empty {
                        fills.push(None);
                    }
                }
                b"patternFill" if section == Some(Section::Fills) => {
                    // `patternType="none"` and the grey125 default both mean
                    // "no colour of ours".
                    in_pattern = attr(e, b"patternType").as_deref() == Some("solid");
                    if empty {
                        in_pattern = false;
                    }
                }
                b"fgColor" if section == Some(Section::Fills) && in_pattern => {
                    fill = rgb_attr(e);
                }
                b"border" if section == Some(Section::Borders) => {
                    border = Borders::default();
                    if empty {
                        borders.push(Borders::default());
                    }
                }
                b"left" | b"right" | b"top" | b"bottom" if section == Some(Section::Borders) => {
                    // An edge counts as drawn when it names a line style;
                    // `<left/>` with no style is Excel's "no border".
                    let on = attr(e, b"style").is_some_and(|s| s != "none");
                    match name.as_slice() {
                        b"left" => border.left = on,
                        b"right" => border.right = on,
                        b"top" => border.top = on,
                        _ => border.bottom = on,
                    }
                }
                b"xf" if section == Some(Section::CellXfs) => {
                    let x = Xf {
                        num_fmt_id: attr(e, b"numFmtId")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0),
                        font_id: attr(e, b"fontId").and_then(|v| v.parse().ok()).unwrap_or(0),
                        fill_id: attr(e, b"fillId").and_then(|v| v.parse().ok()).unwrap_or(0),
                        border_id: attr(e, b"borderId")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0),
                        align: None,
                    };
                    if empty {
                        xfs.push(x);
                    } else {
                        cur_xf = Some(x);
                    }
                }
                b"alignment" => {
                    if let Some(x) = cur_xf.as_mut() {
                        x.align = match attr(e, b"horizontal").as_deref() {
                            Some("left") => Some(HAlign::Left),
                            Some("center") | Some("centerContinuous") => Some(HAlign::Center),
                            Some("right") => Some(HAlign::Right),
                            _ => None,
                        };
                    }
                }
                _ => {}
            }
        }

        out.counts = Counts {
            fonts: fonts.len(),
            fills: fills.len(),
            borders: borders.len(),
            xfs: xfs.len(),
        };
        out.formats = xfs
            .iter()
            .map(|x| {
                let (bold, italic, font_color) = fonts.get(x.font_id).cloned().unwrap_or_default();
                CellFormat {
                    bold,
                    italic,
                    font_color,
                    fill_color: fills.get(x.fill_id).cloned().flatten(),
                    borders: borders.get(x.border_id).copied().unwrap_or_default(),
                    number_format: out.num_fmt_code(x.num_fmt_id),
                    align: x.align,
                }
            })
            .collect();
        out.xf_font = xfs.iter().map(|x| x.font_id).collect();
        Ok(out)
    }

    fn num_fmt_code(&self, id: u32) -> Option<String> {
        if id == 0 {
            return None;
        }
        if let Some(code) = self.custom_num_fmts.get(&id) {
            return Some(code.clone());
        }
        BUILTIN_NUM_FMTS
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, c)| (*c).to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Fonts,
    Fills,
    Borders,
    CellXfs,
    CellStyleXfs,
    NumFmts,
}

#[derive(Default)]
struct FontAccum {
    bold: bool,
    italic: bool,
    color: Option<String>,
    carry: String,
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == key)
        .and_then(|a| a.unescape_value().ok())
        .map(|v| v.into_owned())
}

/// `<b/>`, `<b val="1"/>` and `<b val="true"/>` are all bold; `val="0"` is not.
fn bool_attr(e: &quick_xml::events::BytesStart<'_>) -> bool {
    match attr(e, b"val").as_deref() {
        None => true,
        Some("0") | Some("false") => false,
        Some(_) => true,
    }
}

/// A colour we can model: `rgb="FFRRGGBB"` only. Theme and indexed colours
/// resolve through parts we do not parse, so they read as "no colour" — and,
/// because an unchanged cell keeps its original `s`, they still round-trip.
fn rgb_attr(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let raw = attr(e, b"rgb")?;
    let hex = raw.trim();
    let rgb = match hex.len() {
        8 => &hex[2..],
        6 => hex,
        _ => return None,
    };
    rgb.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| format!("#{}", rgb.to_ascii_lowercase()))
}

/// Re-emit an element we are carrying over verbatim.
fn raw_empty_element(name: &[u8], e: &quick_xml::events::BytesStart<'_>) -> String {
    let mut out = format!("<{}", String::from_utf8_lossy(name));
    for a in e.attributes().flatten() {
        out.push_str(&format!(
            " {}=\"{}\"",
            String::from_utf8_lossy(a.key.as_ref()),
            String::from_utf8_lossy(&a.value)
        ));
    }
    out.push_str("/>");
    out
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Records to append to `xl/styles.xml`, and the `s` index each newly-needed
/// format was assigned.
#[derive(Debug, Default)]
pub(crate) struct StyleAdditions {
    fonts: Vec<String>,
    fills: Vec<String>,
    borders: Vec<String>,
    num_fmts: Vec<(u32, String)>,
    xfs: Vec<String>,
    /// Formats we minted an index for, in the order they were requested.
    assigned: Vec<(CellFormat, usize)>,
    next_font: usize,
    next_fill: usize,
    next_border: usize,
    next_xf: usize,
    next_num_fmt: u32,
}

impl StyleAdditions {
    pub(crate) fn new(sheet: &StyleSheet) -> Self {
        let used_ids: Vec<u32> = sheet.custom_num_fmts.keys().copied().collect();
        StyleAdditions {
            next_font: sheet.counts.fonts,
            next_fill: sheet.counts.fills,
            next_border: sheet.counts.borders,
            next_xf: sheet.counts.xfs,
            next_num_fmt: used_ids
                .iter()
                .copied()
                .max()
                .map(|m| m + 1)
                .unwrap_or(FIRST_CUSTOM_NUM_FMT)
                .max(FIRST_CUSTOM_NUM_FMT),
            ..Default::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.xfs.is_empty()
    }

    /// The `s` index for a format, minting one if this is the first request.
    ///
    /// `base_font` is the font of the cell's original `<xf>`; the synthesized
    /// font copies its typeface and size so changing one attribute does not
    /// silently reset the rest.
    pub(crate) fn index_for(
        &mut self,
        sheet: &StyleSheet,
        format: &CellFormat,
        base_xf: Option<usize>,
    ) -> usize {
        if let Some((_, i)) = self.assigned.iter().find(|(f, _)| f == format) {
            return *i;
        }
        let base_font = base_xf
            .and_then(|i| sheet.xf_font.get(i).copied())
            .unwrap_or(0);
        let carry = sheet.font_carry.get(base_font).cloned().unwrap_or_default();

        let font_id = self.push_font(format, &carry);
        let fill_id = self.push_fill(format);
        let border_id = self.push_border(format);
        let num_fmt_id = self.push_num_fmt(sheet, format);

        let mut xf = format!(
            "<xf numFmtId=\"{}\" fontId=\"{}\" fillId=\"{}\" borderId=\"{}\" xfId=\"0\" \
             applyNumberFormat=\"1\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\"",
            num_fmt_id, font_id, fill_id, border_id
        );
        match format.align {
            Some(a) => {
                xf.push_str(&format!(
                    " applyAlignment=\"1\"><alignment horizontal=\"{}\"/></xf>",
                    a.as_str()
                ));
            }
            None => xf.push_str("/>"),
        }
        self.xfs.push(xf);
        let index = self.next_xf;
        self.next_xf += 1;
        self.assigned.push((format.clone(), index));
        index
    }

    fn push_font(&mut self, f: &CellFormat, carry: &str) -> usize {
        let mut font = String::from("<font>");
        if f.bold {
            font.push_str("<b/>");
        }
        if f.italic {
            font.push_str("<i/>");
        }
        if let Some(c) = &f.font_color {
            font.push_str(&format!("<color rgb=\"{}\"/>", argb(c)));
        }
        font.push_str(carry);
        font.push_str("</font>");
        self.fonts.push(font);
        let id = self.next_font;
        self.next_font += 1;
        id
    }

    fn push_fill(&mut self, f: &CellFormat) -> usize {
        // Fill 0 is always `none` in a well-formed styles.xml, so an unfilled
        // cell needs no new record at all.
        let Some(c) = &f.fill_color else { return 0 };
        self.fills.push(format!(
            "<fill><patternFill patternType=\"solid\"><fgColor rgb=\"{}\"/>\
             <bgColor indexed=\"64\"/></patternFill></fill>",
            argb(c)
        ));
        let id = self.next_fill;
        self.next_fill += 1;
        id
    }

    fn push_border(&mut self, f: &CellFormat) -> usize {
        // Border 0 is always the empty border.
        if f.borders.is_none() {
            return 0;
        }
        let edge = |name: &str, on: bool| {
            if on {
                format!("<{name} style=\"thin\"><color indexed=\"64\"/></{name}>")
            } else {
                format!("<{name}/>")
            }
        };
        self.borders.push(format!(
            "<border>{}{}{}{}<diagonal/></border>",
            edge("left", f.borders.left),
            edge("right", f.borders.right),
            edge("top", f.borders.top),
            edge("bottom", f.borders.bottom),
        ));
        let id = self.next_border;
        self.next_border += 1;
        id
    }

    fn push_num_fmt(&mut self, sheet: &StyleSheet, f: &CellFormat) -> u32 {
        let Some(code) = &f.number_format else {
            return 0;
        };
        if let Some((id, _)) = BUILTIN_NUM_FMTS.iter().find(|(_, c)| c == code) {
            return *id;
        }
        if let Some((id, _)) = sheet.custom_num_fmts.iter().find(|(_, c)| *c == code) {
            return *id;
        }
        if let Some((id, _)) = self.num_fmts.iter().find(|(_, c)| c == code) {
            return *id;
        }
        let id = self.next_num_fmt;
        self.next_num_fmt += 1;
        self.num_fmts.push((id, code.clone()));
        id
    }

    /// Splice the appended records into the original `xl/styles.xml`.
    ///
    /// Every insertion is an append inside an existing collection, so indices
    /// already written into worksheets keep pointing at the same records.
    pub(crate) fn patch(&self, original: &[u8]) -> Result<Vec<u8>, IoError> {
        if self.is_empty() {
            return Ok(original.to_vec());
        }
        let text = std::str::from_utf8(original)
            .map_err(|_| IoError::Malformed("xl/styles.xml is not UTF-8".into()))?;
        let mut out = text.to_string();

        // Applied last-first so earlier offsets stay valid.
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        edits.push(collection_append(&out, "cellXfs", &self.xfs, self.next_xf)?);
        if !self.borders.is_empty() {
            edits.push(collection_append(
                &out,
                "borders",
                &self.borders,
                self.next_border,
            )?);
        }
        if !self.fills.is_empty() {
            edits.push(collection_append(
                &out,
                "fills",
                &self.fills,
                self.next_fill,
            )?);
        }
        edits.push(collection_append(
            &out,
            "fonts",
            &self.fonts,
            self.next_font,
        )?);
        if !self.num_fmts.is_empty() {
            let items: Vec<String> = self
                .num_fmts
                .iter()
                .map(|(id, code)| {
                    format!(
                        "<numFmt numFmtId=\"{}\" formatCode=\"{}\"/>",
                        id,
                        super::xlsx::escape_xml(code)
                    )
                })
                .collect();
            edits.push(num_fmts_append(&out, &items)?);
        }

        // Sort by (start, end) so that when an insertion point coincides with
        // the start of a replacement, the zero-length insertion sorts first
        // and is therefore applied *last*. Sorting on `start` alone leaves the
        // order to sort stability, and the replacement then overwrites the
        // text the insertion just placed.
        edits.sort_by_key(|(start, end, _)| (*start, *end));
        for (start, end, text) in edits.into_iter().rev() {
            out.replace_range(start..end, &text);
        }
        Ok(out.into_bytes())
    }
}

/// A colour in the ARGB spelling xlsx uses, fully opaque.
fn argb(c: &str) -> String {
    format!("FF{}", c.trim_start_matches('#').to_ascii_uppercase())
}

/// Find `</name>`, and return the edit that inserts `items` before it while
/// updating the collection's `count` attribute.
fn collection_append(
    xml: &str,
    name: &str,
    items: &[String],
    new_count: usize,
) -> Result<(usize, usize, String), IoError> {
    let close = format!("</{}>", name);
    let Some(at) = xml.find(&close) else {
        return Err(IoError::Unrepresentable(format!(
            "xl/styles.xml has no <{name}> collection to extend; \
             formatting an imported workbook with an unusual style sheet is not supported"
        )));
    };
    // Rewrite `count` on the opening tag in the same edit, so the span covers
    // the whole element and the two changes cannot be applied out of order.
    let open = format!("<{}", name);
    let Some(open_at) = xml[..at].rfind(&open) else {
        return Err(IoError::Malformed(format!(
            "xl/styles.xml closes <{name}> without opening it"
        )));
    };
    let Some(open_end) = xml[open_at..at].find('>').map(|i| open_at + i + 1) else {
        return Err(IoError::Malformed(format!(
            "xl/styles.xml has a malformed <{name}> tag"
        )));
    };
    let head = &xml[open_at..open_end];
    let head = replace_count(head, new_count);
    let body = &xml[open_end..at];
    Ok((
        open_at,
        at + close.len(),
        format!("{head}{body}{}{close}", items.concat()),
    ))
}

/// `<numFmts>` is optional, so it may have to be created rather than extended.
/// It comes first in the `<styleSheet>` sequence, immediately after the root.
fn num_fmts_append(xml: &str, items: &[String]) -> Result<(usize, usize, String), IoError> {
    if xml.contains("</numFmts>") {
        let existing = xml.matches("<numFmt ").count();
        return collection_append(xml, "numFmts", items, existing + items.len());
    }
    let Some(root_end) = xml.find("<styleSheet").and_then(|at| {
        xml[at..]
            .find('>')
            .map(|i| at + i + 1)
            .filter(|_| !xml[at..].starts_with("<styleSheet/>"))
    }) else {
        return Err(IoError::Unrepresentable(
            "xl/styles.xml has no <styleSheet> root to extend".into(),
        ));
    };
    Ok((
        root_end,
        root_end,
        format!(
            "<numFmts count=\"{}\">{}</numFmts>",
            items.len(),
            items.concat()
        ),
    ))
}

/// Replace `count="N"` in an opening tag, adding it when absent.
fn replace_count(open_tag: &str, count: usize) -> String {
    let Some(at) = open_tag.find("count=\"") else {
        // No count attribute: insert one just after the element name.
        let insert = open_tag
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .unwrap_or(open_tag.len());
        return format!(
            "{} count=\"{}\"{}",
            &open_tag[..insert],
            count,
            &open_tag[insert..]
        );
    };
    let value_start = at + "count=\"".len();
    let Some(rel_end) = open_tag[value_start..].find('"') else {
        return open_tag.to_string();
    };
    format!(
        "{}{}{}",
        &open_tag[..value_start],
        count,
        &open_tag[value_start + rel_end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"<?xml version="1.0"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="1"><numFmt numFmtId="164" formatCode="&quot;$&quot;#,##0.00"/></numFmts>
<fonts count="3">
  <font><sz val="11"/><color theme="1"/><name val="Calibri"/><family val="2"/><scheme val="minor"/></font>
  <font><b/><sz val="8"/><color rgb="FFB3261E"/><name val="Garamond"/></font>
  <font><i/><u/><sz val="11"/><name val="Calibri"/></font>
</fonts>
<fills count="3">
  <fill><patternFill patternType="none"/></fill>
  <fill><patternFill patternType="gray125"/></fill>
  <fill><patternFill patternType="solid"><fgColor rgb="FFEEEEEE"/><bgColor indexed="64"/></patternFill></fill>
</fills>
<borders count="2">
  <border><left/><right/><top/><bottom/><diagonal/></border>
  <border><left style="thin"><color indexed="64"/></left><right/><top style="thin"><color indexed="64"/></top><bottom/><diagonal/></border>
</borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="5">
  <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
  <xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1"/>
  <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
  <xf numFmtId="9" fontId="2" fillId="0" borderId="1" xfId="0"/>
  <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyAlignment="1"><alignment horizontal="center" vertical="top"/></xf>
</cellXfs>
</styleSheet>"##;

    fn parsed() -> StyleSheet {
        StyleSheet::parse(SAMPLE.as_bytes()).unwrap()
    }

    #[test]
    fn the_default_xf_is_no_formatting() {
        assert!(parsed().format_for(Some("0")).is_default());
        assert!(parsed().format_for(None).is_default());
        // An index past the end is Excel's "no formatting", not an error.
        assert!(parsed().format_for(Some("99")).is_default());
    }

    #[test]
    fn font_and_fill_are_read() {
        let f = parsed().format_for(Some("1"));
        assert!(f.bold);
        assert!(!f.italic);
        assert_eq!(f.font_color.as_deref(), Some("#b3261e"));
        assert_eq!(f.fill_color.as_deref(), Some("#eeeeee"));
    }

    #[test]
    fn a_theme_colour_reads_as_no_colour_rather_than_a_wrong_one() {
        // Font 0's colour is `theme="1"`, which resolves through a part we do
        // not parse. Guessing black would be wrong in a dark theme.
        assert_eq!(parsed().format_for(Some("0")).font_color, None);
    }

    #[test]
    fn custom_and_builtin_number_formats_both_resolve() {
        assert_eq!(
            parsed().format_for(Some("2")).number_format.as_deref(),
            Some("\"$\"#,##0.00")
        );
        assert_eq!(
            parsed().format_for(Some("3")).number_format.as_deref(),
            Some("0%")
        );
    }

    #[test]
    fn borders_read_per_edge() {
        let b = parsed().format_for(Some("3")).borders;
        assert!(b.left && b.top);
        assert!(!b.right && !b.bottom);
    }

    #[test]
    fn alignment_reads_only_the_horizontal_part() {
        // xf 4 is `horizontal="center" vertical="top"`; we model neither
        // vertical alignment nor wrapping, so only the horizontal survives.
        assert_eq!(parsed().format_for(Some("4")).align, Some(HAlign::Center));
    }

    #[test]
    fn cell_style_xfs_do_not_shift_the_cell_xf_indices() {
        // The file has one <cellStyleXfs> entry before <cellXfs>. If the two
        // were conflated, index 1 would be the default rather than the bold
        // Garamond one, and every styled cell in the workbook would render
        // with its neighbour's formatting.
        assert_eq!(parsed().formats.len(), 5);
        assert!(parsed().format_for(Some("1")).bold);
    }

    #[test]
    fn a_synthesized_font_keeps_the_original_typeface() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        // Italicise the bold 8pt Garamond cell (xf 1).
        let mut want = sheet.format_for(Some("1"));
        want.italic = true;
        let idx = adds.index_for(&sheet, &want, Some(1));
        assert_eq!(idx, 5, "the new xf should be appended, not overwrite one");
        let font = &adds.fonts[0];
        assert!(font.contains("<b/>") && font.contains("<i/>"));
        assert!(
            font.contains("val=\"8\"") && font.contains("Garamond"),
            "the typeface was reset: {font}"
        );
    }

    #[test]
    fn the_same_format_twice_gets_one_new_xf() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        let f = CellFormat {
            bold: true,
            ..Default::default()
        };
        assert_eq!(adds.index_for(&sheet, &f, None), 5);
        assert_eq!(adds.index_for(&sheet, &f, None), 5);
        assert_eq!(adds.xfs.len(), 1);
    }

    #[test]
    fn an_unfilled_unbordered_format_reuses_the_zero_records() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        adds.index_for(
            &sheet,
            &CellFormat {
                bold: true,
                ..Default::default()
            },
            None,
        );
        assert!(adds.fills.is_empty(), "should reuse fill 0");
        assert!(adds.borders.is_empty(), "should reuse border 0");
        assert!(adds.xfs[0].contains("fillId=\"0\"") && adds.xfs[0].contains("borderId=\"0\""));
    }

    #[test]
    fn an_existing_custom_format_code_is_reused_not_duplicated() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        adds.index_for(
            &sheet,
            &CellFormat {
                number_format: Some("\"$\"#,##0.00".into()),
                ..Default::default()
            },
            None,
        );
        assert!(adds.num_fmts.is_empty(), "should reuse numFmtId 164");
        assert!(adds.xfs[0].contains("numFmtId=\"164\""));
    }

    #[test]
    fn a_new_custom_format_code_does_not_collide_with_an_existing_id() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        adds.index_for(
            &sheet,
            &CellFormat {
                number_format: Some("0.000".into()),
                ..Default::default()
            },
            None,
        );
        assert_eq!(adds.num_fmts[0].0, 165, "164 is already taken");
    }

    #[test]
    fn patching_appends_and_updates_every_count() {
        let sheet = parsed();
        let mut adds = StyleAdditions::new(&sheet);
        adds.index_for(
            &sheet,
            &CellFormat {
                bold: true,
                fill_color: Some("#ff0000".into()),
                borders: Borders::all(),
                number_format: Some("0.000".into()),
                align: Some(HAlign::Right),
                ..Default::default()
            },
            None,
        );
        let out = String::from_utf8(adds.patch(SAMPLE.as_bytes()).unwrap()).unwrap();
        assert!(out.contains("<cellXfs count=\"6\">"));
        assert!(out.contains("<fonts count=\"4\">"));
        assert!(out.contains("<fills count=\"4\">"));
        assert!(out.contains("<borders count=\"3\">"));
        assert!(out.contains("<numFmts count=\"2\">"));
        assert!(out.contains("horizontal=\"right\""));
        // The originals must survive untouched.
        assert!(out.contains("Garamond"));
        assert!(out.contains("<numFmt numFmtId=\"164\""));
        // And the new xf must be last, so index 5 refers to it.
        let xfs: Vec<&str> = out.split("<xf ").collect();
        assert!(xfs.last().unwrap().contains("fillId=\"3\""));
    }

    #[test]
    fn patching_nothing_returns_the_file_unchanged() {
        let sheet = parsed();
        let adds = StyleAdditions::new(&sheet);
        assert_eq!(adds.patch(SAMPLE.as_bytes()).unwrap(), SAMPLE.as_bytes());
    }

    #[test]
    fn a_style_sheet_with_no_numfmts_gains_one() {
        let xml = r#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts>
<fills count="1"><fill/></fills><borders count="1"><border/></borders>
<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#;
        let sheet = StyleSheet::parse(xml.as_bytes()).unwrap();
        let mut adds = StyleAdditions::new(&sheet);
        adds.index_for(
            &sheet,
            &CellFormat {
                number_format: Some("0.000".into()),
                ..Default::default()
            },
            None,
        );
        let out = String::from_utf8(adds.patch(xml.as_bytes()).unwrap()).unwrap();
        assert!(out.contains("<numFmts count=\"1\"><numFmt numFmtId=\"164\""));
        // It has to land immediately after the root, before <fonts>.
        assert!(out.find("<numFmts").unwrap() < out.find("<fonts").unwrap());
    }

    #[test]
    fn count_rewriting_handles_a_missing_attribute() {
        assert_eq!(replace_count("<fonts>", 3), "<fonts count=\"3\">");
        assert_eq!(
            replace_count("<fonts count=\"1\">", 3),
            "<fonts count=\"3\">"
        );
        assert_eq!(
            replace_count("<fonts x=\"y\" count=\"12\">", 3),
            "<fonts x=\"y\" count=\"3\">"
        );
    }

    #[test]
    fn bold_flags_respect_an_explicit_false() {
        let xml = r#"<styleSheet><fonts count="2"><font><b val="0"/></font><font><b val="1"/></font></fonts>
<fills count="1"><fill/></fills><borders count="1"><border/></borders>
<cellXfs count="2"><xf fontId="0"/><xf fontId="1"/></cellXfs></styleSheet>"#;
        let s = StyleSheet::parse(xml.as_bytes()).unwrap();
        assert!(!s.format_for(Some("0")).bold);
        assert!(s.format_for(Some("1")).bold);
    }
}
