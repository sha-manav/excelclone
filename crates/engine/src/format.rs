//! Per-cell presentation: bold, italic, colours, borders, number format and
//! alignment.
//!
//! Three decisions shape this module, and they are worth stating because each
//! one had a plausible alternative.
//!
//! **Formats live beside cells, not inside them.** `Sheet::formats` is a
//! second sparse map keyed by address. The obvious alternative — a `format`
//! field on `Cell` — would mean formatting an empty cell has to invent a cell
//! to hang the format on, which then shows up in the used range, in CSV
//! export, and in every `cells.len()` in the codebase. Formatting a blank
//! column before typing into it is an ordinary gesture, so the model has to
//! allow a format with no value. Keeping them separate also means the
//! evaluator, the dependency graph and the parser need to know nothing about
//! formatting at all: presentation is orthogonal to computation.
//!
//! **Formats are interned.** A sheet where one column is bold holds one
//! `CellFormat` and N small ids, the same shape xlsx uses with `cellXfs`.
//! Interning is a linear scan: real workbooks have tens of distinct formats,
//! not thousands, and a `Vec` keeps the table trivially serializable and
//! deterministically ordered, which the replay tests depend on.
//!
//! **The unit of change is a patch, not a whole format.** "Make this bold"
//! must not clear the fill colour the user set a moment ago. Each
//! [`FormatPatch`] names exactly one attribute, and an action carries a list
//! of them, so the recorded action says what the user asked for rather than
//! the full resulting state.

use crate::addr::RangeAddr;
use serde::{Deserialize, Serialize};

/// Horizontal alignment. `None` on a cell means automatic: numbers, booleans
/// and errors right, everything else left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HAlign {
    Left,
    Center,
    Right,
}

impl HAlign {
    pub fn as_str(&self) -> &'static str {
        match self {
            HAlign::Left => "left",
            HAlign::Center => "center",
            HAlign::Right => "right",
        }
    }
}

/// Which edges of a cell carry a border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Borders {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl Borders {
    pub fn none() -> Self {
        Borders::default()
    }

    pub fn all() -> Self {
        Borders {
            top: true,
            right: true,
            bottom: true,
            left: true,
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Borders::default()
    }
}

/// The border gesture a user makes over a *range*, which is not the same as
/// the border state of any one cell in it: "outline" draws only on the
/// perimeter, so the cells in the middle are left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderPreset {
    /// Every edge of every cell.
    All,
    /// Only the outer edges of the range.
    Outline,
    /// Clear every edge.
    None,
}

impl BorderPreset {
    /// The borders this preset gives a cell at `(row, col)` inside `range`.
    pub fn edges_at(&self, range: RangeAddr, row: u32, col: u32) -> Borders {
        match self {
            BorderPreset::All => Borders::all(),
            BorderPreset::None => Borders::none(),
            BorderPreset::Outline => Borders {
                top: row == range.start.row,
                bottom: row == range.end.row,
                left: col == range.start.col,
                right: col == range.end.col,
            },
        }
    }
}

/// Everything the renderer needs to draw a cell, and the exporter to style
/// one. The default is "no formatting at all", which is never stored.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct CellFormat {
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    /// `#RRGGBB`, lowercase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    #[serde(default, skip_serializing_if = "Borders::is_none")]
    pub borders: Borders,
    /// An Excel format code. `None` is General.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<HAlign>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl CellFormat {
    pub fn is_default(&self) -> bool {
        *self == CellFormat::default()
    }
}

/// One attribute change. A gesture like "bold and red" is two patches in one
/// action, which keeps every patch unambiguous on the wire: `{"set":
/// "fill_color", "value": null}` clears the fill, and an absent patch leaves
/// the attribute alone. An `Option<Option<T>>` field would have had to
/// distinguish those two by JSON `null` versus a missing key, which serde
/// does not do without a custom deserializer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "set", content = "value", rename_all = "snake_case")]
pub enum FormatPatch {
    Bold(bool),
    Italic(bool),
    FontColor(Option<String>),
    FillColor(Option<String>),
    Border(BorderPreset),
    NumberFormat(Option<String>),
    Align(Option<HAlign>),
}

