//! Column widths and row heights, and the units xlsx keeps them in.
//!
//! The model stores both in pixels, because that is what the grid draws and
//! what a drag gesture produces. The file format stores neither:
//!
//! - a column `width` is a count of *characters* of the maximum digit width
//!   (MDW) of the workbook's normal font, plus padding;
//! - a row `ht` is in points.
//!
//! Points are easy. Characters are not, and the conversion below is the pair
//! of formulas from the OOXML documentation rather than an approximation of
//! them, for one reason: opening a file and saving it must not move any
//! column. Both directions truncate, so a naive `px = chars * mdw` would
//! drift by a pixel on most columns and the parity harness would report the
//! state as changed on every round trip. [`px_to_chars`] and [`chars_to_px`]
//! are exact inverses over the pixel widths a grid can produce, and
//! `widths_survive_a_round_trip` holds them to it.

/// Maximum digit width of Calibri 11, the normal font of every xlsx anyone
/// makes today, in pixels.
///
/// The real value comes from the workbook's font metrics, which would mean
/// measuring glyphs. Using the overwhelmingly common one keeps the conversion
/// self-inverse — which is what protects the file — and the worst case for a
/// workbook in some other font is that its columns are drawn slightly wide.
const MDW: f64 = 7.0;

/// Padding Excel adds either side of a column's character count.
const COL_PADDING: f64 = 5.0;

pub const POINTS_PER_PIXEL: f64 = 0.75;

/// A `width` attribute to pixels.
pub fn chars_to_px(chars: f64) -> f64 {
    if !chars.is_finite() || chars <= 0.0 {
        return 0.0;
    }
    (((256.0 * chars + (128.0 / MDW).trunc()) / 256.0) * MDW).trunc() + COL_PADDING
}

/// Pixels back to a `width` attribute.
pub fn px_to_chars(px: f64) -> f64 {
    if !px.is_finite() || px <= COL_PADDING {
        return 0.0;
    }
    ((px - COL_PADDING) / MDW * 100.0 + 0.5).trunc() / 100.0
}

/// A width in the units `rust_xlsxwriter::set_column_width_pixels` wants.
///
/// It calls its argument pixels, but it means the pixels *inside* the column:
/// it adds Excel's padding itself on the way to the `width` attribute. Handing
/// it a rendered width would widen every column by five pixels per save, which
/// is the kind of drift that only shows up after the tenth one.
pub fn px_to_writer_px(px: f64) -> f64 {
    (px - COL_PADDING).max(0.0).round()
}

pub fn points_to_px(points: f64) -> f64 {
    if !points.is_finite() || points <= 0.0 {
        return 0.0;
    }
    (points / POINTS_PER_PIXEL * 100.0).round() / 100.0
}

pub fn px_to_points(px: f64) -> f64 {
    if !px.is_finite() || px <= 0.0 {
        return 0.0;
    }
    // Rounded, not raw: 19.2px is 14.4pt, and the multiplication answers
    // 14.399999999999999. That is the height of every default Calibri row in
    // every file, so writing the long form would rewrite each one of them.
    (px * POINTS_PER_PIXEL * 10_000.0).round() / 10_000.0
}

/// Render a number the way an xlsx attribute wants it: no trailing zeros, no
/// exponent, and no `-0`.
pub fn fmt_num(n: f64) -> String {
    if !n.is_finite() {
        return "0".into();
    }
    let mut s = format!("{:.6}", n);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_default_column() {
        // Excel's default column is 8.43 characters and 64 pixels wide; if
        // this pair is wrong every other conversion is wrong by the same
        // amount and nothing else in the suite would notice.
        assert_eq!(chars_to_px(8.43), 64.0);
        assert_eq!(px_to_chars(64.0), 8.43);
    }

    #[test]
    fn widths_survive_a_round_trip() {
        // Every pixel width a resize can produce, through the file and back.
        // A drift of one pixel per save is exactly the kind of thing that
        // looks like nothing and makes a workbook unrecognisable after a week
        // of edits.
        for px in 6..=2000 {
            let px = px as f64;
            let back = chars_to_px(px_to_chars(px));
            assert_eq!(back, px, "width {px}px came back as {back}px");
        }
    }

    #[test]
    fn heights_survive_a_round_trip() {
        for tenths in 10..=4000 {
            let px = tenths as f64 / 10.0;
            assert_eq!(points_to_px(px_to_points(px)), px, "height {px}px");
        }
    }

    #[test]
    fn common_row_heights_keep_their_points() {
        // 14.4pt is the Calibri 11 default and 15pt the Arial 10 one; both
        // are in almost every real workbook, and a conversion that could not
        // reproduce them would rewrite every row of every file opened.
        for pt in [14.4, 15.0, 12.75, 30.0] {
            assert_eq!(px_to_points(points_to_px(pt)), pt, "{pt}pt");
        }
    }

    #[test]
    fn nonsense_sizes_are_not_propagated() {
        assert_eq!(chars_to_px(f64::NAN), 0.0);
        assert_eq!(chars_to_px(-1.0), 0.0);
        assert_eq!(px_to_chars(0.0), 0.0);
        assert_eq!(points_to_px(f64::INFINITY), 0.0);
    }

    #[test]
    fn attribute_numbers_are_written_plainly() {
        assert_eq!(fmt_num(8.43), "8.43");
        assert_eq!(fmt_num(15.0), "15");
        assert_eq!(fmt_num(14.4), "14.4");
    }
}
