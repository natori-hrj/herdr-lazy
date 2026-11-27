use std::io::{self, Write};

use super::terminal_text::{cell_width, clip};

const GAP: usize = 2;

fn width_up_to(text: &str, limit: usize) -> usize {
    let mut width = 0;
    for ch in text.chars() {
        width += if ch.is_control() { 1 } else { cell_width(ch) };
        if width >= limit {
            return limit;
        }
    }
    width
}

pub(super) struct Columns {
    pub name: usize,
    metadata: usize,
    pub trailing: usize,
}

impl Columns {
    pub fn new<'a>(
        available: usize,
        fields: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
    ) -> Self {
        // Reserve a readable preview when visible text needs it. Short notes and descriptions
        // leave their unused budget available to names and metadata.
        let preview_limit = (available * 2 / 5).min(available.saturating_sub(GAP + 1));
        let (mut name, mut metadata, mut preview) = (0, 0, 0);
        for (label, meta, text) in fields {
            name = name.max(width_up_to(label, available));
            metadata = metadata.max(width_up_to(meta, 12));
            preview = preview.max(width_up_to(text, preview_limit));
        }
        let preview_cost = if preview > 0 { GAP + preview } else { 0 };
        let name = name.min(available.saturating_sub(preview_cost));
        let remaining = available - name;
        if metadata + GAP + preview_cost > remaining {
            metadata = 0;
        }
        let metadata_cost = if metadata > 0 { GAP + metadata } else { 0 };
        Self {
            name,
            metadata,
            trailing: if preview > 0 {
                (remaining - metadata_cost).saturating_sub(GAP)
            } else {
                0
            },
        }
    }

    pub fn write_tail(
        &self,
        out: &mut impl Write,
        mut padding: usize,
        metadata: &str,
        text: &str,
    ) -> io::Result<()> {
        if self.metadata > 0 {
            let (metadata, cells) = clip(metadata, self.metadata);
            padding += GAP;
            write!(out, "{:padding$}{metadata}", "")?;
            padding = self.metadata - cells;
        }
        let (text, _) = clip(text, self.trailing);
        if !text.is_empty() {
            padding += GAP;
            write!(out, "{:padding$}{text}", "")?;
        }
        Ok(())
    }
}
