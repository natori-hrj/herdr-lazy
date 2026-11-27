use std::io::{self, Write};

use unicode_width::UnicodeWidthChar;

use crate::theme::Selection;

pub(super) fn start_selection(
    out: &mut impl Write,
    selection: Selection,
    columns: usize,
) -> io::Result<()> {
    // Fill the row before writing text, whose display width depends on the terminal.
    write!(
        out,
        "{selection}{:columns$}\x1b[1G{}",
        "",
        selection.foreground()
    )
}

pub(super) fn finish_selection(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\x1b[39m\x1b[49m")
}

pub(super) fn cell_width(ch: char) -> usize {
    // Count emoji presentation selectors conservatively for terminals that expand them.
    if ch == '\u{fe0f}' {
        1
    } else {
        ch.width().unwrap_or(0)
    }
}

pub(super) fn clip(text: &str, columns: usize) -> (String, usize) {
    let mut line = String::new();
    let mut width = 0;
    if columns == 0 {
        return (line, width);
    }
    for ch in text.chars() {
        // Keep text fields on their row and prevent them from emitting terminal commands.
        let ch = if ch.is_control() { ' ' } else { ch };
        let cells = cell_width(ch);
        if width + cells > columns {
            while width == columns {
                let removed = line.pop().expect("a full line contains a character");
                width -= cell_width(removed);
            }
            line.push('…');
            return (line, width + 1);
        }
        line.push(ch);
        width += cells;
    }
    (line, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_uses_display_cells_and_flattens_control_characters() {
        assert_eq!(clip("日本語", 4), ("日…".into(), 3));
        assert_eq!(clip("a💡b", 3), ("a…".into(), 2));
        assert_eq!(clip("a\nb\tc\x1bd", 20), ("a b c d".into(), 7));
        assert_eq!(clip("name", 0), (String::new(), 0));
    }
}
