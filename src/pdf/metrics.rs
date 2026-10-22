use super::layout::Face;

/// Text in WinAnsi bytes (the base-14 encoding). UTF-8 is mapped code point
/// by code point; unrepresentable characters become `?`. Deterministic by
/// construction.
pub(super) fn winansi(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let b = match ch {
            '€' => 0x80,
            '…' => 0x85,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '™' => 0x99,
            c if (c as u32) <= 0xFF && (c as u32) >= 0x20 || (c as u32) == 0x0A => c as u8,
            _ => b'?',
        };
        out.push(b);
    }
    out
}

/// Advance width in 1/1000 text units for a WinAnsi code, read from the
/// embedded Ubuntu face. The old per-code Helvetica table is gone: the PDF now
/// embeds the shipped font, so its own metrics drive wrapping.
pub(super) fn width_units(face: Face, code: u8) -> u32 {
    super::fonts::width_units(face, code)
}

/// Width in integer points (round half up) of WinAnsi bytes at `size`.
pub(super) fn measure(face: Face, size: i32, bytes: &[u8]) -> i32 {
    let total: u64 = bytes
        .iter()
        .map(|&b| u64::from(width_units(face, b)))
        .sum::<u64>()
        * size.max(0) as u64;
    ((total + 500) / 1000) as i32
}

/// Greedy word wrap over WinAnsi bytes at a fixed face/size. Whitespace is
/// collapsed; over-long words hard-split so nothing overflows the column.
fn wrap(face: Face, size: i32, bytes: &[u8], max_w: i32) -> Vec<Vec<u8>> {
    let space = measure(face, size, b" ");
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_w = 0;
    for word in bytes.split(|b| *b == b' ' || *b == b'\n') {
        if word.is_empty() {
            continue;
        }
        let mut w = measure(face, size, word);
        if w == 0 {
            continue;
        }
        let mut word = word;
        // Hard-split a word wider than the column.
        while w > max_w && word.len() > 1 {
            let mut n = word.len();
            while n > 1 && measure(face, size, &word[..n]) > max_w {
                n -= 1;
            }
            let (head, tail) = word.split_at(n);
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            lines.push(head.to_vec());
            word = tail;
            w = measure(face, size, word);
        }
        let add = if cur.is_empty() { w } else { w + space };
        if cur_w + add > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if !cur.is_empty() {
            cur.push(b' ');
            cur_w += space;
        }
        cur.extend_from_slice(word);
        cur_w += w;
        if cur_w >= max_w {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Wrap then cap at `max_lines`, marking a truncated tail with ASCII "...".
pub(super) fn wrap_limited(
    face: Face,
    size: i32,
    bytes: &[u8],
    max_w: i32,
    max_lines: usize,
) -> Vec<Vec<u8>> {
    let mut lines = wrap(face, size, bytes, max_w);
    if lines.len() > max_lines {
        let head = lines.remove(max_lines - 1);
        let dots = b"...";
        let mut cut = head;
        while cut.len() >= 3 && measure(face, size, &cut) + measure(face, size, dots) > max_w {
            cut.pop();
        }
        cut.extend_from_slice(dots);
        lines.truncate(max_lines - 1);
        lines.push(cut);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::layout::COL_DESC;

    #[test]
    fn winansi_maps_and_substitutes() {
        assert_eq!(winansi("€5.00"), vec![0x80, b'5', b'.', b'0', b'0']);
        assert_eq!(
            winansi("naïve — ok"),
            vec![b'n', b'a', 0xEF, b'v', b'e', b' ', 0x97, b' ', b'o', b'k']
        );
        assert_eq!(winansi("日本語"), b"???".to_vec());
    }

    #[test]
    fn wrap_breaks_words_to_width() {
        let text = b"the quick brown fox jumps over the lazy dog".to_vec();
        let lines = wrap(Face::Regular, 10, &text, 70);
        assert!(lines.len() >= 3);
        for l in &lines {
            assert!(
                measure(Face::Regular, 10, l) <= 70,
                "{:?} wider than cap",
                String::from_utf8_lossy(l)
            );
        }
    }

    #[test]
    fn truncate_marks_with_dots() {
        let long = "x".repeat(300).into_bytes();
        let lines = wrap_limited(Face::Regular, 9, &long, COL_DESC - 6, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with(b"..."));
    }
}
