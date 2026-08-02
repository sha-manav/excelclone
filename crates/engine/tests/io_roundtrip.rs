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
fn adding_a_sheet_to_a_preserved_workbook_fails_loudly() {
    let mut e = xlsx::import(&handmade_xlsx(SHEET_XML)).unwrap().engine;
    e.apply(&Action::SheetAdd {
        name: "Extra".into(),
    })
    .unwrap();
    let err = xlsx::export(&e.wb).unwrap_err();
    assert!(
        err.to_string().contains("Extra"),
        "expected a loud failure, got {err}"
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
