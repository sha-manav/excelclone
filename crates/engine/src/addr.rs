//! Cell and range addressing (A1 notation).

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_COLS: u32 = 16_384; // XFD
pub const MAX_ROWS: u32 = 1_048_576;

/// Zero-based cell coordinates. Row 0 / col 0 == A1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellAddr {
    pub row: u32,
    pub col: u32,
}

impl CellAddr {
    pub fn new(row: u32, col: u32) -> Self {
        CellAddr { row, col }
    }

    pub fn is_valid(&self) -> bool {
        self.row < MAX_ROWS && self.col < MAX_COLS
    }

    /// Format as A1 (no absolute markers).
    pub fn to_a1(&self) -> String {
        format!("{}{}", col_letters(self.col), self.row + 1)
    }

    /// Parse plain A1 like "B7" (absolute markers allowed and ignored).
    pub fn parse_a1(s: &str) -> Option<CellAddr> {
        let r = ParsedRef::parse(s)?;
        Some(CellAddr {
            row: r.row,
            col: r.col,
        })
    }
}

impl fmt::Display for CellAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_a1())
    }
}

/// Inclusive rectangular range, normalized so start <= end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RangeAddr {
    pub start: CellAddr,
    pub end: CellAddr,
}

impl RangeAddr {
    pub fn new(a: CellAddr, b: CellAddr) -> Self {
        RangeAddr {
            start: CellAddr::new(a.row.min(b.row), a.col.min(b.col)),
            end: CellAddr::new(a.row.max(b.row), a.col.max(b.col)),
        }
    }

    pub fn single(a: CellAddr) -> Self {
        RangeAddr { start: a, end: a }
    }

    pub fn contains(&self, a: CellAddr) -> bool {
        a.row >= self.start.row
            && a.row <= self.end.row
            && a.col >= self.start.col
            && a.col <= self.end.col
    }

    pub fn rows(&self) -> u32 {
        self.end.row - self.start.row + 1
    }

    pub fn cols(&self) -> u32 {
        self.end.col - self.start.col + 1
    }

    pub fn cell_count(&self) -> u64 {
        self.rows() as u64 * self.cols() as u64
    }

    pub fn iter_cells(&self) -> impl Iterator<Item = CellAddr> + '_ {
        let (r0, r1, c0, c1) = (self.start.row, self.end.row, self.start.col, self.end.col);
        (r0..=r1).flat_map(move |r| (c0..=c1).map(move |c| CellAddr::new(r, c)))
    }

    pub fn to_a1(&self) -> String {
        if self.start == self.end {
            self.start.to_a1()
        } else {
            format!("{}:{}", self.start.to_a1(), self.end.to_a1())
        }
    }

    /// Parse "A1:B9" or a single "A1".
    pub fn parse_a1(s: &str) -> Option<RangeAddr> {
        match s.split_once(':') {
            Some((a, b)) => Some(RangeAddr::new(
                CellAddr::parse_a1(a.trim())?,
                CellAddr::parse_a1(b.trim())?,
            )),
            None => Some(RangeAddr::single(CellAddr::parse_a1(s.trim())?)),
        }
    }
}

impl fmt::Display for RangeAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_a1())
    }
}

/// A reference as written in a formula: coordinates plus absolute flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParsedRef {
    pub row: u32,
    pub col: u32,
    pub abs_row: bool,
    pub abs_col: bool,
}

impl ParsedRef {
    pub fn addr(&self) -> CellAddr {
        CellAddr::new(self.row, self.col)
    }

    /// Parse "$A$1", "A1", "$A1", "A$1". Case-insensitive.
    pub fn parse(s: &str) -> Option<ParsedRef> {
        let bytes = s.as_bytes();
        let mut i = 0;
        let abs_col = bytes.first() == Some(&b'$');
        if abs_col {
            i += 1;
        }
        let col_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        if i == col_start || i - col_start > 3 {
            return None;
        }
        let col = parse_col_letters(&s[col_start..i])?;
        let abs_row = bytes.get(i) == Some(&b'$');
        if abs_row {
            i += 1;
        }
        let row_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == row_start || i != bytes.len() {
            return None;
        }
        let row_1: u32 = s[row_start..i].parse().ok()?;
        if row_1 == 0 || row_1 > MAX_ROWS {
            return None;
        }
        Some(ParsedRef {
            row: row_1 - 1,
            col,
            abs_row,
            abs_col,
        })
    }

    pub fn to_a1(&self) -> String {
        format!(
            "{}{}{}{}",
            if self.abs_col { "$" } else { "" },
            col_letters(self.col),
            if self.abs_row { "$" } else { "" },
            self.row + 1
        )
    }

    /// Just the column, as `A` or `$A` — one end of a `A:C` reference.
    pub fn to_col(&self) -> String {
        format!(
            "{}{}",
            if self.abs_col { "$" } else { "" },
            col_letters(self.col)
        )
    }

