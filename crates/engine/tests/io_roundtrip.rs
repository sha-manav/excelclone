//! M2 I/O tests: xlsx and CSV round trips, and the preservation rule - a file
//! opened and saved in Gridline must not lose the parts we do not model.

use std::io::{Cursor, Read, Write};

use engine::io::{csv, xlsx, ImportWarningKind};
use engine::{Action, CellAddr, Engine, RangeAddr, Value};

fn set(e: &mut Engine, sheet: &str, a1: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: sheet.into(),
        addr: CellAddr::parse_a1(a1).unwrap(),
        input: input.into(),
    })
    .unwrap();
}

/// A workbook exercising every literal kind plus formulas and a cross-sheet
/// reference.
fn sample_workbook() -> Engine {
    let mut e = Engine::new();
    e.apply(&Action::SheetAdd {
        name: "Data".into(),
    })
    .unwrap();
    set(&mut e, "Data", "A1", "7");
    set(&mut e, "Sheet1", "A1", "42");
    set(&mut e, "Sheet1", "A2", "3.5");
    set(&mut e, "Sheet1", "A3", "hello");
    set(&mut e, "Sheet1", "A4", "TRUE");
    set(&mut e, "Sheet1", "A5", "#DIV/0!");
    set(&mut e, "Sheet1", "B1", "=SUM(A1:A2)");
    set(&mut e, "Sheet1", "B2", "=A3&\" world\"");
    set(&mut e, "Sheet1", "B3", "=Data!A1*2");
    set(&mut e, "Sheet1", "B4", "=1/0");
    e
}

#[test]
fn xlsx_round_trip_preserves_values_and_formulas() {
    let e = sample_workbook();
    let before = e.wb.state_snapshot();

    let bytes = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&bytes).expect("import");

    assert_eq!(back.engine.wb.state_snapshot(), before);
    assert_eq!(back.engine.value_at("Sheet1", "B1"), Value::Number(45.5));
    assert_eq!(
        back.engine.value_at("Sheet1", "B2"),
        Value::Text("hello world".into())
    );
    assert_eq!(back.engine.value_at("Sheet1", "B3"), Value::Number(14.0));
}

#[test]
fn csv_round_trip() {
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "name");
    set(&mut e, "Sheet1", "B1", "qty, boxed");
    set(&mut e, "Sheet1", "A2", "widget");
    set(&mut e, "Sheet1", "B2", "12");
    set(&mut e, "Sheet1", "A3", "total");
    set(&mut e, "Sheet1", "B3", "=B2*2");

    let sheet_id = e.wb.sheets[0].id;
    let out = csv::export(&e.wb, sheet_id).expect("csv export");
    // Computed values, never formula text.
    assert_eq!(
        String::from_utf8(out.clone()).unwrap(),
        "name,\"qty, boxed\"\nwidget,12\ntotal,24\n"
    );

    let back = csv::import(&out, "Imported").expect("csv import");
    assert!(back.warnings.is_empty());
    let sheet = back.engine.wb.sheet_by_name("Imported").expect("sheet");
    assert_eq!(
        sheet.value(CellAddr::parse_a1("B1").unwrap()).display(),
        "qty, boxed"
    );
    assert_eq!(
        sheet.value(CellAddr::parse_a1("B3").unwrap()).display(),
        "24"
    );
    // Re-exporting the imported sheet reproduces the same bytes.
    let again = csv::export(&back.engine.wb, sheet.id).expect("csv export");
    assert_eq!(again, out);
}

#[test]
fn csv_import_parses_formulas_and_numbers() {
    let back = csv::import(b"1,2,=A1+B1\n,text,\n", "Sheet1").unwrap();
    let e = back.engine;
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(3.0));
    assert_eq!(e.value_at("Sheet1", "A2"), Value::Empty);
    assert_eq!(e.value_at("Sheet1", "B2"), Value::Text("text".into()));
}

#[test]
fn merges_survive_xlsx_round_trip() {
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "title");
    set(&mut e, "Sheet1", "A3", "9");
    e.apply(&Action::MergeApply {
        sheet: "Sheet1".into(),
        range: RangeAddr::parse_a1("A1:C2").unwrap(),
    })
    .unwrap();

    let bytes = xlsx::export(&e.wb).unwrap();
    let back = xlsx::import(&bytes).unwrap();
    let sheet = back.engine.wb.sheet_by_name("Sheet1").unwrap();
    assert_eq!(sheet.merged, vec![RangeAddr::parse_a1("A1:C2").unwrap()]);
    assert_eq!(
        back.engine.value_at("Sheet1", "A1"),
        Value::Text("title".into())
    );
    assert_eq!(back.engine.value_at("Sheet1", "A3"), Value::Number(9.0));
}