impl FormatPatch {
    /// The attribute this patch names, for telemetry. Never the value: a
    /// colour is not personal data, but keeping the vocabulary to attribute
    /// names alone means a future patch carrying user text cannot leak by
    /// being added here and forgotten.
    pub fn attribute(&self) -> &'static str {
        match self {
            FormatPatch::Bold(_) => "bold",
            FormatPatch::Italic(_) => "italic",
            FormatPatch::FontColor(_) => "font_color",
            FormatPatch::FillColor(_) => "fill_color",
            FormatPatch::Border(_) => "border",
            FormatPatch::NumberFormat(_) => "number_format",
            FormatPatch::Align(_) => "align",
        }
    }

    /// Apply to one cell's format. `range` and the cell position are only
    /// consulted by the border preset, which depends on where in the range
    /// the cell sits.
    pub fn apply_to(&self, f: &mut CellFormat, range: RangeAddr, row: u32, col: u32) {
        match self {
            FormatPatch::Bold(v) => f.bold = *v,
            FormatPatch::Italic(v) => f.italic = *v,
            FormatPatch::FontColor(c) => f.font_color = c.clone().map(normalize_color),
            FormatPatch::FillColor(c) => f.fill_color = c.clone().map(normalize_color),
            FormatPatch::Border(p) => f.borders = p.edges_at(range, row, col),
            FormatPatch::NumberFormat(c) => {
                f.number_format = c.clone().filter(|s| {
                    let t = s.trim();
                    !t.is_empty() && !t.eq_ignore_ascii_case("general")
                })
            }
            FormatPatch::Align(a) => f.align = *a,
        }
    }
}

/// Colours are stored in one canonical spelling so that `#FFF`, `#ffffff` and
/// `FFFFFF` intern to the same format rather than three.
fn normalize_color(c: String) -> String {
    let hex: String = c.trim().trim_start_matches('#').to_ascii_lowercase();
    let expanded = if hex.len() == 3 {
        hex.chars().flat_map(|c| [c, c]).collect()
    } else {
        hex
    };
    if expanded.len() == 6 && expanded.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", expanded)
    } else {
        // Not a colour we understand; keep it rather than silently dropping
        // it, so a bad value shows up as a bad value.
        c
    }
}

/// A cell's format, referenced by index.
pub type FormatId = u32;

/// The workbook's format palette: append-only, deduplicated, index-stable.
///
/// Index stability is what lets undo record ids rather than whole formats.
/// The table is never pruned, so an id recorded before an undo is still valid
/// after it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FormatTable {
    entries: Vec<CellFormat>,
}

impl FormatTable {
    /// The id for a format, adding it if this is the first time it is seen.
    /// Returns `None` for the default format, which is represented by the
    /// absence of an entry rather than by an id.
    pub fn intern(&mut self, f: CellFormat) -> Option<FormatId> {
        if f.is_default() {
            return None;
        }
        if let Some(i) = self.entries.iter().position(|e| *e == f) {
            return Some(i as FormatId);
        }
        self.entries.push(f);
        Some((self.entries.len() - 1) as FormatId)
    }

    pub fn get(&self, id: FormatId) -> Option<&CellFormat> {
        self.entries.get(id as usize)
    }

