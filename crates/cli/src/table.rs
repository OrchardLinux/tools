//! Boxed tables for what people read.
use std::io::IsTerminal;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Dim,
    Number,
    Mode,
    Board,
}

#[derive(Clone, Copy)]
pub struct Cell<'a> {
    text: &'a str,
    style: Style,
}

impl<'a> Cell<'a> {
    pub fn plain(text: &'a str) -> Self {
        Self {
            text,
            style: Style::Plain,
        }
    }

    pub fn number(text: &'a str) -> Self {
        Self {
            text,
            style: Style::Number,
        }
    }

    pub fn mode(text: &'a str) -> Self {
        Self {
            text,
            style: Style::Mode,
        }
    }

    /// A value in `style`, or a dimmed dash when there is none.
    pub fn optional(value: Option<&'a str>, style: Style) -> Self {
        match value {
            Some(text) => Self { text, style },
            None => Self {
                text: "—",
                style: Style::Dim,
            },
        }
    }
}

/// Colors only on a terminal, and not when `NO_COLOR` is set.
fn styling_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Column headings in the first row, followed by data rows.
pub fn render(headers: &[&str], rows: &[Vec<Cell<'_>>]) -> String {
    let mut cells = vec![headers.iter().map(|s| Cell::plain(s)).collect()];
    cells.extend_from_slice(rows);
    render_cells(&cells, styling_enabled())
}

fn render_cells(rows: &[Vec<Cell<'_>>], styled: bool) -> String {
    // Strings from a device must not move the cursor or break lines.
    let rows: Vec<Vec<(String, Style)>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let text = cell
                        .text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    (text, cell.style)
                })
                .collect()
        })
        .collect();
    let mut widths = vec![0; rows[0].len()];
    for row in &rows {
        for (width, (cell, _)) in widths.iter_mut().zip(row) {
            *width = (*width).max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let rule = |left, mid, right| {
        let spans: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{left}{}{right}", spans.join(mid))
    };
    let mut lines = vec![rule("┌", "┬", "┐")];
    for (row_index, row) in rows.iter().enumerate() {
        if row_index > 0 {
            lines.push(rule("├", "┼", "┤"));
        }
        let mut line = String::from("│");
        for ((cell, style), width) in row.iter().zip(&widths) {
            let code = match style {
                _ if !styled => None,
                _ if row_index == 0 => Some("\x1b[1m"),
                Style::Plain => None,
                Style::Dim => Some("\x1b[2m"),
                Style::Number => Some("\x1b[33m"),
                Style::Mode => Some("\x1b[34m"),
                Style::Board => Some("\x1b[32m"),
            };
            line.push(' ');
            line.push_str(code.unwrap_or(""));
            line.push_str(cell);
            if code.is_some() {
                line.push_str("\x1b[0m");
            }
            line.push_str(&" ".repeat(width - UnicodeWidthStr::width(cell.as_str())));
            line.push_str(" │");
        }
        lines.push(line);
    }
    lines.push(rule("└", "┴", "┘"));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_and_control_characters() {
        let rows = &[
            vec![Cell::plain("Name"), Cell::plain("Value")],
            vec![Cell::plain("猫"), Cell::plain("1")],
            vec![Cell::plain("abc"), Cell::plain("2\n3")],
        ];
        assert_eq!(
            render_cells(rows, false),
            "┌──────┬───────┐\n│ Name │ Value │\n├──────┼───────┤\n│ 猫   │ 1     │\n├──────┼───────┤\n│ abc  │ 2 3   │\n└──────┴───────┘"
        );
    }

    #[test]
    fn styles_color_the_text_and_leave_the_widths_alone() {
        let rows = &[
            vec![Cell::plain("Mode"), Cell::plain("Location")],
            vec![Cell::mode("dfu"), Cell::number("0x123")],
            vec![
                Cell::optional(None, Style::Mode),
                Cell::optional(Some("j1"), Style::Board),
            ],
        ];
        let styled = render_cells(rows, true);
        assert!(styled.contains("\x1b[1mMode\x1b[0m"));
        assert!(styled.contains("\x1b[34mdfu\x1b[0m"));
        assert!(styled.contains("\x1b[33m0x123\x1b[0m"));
        assert!(styled.contains("\x1b[2m—\x1b[0m"));
        assert!(styled.contains("\x1b[32mj1\x1b[0m"));
        let plain = render_cells(rows, false);
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("│ dfu  │ 0x123    │"));
    }
}