// ---------------------------------------------------------------------------
// Preservation
// ---------------------------------------------------------------------------

const CHART_XML: &[u8] =
    b"<?xml version=\"1.0\"?><chartSpace><gridline-marker>keep me exactly</gridline-marker></chartSpace>";
const VBA_BIN: &[u8] = &[
    0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00, 0x42, 0xFF, 0x07,
];

fn zip_parts(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(Cursor::new(&mut out));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in parts {
            zw.start_file(*name, options).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }
    out
}

fn part_of(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut out = Vec::new();
    f.read_to_end(&mut out).unwrap();
    out
}

/// A hand-built package: the minimum Excel needs, a worksheet whose sheetData
/// is surrounded by elements we do not model, plus two junk parts.
fn handmade_xlsx(sheet_xml: &str) -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
    let root_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Books" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#;
    let styles = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="3"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="2" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>"#;
    zip_parts(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", workbook_rels),
        ("xl/styles.xml", styles),
        ("xl/worksheets/sheet1.xml", sheet_xml.as_bytes()),
        ("xl/charts/chart1.xml", CHART_XML),
        ("xl/vbaProject.bin", VBA_BIN),
    ])
}

const SHEET_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr><tabColor rgb="FFFF0000"/></sheetPr><dimension ref="A1:B3"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><cols><col min="1" max="1" width="24.5" customWidth="1"/></cols><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>title</t></is></c><c r="B1" s="2"><v>10</v></c></row><row r="2" ht="31.5" customHeight="1"><c r="A2"><v>5</v></c><c r="B2"><f>B1*2</f><v>20</v></c></row><row r="3"><c r="A3" s="1"/></row></sheetData><conditionalFormatting sqref="B1:B2"><cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan"><formula>3</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="whole" sqref="A2" operator="greaterThan"><formula1>0</formula1></dataValidation></dataValidations><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#;

#[test]
fn unmodeled_parts_survive_open_and_save() {
    let original = handmade_xlsx(SHEET_XML);
    let imported = xlsx::import(&original).expect("import");

    // Features we cannot model are reported, not swallowed.
    let details: Vec<String> = imported
        .warnings
        .iter()
        .filter(|w| w.kind == ImportWarningKind::UnsupportedFeature)
        .map(|w| w.detail.clone())
        .collect();
    let joined = details.join("\n");
    assert!(joined.contains("charts"), "no chart warning in {joined:?}");
    assert!(joined.contains("VBA"), "no VBA warning in {joined:?}");
    assert!(joined.contains("conditional formatting"), "{joined:?}");
    assert!(joined.contains("data validation"), "{joined:?}");

    let mut e = imported.engine;
    assert_eq!(e.value_at("Books", "A1"), Value::Text("title".into()));
    assert_eq!(e.value_at("Books", "B2"), Value::Number(20.0));

    set(&mut e, "Books", "B1", "50");
    assert_eq!(e.value_at("Books", "B2"), Value::Number(100.0));
    set(&mut e, "Books", "D5", "new");

    let saved = xlsx::export(&e.wb).expect("export");

    // The parts we do not understand come back byte-identical.
    assert_eq!(part_of(&saved, "xl/charts/chart1.xml"), CHART_XML);
    assert_eq!(part_of(&saved, "xl/vbaProject.bin"), VBA_BIN);
    assert_eq!(
        part_of(&saved, "xl/styles.xml"),
        part_of(&original, "xl/styles.xml")
    );
    assert_eq!(
        part_of(&saved, "[Content_Types].xml"),
        part_of(&original, "[Content_Types].xml")
    );

    // The worksheet keeps every sibling of <sheetData>, plus row and cell
    // formatting that lives inside it.
    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    for keep in [
        "<tabColor rgb=\"FFFF0000\"/>",
        "<col min=\"1\" max=\"1\" width=\"24.5\" customWidth=\"1\"/>",
        "<conditionalFormatting sqref=\"B1:B2\">",
        "<dataValidation type=\"whole\" sqref=\"A2\" operator=\"greaterThan\">",
        "<pageMargins left=\"0.7\"",
        "ht=\"31.5\" customHeight=\"1\"",
        "<c r=\"B1\" s=\"2\">",
        "<c r=\"A3\" s=\"1\"/>",
    ] {
        assert!(sheet.contains(keep), "lost {keep:?} from\n{sheet}");
    }
    assert!(
        sheet.contains("<v>50</v>"),
        "new value missing from\n{sheet}"
    );
    assert!(sheet.contains("<f>B1*2</f>"));
    // The dimension hint follows the new used range.
    assert!(sheet.contains("<dimension ref=\"A1:D5\"/>"), "{sheet}");

    // And the saved file still opens with the new value in place.
    let reopened = xlsx::import(&saved).expect("reimport");
    assert_eq!(reopened.engine.value_at("Books", "B1"), Value::Number(50.0));
    assert_eq!(
        reopened.engine.value_at("Books", "B2"),
        Value::Number(100.0)
    );
    assert_eq!(
        reopened.engine.wb.state_snapshot(),
        e.wb.state_snapshot(),
        "state changed across a preserved round trip"
    );
}

