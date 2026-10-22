//! Embedded Ubuntu font faces for the invoice document.
//!
//! The renderer used base-14 Helvetica until #new. The document now embeds the
//! shipped Ubuntu Regular/Bold/Italic/BoldItalic TrueType faces as simple PDF
//! fonts with `/WinAnsiEncoding`: the content stream keeps the same one-byte
//! WinAnsi text the layout already produces, while viewers draw the real Ubuntu
//! glyphs. Widths and descriptor metrics are read once from the font tables and
//! cached in a `OnceLock`, so the render stays deterministic and allocation-free
//! apart from the one-time parse.
//!
//! The fonts and their UFL licence live beside this file. Download source:
//! <https://github.com/google/fonts/tree/main/ufl/ubuntu> (Ubuntu Font Licence,
//! see `fonts/UFL.txt`).

use std::sync::OnceLock;

use super::layout::Face;

/// Ubuntu Regular (weight 400).
const REGULAR: &[u8] = include_bytes!("fonts/Ubuntu-Regular.ttf");
/// Ubuntu Bold (weight 700).
const BOLD: &[u8] = include_bytes!("fonts/Ubuntu-Bold.ttf");
/// Ubuntu Italic.
const ITALIC: &[u8] = include_bytes!("fonts/Ubuntu-Italic.ttf");
/// Ubuntu Bold Italic.
const BOLD_ITALIC: &[u8] = include_bytes!("fonts/Ubuntu-BoldItalic.ttf");

/// Per-face metrics in PDF's 1/1000 text-space units.
pub(super) struct FontMetrics {
    /// Advance widths for WinAnsi codes 32..=255, indexed by `code - 32`.
    pub(super) widths: [u32; 224],
    pub(super) ascent: f32,
    pub(super) descent: f32,
    pub(super) cap_height: f32,
    pub(super) bbox: [f32; 4],
    pub(super) italic_angle: f32,
}

impl FontMetrics {
    fn width(&self, code: u8) -> u32 {
        if code < 32 {
            0
        } else {
            self.widths[(code - 32) as usize]
        }
    }
}

static FONTS: OnceLock<[FontMetrics; 4]> = OnceLock::new();

fn fonts() -> &'static [FontMetrics; 4] {
    FONTS.get_or_init(|| {
        [
            parse(REGULAR),
            parse(BOLD),
            parse(ITALIC),
            parse(BOLD_ITALIC),
        ]
    })
}

/// Parse one bundled face into the small metric set the PDF writer needs.
fn parse(data: &'static [u8]) -> FontMetrics {
    let face =
        ttf_parser::Face::parse(data, 0).expect("bundled Ubuntu font is a valid TrueType face");
    let upem = u32::from(face.units_per_em()).max(1);
    let scale = |value: i16| value as f32 * 1000.0 / upem as f32;
    let mut widths = [0u32; 224];
    for code in 32..=255u8 {
        let ch = winansi_char(code).unwrap_or('?');
        let glyph = face.glyph_index(ch).or_else(|| face.glyph_index('?'));
        let advance = glyph
            .and_then(|g| face.glyph_hor_advance(g))
            .map(u32::from)
            .unwrap_or(upem / 2);
        // 1/1000 text units; a zero advance is a rendering bug, not a font.
        widths[(code - 32) as usize] = (advance * 1000 / upem).max(1);
    }
    let bbox = face.global_bounding_box();
    FontMetrics {
        widths,
        ascent: scale(face.ascender()),
        descent: scale(face.descender()),
        cap_height: scale(face.capital_height().unwrap_or(face.ascender())),
        bbox: [
            scale(bbox.x_min),
            scale(bbox.y_min),
            scale(bbox.x_max),
            scale(bbox.y_max),
        ],
        italic_angle: face.italic_angle(),
    }
}

/// The metrics for a doc face.
pub(super) fn metrics(face: Face) -> &'static FontMetrics {
    &fonts()[face_index(face)]
}

/// The embedded TrueType bytes for a doc face.
pub(super) fn font_bytes(face: Face) -> &'static [u8] {
    match face {
        Face::Regular => REGULAR,
        Face::Bold => BOLD,
        Face::Oblique => ITALIC,
        Face::BoldOblique => BOLD_ITALIC,
    }
}

/// PostScript base name written in the PDF font dictionaries.
pub(super) fn postscript_name(face: Face) -> &'static str {
    match face {
        Face::Regular => "Ubuntu",
        Face::Bold => "Ubuntu-Bold",
        Face::Oblique => "Ubuntu-Italic",
        Face::BoldOblique => "Ubuntu-BoldItalic",
    }
}

/// Advance width in 1/1000 text units for a WinAnsi code.
pub(super) fn width_units(face: Face, code: u8) -> u32 {
    metrics(face).width(code)
}

fn face_index(face: Face) -> usize {
    match face {
        Face::Regular => 0,
        Face::Bold => 1,
        Face::Oblique => 2,
        Face::BoldOblique => 3,
    }
}

/// The Unicode character a WinAnsi code draws, for the font's cmap lookup.
/// Mirrors [`super::metrics::winansi`] exactly; undefined slots return None.
fn winansi_char(code: u8) -> Option<char> {
    let ch = match code {
        0x80 => '€',
        0x85 => '…',
        0x91 => '‘',
        0x92 => '’',
        0x93 => '“',
        0x94 => '”',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x99 => '™',
        0x20..=0x7E | 0xA0..=0xFF => code as char,
        _ => return None,
    };
    Some(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_has_metrics_and_sane_widths() {
        for face in [Face::Regular, Face::Bold, Face::Oblique, Face::BoldOblique] {
            let metrics = metrics(face);
            assert!(metrics.ascent > 0.0, "{face:?}");
            assert!(metrics.descent < 0.0, "{face:?}");
            assert!(metrics.cap_height > 0.0, "{face:?}");
            assert!(metrics.width(b'A') > 0, "{face:?}");
            assert!(metrics.width(b' ') > 0, "{face:?}");
            assert!(!font_bytes(face).is_empty(), "{face:?}");
            assert!(!postscript_name(face).is_empty(), "{face:?}");
        }
    }

    #[test]
    fn bold_is_wider_than_regular_for_a_representative_glyph() {
        assert!(width_units(Face::Bold, b'W') > width_units(Face::Regular, b'W'));
    }
}
