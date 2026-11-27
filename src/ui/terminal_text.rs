use std::borrow::Cow;

use unicode_segmentation::{GraphemeIndices, UnicodeSegmentation};
use unicode_width::UnicodeWidthStr;

pub(super) fn one_line(text: &str) -> Cow<'_, str> {
    // Keep text fields on their row and prevent them from emitting terminal commands.
    if text.chars().any(char::is_control) {
        Cow::Owned(
            text.chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

pub(super) fn display_width(text: &str) -> usize {
    text.width()
}

// At a grapheme boundary, a joining sequence can briefly go over budget and
// then fit when the next grapheme is included (for example, Tifinagh ⵏ⵿ⴾ).
// Check both sides of the next boundary while searching for the cut.
fn fitting_count(ends: &[usize], columns: usize, width: impl Fn(usize) -> usize) -> usize {
    let mut start = 0;
    let mut end = ends.len();
    while start < end {
        let mid = start + (end - start) / 2;
        if ends
            .get(mid + 1)
            .is_some_and(|&next| width(next) <= columns)
        {
            start = mid + 2;
        } else if width(ends[mid]) <= columns {
            start = mid + 1;
        } else {
            end = mid;
        }
    }
    start
}

fn fit_prefix(
    text: &str,
    start: usize,
    graphemes: &mut GraphemeIndices<'_>,
    columns: usize,
    ends: &mut Vec<usize>,
) -> usize {
    // Grow the search window geometrically. Wrapping can stop near the
    // current row even when the rest of the log line is very long.
    let width = |end| display_width(&text[start..end]);
    let mut lookahead = graphemes.clone();
    ends.clear();
    let mut batch = 2;
    loop {
        let before = ends.len();
        ends.extend(
            lookahead
                .by_ref()
                .take(batch)
                .map(|(index, cluster)| index + cluster.len()),
        );
        if ends.len() - before < batch
            || ends[ends.len() - 2..]
                .iter()
                .all(|&end| width(end) > columns)
        {
            break;
        }
        batch *= 2;
    }
    let count = fitting_count(ends, columns, width);
    count.checked_sub(1).map_or(start, |last| {
        graphemes.nth(last);
        ends[last]
    })
}

pub(super) fn clip(text: &str, columns: usize) -> (String, usize) {
    if columns == 0 {
        return (String::new(), 0);
    }
    let text = one_line(text);
    let width = display_width(&text);
    if width <= columns {
        return (text.into_owned(), width);
    }

    let end = fit_prefix(
        &text,
        0,
        &mut text.grapheme_indices(true),
        columns - 1,
        &mut Vec::new(),
    );
    let clipped = format!("{}…", &text[..end]);
    let width = display_width(&clipped);
    (clipped, width)
}

pub(super) fn clip_tail(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let text = one_line(text);
    if display_width(&text) <= columns {
        return text.into_owned();
    }

    // The pinned segmentation iterator can group Indic text differently
    // when run backwards. Reverse the offsets from a forward pass instead.
    let mut starts: Vec<_> = text
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .collect();
    starts.reverse();
    let count = fitting_count(&starts, columns - 1, |start| display_width(&text[start..]));
    let start = count.checked_sub(1).map_or(text.len(), |last| starts[last]);
    format!("…{}", &text[start..])
}

pub(super) fn wrap_line(text: &str, columns: usize) -> impl Iterator<Item = String> + '_ {
    let columns = columns.max(1);
    let mut graphemes = text.grapheme_indices(true);
    let mut ends = Vec::new();
    let mut start = 0;
    let mut empty = text.is_empty();
    std::iter::from_fn(move || {
        if std::mem::take(&mut empty) {
            return Some(String::new());
        }
        if start == text.len() {
            return None;
        }

        let end = fit_prefix(text, start, &mut graphemes, columns, &mut ends);
        if end > start {
            let line = text[start..end].to_owned();
            start = end;
            return Some(line);
        }

        // A single oversized cluster gets a marker. Fill any remaining
        // cells from the following graphemes using their new line context.
        graphemes.next();
        let rest = ends[0];
        start = fit_prefix(text, rest, &mut graphemes, columns - 1, &mut ends);
        Some(format!("…{}", &text[rest..start]))
    })
}

/// Prefer whitespace breaks and wrap long words at grapheme boundaries.
pub(super) fn wrap_words(text: &str, columns: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut width = 0;
    let mut graphemes = text.grapheme_indices(true);
    let is_blank = |grapheme: &str| grapheme.chars().all(char::is_whitespace);
    while let Some((start, _)) = graphemes.find(|&(_, grapheme)| !is_blank(grapheme)) {
        let end = graphemes
            .find(|&(_, grapheme)| is_blank(grapheme))
            .map_or(text.len(), |(end, _)| end);
        let word = &text[start..end];
        // The inserted ASCII space breaks width context between words.
        let cells = display_width(word);
        if !line.is_empty() && width + 1 + cells > columns {
            out.extend(wrap_line(&line, columns));
            line.clear();
            width = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            width += 1;
        }
        line.push_str(word);
        width += cells;
    }
    if !line.is_empty() {
        out.extend(wrap_line(&line, columns));
    }
    out
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