#[test]
fn merge_changes_are_written_into_the_preserved_package() {
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::MergeApply {
        sheet: "Books".into(),
        range: RangeAddr::parse_a1("A1:B1").unwrap(),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).unwrap();
    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(sheet.contains("<mergeCell ref=\"A1:B1\"/>"), "{sheet}");
    // mergeCells must precede conditionalFormatting to stay schema-valid.
    assert!(sheet.find("<mergeCells").unwrap() < sheet.find("<conditionalFormatting").unwrap());
    let back = xlsx::import(&saved).unwrap();
    assert_eq!(
        back.engine.wb.sheet_by_name("Books").unwrap().merged,
        vec![RangeAddr::parse_a1("A1:B1").unwrap()]
    );
}

#[test]
fn a_sheet_added_after_import_survives_the_round_trip() {
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::SheetAdd {
        name: "Extra".into(),
    })
    .unwrap();
    set(&mut e, "Extra", "B2", "7");
    set(&mut e, "Extra", "B3", "=B2*6");
    // A cross-sheet reference proves the new sheet is a first-class one and
    // not a decoration bolted onto the package.
    set(&mut e, "Books", "D1", "=Extra!B3");
    assert_eq!(e.value_at("Books", "D1"), Value::Number(42.0));

    let saved = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&saved).expect("reimport");
    assert_eq!(
        back.engine
            .wb
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Books", "Extra"]
    );
    assert_eq!(back.engine.value_at("Extra", "B3"), Value::Number(42.0));
    assert_eq!(back.engine.value_at("Books", "D1"), Value::Number(42.0));

    // The generated part is declared everywhere a reader will look for it.
    let part = String::from_utf8(part_of(&saved, "xl/worksheets/sheet2.xml")).unwrap();
    assert!(part.contains("<v>7</v>"), "{part}");
    let types = String::from_utf8(part_of(&saved, "[Content_Types].xml")).unwrap();
    assert!(types.contains("/xl/worksheets/sheet2.xml"), "{types}");
    let rels = String::from_utf8(part_of(&saved, "xl/_rels/workbook.xml.rels")).unwrap();
    assert!(rels.contains("worksheets/sheet2.xml"), "{rels}");
    // The vbaProject relationship rId2 was already taken, so the new one must
    // not have reused it.
    assert!(rels.contains("Target=\"vbaProject.bin\""), "{rels}");

    // And the sheet the file arrived with keeps everything it had.
    let original = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        original.contains("<tabColor rgb=\"FFFF0000\"/>"),
        "{original}"
    );
}

