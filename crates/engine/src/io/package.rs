//! Package-level surgery: `xl/workbook.xml`, its relationships, and
//! `[Content_Types].xml`.
//!
//! Adding, renaming or deleting a sheet is the one edit that cannot be
//! contained inside a worksheet part. The sheet list lives in
//! `xl/workbook.xml`, the part it points at is named in
//! `xl/_rels/workbook.xml.rels`, and a new part needs a content-type override
//! in `[Content_Types].xml`. Three files, all of which the rest of the
//! importer deliberately never touches.
//!
//! Everything here is a **splice into the original bytes**, never a
//! regeneration. A workbook whose sheet list did not change must come back
//! byte-identical — attribute order, `state="hidden"`, `extLst` and all — and
//! the only way to promise that is to write nothing when nothing changed, and
//! to change only the bytes that must change when something did.
//!
//! Sheets are tracked by [`SheetId`], not by name. A rename would otherwise be
//! indistinguishable from deleting one sheet and adding another, and the
//! renamed sheet would lose everything the original part held that we do not
//! model: its columns, its conditional formatting, its drawings.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;

use super::IoError;
use crate::model::SheetId;

pub(crate) const WORKBOOK_PART: &str = "xl/workbook.xml";
pub(crate) const WORKBOOK_RELS_PART: &str = "xl/_rels/workbook.xml.rels";
pub(crate) const CONTENT_TYPES_PART: &str = "[Content_Types].xml";
pub(crate) const CALC_CHAIN_PART: &str = "xl/calcChain.xml";

const WORKSHEET_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
const WORKSHEET_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const SPREADSHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

/// Where one model sheet's XML lives in the package.
#[derive(Debug, Clone)]
pub(crate) struct SheetSlot {
    pub(crate) sheet_id: SheetId,
    pub(crate) part: String,
    /// True when the part did not exist at import and this export creates it.
    pub(crate) fresh: bool,
}

/// Everything export needs to do about a changed sheet list.
#[derive(Debug, Default)]
pub(crate) struct PackagePlan {
    /// One slot per model sheet, in model order.
    pub(crate) slots: Vec<SheetSlot>,
    /// Parts to leave out of the written zip.
    pub(crate) dropped: Vec<String>,
    /// Rewritten package parts, keyed by zip part name.
    pub(crate) patches: Vec<(String, Vec<u8>)>,
}

#[cfg(test)]
impl PackagePlan {
    pub(crate) fn part_for(&self, id: SheetId) -> Option<&str> {
        self.slots
            .iter()
            .find(|s| s.sheet_id == id)
            .map(|s| s.part.as_str())
    }
}

/// What the package currently says about one `<sheet>` element.
#[derive(Debug, Clone)]
struct SheetElem {
    /// Byte range of the whole `<sheet .../>` element in `xl/workbook.xml`.
    span: std::ops::Range<usize>,
    /// The element's bytes (tag name and attributes, no angle brackets), so a
    /// rename can rewrite one attribute and leave the others —
    /// `state="hidden"` above all — exactly as they were.
    raw: Vec<u8>,
    /// Length of the tag name within `raw`; quick-xml needs it to re-read the
    /// attributes back out.
    name_len: usize,
    name: String,
    rid: String,
    xlsx_sheet_id: u32,
}

/// The `<sheet>` elements, and where a new one would go.
struct WorkbookSheets {
    elems: Vec<SheetElem>,
    /// Offset of `</sheets>`, where a new `<sheet>` is inserted.
    insert_at: usize,
    /// Span of `<definedNames>`, when the file has one, and the offset a new
    /// one goes at. The schema puts it after `<sheets>`, so that is where.
    defined_names: Option<std::ops::Range<usize>>,
    /// The attribute key an existing element uses for the relationship id,
    /// e.g. `r:id`. Taken from the file rather than assumed, because the
    /// prefix bound to the relationships namespace is the author's choice.
    rid_key: String,
}

