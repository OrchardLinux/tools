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
    /// Something that is wrong.
    Bad,
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

    pub fn styled(text: &'a str, style: Style) -> Self {
        Self { text, style }
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

/// The terminal's width in columns; None when stdout is not a terminal,
/// where a table keeps its full width.
fn terminal_columns() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        let got = unsafe { libc::ioctl(std::io::stdout().as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
        if got == 0 && size.ws_col > 0 {
            return Some(size.ws_col as usize);
        }
    }
    None
}

/// Column headings in the first row, followed by data rows.
pub fn render(headers: &[&str], rows: &[Vec<Cell<'_>>]) -> String {
    let mut cells = vec![headers.iter().map(|s| Cell::plain(s)).collect()];
    cells.extend_from_slice(rows);
    render_cells(&cells, styling_enabled(), true, terminal_columns())
}

/// The same for a list of short rows: one rule, under the headings.
pub fn render_list(headers: &[&str], rows: &[Vec<Cell<'_>>]) -> String {
    let mut cells = vec![headers.iter().map(|s| Cell::plain(s)).collect()];
    cells.extend_from_slice(rows);
    render_cells(&cells, styling_enabled(), false, terminal_columns())
}

/// The narrowest a column is made before the table gives up its box.
const NARROWEST: usize = 6;

/// `text` in pieces of at most `width` columns, broken after a space, `/`
/// or `-` where there is one, else anywhere.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = text;
    while UnicodeWidthStr::width(rest) > width {
        let (mut end, mut used, mut soft) = (0, 0, None);
        for (at, c) in rest.char_indices() {
            let wide = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + wide > width {
                break;
            }
            used += wide;
            end = at + c.len_utf8();
            if matches!(c, ' ' | '/' | '-') {
                soft = Some(end);
            }
        }
        // One character wider than the column still has to go somewhere.
        let end = match soft.unwrap_or(end) {
            0 => rest.chars().next().map_or(rest.len(), char::len_utf8),
            end => end,
        };
        lines.push(rest[..end].trim_end().to_owned());
        rest = rest[end..].trim_start();
    }
    lines.push(rest.to_owned());
    lines
}

fn render_cells(rows: &[Vec<Cell<'_>>], styled: bool, ruled: bool, columns: Option<usize>) -> String {
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
    // Too wide for the terminal: the widest columns give way and their
    // text wraps inside them. Where even that does not fit, each row is
    // written as lines of its own, heading and value.
    if let Some(columns) = columns {
        let total = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * widths.len() + 1;
        while total(&widths) > columns {
            let widest = (0..widths.len()).max_by_key(|i| widths[*i]).expect("a table has columns");
            if widths[widest] <= NARROWEST {
                return stacked(&rows, styled);
            }
            widths[widest] -= 1;
        }
    }
    let rule = |left, mid, right| {
        let spans: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{left}{}{right}", spans.join(mid))
    };
    let mut lines = vec![rule("┌", "┬", "┐")];
    for (row_index, row) in rows.iter().enumerate() {
        if row_index == 1 || (ruled && row_index > 0) {
            lines.push(rule("├", "┼", "┤"));
        }
        let pieces: Vec<Vec<String>> = row.iter().zip(&widths).map(|((cell, _), width)| wrapped(cell, *width)).collect();
        for n in 0..pieces.iter().map(Vec::len).max().unwrap_or(1) {
            let mut line = String::from("│");
            for (((_, style), width), pieces) in row.iter().zip(&widths).zip(&pieces) {
                let piece = pieces.get(n).map_or("", String::as_str);
                let code = code(*style, styled, row_index == 0);
                line.push(' ');
                line.push_str(code.unwrap_or(""));
                line.push_str(piece);
                if code.is_some() {
                    line.push_str("\x1b[0m");
                }
                line.push_str(&" ".repeat(width.saturating_sub(UnicodeWidthStr::width(piece))));
                line.push_str(" │");
            }
            lines.push(line);
        }
    }
    lines.push(rule("└", "┴", "┘"));
    lines.join("\n")
}

fn code(style: Style, styled: bool, heading: bool) -> Option<&'static str> {
    match style {
        _ if !styled => None,
        _ if heading => Some("\x1b[1m"),
        Style::Plain => None,
        Style::Dim => Some("\x1b[2m"),
        Style::Number => Some("\x1b[33m"),
        Style::Mode => Some("\x1b[34m"),
        Style::Board => Some("\x1b[32m"),
        Style::Bad => Some("\x1b[31m"),
    }
}

/// A table too wide for any box: each row as its headings and values, one
/// a line, with an empty line between rows.
fn stacked(rows: &[Vec<(String, Style)>], styled: bool) -> String {
    let headings = &rows[0];
    let label = headings.iter().map(|(h, _)| UnicodeWidthStr::width(h.as_str())).max().unwrap_or(0);
    let mut blocks = Vec::new();
    for row in &rows[1..] {
        let mut lines = Vec::new();
        for ((heading, _), (cell, style)) in headings.iter().zip(row) {
            let pad = " ".repeat(label - UnicodeWidthStr::width(heading.as_str()));
            let (bold, off) = if styled { ("\x1b[1m", "\x1b[0m") } else { ("", "") };
            let value = match code(*style, styled, false) {
                Some(code) => format!("{code}{cell}\x1b[0m"),
                None => cell.clone(),
            };
            lines.push(format!("{bold}{heading}{off}{pad}  {value}"));
        }
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
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
            render_cells(rows, false, true, None),
            "┌──────┬───────┐\n│ Name │ Value │\n├──────┼───────┤\n│ 猫   │ 1     │\n├──────┼───────┤\n│ abc  │ 2 3   │\n└──────┴───────┘"
        );
    }

    #[test]
    fn a_list_has_one_rule_under_its_headings() {
        let rows = &[
            vec![Cell::plain("Check"), Cell::plain("State")],
            vec![Cell::plain("git"), Cell::plain("ready")],
            vec![Cell::plain("Podman"), Cell::styled("stopped", Style::Bad)],
        ];
        assert_eq!(
            render_cells(rows, false, false, None),
            "┌────────┬─────────┐\n│ Check  │ State   │\n├────────┼─────────┤\n│ git    │ ready   │\n│ Podman │ stopped │\n└────────┴─────────┘"
        );
        assert!(render_cells(rows, true, false, None).contains("\x1b[31mstopped\x1b[0m"));
    }

    fn devices() -> Vec<Vec<Cell<'static>>> {
        vec![
            vec![Cell::plain("Mode"), Cell::plain("Location"), Cell::plain("Serial port"), Cell::plain("Product")],
            vec![Cell::mode("m1n1"), Cell::number("0x2100000"), Cell::plain("/dev/cu.usbmodemDMPW59SKJF8N1"), Cell::plain("m1n1 uartproxy c9ef56f-dirty")],
        ]
    }

    #[test]
    fn a_table_wider_than_the_terminal_wraps_inside_its_columns() {
        let table = render_cells(&devices(), false, true, Some(60));
        for line in table.lines() {
            assert!(UnicodeWidthStr::width(line) <= 60, "{line}");
        }
        assert_eq!(
            table,
            "┌──────┬───────────┬───────────────────┬───────────────────┐\n\
             │ Mode │ Location  │ Serial port       │ Product           │\n\
             ├──────┼───────────┼───────────────────┼───────────────────┤\n\
             │ m1n1 │ 0x2100000 │ /dev/             │ m1n1 uartproxy    │\n\
             │      │           │ cu.usbmodemDMPW59 │ c9ef56f-dirty     │\n\
             │      │           │ SKJF8N1           │                   │\n\
             └──────┴───────────┴───────────────────┴───────────────────┘"
        );
        // As wide as it needs: nothing changes.
        assert_eq!(render_cells(&devices(), false, true, Some(200)), render_cells(&devices(), false, true, None));
    }

    #[test]
    fn a_table_that_fits_no_box_is_written_row_by_row() {
        assert_eq!(
            render_cells(&devices(), false, true, Some(30)),
            "Mode         m1n1\nLocation     0x2100000\nSerial port  /dev/cu.usbmodemDMPW59SKJF8N1\nProduct      m1n1 uartproxy c9ef56f-dirty"
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
        let styled = render_cells(rows, true, true, None);
        assert!(styled.contains("\x1b[1mMode\x1b[0m"));
        assert!(styled.contains("\x1b[34mdfu\x1b[0m"));
        assert!(styled.contains("\x1b[33m0x123\x1b[0m"));
        assert!(styled.contains("\x1b[2m—\x1b[0m"));
        assert!(styled.contains("\x1b[32mj1\x1b[0m"));
        let plain = render_cells(rows, false, true, None);
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("│ dfu  │ 0x123    │"));
    }
}