#[test]
fn renaming_a_sheet_keeps_its_part_and_everything_in_it() {
    // The whole reason parts are tracked by id: a rename must not throw away
    // the columns, conditional formatting and tab colour the sheet arrived
    // with, which is exactly what treating it as delete-then-add would do.
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::SheetRename {
        from: "Books".into(),
        to: "Ledger".into(),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export");

    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(sheet.contains("<tabColor rgb=\"FFFF0000\"/>"), "{sheet}");
    assert!(
        sheet.contains("<conditionalFormatting sqref=\"B1:B2\">"),
        "{sheet}"
    );
    assert!(
        sheet.contains("<col min=\"1\" max=\"1\" width=\"24.5\""),
        "{sheet}"
    );

    let workbook = String::from_utf8(part_of(&saved, "xl/workbook.xml")).unwrap();
    assert!(workbook.contains("name=\"Ledger\""), "{workbook}");
    assert!(!workbook.contains("name=\"Books\""), "{workbook}");
    // A rename adds no part, so these two are untouched.
    assert_eq!(
        part_of(&saved, "[Content_Types].xml"),
        part_of(&handmade_xlsx(SHEET_XML), "[Content_Types].xml")
    );

    let back = xlsx::import(&saved).expect("reimport");
    assert_eq!(back.engine.value_at("Ledger", "B2"), Value::Number(20.0));
    assert!(back.engine.wb.sheet_by_name("Books").is_none());
}

#[test]
fn deleting_a_sheet_removes_its_part_and_every_reference_to_it() {
    // A part left behind with no `<sheet>` pointing at it is dead weight; a
    // `<sheet>` left behind with no part makes Excel offer to repair the file.
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::SheetAdd {
        name: "Scratch".into(),
    })
    .unwrap();
    set(&mut e, "Scratch", "A1", "1");
    let saved = xlsx::export(&e.wb).expect("export");

    let mut e = xlsx::import(&saved).expect("reimport").engine;
    e.apply(&Action::SheetDelete {
        name: "Scratch".into(),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export after delete");

    let names: Vec<String> = zip::ZipArchive::new(Cursor::new(&saved))
        .unwrap()
        .file_names()
        .map(|s| s.to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n == "xl/worksheets/sheet2.xml"),
        "the deleted sheet's part is still in the package: {names:?}"
    );
    let workbook = String::from_utf8(part_of(&saved, "xl/workbook.xml")).unwrap();
    assert!(!workbook.contains("Scratch"), "{workbook}");
    let types = String::from_utf8(part_of(&saved, "[Content_Types].xml")).unwrap();
    assert!(!types.contains("sheet2.xml"), "{types}");
    assert!(
        types.contains("sheet1.xml"),
        "the survivor lost its override"
    );

    let back = xlsx::import(&saved).expect("reimport after delete");
    assert_eq!(
        back.engine
            .wb
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Books"]
    );
    assert_eq!(back.engine.value_at("Books", "B2"), Value::Number(20.0));
}

#[test]
fn deleting_the_sheet_a_formula_points_at_leaves_a_ref_error_not_a_stale_value() {
    // Excel's own behaviour, and the loud one: a formula that silently kept
    // its last answer would be worse than one that says the sheet is gone.
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::SheetAdd {
        name: "Scratch".into(),
    })
    .unwrap();
    set(&mut e, "Scratch", "A1", "9");
    set(&mut e, "Books", "D1", "=Scratch!A1");
    assert_eq!(e.value_at("Books", "D1"), Value::Number(9.0));

    e.apply(&Action::SheetDelete {
        name: "Scratch".into(),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&saved).expect("reimport");
    assert!(
        matches!(back.engine.value_at("Books", "D1"), Value::Error(_)),
        "got {:?}",
        back.engine.value_at("Books", "D1")
    );
}

#[test]
fn unparseable_formula_warns_and_keeps_the_text() {
    let sheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>SUM(</f><v>0</v></c><c r="B1"><f>1+2</f><v>3</v></c></row></sheetData></worksheet>"#;
    let imported = xlsx::import(&handmade_xlsx(sheet)).expect("import");

    let warning = imported
        .warnings
        .iter()
        .find(|w| w.kind == ImportWarningKind::UnparseableFormula)
        .expect("expected an UnparseableFormula warning");
    assert!(warning.detail.contains("SUM("), "{}", warning.detail);
    assert!(warning.detail.contains("A1"), "{}", warning.detail);

    let e = imported.engine;
    assert_eq!(e.value_at("Books", "A1"), Value::Text("=SUM(".into()));
    assert_eq!(e.value_at("Books", "B1"), Value::Number(3.0));

    // The text survives a save too.
    let saved = xlsx::export(&e.wb).unwrap();
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(xml.contains("=SUM("), "{xml}");
}

#[test]
fn corrupt_input_errors_instead_of_panicking() {
    assert!(xlsx::import(b"").is_err());
    assert!(xlsx::import(b"PK\x03\x04 truncated right here").is_err());
    assert!(xlsx::import(&[0u8; 4096]).is_err());

    // A valid zip that is not a workbook.
    let not_a_workbook = zip_parts(&[("hello.txt", b"nothing to see")]);
    assert!(xlsx::import(&not_a_workbook).is_err());

    // A real package truncated half way through.
    let full = handmade_xlsx(SHEET_XML);
    assert!(xlsx::import(&full[..full.len() / 2]).is_err());

    // A worksheet whose XML is malformed.
    let broken = handmade_xlsx("<worksheet><sheetData><row r=\"1\"><c r=\"A1\">");
    assert!(xlsx::import(&broken).is_err());
}

/// The demo fixture: a real two-sheet package with shared strings, a theme and
/// a style sheet, none of which the hand-built one above has.
fn demo_fixture() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures/demo-dues-ledger.xlsx");
    std::fs::read(path).expect("the demo fixture")
}

#[test]
fn the_demo_fixture_survives_a_sheet_being_added_renamed_and_deleted() {
    // The hand-built package above is a convenient minimum; this is the file
    // the demo actually opens, and the one whose extra parts — sharedStrings,
    // theme, docProps — a package rewrite could break.
    let mut e = xlsx::import(&demo_fixture()).expect("import").engine;
    let before = e.value_at("Ledger", "A1");

    e.apply(&Action::SheetRename {
        from: "Rates".into(),
        to: "Fees".into(),
    })
    .unwrap();
    e.apply(&Action::SheetAdd {
        name: "Summary".into(),
    })
    .unwrap();
    set(&mut e, "Summary", "A1", "=SUM(Ledger!D2:D9)");
    let expected = e.value_at("Summary", "A1");
    assert!(
        matches!(expected, Value::Number(n) if n > 0.0),
        "the fixture's ledger totals to {expected:?}, so this proves nothing"
    );

    let saved = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&saved).expect("reimport");
    assert_eq!(
        back.engine
            .wb
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Ledger", "Fees", "Summary"]
    );
    assert_eq!(back.engine.value_at("Summary", "A1"), expected);
    assert_eq!(back.engine.value_at("Ledger", "A1"), before);

    // Now delete one and save again, from the reimported copy — the second
    // trip is where a half-updated package shows up.
    let mut e = back.engine;
    e.apply(&Action::SheetDelete {
        name: "Fees".into(),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export after delete");
    let back = xlsx::import(&saved).expect("reimport after delete");
    assert_eq!(
        back.engine
            .wb
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Ledger", "Summary"]
    );
    assert_eq!(back.engine.value_at("Summary", "A1"), expected);
    // The parts nobody modelled are still there.
    for part in ["xl/theme/theme1.xml", "docProps/core.xml"] {
        assert_eq!(
            part_of(&saved, part),
            part_of(&demo_fixture(), part),
            "{part} changed"
        );
    }
}

/// The same minimum package, with no `xl/styles.xml` at all — which some
/// generators really do produce, and which used to make any formatting a hard
/// export failure.
fn handmade_without_styles() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Plain" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>4</v></c><c r="B1"><v>6</v></c></row></sheetData></worksheet>"#;
    zip_parts(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", workbook_rels),
        ("xl/worksheets/sheet1.xml", sheet),
    ])
}