    /// The format for an optional id; the default when there is none.
    pub fn resolve(&self, id: Option<FormatId>) -> CellFormat {
        id.and_then(|i| self.get(i)).cloned().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::CellAddr;

    fn r(a: &str) -> RangeAddr {
        RangeAddr::parse_a1(a).unwrap()
    }

    #[test]
    fn default_format_is_never_interned() {
        let mut t = FormatTable::default();
        assert_eq!(t.intern(CellFormat::default()), None);
        assert!(t.is_empty());
    }

    #[test]
    fn identical_formats_share_an_id() {
        let mut t = FormatTable::default();
        let a = t.intern(CellFormat {
            bold: true,
            ..Default::default()
        });
        let b = t.intern(CellFormat {
            bold: true,
            ..Default::default()
        });
        let c = t.intern(CellFormat {
            italic: true,
            ..Default::default()
        });
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn colors_normalize_to_one_spelling() {
        let mut f = CellFormat::default();
        let range = r("A1");
        FormatPatch::FillColor(Some("#FFF".into())).apply_to(&mut f, range, 0, 0);
        assert_eq!(f.fill_color.as_deref(), Some("#ffffff"));
        let mut g = CellFormat::default();
        FormatPatch::FillColor(Some("ffffff".into())).apply_to(&mut g, range, 0, 0);
        assert_eq!(f, g);
    }

    #[test]
    fn general_number_format_is_stored_as_none() {
        let mut f = CellFormat::default();
        let range = r("A1");
        FormatPatch::NumberFormat(Some("General".into())).apply_to(&mut f, range, 0, 0);
        assert_eq!(f.number_format, None);
        FormatPatch::NumberFormat(Some("0.00".into())).apply_to(&mut f, range, 0, 0);
        assert_eq!(f.number_format.as_deref(), Some("0.00"));
    }

    #[test]
    fn outline_touches_only_the_perimeter() {
        let range = r("B2:D4");
        // A corner gets two edges, a middle-of-edge cell one, the centre none.
        assert_eq!(
            BorderPreset::Outline.edges_at(range, 1, 1),
            Borders {
                top: true,
                left: true,
                bottom: false,
                right: false
            }
        );
        assert_eq!(
            BorderPreset::Outline.edges_at(range, 1, 2),
            Borders {
                top: true,
                left: false,
                bottom: false,
                right: false
            }
        );
        assert!(BorderPreset::Outline.edges_at(range, 2, 2).is_none());
        // A single-cell range is its own perimeter on all four sides.
        assert_eq!(
            BorderPreset::Outline.edges_at(r("A1"), 0, 0),
            Borders::all()
        );
    }

    #[test]
    fn all_and_none_ignore_position() {
        let range = r("A1:C3");
        assert_eq!(BorderPreset::All.edges_at(range, 1, 1), Borders::all());
        assert!(BorderPreset::None.edges_at(range, 0, 0).is_none());
    }

    #[test]
    fn patches_are_independent() {
        // The failure this guards against: "make it bold" clearing the fill.
        let range = r("A1");
        let mut f = CellFormat::default();
        FormatPatch::FillColor(Some("#ff0000".into())).apply_to(&mut f, range, 0, 0);
        FormatPatch::Bold(true).apply_to(&mut f, range, 0, 0);
        assert!(f.bold);
        assert_eq!(f.fill_color.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn patch_json_distinguishes_clearing_from_leaving_alone() {
        let clear: FormatPatch =
            serde_json::from_str(r#"{"set":"fill_color","value":null}"#).unwrap();
        assert_eq!(clear, FormatPatch::FillColor(None));
        let set: FormatPatch =
            serde_json::from_str(r##"{"set":"fill_color","value":"#ff0000"}"##).unwrap();
        assert_eq!(set, FormatPatch::FillColor(Some("#ff0000".into())));
        assert_eq!(
            serde_json::to_string(&FormatPatch::Bold(true)).unwrap(),
            r#"{"set":"bold","value":true}"#
        );
    }

    #[test]
    fn format_json_omits_defaults() {
        let f = CellFormat {
            bold: true,
            ..Default::default()
        };
        assert_eq!(serde_json::to_string(&f).unwrap(), r#"{"bold":true}"#);
        assert_eq!(serde_json::to_string(&CellFormat::default()).unwrap(), "{}");
    }

    #[test]
    fn ids_survive_later_interning() {
        let mut t = FormatTable::default();
        let first = t
            .intern(CellFormat {
                bold: true,
                ..Default::default()
            })
            .unwrap();
        for i in 0..10 {
            t.intern(CellFormat {
                number_format: Some(format!("0.{}", i)),
                ..Default::default()
            });
        }
        assert!(t.get(first).unwrap().bold);
    }

    #[test]
    fn addresses_are_unused_here_but_ranges_are_not() {
        // Guards the border preset against an off-by-one when the range does
        // not start at the origin.
        let range = RangeAddr::new(CellAddr::new(5, 5), CellAddr::new(7, 7));
        assert!(BorderPreset::Outline.edges_at(range, 5, 5).top);
        assert!(!BorderPreset::Outline.edges_at(range, 6, 6).top);
    }
}