/// Build the plan for a workbook whose sheets may have changed.
///
/// `model` is the workbook's sheets in order; `imported` maps the sheets that
/// came from the file to their parts. `part_names` is every zip entry, used so
/// a generated part cannot collide with one already there.
pub(crate) fn plan(
    model: &[(SheetId, String)],
    imported: &[(SheetId, String)],
    parts: &Parts<'_>,
    part_names: &[String],
    // `defined_names`: replacement `<definedNames>` XML, or None to leave the
    // element exactly as it was. Empty means remove it — a workbook with no
    // names should not carry an empty element for a reader to tolerate.
    defined_names: Option<String>,
) -> Result<PackagePlan, IoError> {
    let book = scan_workbook(parts.workbook)?;
    let rel_targets = scan_rel_targets(parts.rels)?;

    // Part name -> the <sheet> element that points at it.
    let mut elem_for_part: HashMap<&str, &SheetElem> = HashMap::new();
    for elem in &book.elems {
        if let Some(part) = rel_targets.get(&elem.rid) {
            elem_for_part.insert(part.as_str(), elem);
        }
    }

    let mut slots = Vec::new();
    let mut renames: Vec<(&SheetElem, &str)> = Vec::new();
    let mut additions: Vec<(String, String, u32, &str)> = Vec::new(); // part, rid, sheetId, name
    let mut next_rid = next_rel_id(parts.rels)?;
    let mut next_sheet_id = book
        .elems
        .iter()
        .map(|e| e.xlsx_sheet_id)
        .max()
        .unwrap_or(0)
        + 1;
    let mut taken: Vec<String> = part_names.to_vec();

    for (id, name) in model {
        match imported.iter().find(|(sid, _)| sid == id) {
            Some((_, part)) => {
                if let Some(elem) = elem_for_part.get(part.as_str()) {
                    if elem.name != *name {
                        renames.push((elem, name));
                    }
                }
                slots.push(SheetSlot {
                    sheet_id: *id,
                    part: part.clone(),
                    fresh: false,
                });
            }
            None => {
                let part = free_part_name(&taken);
                taken.push(part.clone());
                let rid = format!("rId{next_rid}");
                next_rid += 1;
                additions.push((part.clone(), rid, next_sheet_id, name));
                next_sheet_id += 1;
                slots.push(SheetSlot {
                    sheet_id: *id,
                    part,
                    fresh: true,
                });
            }
        }
    }

    // Parts whose sheet the model no longer has.
    let mut dropped: Vec<String> = Vec::new();
    let mut removed_elems: Vec<&SheetElem> = Vec::new();
    for (id, part) in imported {
        if model.iter().any(|(sid, _)| sid == id) {
            continue;
        }
        dropped.push(part.clone());
        if let Some(part_rels) = sibling_rels(part) {
            if part_names.contains(&part_rels) {
                dropped.push(part_rels);
            }
        }
        if let Some(elem) = elem_for_part.get(part.as_str()) {
            removed_elems.push(elem);
        }
    }

    let mut plan = PackagePlan {
        slots,
        dropped,
        patches: Vec::new(),
    };
    // A defined name changing rewrites `xl/workbook.xml` even when the sheet
    // list did not, because that is where the names live.
    let names_changed = defined_names.is_some();
    if renames.is_empty() && additions.is_empty() && removed_elems.is_empty() && !names_changed {
        // Nothing about the sheet list changed, so nothing at package level is
        // rewritten and every one of these parts survives byte-identical.
        return Ok(plan);
    }

    plan.patches.push((
        WORKBOOK_PART.to_string(),
        patch_workbook(
            parts.workbook,
            &book,
            &renames,
            &additions,
            &removed_elems,
            defined_names,
        )?,
    ));
    if !additions.is_empty() || !removed_elems.is_empty() {
        let removed_rids: Vec<&str> = removed_elems.iter().map(|e| e.rid.as_str()).collect();
        plan.patches.push((
            WORKBOOK_RELS_PART.to_string(),
            patch_rels(parts.rels, &additions, &removed_rids)?,
        ));
        let added_parts: Vec<&str> = additions.iter().map(|(p, ..)| p.as_str()).collect();
        if let Some(types) = parts.content_types {
            plan.patches.push((
                CONTENT_TYPES_PART.to_string(),
                patch_content_types(types, &added_parts, &plan.dropped)?,
            ));
        }
        // `xl/calcChain.xml` caches which cells to recalculate, keyed by sheet
        // *index*. Adding or deleting a sheet renumbers those indices, and a
        // stale chain makes Excel offer to repair the file. It is a cache:
        // dropping it costs a recalculation on open and nothing else.
        if part_names.iter().any(|n| n == CALC_CHAIN_PART) {
            plan.dropped.push(CALC_CHAIN_PART.to_string());
        }
    }
    Ok(plan)
}