#[test]
fn a_package_with_no_style_sheet_can_still_be_formatted() {
    use engine::{BorderPreset, FormatPatch};

    let mut e = xlsx::import(&handmade_without_styles())
        .expect("import")
        .engine;
    e.apply(&Action::FormatApply {
        sheet: "Plain".into(),
        range: RangeAddr::parse_a1("A1:B1").unwrap(),
        patches: vec![
            FormatPatch::Bold(true),
            FormatPatch::FillColor(Some("#ffcc00".into())),
            FormatPatch::Border(BorderPreset::All),
        ],
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export");

    // The style sheet exists, is declared, and is reachable.
    let styles = String::from_utf8(part_of(&saved, "xl/styles.xml")).unwrap();
    assert!(
        styles.contains("<b/>"),
        "the bold font was not recorded: {styles}"
    );
    let types = String::from_utf8(part_of(&saved, "[Content_Types].xml")).unwrap();
    assert!(types.contains("/xl/styles.xml"), "{types}");
    let rels = String::from_utf8(part_of(&saved, "xl/_rels/workbook.xml.rels")).unwrap();
    assert!(rels.contains("Target=\"styles.xml\""), "{rels}");

    // A cell nobody formatted must not inherit the new format: the default
    // record has to stay at index 0.
    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(!sheet.contains("<c r=\"C9\" s=\"0\""), "{sheet}");

    let back = xlsx::import(&saved).expect("reimport");
    let formats = &back.engine.wb;
    let sheet_model = formats.sheet_by_name("Plain").unwrap();
    let id = sheet_model
        .format_id(CellAddr::parse_a1("A1").unwrap())
        .expect("A1 has a format");
    let format = formats.formats.get(id).expect("the format resolves");
    assert!(format.bold, "{format:?}");
    assert_eq!(format.fill_color.as_deref(), Some("#ffcc00"), "{format:?}");
    // ...and an untouched cell has none at all.
    assert!(sheet_model
        .format_id(CellAddr::parse_a1("A5").unwrap())
        .is_none());
}

// ---------------------------------------------------------------------------
// Column widths and row heights
// ---------------------------------------------------------------------------

fn resize(e: &mut Engine, axis: engine::Axis, at: u32, count: u32, size: Option<f64>) {
    e.apply(&Action::Resize {
        sheet: "Sheet1".into(),
        axis,
        at,
        count,
        size,
    })
    .unwrap();
}

#[test]
fn sizes_survive_a_round_trip_through_a_generated_file() {
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "wide");
    resize(&mut e, engine::Axis::Col, 0, 1, Some(180.0));
    resize(&mut e, engine::Axis::Row, 4, 1, Some(48.0));

    let bytes = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&bytes).expect("import");
    let sheet = back.engine.wb.sheet_by_name("Sheet1").unwrap();
    assert_eq!(sheet.col_widths.get(&0), Some(&180.0));
    assert_eq!(sheet.row_heights.get(&4), Some(&48.0));
    // Row 5 exists in the file only because it is tall; it must not have
    // acquired a cell on the way.
    assert!(!sheet.cells.contains_key(&CellAddr::parse_a1("A5").unwrap()));
}

