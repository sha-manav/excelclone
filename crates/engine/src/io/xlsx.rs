//! xlsx import and export.
//!
//! Import reads values, formulas and merges with `calamine` and replays them
//! through `Engine::apply`. In the same pass it keeps every entry of the
//! original zip package (plus per-sheet style indices and row attributes) in a
//! [`PreservedPackage`] hanging off the workbook.
//!
//! Export uses that package when it exists: the zip is rebuilt entry by entry
//! from the original bytes and only the `<sheetData>` (and `<mergeCells>`)
//! elements of modeled worksheets are regenerated, so charts, pivot caches,
//! VBA, conditional formatting, styles and everything else we do not model
//! survive a round trip untouched (spec §5). A workbook with no preserved
//! package - one built from scratch - is written with `rust_xlsxwriter`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io::{Cursor, Read, Write};
use std::ops::Range as ByteSpan;

use calamine::{Data, Reader, SheetType, Xlsx};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::package;
use super::sizes;
use super::styles::{StyleAdditions, StyleSheet};
use super::{apply_cell, install_sheets};
// Re-exported so callers can spell them `xlsx::ImportResult` too.
pub use super::{ImportResult, ImportWarning, ImportWarningKind, IoError};
use crate::addr::{CellAddr, RangeAddr};
use crate::engine::{Action, Engine};
use crate::format::CellFormat;
use crate::model::{Cell, CellContent, Sheet, SheetId, Workbook};
use crate::value::Value;

/// The part every xlsx keeps its formatting in.
const STYLES_PART: &str = "xl/styles.xml";

/// Refuse packages whose declared uncompressed size is absurd, so a zip bomb
/// cannot exhaust memory during import.
const MAX_TOTAL_UNCOMPRESSED: u64 = 1 << 29; // 512 MiB

// ---------------------------------------------------------------------------
// Preserved package
// ---------------------------------------------------------------------------

/// One entry of the original xlsx zip, kept verbatim.
#[derive(Clone)]
pub struct PreservedEntry {
    pub name: String,
    /// Uncompressed content, exactly as read.
    pub data: Vec<u8>,
    /// Whether the original entry was compressed (we re-use the method).
    pub compressed: bool,
    pub is_dir: bool,
}

impl fmt::Debug for PreservedEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreservedEntry")
            .field("name", &self.name)
            .field("bytes", &self.data.len())
            .field("compressed", &self.compressed)
            .finish()
    }
}

/// The per-worksheet detail we must carry across a round trip because it lives
/// inside `<sheetData>`, the one element export regenerates.
#[derive(Clone, Debug, Default)]
struct PreservedSheet {
    /// `s` (style index) attribute per cell, verbatim.
    styles: BTreeMap<CellAddr, String>,
    /// Attributes of each `<row>` other than `r`/`spans`, verbatim.
    row_attrs: BTreeMap<u32, String>,
    /// Merge ranges as found on import, so export can tell whether the model
    /// changed them.
    merged: BTreeSet<String>,
    /// Column widths and row heights as found on import, in pixels, on the
    /// same terms as `merged`: export compares them with the model and leaves
    /// the original bytes alone when nothing moved.
    col_widths: BTreeMap<u32, f64>,
    row_heights: BTreeMap<u32, f64>,
    /// Attributes of each `<col>` other than `min`/`max`/`width`/
    /// `customWidth`, verbatim, so regenerating the element does not drop the
    /// column styles and outline levels we do not model.
    col_attrs: BTreeMap<u32, String>,
}

/// Every entry of the imported package plus the map from sheet name to
/// worksheet part.
#[derive(Clone)]
pub struct PreservedPackage {
    entries: Vec<PreservedEntry>,
    /// Modeled sheet -> zip part name, in workbook order.
    ///
    /// Keyed by `SheetId` rather than by name, because a rename must stay a
    /// rename: matching on the name would make it indistinguishable from
    /// deleting one sheet and adding another, and the renamed sheet would lose
    /// everything its original part held that we do not model.
    sheet_parts: Vec<(SheetId, String)>,
    /// Part name -> detail captured from the original sheet XML.
    sheets: HashMap<String, PreservedSheet>,
    /// Defined names as imported: the modeled ones by uppercase name, and the
    /// raw XML of everything else — sheet-scoped names and the `_xlnm.`
    /// built-ins like print areas, which have to be written back untouched
    /// when the element is regenerated.
    defined_names: BTreeMap<String, String>,
    other_defined_names: Vec<String>,
    /// `xl/styles.xml`, parsed down to the formatting we model, so export can
    /// tell an untouched cell (write its original `s` back) from an edited one
    /// (append a new `<xf>`).
    styles: StyleSheet,
}

impl fmt::Debug for PreservedPackage {
    /// Entries hold whole file bodies; a derived Debug would swamp any dump of
    /// the workbook.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreservedPackage")
            .field("entries", &self.entries.len())
            .field("sheet_parts", &self.sheet_parts)
            .finish()
    }
}