/// The three package parts the plan reads.
pub(crate) struct Parts<'a> {
    pub(crate) workbook: &'a [u8],
    pub(crate) rels: &'a [u8],
    pub(crate) content_types: Option<&'a [u8]>,
}

/// Declare a part in `xl/_rels/workbook.xml.rels`, returning the patched bytes
/// and the relationship id it was given.
pub(crate) fn add_relationship(
    rels: &[u8],
    typ: &str,
    target: &str,
) -> Result<(Vec<u8>, String), IoError> {
    let id = format!("rId{}", next_rel_id(rels)?);
    let text = std::str::from_utf8(rels)
        .map_err(|_| IoError::Malformed(format!("{WORKBOOK_RELS_PART} is not UTF-8")))?;
    let Some(close) = text.rfind("</Relationships>") else {
        return Err(IoError::Malformed(format!(
            "{WORKBOOK_RELS_PART} has no </Relationships>"
        )));
    };
    let element = format!("<Relationship Id=\"{id}\" Type=\"{typ}\" Target=\"{target}\"/>");
    Ok((splice(rels, vec![(close, close, element)]), id))
}

/// Declare a part's content type in `[Content_Types].xml`.
pub(crate) fn add_override(
    types: &[u8],
    part: &str,
    content_type: &str,
) -> Result<Vec<u8>, IoError> {
    let text = std::str::from_utf8(types)
        .map_err(|_| IoError::Malformed(format!("{CONTENT_TYPES_PART} is not UTF-8")))?;
    let Some(close) = text.rfind("</Types>") else {
        return Err(IoError::Malformed(format!(
            "{CONTENT_TYPES_PART} has no </Types>"
        )));
    };
    let element = format!("<Override PartName=\"/{part}\" ContentType=\"{content_type}\"/>");
    Ok(splice(types, vec![(close, close, element)]))
}

/// A worksheet part with nothing in it, ready for `patch_sheet_xml` to fill.
///
/// The `<dimension>` is present but wrong on purpose: patching rewrites it
/// from the sheet's used range, and an element that is not there cannot be
/// rewritten.
pub(crate) fn empty_worksheet() -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <worksheet xmlns=\"{SPREADSHEET_NS}\"><dimension ref=\"A1\"/><sheetData/></worksheet>"
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn scan_workbook(xml: &[u8]) -> Result<WorkbookSheets, IoError> {
    let mut elems = Vec::new();
    let mut insert_at = None;
    let mut rid_key = None;
    let mut defined_names: Option<std::ops::Range<usize>> = None;
    let mut reader = XmlReader::from_reader(xml);
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(IoError::from)?;
        let end = reader.buffer_position() as usize;
        match event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                match e.name().local_name().as_ref() {
                    // An empty element is its own span; a start tag has its
                    // end filled in when the closing tag turns up.
                    b"definedNames" => {
                        defined_names = Some(start..end);
                        continue;
                    }
                    b"sheet" => {}
                    _ => continue,
                }
                let (mut name, mut rid, mut sheet_id) = (None, None, 0u32);
                for attr in e.attributes().flatten() {
                    let value = attr.unescape_value().unwrap_or_default().into_owned();
                    match attr.key.local_name().as_ref() {
                        b"name" => name = Some(value),
                        // `r:id`. `sheetId` has a different local name, so
                        // matching on the local name alone is unambiguous.
                        b"id" => {
                            rid_key = Some(String::from_utf8_lossy(attr.key.as_ref()).into_owned());
                            rid = Some(value);
                        }
                        b"sheetId" => sheet_id = value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                if let (Some(name), Some(rid)) = (name, rid) {
                    elems.push(SheetElem {
                        span: start..end,
                        name_len: e.name().as_ref().len(),
                        raw: e.to_vec(),
                        name,
                        rid,
                        xlsx_sheet_id: sheet_id,
                    });
                }
            }
            Event::End(e) if e.name().local_name().as_ref() == b"sheets" => {
                insert_at = Some(start);
            }
            Event::End(e) if e.name().local_name().as_ref() == b"definedNames" => {
                if let Some(r) = &mut defined_names {
                    r.end = end;
                }
            }
            _ => {}
        }
    }
    let Some(insert_at) = insert_at else {
        return Err(IoError::Malformed(
            "xl/workbook.xml has no <sheets> element".into(),
        ));
    };
    Ok(WorkbookSheets {
        elems,
        insert_at,
        defined_names,
        rid_key: rid_key.unwrap_or_else(|| "r:id".into()),
    })
}