#[test]
fn no_width_drifts_by_a_pixel_on_save() {
    // One width proves the plumbing; a spread proves the arithmetic. The
    // generated-file path goes through a different writer from the preserved
    // one and has its own idea of what a "pixel" is, so it gets its own sweep.
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "x");
    let widths: Vec<f64> = (0..40).map(|i| 24.0 + i as f64 * 12.0).collect();
    for (i, px) in widths.iter().enumerate() {
        resize(&mut e, engine::Axis::Col, i as u32, 1, Some(*px));
        resize(&mut e, engine::Axis::Row, i as u32, 1, Some(px / 4.0));
    }

    // Twice, because a conversion can be wrong in a way one pass hides.
    let mut wb = e.wb.clone();
    for pass in 1..=2 {
        let bytes = xlsx::export(&wb).expect("export");
        wb = xlsx::import(&bytes).expect("import").engine.wb;
        let sheet = wb.sheet_by_name("Sheet1").unwrap();
        for (i, px) in widths.iter().enumerate() {
            assert_eq!(
                sheet.col_widths.get(&(i as u32)),
                Some(px),
                "column {i} drifted on pass {pass}"
            );
            assert_eq!(
                sheet.row_heights.get(&(i as u32)),
                Some(&(px / 4.0)),
                "row {i} drifted on pass {pass}"
            );
        }
    }
}

#[test]
fn a_width_read_from_a_file_is_the_width_written_back() {
    // The interesting case is a file we did not write: `width="24.5"` is not a
    // number our pixel arithmetic produces, and an open-and-save must not
    // quietly renumber it.
    let original = handmade_xlsx(SHEET_XML);
    let mut e = xlsx::import(&original).expect("import").engine;
    assert_eq!(
        e.wb.sheet_by_name("Books").unwrap().col_widths.get(&0),
        Some(&176.0),
        "the <cols> width did not reach the model"
    );
    assert_eq!(
        e.wb.sheet_by_name("Books").unwrap().row_heights.get(&1),
        Some(&42.0),
        "31.5pt is 42px"
    );

    // Editing a cell is not editing a width.
    set(&mut e, "Books", "A1", "changed");
    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        xml.contains("<col min=\"1\" max=\"1\" width=\"24.5\" customWidth=\"1\"/>"),
        "the original width was rewritten:\n{xml}"
    );
    assert!(
        xml.contains("ht=\"31.5\" customHeight=\"1\""),
        "the original height was rewritten:\n{xml}"
    );
}

#[test]
fn resizing_an_imported_sheet_rewrites_only_what_moved() {
    let original = handmade_xlsx(SHEET_XML);
    let mut e = xlsx::import(&original).expect("import").engine;
    e.apply(&Action::Resize {
        sheet: "Books".into(),
        axis: engine::Axis::Col,
        at: 2,
        count: 1,
        size: Some(300.0),
    })
    .unwrap();
    e.apply(&Action::Resize {
        sheet: "Books".into(),
        axis: engine::Axis::Row,
        at: 0,
        count: 1,
        size: Some(60.0),
    })
    .unwrap();

    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(xml.contains("<col min=\"3\" max=\"3\""), "{xml}");
    assert!(xml.contains("ht=\"45\" customHeight=\"1\""), "{xml}");
    // Row 2's height was not part of the gesture and keeps its exact text.
    assert!(xml.contains("ht=\"31.5\" customHeight=\"1\""), "{xml}");

    // Reopening gives back every size, including column 1's, which the
    // regenerated <cols> had to carry over.
    let back = xlsx::import(&saved).expect("reimport");
    let sheet = back.engine.wb.sheet_by_name("Books").unwrap();
    assert_eq!(sheet.col_widths.get(&0), Some(&176.0));
    assert_eq!(sheet.col_widths.get(&2), Some(&300.0));
    assert_eq!(sheet.row_heights.get(&0), Some(&60.0));
    assert_eq!(sheet.row_heights.get(&1), Some(&42.0));
}