    /// Just the row, as `1` or `$1` — one end of a `1:5` reference.
    pub fn to_row(&self) -> String {
        format!("{}{}", if self.abs_row { "$" } else { "" }, self.row + 1)
    }

    /// Parse a bare column, `A` or `$C`. The row is left at 0.
    pub fn parse_col(s: &str) -> Option<ParsedRef> {
        let abs_col = s.starts_with('$');
        let letters = if abs_col { &s[1..] } else { s };
        if letters.is_empty()
            || letters.len() > 3
            || !letters.bytes().all(|b| b.is_ascii_alphabetic())
        {
            return None;
        }
        Some(ParsedRef {
            row: 0,
            col: parse_col_letters(letters)?,
            abs_row: false,
            abs_col,
        })
    }

    /// Parse a bare row number, `1` or `$5`. The column is left at 0.
    pub fn parse_row(s: &str) -> Option<ParsedRef> {
        let abs_row = s.starts_with('$');
        let digits = if abs_row { &s[1..] } else { s };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let row_1: u32 = digits.parse().ok()?;
        if row_1 == 0 || row_1 > MAX_ROWS {
            return None;
        }
        Some(ParsedRef {
            row: row_1 - 1,
            col: 0,
            abs_row,
            abs_col: false,
        })
    }

    /// Shift the relative parts by (dr, dc); returns None when out of bounds (#REF!).
    pub fn shifted(&self, dr: i64, dc: i64) -> Option<ParsedRef> {
        let row = if self.abs_row {
            self.row as i64
        } else {
            self.row as i64 + dr
        };
        let col = if self.abs_col {
            self.col as i64
        } else {
            self.col as i64 + dc
        };
        if row < 0 || row >= MAX_ROWS as i64 || col < 0 || col >= MAX_COLS as i64 {
            return None;
        }
        Some(ParsedRef {
            row: row as u32,
            col: col as u32,
            ..*self
        })
    }
}

/// 0 -> "A", 25 -> "Z", 26 -> "AA", 16383 -> "XFD".
pub fn col_letters(mut col: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// "A" -> 0, "Z" -> 25, "AA" -> 26. Case-insensitive. None if out of range.
pub fn parse_col_letters(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 {
        return None;
    }
    let mut col: u32 = 0;
    for b in s.bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
    }
    let col = col - 1;
    if col >= MAX_COLS {
        return None;
    }
    Some(col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_letter_round_trip() {
        for c in [0, 1, 25, 26, 27, 51, 52, 701, 702, 703, 16_383] {
            assert_eq!(parse_col_letters(&col_letters(c)), Some(c), "col {c}");
        }
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(701), "ZZ");
        assert_eq!(col_letters(702), "AAA");
        assert_eq!(col_letters(16_383), "XFD");
        assert_eq!(parse_col_letters("XFE"), None);
    }

    #[test]
    fn parse_refs() {
        let r = ParsedRef::parse("$B$7").unwrap();
        assert_eq!((r.row, r.col, r.abs_row, r.abs_col), (6, 1, true, true));
        let r = ParsedRef::parse("b7").unwrap();
        assert_eq!((r.row, r.col, r.abs_row, r.abs_col), (6, 1, false, false));
        let r = ParsedRef::parse("$AA10").unwrap();
        assert_eq!((r.row, r.col, r.abs_row, r.abs_col), (9, 26, false, true));
        assert!(ParsedRef::parse("A0").is_none());
        assert!(ParsedRef::parse("A").is_none());
        assert!(ParsedRef::parse("1").is_none());
        assert!(ParsedRef::parse("A1B").is_none());
        assert!(ParsedRef::parse("XFE1").is_none());
        assert!(ParsedRef::parse("A1048577").is_none());
        assert!(ParsedRef::parse("A1048576").is_some());
    }

    #[test]
    fn range_normalization() {
        let r = RangeAddr::parse_a1("B9:A1").unwrap();
        assert_eq!(r.start, CellAddr::new(0, 0));
        assert_eq!(r.end, CellAddr::new(8, 1));
        assert_eq!(r.to_a1(), "A1:B9");
        assert_eq!(r.rows(), 9);
        assert_eq!(r.cols(), 2);
        assert!(r.contains(CellAddr::new(4, 1)));
        assert!(!r.contains(CellAddr::new(9, 0)));
    }

    #[test]
    fn shifted_refs() {
        let r = ParsedRef::parse("B2").unwrap();
        let s = r.shifted(2, 3).unwrap();
        assert_eq!(s.to_a1(), "E4");
        let abs = ParsedRef::parse("$B$2").unwrap();
        assert_eq!(abs.shifted(5, 5).unwrap().to_a1(), "$B$2");
        assert!(ParsedRef::parse("A1").unwrap().shifted(-1, 0).is_none());
    }
}
