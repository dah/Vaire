use super::*;

pub(in crate::tui) struct WrappedText {
    pub(in crate::tui) text: String,
    pub(in crate::tui) rows: usize,
    pub(in crate::tui) tail_column: usize,
}

pub(in crate::tui) struct ParagraphWindow {
    pub(in crate::tui) lines: Vec<Line<'static>>,
    pub(in crate::tui) scroll: u16,
}

pub(in crate::tui) fn wrapped_line_rows(line: &Line<'static>, width: u16) -> usize {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .max(1)
}

/// Selects only the logical lines needed for the requested visual viewport.
///
/// Ratatui's paragraph scroll offset is a `u16`. Retained transcript state can contain more than
/// 65,535 explicit newline rows even when its byte size is bounded, so clamping the global offset
/// would render a stale middle section instead of the requested tail. Counting first and then
/// retaining the intersecting logical-line range keeps the residual offset local to one bounded
/// line while preserving Ratatui's native word wrapping and span styles.
pub(in crate::tui) fn paragraph_window(
    lines: Vec<Line<'static>>,
    width: u16,
    height: u16,
    scroll_from_bottom: usize,
) -> ParagraphWindow {
    if lines.is_empty() {
        return ParagraphWindow { lines, scroll: 0 };
    }

    let row_counts = lines
        .iter()
        .map(|line| wrapped_line_rows(line, width))
        .collect::<Vec<_>>();
    let total_rows = row_counts
        .iter()
        .fold(0usize, |total, rows| total.saturating_add(*rows));
    let maximum_top = total_rows.saturating_sub(usize::from(height));
    let target_top = maximum_top.saturating_sub(scroll_from_bottom.min(maximum_top));

    let mut rows_before = 0usize;
    let mut start = 0usize;
    while start < row_counts.len() && rows_before.saturating_add(row_counts[start]) <= target_top {
        rows_before = rows_before.saturating_add(row_counts[start]);
        start += 1;
    }
    if start == lines.len() {
        start = lines.len() - 1;
        rows_before = rows_before.saturating_sub(row_counts[start]);
    }

    let residual = target_top.saturating_sub(rows_before);
    // Reducer retention bounds keep every individual transcript line below this limit at the
    // minimum supported pane width; reasoning entries have a tighter 32-KiB bound. The windowing
    // above removes the unbounded aggregate offset.
    let scroll = u16::try_from(residual).unwrap_or(u16::MAX);
    let required_rows = residual.saturating_add(usize::from(height));
    let mut selected_rows = 0usize;
    let mut end = start;
    while end < lines.len() && selected_rows < required_rows {
        selected_rows = selected_rows.saturating_add(row_counts[end]);
        end += 1;
    }

    ParagraphWindow {
        lines: lines[start..end].to_vec(),
        scroll,
    }
}

impl WrappedText {
    pub(in crate::tui) fn retain_tail_rows(mut self, maximum: usize) -> Self {
        let maximum = maximum.max(1);
        if self.rows <= maximum {
            return self;
        }
        let skip = self.rows - maximum;
        let byte_index = self
            .text
            .match_indices('\n')
            .nth(skip - 1)
            .map_or(self.text.len(), |(index, _)| index + 1);
        self.text = self.text[byte_index..].to_owned();
        self.rows = maximum;
        self
    }
}

pub(in crate::tui) fn wrap_for_display(value: &str, width: u16) -> WrappedText {
    let width = usize::from(width.max(1));
    let mut text = String::with_capacity(value.len());
    let mut row = 0_usize;
    let mut column = 0_usize;
    for grapheme in value.graphemes(true) {
        if grapheme == "\n" {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if column > 0 && column.saturating_add(grapheme_width) > width {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
        }
        text.push_str(grapheme);
        column = column.saturating_add(grapheme_width);
    }
    if column >= width && !text.is_empty() {
        text.push('\n');
        row = row.saturating_add(1);
        column = 0;
    }
    WrappedText {
        text,
        rows: row.saturating_add(1),
        tail_column: column,
    }
}

pub(in crate::tui) fn truncate_for_display(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let content_width = width.saturating_sub(1);
    let mut truncated = String::with_capacity(value.len().min(width));
    let mut used = 0_usize;
    for grapheme in value.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used.saturating_add(grapheme_width) > content_width {
            break;
        }
        truncated.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    truncated.push('…');
    truncated
}