#[test]
fn regenerating_cols_keeps_the_attributes_we_do_not_model() {
    // A column style, an outline level and a hidden column. None of them is
    // in the model, and rewriting <cols> for an unrelated resize must not be
    // how they disappear.
    let sheet_xml = SHEET_XML.replace(
        r#"<col min="1" max="1" width="24.5" customWidth="1"/>"#,
        r#"<col min="1" max="1" width="24.5" customWidth="1" style="2"/><col min="4" max="4" hidden="1" outlineLevel="1"/>"#,
    );
    let mut e = xlsx::import(&handmade_xlsx(&sheet_xml))
        .expect("import")
        .engine;
    e.apply(&Action::Resize {
        sheet: "Books".into(),
        axis: engine::Axis::Col,
        at: 1,
        count: 1,
        size: Some(140.0),
    })
    .unwrap();

    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        xml.contains(r#"style="2""#),
        "the column style was dropped:\n{xml}"
    );
    assert!(
        xml.contains(r#"<col min="4" max="4" hidden="1" outlineLevel="1"/>"#),
        "the hidden column lost its attributes:\n{xml}"
    );
    // ...and column D, which has no width at all, did not acquire one.
    assert!(!xml.contains(r#"min="4" max="4" width="#), "{xml}");
}

#[test]
fn a_sheet_wide_column_run_does_not_explode_the_file() {
    let sheet_xml = SHEET_XML.replace(
        r#"<col min="1" max="1" width="24.5" customWidth="1"/>"#,
        r#"<col min="1" max="16384" width="12" customWidth="1"/>"#,
    );
    let mut e = xlsx::import(&handmade_xlsx(&sheet_xml))
        .expect("import")
        .engine;
    assert_eq!(
        e.wb.sheet_by_name("Books").unwrap().col_widths.len(),
        16_384
    );
    // Now move one column, forcing the whole element to be regenerated.
    e.apply(&Action::Resize {
        sheet: "Books".into(),
        axis: engine::Axis::Col,
        at: 0,
        count: 1,
        size: Some(200.0),
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        xml.contains(r#"<col min="2" max="16384""#),
        "the run was not coalesced:\n{}",
        &xml[..xml.len().min(2000)]
    );
    assert!(
        xml.len() < 20_000,
        "regenerated <cols> is {} bytes; the run was written out one column \
         at a time",
        xml.len()
    );
}

#[test]
fn frozen_panes_survive_a_round_trip_and_undo_takes_them_back() {
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "header");
    e.apply(&Action::FreezePanes {
        sheet: "Sheet1".into(),
        rows: 1,
        cols: 2,
    })
    .unwrap();

    let bytes = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&bytes).expect("import");
    let sheet = back.engine.wb.sheet_by_name("Sheet1").unwrap();
    assert_eq!((sheet.frozen_rows, sheet.frozen_cols), (1, 2));

    e.apply(&Action::Undo).unwrap();
    let sheet = e.wb.sheet_by_name("Sheet1").unwrap();
    assert_eq!((sheet.frozen_rows, sheet.frozen_cols), (0, 0));
}

#[test]
fn freezing_an_imported_sheet_leaves_the_rest_of_its_sheet_view_alone() {
    // `<sheetView>` also carries zoom, gridline settings and the saved
    // selection. A freeze must patch the pane, not regenerate the element.
    let sheet_xml = SHEET_XML.replace(
        r#"<sheetViews><sheetView workbookViewId="0"/></sheetViews>"#,
        r#"<sheetViews><sheetView showGridLines="0" zoomScale="85" workbookViewId="0">"#
            .to_string()
            .as_str(),
    ) + "";
    let sheet_xml = sheet_xml.replace(
        r#"workbookViewId="0">"#,
        r#"workbookViewId="0"><selection activeCell="B7" sqref="B7"/></sheetView></sheetViews>"#,
    );
    let mut e = xlsx::import(&handmade_xlsx(&sheet_xml))
        .expect("import")
        .engine;
    e.apply(&Action::FreezePanes {
        sheet: "Books".into(),
        rows: 2,
        cols: 0,
    })
    .unwrap();

    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(xml.contains(r#"ySplit="2""#), "{xml}");
    assert!(xml.contains(r#"state="frozen""#), "{xml}");
    assert!(xml.contains(r#"topLeftCell="A3""#), "{xml}");
    for keep in [r#"showGridLines="0""#, r#"zoomScale="85""#, r#"sqref="B7""#] {
        assert!(xml.contains(keep), "lost {keep} from\n{xml}");
    }

    // Unfreezing removes the element rather than writing a zero split.
    e.apply(&Action::FreezePanes {
        sheet: "Books".into(),
        rows: 0,
        cols: 0,
    })
    .unwrap();
    let saved = xlsx::export(&e.wb).expect("export");
    let xml = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(!xml.contains("<pane"), "{xml}");
    assert!(xml.contains(r#"zoomScale="85""#), "{xml}");
}

#[test]
fn a_rule_reaches_the_file_with_a_dxf_of_its_own() {
    let mut e = Engine::new();
    set(&mut e, "Sheet1", "A1", "9");
    e.apply(&Action::CondAdd {
        sheet: "Sheet1".into(),
        rule: engine::CondRule {
            range: RangeAddr::parse_a1("A1:A9").unwrap(),
            test: engine::CondTest::CellIs {
                op: engine::CondOp::GreaterThan,
                operands: vec!["5".into()],
            },
            format: engine::CellFormat {
                fill_color: Some("#ff0000".into()),
                bold: true,
                ..engine::CellFormat::default()
            },
        },
    })
    .unwrap();

    let saved = xlsx::export(&e.wb).expect("export");
    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        sheet.contains(r#"<conditionalFormatting sqref="A1:A9">"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"type="cellIs""#), "{sheet}");
    assert!(sheet.contains(r#"operator="greaterThan""#), "{sheet}");
    assert!(sheet.contains("<formula>5</formula>"), "{sheet}");

    // The presentation lives in a <dxf>, not in a <xf>: a rule is a
    // differential format and a cell's own styling shows through it.
    let styles = String::from_utf8(part_of(&saved, "xl/styles.xml")).unwrap();
    assert!(styles.contains("<dxfs"), "no dxfs collection in {styles}");
    assert!(styles.contains("<b/>"), "{styles}");
    // A dxf fill names bgColor where a cell fill names fgColor.
    assert!(styles.contains(r#"<bgColor rgb="FFFF0000"/>"#), "{styles}");

    // The dxfId the rule cites has to exist.
    let dxf_id: usize = sheet
        .split(r#"dxfId=""#)
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.parse().ok())
        .expect("the rule names a dxfId");
    assert!(
        dxf_id < styles.matches("<dxf>").count(),
        "the rule points at dxf {dxf_id}, and the file has {}",
        styles.matches("<dxf>").count()
    );
}

#[test]
fn rules_a_file_arrived_with_are_left_where_they_are() {
    // The handmade fixture carries a <conditionalFormatting> this engine does
    // not model. Adding one of our own must append rather than replace, or
    // saving would silently delete rules the user made in Excel.
    let original = handmade_xlsx(SHEET_XML);
    let mut e = xlsx::import(&original).expect("import").engine;
    e.apply(&Action::CondAdd {
        sheet: "Books".into(),
        rule: engine::CondRule {
            range: RangeAddr::parse_a1("D1:D9").unwrap(),
            test: engine::CondTest::Blank { negate: false },
            format: engine::CellFormat {
                italic: true,
                ..engine::CellFormat::default()
            },
        },
    })
    .unwrap();

    let saved = xlsx::export(&e.wb).expect("export");
    let sheet = String::from_utf8(part_of(&saved, "xl/worksheets/sheet1.xml")).unwrap();
    assert!(
        sheet.contains(r#"<conditionalFormatting sqref="B1:B2">"#),
        "the original rule was dropped:\n{sheet}"
    );
    assert!(
        sheet.contains(r#"<conditionalFormatting sqref="D1:D9">"#),
        "the new rule was not written:\n{sheet}"
    );
    // ...and the new dxf did not take an id the original file already used.
    let styles = String::from_utf8(part_of(&saved, "xl/styles.xml")).unwrap();
    assert!(styles.contains("<dxfs"), "{styles}");
}