/// Relationship id -> worksheet part name, for the workbook's relationships.
fn scan_rel_targets(xml: &[u8]) -> Result<HashMap<String, String>, IoError> {
    let mut out = HashMap::new();
    for rel in scan_rels(xml)? {
        if rel.typ.is_empty() || rel.typ.ends_with("/worksheet") {
            out.insert(rel.id, super::xlsx::resolve_target(&rel.target));
        }
    }
    Ok(out)
}

struct Relationship {
    id: String,
    target: String,
    typ: String,
}

fn scan_rels(xml: &[u8]) -> Result<Vec<Relationship>, IoError> {
    let mut out = Vec::new();
    let mut reader = XmlReader::from_reader(xml);
    loop {
        match reader.read_event().map_err(IoError::from)? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                if e.name().local_name().as_ref() != b"Relationship" {
                    continue;
                }
                let (mut id, mut target, mut typ) = (None, None, String::new());
                for attr in e.attributes().flatten() {
                    let value = attr.unescape_value().unwrap_or_default().into_owned();
                    match attr.key.local_name().as_ref() {
                        b"Id" => id = Some(value),
                        b"Target" => target = Some(value),
                        b"Type" => typ = value,
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    out.push(Relationship { id, target, typ });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// One past the highest `rIdN` in the file, so a new id collides with nothing
/// — including relationships that have nothing to do with worksheets.
fn next_rel_id(xml: &[u8]) -> Result<u32, IoError> {
    let highest = scan_rels(xml)?
        .iter()
        .filter_map(|r| r.id.strip_prefix("rId").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0);
    Ok(highest + 1)
}

fn free_part_name(taken: &[String]) -> String {
    for n in 1u32.. {
        let candidate = format!("xl/worksheets/sheet{n}.xml");
        if !taken.iter().any(|t| t.eq_ignore_ascii_case(&candidate)) {
            return candidate;
        }
    }
    unreachable!("u32 exhausted")
}

/// `xl/worksheets/sheet2.xml` -> `xl/worksheets/_rels/sheet2.xml.rels`.
fn sibling_rels(part: &str) -> Option<String> {
    let (dir, file) = part.rsplit_once('/')?;
    Some(format!("{dir}/_rels/{file}.rels"))
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Apply spliced edits to a buffer, last-first so earlier offsets stay valid.
///
/// Sorted by `(start, end)` so a zero-length insertion that shares an offset
/// with a replacement is applied *after* it rather than being overwritten —
/// the same ordering trap `styles.rs` documents.
fn splice(original: &[u8], mut edits: Vec<(usize, usize, String)>) -> Vec<u8> {
    let mut out = original.to_vec();
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    for (start, end, text) in edits.into_iter().rev() {
        out.splice(start..end, text.into_bytes());
    }
    out
}

fn patch_workbook(
    original: &[u8],
    book: &WorkbookSheets,
    renames: &[(&SheetElem, &str)],
    additions: &[(String, String, u32, &str)],
    removed: &[&SheetElem],
    defined_names: Option<String>,
) -> Result<Vec<u8>, IoError> {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    if let Some(xml) = defined_names {
        match &book.defined_names {
            Some(span) => edits.push((span.start, span.end, xml)),
            // The schema puts `<definedNames>` immediately after `<sheets>`,
            // and `insert_at` is the offset of `</sheets>` — so past its
            // closing tag is where a new one goes.
            None if xml.is_empty() => {}
            None => {
                let after = book.insert_at + b"</sheets>".len();
                edits.push((after, after, xml));
            }
        }
    }
    for (elem, new_name) in renames {
        edits.push((
            elem.span.start,
            elem.span.end,
            rename_element(&elem.raw, elem.name_len, new_name)?,
        ));
    }
    for elem in removed {
        edits.push((elem.span.start, elem.span.end, String::new()));
    }
    let mut inserted = String::new();
    for (_, rid, sheet_id, name) in additions {
        inserted.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{}\" {}=\"{}\"/>",
            super::xlsx::escape_xml(name),
            sheet_id,
            book.rid_key,
            rid
        ));
    }
    if !inserted.is_empty() {
        edits.push((book.insert_at, book.insert_at, inserted));
    }
    Ok(splice(original, edits))
}

/// Rewrite one `<sheet>` element's `name`, keeping every other attribute.
fn rename_element(raw: &[u8], name_len: usize, new_name: &str) -> Result<String, IoError> {
    let e = quick_xml::events::BytesStart::from_content(String::from_utf8_lossy(raw), name_len);
    let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut out = format!("<{tag}");
    for attr in e.attributes() {
        let attr = attr.map_err(|e| IoError::Malformed(format!("xl/workbook.xml: {e}")))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = if attr.key.local_name().as_ref() == b"name" {
            super::xlsx::escape_xml(new_name)
        } else {
            // Re-escaped from the decoded value rather than copied raw, so a
            // name with an ampersand in it cannot produce invalid XML.
            super::xlsx::escape_xml(&attr.unescape_value().unwrap_or_default())
        };
        out.push_str(&format!(" {key}=\"{value}\""));
    }
    out.push_str("/>");
    Ok(out)
}

fn patch_rels(
    original: &[u8],
    additions: &[(String, String, u32, &str)],
    removed_rids: &[&str],
) -> Result<Vec<u8>, IoError> {
    let text = std::str::from_utf8(original)
        .map_err(|_| IoError::Malformed(format!("{WORKBOOK_RELS_PART} is not UTF-8")))?;
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    if !removed_rids.is_empty() {
        let mut reader = XmlReader::from_reader(original);
        loop {
            let start = reader.buffer_position() as usize;
            let event = reader.read_event().map_err(IoError::from)?;
            let end = reader.buffer_position() as usize;
            match event {
                Event::Eof => break,
                Event::Start(e) | Event::Empty(e) => {
                    if e.name().local_name().as_ref() != b"Relationship" {
                        continue;
                    }
                    let id = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == b"Id")
                        .map(|a| a.unescape_value().unwrap_or_default().into_owned())
                        .unwrap_or_default();
                    if removed_rids.contains(&id.as_str()) {
                        edits.push((start, end, String::new()));
                    }
                }
                _ => {}
            }
        }
    }

    if !additions.is_empty() {
        let Some(close) = text.rfind("</Relationships>") else {
            return Err(IoError::Malformed(format!(
                "{WORKBOOK_RELS_PART} has no </Relationships>"
            )));
        };
        let mut added = String::new();
        for (part, rid, _, _) in additions {
            // Targets are relative to `xl/`, which is where workbook.xml lives.
            let target = part.strip_prefix("xl/").unwrap_or(part);
            added.push_str(&format!(
                "<Relationship Id=\"{rid}\" Type=\"{WORKSHEET_REL_TYPE}\" Target=\"{target}\"/>"
            ));
        }
        edits.push((close, close, added));
    }
    Ok(splice(original, edits))
}

fn patch_content_types(
    original: &[u8],
    added_parts: &[&str],
    dropped: &[String],
) -> Result<Vec<u8>, IoError> {
    let text = std::str::from_utf8(original)
        .map_err(|_| IoError::Malformed(format!("{CONTENT_TYPES_PART} is not UTF-8")))?;
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    let mut reader = XmlReader::from_reader(original);
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(IoError::from)?;
        let end = reader.buffer_position() as usize;
        match event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                if e.name().local_name().as_ref() != b"Override" {
                    continue;
                }
                let name = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.local_name().as_ref() == b"PartName")
                    .map(|a| a.unescape_value().unwrap_or_default().into_owned())
                    .unwrap_or_default();
                let part = name.trim_start_matches('/');
                if dropped.iter().any(|d| d == part) {
                    edits.push((start, end, String::new()));
                }
            }
            _ => {}
        }
    }

    if !added_parts.is_empty() {
        let Some(close) = text.rfind("</Types>") else {
            return Err(IoError::Malformed(format!(
                "{CONTENT_TYPES_PART} has no </Types>"
            )));
        };
        let mut added = String::new();
        for part in added_parts {
            added.push_str(&format!(
                "<Override PartName=\"/{part}\" ContentType=\"{WORKSHEET_CONTENT_TYPE}\"/>"
            ));
        }
        edits.push((close, close, added));
    }
    Ok(splice(original, edits))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKBOOK: &[u8] = br#"<?xml version="1.0"?>