impl PreservedPackage {
    /// Zip part names in the original order.
    pub fn part_names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// The uncompressed bytes of a part, as imported.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.data.as_slice())
    }

    fn read(bytes: &[u8]) -> Result<Self, IoError> {
        let mut zip = ZipArchive::new(Cursor::new(bytes))?;
        let mut entries = Vec::with_capacity(zip.len());
        let mut budget: u64 = MAX_TOTAL_UNCOMPRESSED;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            let name = f.name().to_string();
            let is_dir = f.is_dir();
            let compressed = f.compression() != CompressionMethod::Stored;
            let mut data = Vec::new();
            if !is_dir {
                // Bound the *actual* bytes read, not the size the header
                // claims, so a lying header cannot exhaust memory.
                f.by_ref().take(budget + 1).read_to_end(&mut data)?;
                budget = budget.checked_sub(data.len() as u64).ok_or_else(|| {
                    IoError::Malformed("package expands to more than 512 MiB".into())
                })?;
            }
            entries.push(PreservedEntry {
                name,
                data,
                compressed,
                is_dir,
            });
        }
        Ok(PreservedPackage {
            entries,
            sheet_parts: Vec::new(),
            sheets: HashMap::new(),
            defined_names: BTreeMap::new(),
            other_defined_names: Vec::new(),
            styles: StyleSheet::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Feature detection
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FeatureFlags {
    charts: bool,
    pivot: bool,
    vba: bool,
    macros: bool,
    tables: bool,
    comments: bool,
    external_links: bool,
    conditional_formatting: bool,
    data_validation: bool,
}

impl FeatureFlags {
    /// Detect from zip part names what the package carries that we do not model.
    fn scan_parts(&mut self, names: impl Iterator<Item = String>) {
        for name in names {
            let n = name.to_ascii_lowercase();
            self.charts |= n.contains("xl/charts/");
            self.pivot |= n.contains("xl/pivotcache") || n.contains("xl/pivottables");
            self.vba |= n.ends_with("vbaproject.bin");
            self.macros |= n.contains("xl/macrosheets/") || n.ends_with("vbaproject.bin");
            self.tables |= n.contains("xl/tables/");
            self.comments |= n.contains("xl/comments") || n.contains("threadedcomments");
            self.external_links |= n.contains("xl/externallinks");
        }
    }

    fn warnings(&self) -> Vec<ImportWarning> {
        let mut out = Vec::new();
        let mut push = |on: bool, detail: &str| {
            if on {
                out.push(ImportWarning::new(
                    ImportWarningKind::UnsupportedFeature,
                    detail,
                ));
            }
        };
        push(
            self.charts,
            "workbook contains charts (xl/charts/); they are preserved on save but not editable in Gridline",
        );
        push(
            self.pivot,
            "workbook contains pivot tables or pivot caches (xl/pivotTables/, xl/pivotCache/); preserved on save but not recalculated",
        );
        push(
            self.vba,
            "workbook contains a VBA project (xl/vbaProject.bin); preserved on save but never executed",
        );
        push(
            self.macros,
            "workbook contains macros; preserved on save but never executed",
        );
        push(
            self.tables,
            "workbook contains structured tables (xl/tables/); preserved on save but not modeled",
        );
        push(
            self.comments,
            "workbook contains comments or notes; preserved on save but not shown",
        );
        push(
            self.external_links,
            "workbook contains links to external workbooks (xl/externalLinks/); preserved on save but not resolved",
        );
        push(
            self.conditional_formatting,
            "worksheets use conditional formatting; preserved on save but not evaluated",
        );
        push(
            self.data_validation,
            "worksheets use data validation; preserved on save but not enforced",
        );
        out
    }
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Read an xlsx package into a fully recalculated engine.
pub fn import(bytes: &[u8]) -> Result<ImportResult, IoError> {
    let mut warnings = Vec::new();
    let mut features = FeatureFlags::default();

    let mut package = PreservedPackage::read(bytes)?;
    features.scan_parts(package.entries.iter().map(|e| e.name.clone()));

    let mut book: Xlsx<Cursor<&[u8]>> = Xlsx::new(Cursor::new(bytes))?;

    // Only plain worksheets are modeled; anything else is reported and left to
    // the preserved package.
    let mut names: Vec<String> = Vec::new();
    for meta in book.sheets_metadata() {
        match meta.typ {
            SheetType::WorkSheet => names.push(meta.name.clone()),
            other => warnings.push(ImportWarning::new(
                ImportWarningKind::UnsupportedSheetType,
                format!(
                    "sheet '{}' is a {:?} and is preserved but not modeled",
                    meta.name, other
                ),
            )),
        }
    }

    // Map worksheet names to zip parts and capture what lives inside
    // <sheetData> before we regenerate it on export. The mapping is rekeyed to
    // `SheetId` once the sheets exist; until then a name is all we have.
    let named_parts: Vec<(String, String)> = resolve_sheet_parts(&package.entries)?
        .into_iter()
        .filter(|(name, _)| names.iter().any(|n| n == name))
        .collect();
    for (_, part) in &named_parts {
        let Some(xml) = package.part(part).map(|b| b.to_vec()) else {
            continue;
        };
        let sheet = scan_worksheet(&xml, &mut features)?;
        package.sheets.insert(part.clone(), sheet);
    }
    // A package with no style sheet gets the default one, declared properly,
    // before anything reads it. Doing this at import rather than at export
    // means the rest of the code has exactly one case to handle: there is
    // always somewhere to record a format.
    if package.part(STYLES_PART).is_none() {
        install_default_styles(&mut package)?;
    }
    // A style sheet we cannot parse is reported rather than fatal: the cells
    // still import, they just arrive unformatted, and the original indices are
    // preserved so an unedited round trip is still lossless.
    if let Some(bytes) = package.part(STYLES_PART).map(|b| b.to_vec()) {
        match StyleSheet::parse(&bytes) {
            Ok(s) => package.styles = s,
            Err(e) => warnings.push(ImportWarning::new(
                ImportWarningKind::UnsupportedFeature,
                format!("{STYLES_PART} could not be read ({e}); cell formatting was not imported"),
            )),
        }
    }
    warnings.extend(features.warnings());

    let mut engine = Engine::new();
    install_sheets(&mut engine, &names)?;
    package.sheet_parts = named_parts
        .into_iter()
        .filter_map(|(name, part)| Some((engine.wb.sheet_id_by_name(&name)?, part)))
        .collect();

    for name in &names {
        // Merges first: merging clears every cell but the anchor, so applying
        // them after the values would delete imported data.
        if let Some(merges) = book.worksheet_merge_cells(name) {
            for dim in merges? {
                let (start, end) = (dim.start, dim.end);
                let range =
                    RangeAddr::new(CellAddr::new(start.0, start.1), CellAddr::new(end.0, end.1));
                if !range.start.is_valid() || !range.end.is_valid() {
                    warnings.push(ImportWarning::new(
                        ImportWarningKind::Other,
                        format!("sheet '{}': merged range out of bounds, dropped", name),
                    ));
                    continue;
                }
                engine
                    .apply(&Action::MergeApply {
                        sheet: name.clone(),
                        range,
                    })
                    .map_err(|e| IoError::Apply(e.to_string()))?;
            }
        }

        let inputs = sheet_inputs(&mut book, name, &mut warnings)?;
        for (addr, input) in inputs {
            apply_cell(&mut engine, name, addr, &input, &mut warnings)?;
        }
    }

    if let Some(bytes) = package.part(package::WORKBOOK_PART).map(|b| b.to_vec()) {
        let (modeled, other) = scan_defined_names(&bytes)?;
        package.defined_names = modeled.clone();
        package.other_defined_names = other;
        engine.wb.names = modeled;
    }
    install_formats(&mut engine, &package);
    install_sizes(&mut engine, &package);
    engine.wb.preserved = Some(package);
    // Opening a file is a starting point, not an edit.
    engine.clear_history();
    Ok(ImportResult { engine, warnings })
}

/// Give a package with no `xl/styles.xml` the default one, plus the
/// relationship and content-type override that make it a real part.
///
/// The records it contains are the ones every xlsx has and index 0 of each
/// collection must be the default, because a cell with no `s` attribute means
/// `s="0"`. Appending to empty collections instead would make the first format
/// anyone applies the default for the whole workbook.
fn install_default_styles(package: &mut PreservedPackage) -> Result<(), IoError> {
    const DEFAULT_STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill>\
<fill><patternFill patternType=\"gray125\"/></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
<cellXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/></cellXfs>\
</styleSheet>";
    const STYLES_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    const STYLES_CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml";

    let Some(rels_at) = package
        .entries
        .iter()
        .position(|e| e.name == package::WORKBOOK_RELS_PART)
    else {
        // No workbook relationships at all: a part we added could not be
        // referenced, so leave the package alone and let export refuse to
        // record formatting rather than write a file nothing can open.
        return Ok(());
    };
    let (rels, _) = package::add_relationship(
        &package.entries[rels_at].data,
        STYLES_REL_TYPE,
        "styles.xml",
    )?;
    package.entries[rels_at].data = rels;

    if let Some(at) = package
        .entries
        .iter()
        .position(|e| e.name == package::CONTENT_TYPES_PART)
    {
        package.entries[at].data =
            package::add_override(&package.entries[at].data, STYLES_PART, STYLES_CONTENT_TYPE)?;
    }
    package.entries.push(PreservedEntry {
        name: STYLES_PART.to_string(),
        data: DEFAULT_STYLES.as_bytes().to_vec(),
        compressed: true,
        is_dir: false,
    });
    Ok(())
}

/// Populate `Sheet::formats` from the original `s` indices.
///
/// This is written straight into the model rather than replayed as
/// `FormatApply` actions. Formatting is not something the user did in this
/// session, and pushing thousands of format actions onto the undo stack would
/// make the first Ctrl+Z after opening a file unformat part of it.
fn install_formats(engine: &mut Engine, package: &PreservedPackage) {
    for (sid, part) in &package.sheet_parts {
        let Some(detail) = package.sheets.get(part) else {
            continue;
        };
        let sid = *sid;
        for (addr, s) in &detail.styles {
            let format = package.styles.format_for(Some(s));
            if format.is_default() {
                continue;
            }
            if let Some(id) = engine.wb.formats.intern(format) {
                engine
                    .wb
                    .sheet_mut(sid)
                    .expect("sheet exists")
                    .formats
                    .insert(*addr, id);
            }
        }
    }
}

/// Read `<definedNames>` out of `xl/workbook.xml`.
///
/// Only workbook-scoped, non-built-in names are modeled. A name with a
/// `localSheetId` is scoped to one sheet and a `_xlnm.` name is a print area
/// or a filter range; both are kept as raw XML so regenerating the element
/// does not delete them.
fn scan_defined_names(xml: &[u8]) -> Result<(BTreeMap<String, String>, Vec<String>), IoError> {
    let mut modeled = BTreeMap::new();
    let mut other = Vec::new();
    let mut reader = XmlReader::from_reader(xml);
    let mut current: Option<(String, bool, String)> = None;
    loop {
        match reader.read_event().map_err(IoError::from)? {
            Event::Eof => break,
            Event::Start(e) if e.name().local_name().as_ref() == b"definedName" => {
                let mut name = String::new();
                let mut local = false;
                let mut raw = format!("<{}", String::from_utf8_lossy(e.name().as_ref()));
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                    let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
                    if key == "name" {
                        name = attr.unescape_value().unwrap_or_default().into_owned();
                    }
                    if key == "localSheetId" {
                        local = true;
                    }
                    raw.push_str(&format!(" {key}=\"{value}\""));
                }
                raw.push('>');
                current = Some((name, local, raw));
            }
            Event::Text(t) => {
                if let Some((_, _, raw)) = &mut current {
                    raw.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::End(e) if e.name().local_name().as_ref() == b"definedName" => {
                let Some((name, local, mut raw)) = current.take() else {
                    continue;
                };
                let body = raw
                    .split_once('>')
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                raw.push_str("</definedName>");
                if local || name.starts_with("_xlnm.") || name.is_empty() {
                    other.push(raw);
                } else {
                    modeled.insert(name.to_ascii_uppercase(), unescape_xml(&body));
                }
            }
            _ => {}
        }
    }
    Ok((modeled, other))
}

/// Undo the five XML entities `escape_xml` writes. The body of a
/// `<definedName>` is a formula, and `&amp;` in it means `&`.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Regenerate `<definedNames>` from the model, carrying over the entries we
/// did not model.
fn write_defined_names(names: &BTreeMap<String, String>, other: &[String]) -> String {
    if names.is_empty() && other.is_empty() {
        return String::new();
    }
    let mut out = String::from("<definedNames>");
    for (name, refers_to) in names {
        out.push_str(&format!(
            "<definedName name=\"{}\">{}</definedName>",
            escape_xml(name),
            escape_xml(refers_to.strip_prefix('=').unwrap_or(refers_to))
        ));
    }
    for raw in other {
        out.push_str(raw);
    }
    out.push_str("</definedNames>");
    out
}

/// Populate `Sheet::col_widths` and `Sheet::row_heights` from the original
/// `<cols>` and `<row ht=…>`.
///
/// Written straight into the model for the same reason as the formats: the
/// widths a file arrives with are not a gesture the user made in this session,
/// and replaying them as `Resize` actions would put them on the undo stack.
fn install_sizes(engine: &mut Engine, package: &PreservedPackage) {
    for (sid, part) in &package.sheet_parts {
        let Some(detail) = package.sheets.get(part) else {
            continue;
        };
        let Some(sheet) = engine.wb.sheet_mut(*sid) else {
            continue;
        };
        sheet.col_widths = detail.col_widths.clone();
        sheet.row_heights = detail.row_heights.clone();
    }
}

/// The user-visible input string for every populated cell of one worksheet:
/// formulas win over cached values, everything else becomes a literal.
fn sheet_inputs(
    book: &mut Xlsx<Cursor<&[u8]>>,
    name: &str,
    warnings: &mut Vec<ImportWarning>,
) -> Result<BTreeMap<CellAddr, String>, IoError> {
    let mut out: BTreeMap<CellAddr, String> = BTreeMap::new();
    let mut note_oob = |addr: (usize, usize)| {
        warnings.push(ImportWarning::new(
            ImportWarningKind::Other,
            format!(
                "sheet '{}': cell at row {} col {} is outside the grid, dropped",
                name, addr.0, addr.1
            ),
        ));
    };

    let formulas = book.worksheet_formula(name)?;
    let (fr, fc) = formulas.start().unwrap_or((0, 0));
    for (r, c, f) in formulas.used_cells() {
        if f.trim().is_empty() {
            continue;
        }
        match cell_addr(fr as usize + r, fc as usize + c) {
            Some(addr) => {
                out.insert(addr, format!("={}", f));
            }
            None => note_oob((r, c)),
        }
    }

    let values = book.worksheet_range(name)?;
    let (vr, vc) = values.start().unwrap_or((0, 0));
    for (r, c, d) in values.used_cells() {
        let Some(input) = data_to_input(d) else {
            continue;
        };
        match cell_addr(vr as usize + r, vc as usize + c) {
            Some(addr) => {
                out.entry(addr).or_insert(input);
            }
            None => note_oob((r, c)),
        }
    }
    Ok(out)
}

fn cell_addr(row: usize, col: usize) -> Option<CellAddr> {
    let addr = CellAddr::new(u32::try_from(row).ok()?, u32::try_from(col).ok()?);
    addr.is_valid().then_some(addr)
}

/// Convert a calamine value into the text a user would have typed. Strings get
/// a leading apostrophe so numeric-looking text stays text.
fn data_to_input(d: &Data) -> Option<String> {
    Some(match d {
        Data::Empty => return None,
        Data::Int(i) => i.to_string(),
        Data::Float(f) => crate::value::format_number_general(*f),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::String(s) if s.is_empty() => return None,
        Data::String(s) => format!("'{}", s),
        // Dates arrive as typed values; the model stores Excel serials.
        Data::DateTime(dt) => crate::value::format_number_general(dt.as_f64()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => format!("'{}", s),
        Data::Error(e) => e.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Package layout: workbook.xml + rels -> worksheet parts
// ---------------------------------------------------------------------------

/// Resolve sheet name -> worksheet part name via `xl/workbook.xml` and its
/// relationships, in workbook order.
fn resolve_sheet_parts(entries: &[PreservedEntry]) -> Result<Vec<(String, String)>, IoError> {
    let find = |name: &str| {
        entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.data.as_slice())
    };
    let Some(workbook) = find("xl/workbook.xml") else {
        return Err(IoError::Malformed("missing xl/workbook.xml".into()));
    };
    let rels = find("xl/_rels/workbook.xml.rels").unwrap_or(&[]);

    // rId -> target part name, resolved against the "xl/" base.
    let mut targets: HashMap<String, String> = HashMap::new();
    let mut reader = XmlReader::from_reader(rels);
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
                    if typ.is_empty() || typ.ends_with("/worksheet") {
                        targets.insert(id, resolve_target(&target));
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    let mut reader = XmlReader::from_reader(workbook);
    loop {
        match reader.read_event().map_err(IoError::from)? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                if e.name().local_name().as_ref() != b"sheet" {
                    continue;
                }
                let (mut name, mut rid) = (None, None);
                for attr in e.attributes().flatten() {
                    let value = attr.unescape_value().unwrap_or_default().into_owned();
                    match attr.key.local_name().as_ref() {
                        b"name" => name = Some(value),
                        // `r:id`; `sheetId` has a different local name.
                        b"id" => rid = Some(value),
                        _ => {}
                    }
                }
                if let (Some(name), Some(part)) = (name, rid.and_then(|r| targets.get(&r))) {
                    if entries.iter().any(|e| e.name == *part) {
                        out.push((name, part.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Resolve a relationship target against the `xl/` base, collapsing `..`.
pub(crate) fn resolve_target(target: &str) -> String {
    let raw = target.replace('\\', "/");
    let joined = match raw.strip_prefix('/') {
        Some(abs) => abs.to_string(),
        None => format!("xl/{}", raw),
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ---------------------------------------------------------------------------
// Worksheet XML scanning
// ---------------------------------------------------------------------------

/// Capture style indices, row attributes and merges from an original sheet
/// part, and note the unmodeled features it uses.
fn scan_worksheet(xml: &[u8], features: &mut FeatureFlags) -> Result<PreservedSheet, IoError> {
    let mut out = PreservedSheet::default();
    let mut reader = XmlReader::from_reader(xml);
    let mut row: u32 = 0;
    let mut col: u32 = 0;
    loop {
        let event = reader.read_event().map_err(IoError::from)?;
        match event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.name().local_name().as_ref() {
                b"col" => {
                    let mut min = None;
                    let mut max = None;
                    let mut width = None;
                    // Everything else the element carries — style, hidden,
                    // outlineLevel, bestFit, collapsed. Not modeled, but
                    // regenerating `<cols>` must not be how they get lost.
                    let mut rest = String::new();
                    for attr in e.attributes().flatten() {
                        let value = attr.unescape_value().unwrap_or_default().into_owned();
                        match attr.key.as_ref() {
                            b"min" => min = value.parse::<u32>().ok(),
                            b"max" => max = value.parse::<u32>().ok(),
                            b"width" => width = value.parse::<f64>().ok(),
                            b"customWidth" => {}
                            key => {
                                rest.push_str(&format!(
                                    " {}=\"{}\"",
                                    String::from_utf8_lossy(key),
                                    String::from_utf8_lossy(attr.value.as_ref())
                                ));
                            }
                        }
                    }
                    let Some(min) = min else { continue };
                    // One element covers a run, and `max="16384"` — the whole
                    // sheet — is common. Expanding it costs a couple of
                    // hundred kilobytes at the very worst, and export
                    // coalesces the runs back, which is a better trade than a
                    // cap that would silently drop the width of every column
                    // past it.
                    let max = max.unwrap_or(min).min(crate::addr::MAX_COLS);
                    for c in min..=max {
                        let i = c.saturating_sub(1);
                        if let Some(w) = width {
                            out.col_widths.insert(i, sizes::chars_to_px(w));
                        }
                        if !rest.is_empty() {
                            out.col_attrs.insert(i, rest.clone());
                        }
                    }
                }
                b"row" => {
                    col = 0;
                    let mut attrs = String::new();
                    // Attribute order is not guaranteed, so the height is held
                    // until `r` has certainly been seen.
                    let mut height = None;
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                        if key == "ht" {
                            height = attr
                                .unescape_value()
                                .ok()
                                .and_then(|v| v.parse::<f64>().ok())
                                .map(sizes::points_to_px);
                        }
                        // `r` and `spans` are regenerated; everything else
                        // (heights, hidden, outline level) is carried over.
                        if key == "r" {
                            row = attr
                                .unescape_value()
                                .ok()
                                .and_then(|v| v.parse::<u32>().ok())
                                .map(|n| n.saturating_sub(1))
                                .unwrap_or(row);
                            continue;
                        }
                        if key == "spans" {
                            continue;
                        }
                        attrs.push_str(&format!(
                            "{}=\"{}\" ",
                            key,
                            String::from_utf8_lossy(attr.value.as_ref())
                        ));
                    }
                    let attrs = attrs.trim_end().to_string();
                    if !attrs.is_empty() {
                        out.row_attrs.insert(row, attrs);
                    }
                    if let Some(px) = height.filter(|px| *px > 0.0) {
                        out.row_heights.insert(row, px);
                    }
                }
                b"c" => {
                    let mut addr = None;
                    let mut style = None;
                    for attr in e.attributes().flatten() {
                        let value = attr.unescape_value().unwrap_or_default().into_owned();
                        match attr.key.as_ref() {
                            b"r" => addr = CellAddr::parse_a1(&value),
                            b"s" => style = Some(value),
                            _ => {}
                        }
                    }
                    // `r` is optional in the schema: fall back to position.
                    let addr = addr.unwrap_or(CellAddr::new(row, col));
                    col = addr.col.saturating_add(1);
                    if let Some(s) = style {
                        out.styles.insert(addr, s);
                    }
                }
                b"mergeCell" => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"ref" {
                            let value = attr.unescape_value().unwrap_or_default().into_owned();
                            if let Some(r) = RangeAddr::parse_a1(&value) {
                                out.merged.insert(r.to_a1());
                            }
                        }
                    }
                }
                b"conditionalFormatting" => features.conditional_formatting = true,
                b"dataValidation" | b"dataValidations" => features.data_validation = true,
                _ => {}
            },
            _ => {}
        }
    }
    Ok(out)
}

/// Byte spans of the elements export rewrites.
#[derive(Default)]
struct SheetSpans {
    sheet_data: Option<ByteSpan<usize>>,
    merge_cells: Option<ByteSpan<usize>>,
    dimension: Option<ByteSpan<usize>>,
    cols: Option<ByteSpan<usize>>,
    /// Where a new `<mergeCells>` element must go to keep schema order.
    merge_insert: Option<usize>,
}

/// Elements that the schema places after `<mergeCells>`; a new mergeCells
/// element is inserted before the first of them.
const AFTER_MERGE_CELLS: &[&[u8]] = &[
    b"phoneticPr",
    b"conditionalFormatting",
    b"dataValidations",
    b"hyperlinks",
    b"printOptions",
    b"pageMargins",
    b"pageSetup",
    b"headerFooter",
    b"rowBreaks",
    b"colBreaks",
    b"customProperties",
    b"cellWatches",
    b"ignoredErrors",
    b"smartTags",
    b"drawing",
    b"drawingHF",
    b"picture",
    b"oleObjects",
    b"controls",
    b"webPublishItems",
    b"tableParts",
    b"extLst",
    b"legacyDrawing",
    b"legacyDrawingHF",
];

fn scan_spans(xml: &[u8]) -> Result<SheetSpans, IoError> {
    let mut spans = SheetSpans::default();
    let mut reader = XmlReader::from_reader(xml);
    let mut depth: i32 = 0;
    let mut worksheet_end = None;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(IoError::from)?;
        let end = reader.buffer_position() as usize;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                let local = name.local_name();
                if depth == 1 {
                    match local.as_ref() {
                        b"sheetData" => spans.sheet_data = Some(start..start),
                        b"mergeCells" => spans.merge_cells = Some(start..start),
                        b"dimension" => spans.dimension = Some(start..start),
                        b"cols" => spans.cols = Some(start..start),
                        other => {
                            if spans.merge_insert.is_none() && AFTER_MERGE_CELLS.contains(&other) {
                                spans.merge_insert = Some(start);
                            }
                        }
                    }
                }
                depth += 1;
            }
            Event::Empty(e) => {
                let name = e.name();
                let local = name.local_name();
                if depth == 1 {
                    match local.as_ref() {
                        b"sheetData" => spans.sheet_data = Some(start..end),
                        b"mergeCells" => spans.merge_cells = Some(start..end),
                        b"dimension" => spans.dimension = Some(start..end),
                        b"cols" => spans.cols = Some(start..end),
                        other => {
                            if spans.merge_insert.is_none() && AFTER_MERGE_CELLS.contains(&other) {
                                spans.merge_insert = Some(start);
                            }
                        }
                    }
                }
            }
            Event::End(e) => {
                depth -= 1;
                let name = e.name();
                match name.local_name().as_ref() {
                    b"sheetData" if depth == 1 => {
                        if let Some(s) = &mut spans.sheet_data {
                            s.end = end;
                        }
                    }
                    b"mergeCells" if depth == 1 => {
                        if let Some(s) = &mut spans.merge_cells {
                            s.end = end;
                        }
                    }
                    b"dimension" if depth == 1 => {
                        if let Some(s) = &mut spans.dimension {
                            s.end = end;
                        }
                    }
                    b"cols" if depth == 1 => {
                        if let Some(s) = &mut spans.cols {
                            s.end = end;
                        }
                    }
                    b"worksheet" if depth == 0 => worksheet_end = Some(start),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if spans.merge_insert.is_none() {
        spans.merge_insert = worksheet_end;
    }
    Ok(spans)
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Write a workbook as xlsx: patch the imported package when there is one,
/// otherwise generate a fresh file.
pub fn export(wb: &Workbook) -> Result<Vec<u8>, IoError> {
    match &wb.preserved {
        Some(package) => export_preserved(wb, package),
        None => export_fresh(wb),
    }
}

fn export_preserved(wb: &Workbook, package: &PreservedPackage) -> Result<Vec<u8>, IoError> {
    // Work out where every sheet's XML goes before touching any of it. A sheet
    // added since import needs a part invented for it, one deleted needs its
    // part and every reference to it removed, and one renamed keeps the part it
    // has. The plan answers all three, and rewrites nothing when the sheet list
    // is unchanged.
    let model: Vec<(SheetId, String)> = wb.sheets.iter().map(|s| (s.id, s.name.clone())).collect();
    let Some(workbook_xml) = package.part(package::WORKBOOK_PART) else {
        return Err(IoError::Malformed(format!(
            "missing {}",
            package::WORKBOOK_PART
        )));
    };
    let parts = package::Parts {
        workbook: workbook_xml,
        rels: package.part(package::WORKBOOK_RELS_PART).unwrap_or(&[]),
        content_types: package.part(package::CONTENT_TYPES_PART),
    };
    let part_names: Vec<String> = package.entries.iter().map(|e| e.name.clone()).collect();
    // `<definedNames>` is left exactly as it was unless a name actually
    // changed, the same rule as `<cols>`: the element holds sheet-scoped
    // names and print areas this engine does not model.
    let names_xml = (wb.names != package.defined_names)
        .then(|| write_defined_names(&wb.names, &package.other_defined_names));
    let plan = package::plan(&model, &package.sheet_parts, &parts, &part_names, names_xml)?;

    // Resolve every cell's style index next, because doing so is what
    // discovers which new `<xf>` records `xl/styles.xml` needs; the sheets and
    // the style sheet then get patched from the same answer.
    let mut additions = StyleAdditions::new(&package.styles);
    let mut style_attrs: HashMap<String, BTreeMap<CellAddr, String>> = HashMap::new();
    for slot in &plan.slots {
        let Some(sheet) = wb.sheet(slot.sheet_id) else {
            continue;
        };
        let detail = package.sheets.get(&slot.part).cloned().unwrap_or_default();
        style_attrs.insert(
            slot.part.clone(),
            resolve_style_indices(wb, sheet, &detail, package, &mut additions),
        );
    }

    if !additions.is_empty() && package.part(STYLES_PART).is_none() {
        return Err(IoError::Unrepresentable(format!(
            "the workbook has no {STYLES_PART} to record new formatting in; \
             formatting a package without a style sheet is not supported yet"
        )));
    }

    let mut patched: HashMap<String, Vec<u8>> = plan.patches.iter().cloned().collect();
    for slot in &plan.slots {
        let Some(sheet) = wb.sheet(slot.sheet_id) else {
            continue;
        };
        // A sheet added since import has no part to patch, so it is patched
        // into an empty one — which keeps every worksheet on the same code
        // path and means a generated sheet is written by the same writer as
        // an imported one.
        let original: Vec<u8> = match package.entries.iter().find(|e| e.name == slot.part) {
            Some(entry) => entry.data.clone(),
            None => package::empty_worksheet(),
        };
        let detail = package.sheets.get(&slot.part).cloned().unwrap_or_default();
        let attrs = style_attrs.remove(&slot.part).unwrap_or_default();
        patched.insert(
            slot.part.clone(),
            patch_sheet_xml(&original, sheet, &detail, &attrs)?,
        );
    }
    if !additions.is_empty() {
        if let Some(original) = package.part(STYLES_PART) {
            patched.insert(STYLES_PART.to_string(), additions.patch(original)?);
        }
    }

    let mut out = Vec::new();
    {
        let mut zw = ZipWriter::new(Cursor::new(&mut out));
        for entry in &package.entries {
            if plan.dropped.contains(&entry.name) {
                continue;
            }
            let options = SimpleFileOptions::default().compression_method(if entry.compressed {
                CompressionMethod::Deflated
            } else {
                CompressionMethod::Stored
            });
            if entry.is_dir {
                zw.add_directory(entry.name.as_str(), options)?;
                continue;
            }
            zw.start_file(entry.name.as_str(), options)?;
            match patched.get(entry.name.as_str()) {
                Some(bytes) => zw.write_all(bytes)?,
                None => zw.write_all(&entry.data)?,
            }
        }
        // Parts this export invented go last; a zip has no required order and
        // appending keeps every original entry at its original offset.
        for slot in plan.slots.iter().filter(|s| s.fresh) {
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            zw.start_file(slot.part.as_str(), options)?;
            let bytes = patched.get(slot.part.as_str()).cloned().unwrap_or_default();
            zw.write_all(&bytes)?;
        }
        zw.finish()?;
    }
    Ok(out)
}

/// Replace only `<sheetData>` (and `<mergeCells>` when the model changed the
/// merges) in the original worksheet part, leaving every sibling element -
/// cols, sheetPr, autoFilter, conditionalFormatting, drawing references - as
/// it was.
/// The `s` index every cell should carry on export, as a ready-to-splice
/// attribute string.
///
/// A cell whose format still matches what its original `<xf>` said keeps that
/// exact index, so a workbook opened and saved without touching the formatting
/// is byte-identical in this respect — including for the parts of that `<xf>`
/// we never modelled. Only a cell whose format actually changed gets a new
/// index, and that index is appended rather than substituted, so nothing else
/// in the file shifts.
fn resolve_style_indices(
    wb: &Workbook,
    sheet: &Sheet,
    detail: &PreservedSheet,
    package: &PreservedPackage,
    additions: &mut StyleAdditions,
) -> BTreeMap<CellAddr, String> {
    let mut out = BTreeMap::new();
    let addrs: BTreeSet<CellAddr> = detail
        .styles
        .keys()
        .copied()
        .chain(sheet.formats.keys().copied())
        .collect();
    for addr in addrs {
        let original_s = detail.styles.get(&addr);
        let current: CellFormat = wb.formats.resolve(sheet.format_id(addr));
        if current == package.styles.format_for(original_s.map(|s| s.as_str())) {
            if let Some(s) = original_s {
                out.insert(addr, s.clone());
            }
            continue;
        }
        let base = original_s.and_then(|s| s.parse::<usize>().ok());
        let index = additions.index_for(&package.styles, &current, base);
        out.insert(addr, index.to_string());
    }
    out
}

fn patch_sheet_xml(
    original: &[u8],
    sheet: &Sheet,
    detail: &PreservedSheet,
    style_attrs: &BTreeMap<CellAddr, String>,
) -> Result<Vec<u8>, IoError> {
    let spans = scan_spans(original)?;
    let Some(sheet_data) = spans.sheet_data.clone() else {
        return Err(IoError::Malformed(format!(
            "worksheet part for sheet '{}' has no <sheetData>",
            sheet.name
        )));
    };

    let sheet_data_start = sheet_data.start;
    let mut edits: Vec<(ByteSpan<usize>, String)> =
        vec![(sheet_data, write_sheet_data(sheet, detail, style_attrs))];

    let current: BTreeSet<String> = sheet.merged.iter().map(|r| r.to_a1()).collect();
    if current != detail.merged {
        let xml = write_merge_cells(&current);
        match (spans.merge_cells.clone(), spans.merge_insert) {
            (Some(span), _) => edits.push((span, xml)),
            (None, _) if xml.is_empty() => {}
            (None, Some(at)) => edits.push((at..at, xml)),
            (None, None) => {
                return Err(IoError::Malformed(format!(
                    "worksheet part for sheet '{}' has nowhere to put <mergeCells>",
                    sheet.name
                )))
            }
        }
    }

    // `<cols>` is left exactly as it was unless a width actually moved. The
    // element carries styles and outline levels we do not model, so
    // regenerating it unconditionally would throw those away on every save.
    if sheet.col_widths != detail.col_widths {
        let xml = write_cols(&sheet.col_widths, &detail.col_attrs);
        match spans.cols.clone() {
            Some(span) => edits.push((span, xml)),
            // The schema puts `<cols>` immediately before `<sheetData>`, so
            // that offset is the one place a new one can go.
            None if xml.is_empty() => {}
            None => edits.push((sheet_data_start..sheet_data_start, xml)),
        }
    }

    // `<dimension>` is a hint that readers trust; a stale one hides cells we
    // just added.
    if let Some(span) = spans.dimension.clone() {
        let bounds = emitted_bounds(sheet, detail, style_attrs)
            .map(|r| r.to_a1())
            .unwrap_or_else(|| "A1".to_string());
        edits.push((span, format!("<dimension ref=\"{}\"/>", bounds)));
    }

    // Apply from the end so earlier spans keep their offsets.
    // (start, end), so a zero-length insertion that shares an offset with a
    // replacement is applied after it rather than being overwritten by it.
    edits.sort_by_key(|(span, _)| (span.start, span.end));
    let mut out = original.to_vec();
    for (span, text) in edits.into_iter().rev() {
        if span.start > out.len() || span.end > out.len() || span.start > span.end {
            return Err(IoError::Malformed("worksheet XML span out of range".into()));
        }
        out.splice(span, text.into_bytes());
    }
    Ok(out)
}

/// Bounding box of everything `write_sheet_data` emits: model cells plus the
/// formatting-only cells we carry over.
fn emitted_bounds(
    sheet: &Sheet,
    detail: &PreservedSheet,
    style_attrs: &BTreeMap<CellAddr, String>,
) -> Option<RangeAddr> {
    let mut addrs = sheet
        .cells
        .keys()
        .chain(detail.styles.keys())
        .chain(style_attrs.keys());
    let first = *addrs.next()?;
    let mut range = RangeAddr::single(first);
    for a in addrs {
        range.start.row = range.start.row.min(a.row);
        range.start.col = range.start.col.min(a.col);
        range.end.row = range.end.row.max(a.row);
        range.end.col = range.end.col.max(a.col);
    }
    Some(range)
}

fn write_merge_cells(ranges: &BTreeSet<String>) -> String {
    if ranges.is_empty() {
        return String::new();
    }
    let mut out = format!("<mergeCells count=\"{}\">", ranges.len());
    for r in ranges {
        out.push_str(&format!("<mergeCell ref=\"{}\"/>", escape_xml(r)));
    }
    out.push_str("</mergeCells>");
    out
}

/// Generate `<cols>` from the model, coalescing equal adjacent widths.
///
/// Coalescing is not cosmetic: a file that arrived saying "columns 1 to 16384
/// are 90 pixels" imports as sixteen thousand entries, and writing them back
/// one element apiece would turn a 200-byte element into half a megabyte.
fn write_cols(widths: &BTreeMap<u32, f64>, attrs: &BTreeMap<u32, String>) -> String {
    let columns: BTreeSet<u32> = widths.keys().chain(attrs.keys()).copied().collect();
    if columns.is_empty() {
        return String::new();
    }
    // Runs are keyed by everything the element will say, not by width alone:
    // two adjacent columns of the same width but different styles are two
    // elements, and merging them would move a style onto a column that never
    // had one.
    type Run<'a> = (u32, u32, Option<f64>, Option<&'a String>);
    let mut runs: Vec<Run> = Vec::new();
    for col in columns {
        let px = widths.get(&col).copied();
        let rest = attrs.get(&col);
        match runs.last_mut() {
            Some((_, end, w, a)) if *end + 1 == col && *w == px && *a == rest => *end = col,
            _ => runs.push((col, col, px, rest)),
        }
    }
    let mut out = String::from("<cols>");
    for (start, end, px, rest) in runs {
        let width = px
            .map(|px| {
                format!(
                    " width=\"{}\" customWidth=\"1\"",
                    sizes::fmt_num(sizes::px_to_chars(px))
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "<col min=\"{}\" max=\"{}\"{}{}/>",
            start + 1,
            end + 1,
            width,
            rest.map(|s| s.as_str()).unwrap_or_default()
        ));
    }
    out.push_str("</cols>");
    out
}

/// The `<row>` attributes to write: the original ones, with `ht` and
/// `customHeight` replaced from the model when the model disagrees.
///
/// Leaving them alone when it agrees is what keeps an untouched round trip
/// exact — `ht="14.4"` is not something our pixel arithmetic can reproduce
/// digit for digit, and a save should not silently renumber every row of a
/// file the user only opened.
fn row_attrs_for(row: u32, sheet: &Sheet, detail: &PreservedSheet) -> String {
    let modeled = sheet.row_heights.get(&row);
    if modeled == detail.row_heights.get(&row) {
        return detail
            .row_attrs
            .get(&row)
            .map(|a| format!(" {}", a))
            .unwrap_or_default();
    }
    let kept: Vec<&str> = detail
        .row_attrs
        .get(&row)
        .map(|a| {
            a.split_whitespace()
                .filter(|kv| !kv.starts_with("ht=") && !kv.starts_with("customHeight="))
                .collect()
        })
        .unwrap_or_default();
    let mut out = String::new();
    for kv in kept {
        out.push(' ');
        out.push_str(kv);
    }
    if let Some(px) = modeled {
        out.push_str(&format!(
            " ht=\"{}\" customHeight=\"1\"",
            sizes::fmt_num(sizes::px_to_points(*px))
        ));
    }
    out
}

/// Generate `<sheetData>` from the model, re-attaching each cell's original
/// style index and each row's original attributes. Rows and cells that carry
/// only formatting are emitted empty so that formatting is not lost.
fn write_sheet_data(
    sheet: &Sheet,
    detail: &PreservedSheet,
    style_attrs: &BTreeMap<CellAddr, String>,
) -> String {
    let mut rows: BTreeMap<u32, BTreeMap<u32, Option<&Cell>>> = BTreeMap::new();
    for (addr, cell) in &sheet.cells {
        rows.entry(addr.row)
            .or_default()
            .insert(addr.col, Some(cell));
    }
    for addr in detail.styles.keys().chain(style_attrs.keys()) {
        rows.entry(addr.row)
            .or_default()
            .entry(addr.col)
            .or_insert(None);
    }
    for row in detail.row_attrs.keys().chain(sheet.row_heights.keys()) {
        rows.entry(*row).or_default();
    }
    if rows.is_empty() {
        return "<sheetData/>".to_string();
    }

    let mut out = String::from("<sheetData>");
    for (r, cells) in rows {
        let attrs = row_attrs_for(r, sheet, detail);
        if cells.is_empty() {
            out.push_str(&format!("<row r=\"{}\"{}/>", r + 1, attrs));
            continue;
        }
        out.push_str(&format!("<row r=\"{}\"{}>", r + 1, attrs));
        for (c, cell) in cells {
            let addr = CellAddr::new(r, c);
            let style = style_attrs
                .get(&addr)
                .map(|s| format!(" s=\"{}\"", escape_xml(s)))
                .unwrap_or_default();
            out.push_str(&write_cell(addr, cell, &style));
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData>");
    out
}

fn write_cell(addr: CellAddr, cell: Option<&Cell>, style: &str) -> String {
    let r = addr.to_a1();
    let Some(cell) = cell else {
        return format!("<c r=\"{}\"{}/>", r, style);
    };
    match &cell.content {
        CellContent::Literal(v) => match v {
            Value::Text(t) => format!(
                // Inline strings keep us out of xl/sharedStrings.xml, which we
                // then never have to rewrite.
                "<c r=\"{}\"{} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                r,
                style,
                escape_xml(t)
            ),
            Value::Bool(b) => format!(
                "<c r=\"{}\"{} t=\"b\"><v>{}</v></c>",
                r,
                style,
                if *b { 1 } else { 0 }
            ),
            Value::Error(e) => format!(
                "<c r=\"{}\"{} t=\"e\"><v>{}</v></c>",
                r,
                style,
                escape_xml(e.code())
            ),
            Value::Number(n) => match number_xml(*n) {
                Some(text) => format!("<c r=\"{}\"{}><v>{}</v></c>", r, style, text),
                None => format!("<c r=\"{}\"{} t=\"e\"><v>#NUM!</v></c>", r, style),
            },
            Value::Empty => format!("<c r=\"{}\"{}/>", r, style),
        },
        CellContent::Formula { src, cached, .. } => {
            let f = format!("<f>{}</f>", escape_xml(src));
            match cached {
                Value::Number(n) => match number_xml(*n) {
                    Some(text) => format!("<c r=\"{}\"{}>{}<v>{}</v></c>", r, style, f, text),
                    None => format!("<c r=\"{}\"{} t=\"e\">{}<v>#NUM!</v></c>", r, style, f),
                },
                Value::Bool(b) => format!(
                    "<c r=\"{}\"{} t=\"b\">{}<v>{}</v></c>",
                    r,
                    style,
                    f,
                    if *b { 1 } else { 0 }
                ),
                Value::Error(e) => format!(
                    "<c r=\"{}\"{} t=\"e\">{}<v>{}</v></c>",
                    r,
                    style,
                    f,
                    escape_xml(e.code())
                ),
                // A formula result that is text uses t="str", not inlineStr.
                Value::Text(t) => format!(
                    "<c r=\"{}\"{} t=\"str\">{}<v>{}</v></c>",
                    r,
                    style,
                    f,
                    escape_xml(t)
                ),
                Value::Empty => format!("<c r=\"{}\"{}>{}</c>", r, style, f),
            }
        }
    }
}

/// Full-precision number text for `<v>`; None for values XML cannot express.
fn number_xml(n: f64) -> Option<String> {
    n.is_finite().then(|| format!("{}", n))
}

pub(crate) fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters other than tab/newline/return are illegal in
            // XML 1.0; Excel writes them as _xHHHH_ escapes.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {
                out.push_str(&format!("_x{:04X}_", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

/// Write a workbook that has no original package: values, formulas, merges and
/// sheet names, nothing else.
fn export_fresh(wb: &Workbook) -> Result<Vec<u8>, IoError> {
    let mut book = rust_xlsxwriter::Workbook::new();
    for (name, refers_to) in &wb.names {
        // rust_xlsxwriter wants the leading `=`; the model stores what xlsx
        // stores, which does not have one.
        let formula = format!("={}", refers_to.strip_prefix('=').unwrap_or(refers_to));
        book.define_name(name, &formula)?;
    }
    for sheet in &wb.sheets {
        let ws = book.add_worksheet();
        ws.set_name(&sheet.name)?;

        // Widths and heights before anything else, so a sheet that is only
        // resized still writes them: rust_xlsxwriter keeps them whether or not
        // the column holds a cell.
        //
        // In pixels rather than characters, because `set_column_width` takes
        // the count *without* the padding Excel adds and then adds it back —
        // handing it the attribute value would make every column five pixels
        // wider on each save.
        for (&col, &px) in &sheet.col_widths {
            let writer_px = sizes::px_to_writer_px(px) as i64;
            if let (Ok(c), Ok(w)) = (u16::try_from(col), u16::try_from(writer_px)) {
                ws.set_column_width_pixels(c, w)?;
            }
        }
        for (&row, &px) in &sheet.row_heights {
            if let Ok(h) = u16::try_from(px.round() as i64) {
                ws.set_row_height_pixels(row, h)?;
            }
        }

        // merge_range fills the whole range with blanks, so it must run before
        // the values that land inside it.
        for m in &sheet.merged {
            if m.start == m.end {
                continue; // Excel has no single-cell merge.
            }
            let (r0, c0) = rc(m.start)?;
            let (r1, c1) = rc(m.end)?;
            ws.merge_range(r0, c0, r1, c1, "", &rust_xlsxwriter::Format::default())?;
        }

        // Addresses that carry formatting but no value still have to be
        // written, or a bold empty column would vanish on save.
        let addrs: BTreeSet<CellAddr> = sheet
            .cells
            .keys()
            .copied()
            .chain(sheet.formats.keys().copied())
            .collect();
        for addr in addrs {
            let (row, col) = rc(addr)?;
            let format = writer_format(&wb.formats.resolve(sheet.format_id(addr)));
            let fmt = format.as_ref();
            let Some(cell) = sheet.cells.get(&addr) else {
                if let Some(f) = fmt {
                    ws.write_blank(row, col, f)?;
                }
                continue;
            };
            match (&cell.content, fmt) {
                (CellContent::Literal(Value::Number(n)), None) => {
                    ws.write_number(row, col, *n)?;
                }
                (CellContent::Literal(Value::Number(n)), Some(f)) => {
                    ws.write_number_with_format(row, col, *n, f)?;
                }
                (CellContent::Literal(Value::Text(t)), None) => {
                    ws.write_string(row, col, t)?;
                }
                (CellContent::Literal(Value::Text(t)), Some(f)) => {
                    ws.write_string_with_format(row, col, t, f)?;
                }
                (CellContent::Literal(Value::Bool(b)), None) => {
                    ws.write_boolean(row, col, *b)?;
                }
                (CellContent::Literal(Value::Bool(b)), Some(f)) => {
                    ws.write_boolean_with_format(row, col, *b, f)?;
                }
                // Excel has no literal error cell; the text round-trips back
                // to an error because our parser reads error codes.
                (CellContent::Literal(Value::Error(e)), None) => {
                    ws.write_string(row, col, e.code())?;
                }
                (CellContent::Literal(Value::Error(e)), Some(f)) => {
                    ws.write_string_with_format(row, col, e.code(), f)?;
                }
                (CellContent::Literal(Value::Empty), None) => {}
                (CellContent::Literal(Value::Empty), Some(f)) => {
                    ws.write_blank(row, col, f)?;
                }
                (CellContent::Formula { src, cached, .. }, fmt) => {
                    let mut f = rust_xlsxwriter::Formula::new(src);
                    if !matches!(cached, Value::Empty) {
                        f = f.set_result(cached.display());
                    }
                    match fmt {
                        Some(style) => ws.write_formula_with_format(row, col, f, style)?,
                        None => ws.write_formula(row, col, f)?,
                    };
                }
            }
        }
    }
    Ok(book.save_to_buffer()?)
}

/// Translate a `CellFormat` into the writer's own format type. `None` for the
/// default, so unformatted cells are written exactly as they were before
/// formatting existed.
fn writer_format(f: &CellFormat) -> Option<rust_xlsxwriter::Format> {
    use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder};
    if f.is_default() {
        return None;
    }
    let mut out = Format::new();
    if f.bold {
        out = out.set_bold();
    }
    if f.italic {
        out = out.set_italic();
    }
    if let Some(c) = f.font_color.as_deref().and_then(parse_rgb) {
        out = out.set_font_color(Color::RGB(c));
    }
    if let Some(c) = f.fill_color.as_deref().and_then(parse_rgb) {
        out = out.set_background_color(Color::RGB(c));
    }
    if f.borders.top {
        out = out.set_border_top(FormatBorder::Thin);
    }
    if f.borders.bottom {
        out = out.set_border_bottom(FormatBorder::Thin);
    }
    if f.borders.left {
        out = out.set_border_left(FormatBorder::Thin);
    }
    if f.borders.right {
        out = out.set_border_right(FormatBorder::Thin);
    }
    if let Some(code) = &f.number_format {
        out = out.set_num_format(code);
    }
    if let Some(a) = f.align {
        out = out.set_align(match a {
            crate::format::HAlign::Left => FormatAlign::Left,
            crate::format::HAlign::Center => FormatAlign::Center,
            crate::format::HAlign::Right => FormatAlign::Right,
        });
    }
    Some(out)
}

/// `#rrggbb` to the 0xRRGGBB the writer wants.
fn parse_rgb(c: &str) -> Option<u32> {
    u32::from_str_radix(c.trim_start_matches('#'), 16).ok()
}

fn rc(addr: CellAddr) -> Result<(u32, u16), IoError> {
    let col = u16::try_from(addr.col)
        .map_err(|_| IoError::XlsxWrite(format!("column out of range at {}", addr.to_a1())))?;
    Ok((addr.row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_resolution() {
        assert_eq!(
            resolve_target("worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve_target("/xl/worksheets/sheet2.xml"),
            "xl/worksheets/sheet2.xml"
        );
        assert_eq!(
            resolve_target("../xl/worksheets/sheet3.xml"),
            "xl/worksheets/sheet3.xml"
        );
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_xml("a<b&c\"'"), "a&lt;b&amp;c&quot;&apos;");
        assert_eq!(escape_xml("a\u{1}b"), "a_x0001_b");
    }

    #[test]
    fn spans_of_minimal_sheet() {
        let xml = br#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><pageMargins left="1"/></worksheet>"#;
        let spans = scan_spans(xml).unwrap();
        let s = spans.sheet_data.unwrap();
        assert_eq!(
            &xml[s.clone()],
            br#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#
        );
        assert!(spans.merge_cells.is_none());
        assert_eq!(spans.merge_insert, Some(s.end));
    }

    #[test]
    fn corrupt_package_errors() {
        assert!(import(b"not a zip at all").is_err());
        assert!(import(&[]).is_err());
    }
}