<workbook xmlns:r="http://x"><sheets><sheet name="Books" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="4" state="hidden" r:id="rId3"/></sheets></workbook>"#;

    const RELS: &[u8] = br#"<?xml version="1.0"?>
<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet7.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

    const TYPES: &[u8] = br#"<?xml version="1.0"?>
<Types><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="ws"/><Override PartName="/xl/worksheets/sheet7.xml" ContentType="ws"/></Types>"#;

    fn parts() -> Parts<'static> {
        Parts {
            workbook: WORKBOOK,
            rels: RELS,
            content_types: Some(TYPES),
        }
    }

    fn names() -> Vec<String> {
        [
            "[Content_Types].xml",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/sheet7.xml",
            "xl/calcChain.xml",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn imported() -> Vec<(SheetId, String)> {
        vec![
            (SheetId(0), "xl/worksheets/sheet1.xml".into()),
            (SheetId(1), "xl/worksheets/sheet7.xml".into()),
        ]
    }

    fn patch_of<'a>(plan: &'a PackagePlan, part: &str) -> &'a str {
        let bytes = plan
            .patches
            .iter()
            .find(|(n, _)| n == part)
            .map(|(_, b)| b.as_slice())
            .unwrap_or_else(|| panic!("no patch for {part}"));
        std::str::from_utf8(bytes).unwrap()
    }

    #[test]
    fn an_untouched_sheet_list_rewrites_nothing() {
        // The whole preservation promise rests on this: a workbook saved after
        // editing a cell must not have its package parts regenerated.
        let model = vec![
            (SheetId(0), "Books".to_string()),
            (SheetId(1), "Notes".into()),
        ];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        assert!(plan.patches.is_empty());
        assert!(plan.dropped.is_empty());
        assert_eq!(plan.part_for(SheetId(1)), Some("xl/worksheets/sheet7.xml"));
    }

    #[test]
    fn a_rename_keeps_the_part_and_every_other_attribute() {
        // Tracking by id rather than name is what makes this a rename instead
        // of a delete and an add — and so what keeps the sheet's columns,
        // conditional formatting and drawings.
        let model = vec![
            (SheetId(0), "Books".to_string()),
            (SheetId(1), "Archive".into()),
        ];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        assert_eq!(plan.part_for(SheetId(1)), Some("xl/worksheets/sheet7.xml"));
        assert!(!plan.slots[1].fresh);

        let out = patch_of(&plan, WORKBOOK_PART);
        assert!(out.contains(r#"name="Archive""#), "{out}");
        assert!(!out.contains(r#"name="Notes""#), "{out}");
        assert!(
            out.contains(r#"state="hidden""#),
            "hidden state lost: {out}"
        );
        assert!(out.contains(r#"sheetId="4""#), "{out}");
        // A rename touches no part but the workbook itself.
        assert_eq!(plan.patches.len(), 1);
        assert!(plan.dropped.is_empty());
    }

    #[test]
    fn a_new_sheet_gets_a_part_a_relationship_and_a_content_type() {
        let model = vec![
            (SheetId(0), "Books".to_string()),
            (SheetId(1), "Notes".into()),
            (SheetId(2), "Extra".into()),
        ];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        let part = plan.part_for(SheetId(2)).unwrap().to_string();
        // sheet1 and sheet7 are taken; the next free name is sheet2, not
        // sheet8 — the numbering is a filename, not an index.
        assert_eq!(part, "xl/worksheets/sheet2.xml");
        assert!(plan.slots[2].fresh);

        let book = patch_of(&plan, WORKBOOK_PART);
        // rId9 is a styles relationship, so the new id must clear it.
        assert!(
            book.contains(r#"<sheet name="Extra" sheetId="5" r:id="rId10"/>"#),
            "{book}"
        );
        let rels = patch_of(&plan, WORKBOOK_RELS_PART);
        assert!(rels.contains(r#"Id="rId10""#), "{rels}");
        assert!(rels.contains(r#"Target="worksheets/sheet2.xml""#), "{rels}");
        let types = patch_of(&plan, CONTENT_TYPES_PART);
        assert!(types.contains("/xl/worksheets/sheet2.xml"), "{types}");
        assert!(types.contains(WORKSHEET_CONTENT_TYPE), "{types}");
    }

    #[test]
    fn a_deleted_sheet_loses_its_element_relationship_override_and_part() {
        let model = vec![(SheetId(0), "Books".to_string())];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        assert!(plan
            .dropped
            .contains(&"xl/worksheets/sheet7.xml".to_string()));

        let book = patch_of(&plan, WORKBOOK_PART);
        assert!(!book.contains("Notes"), "{book}");
        assert!(book.contains("Books"), "{book}");
        let rels = patch_of(&plan, WORKBOOK_RELS_PART);
        assert!(!rels.contains(r#"Id="rId3""#), "{rels}");
        assert!(
            rels.contains(r#"Id="rId9""#),
            "the styles rel was removed too"
        );
        let types = patch_of(&plan, CONTENT_TYPES_PART);
        assert!(!types.contains("sheet7.xml"), "{types}");
        assert!(types.contains("sheet1.xml"), "{types}");
    }

    #[test]
    fn changing_the_sheet_set_drops_the_calculation_chain() {
        // calcChain is keyed by sheet index; renumbering makes it point at the
        // wrong cells, and Excel offers to repair the file.
        let model = vec![(SheetId(0), "Books".to_string())];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        assert!(plan.dropped.contains(&CALC_CHAIN_PART.to_string()));
    }

    #[test]
    fn a_rename_alone_keeps_the_calculation_chain() {
        // Nothing is renumbered, so throwing away the cache would be a cost
        // paid for no reason.
        let model = vec![
            (SheetId(0), "Books".to_string()),
            (SheetId(1), "Archive".into()),
        ];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        assert!(plan.dropped.is_empty());
    }

    #[test]
    fn a_sheet_name_with_xml_in_it_is_escaped() {
        let model = vec![
            (SheetId(0), "Books".to_string()),
            (SheetId(1), "A & B <ok>".into()),
        ];
        let plan = plan(&model, &imported(), &parts(), &names(), None).unwrap();
        let book = patch_of(&plan, WORKBOOK_PART);
        assert!(book.contains("A &amp; B &lt;ok&gt;"), "{book}");
        // ...and it parses back to the name we meant.
        let reread = scan_workbook(book.as_bytes()).unwrap();
        assert_eq!(reread.elems[1].name, "A & B <ok>");
    }

    #[test]
    fn a_generated_worksheet_is_well_formed_and_empty() {
        let xml = empty_worksheet();
        let text = std::str::from_utf8(&xml).unwrap();
        assert!(text.contains("<sheetData/>"), "{text}");
        let mut reader = XmlReader::from_reader(xml.as_slice());
        loop {
            match reader.read_event().unwrap() {
                Event::Eof => break,
                _ => continue,
            }
        }
    }
}
