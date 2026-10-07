//! Web links in terminal output: find the `http(s)://` URL under a cell, so a
//! front-end can open it (the desktop's Cmd-click, Ctrl-click elsewhere).
//!
//! Plain text only: the URL is read off the visible cells, the way iTerm or
//! VS Code's terminal find one, not from OSC 8 hyperlink escapes.
//!
//! A URL longer than the terminal is wide wraps onto the next row with no
//! newline, so rows are read as one line while each one runs to the last
//! column. Only ASCII URL characters belong to a link: a box border (`│`), a
//! space or a quote ends it, and trailing sentence punctuation and an
//! unbalanced closing bracket (a Markdown `[text](url)`) are left out.
//!
//! Only `http` and `https` are found, so whatever an agent prints, a click can
//! only ever hand a web page to the browser.

use super::{CellWidth, GridView};

/// One row's part of a link, for underlining it: `cols` cells from `col`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkSegment {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
}

/// A link found in the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The URL, as printed.
    pub url: String,
    /// Where it is on screen, one segment per row it spans, top to bottom.
    pub segments: Vec<LinkSegment>,
}

impl Link {
    /// Whether the link covers cell `(row, col)`.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.segments
            .iter()
            .any(|s| s.row == row && (s.col..s.col + s.cols).contains(&col))
    }
}

/// How many rows above and below a wrapped link may reach.
const MAX_WRAPPED_ROWS: u16 = 8;

/// The link under visible cell `(row, col)`, if there is one.
pub fn link_at(grid: &dyn GridView, row: u16, col: u16) -> Option<Link> {
    let (rows, cols) = grid.size();
    if row >= rows || col >= cols {
        return None;
    }
    // The run of rows that wrap into each other around `row`.
    let mut first = row;
    while first > 0 && row - first < MAX_WRAPPED_ROWS && runs_to_the_edge(grid, first - 1, cols) {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < rows && last - row < MAX_WRAPPED_ROWS && runs_to_the_edge(grid, last, cols) {
        last += 1;
    }

    // The line as one string, each character remembering its cell.
    let mut chars: Vec<(char, u16, u16, u16)> = Vec::new(); // (char, row, col, width)
    for r in first..=last {
        grid.visit_row(r, &mut |c, cell| {
            let width = match cell.width {
                CellWidth::WideContinuation => return,
                CellWidth::Wide => 2,
                CellWidth::Narrow => 1,
            };
            // A cell holding a grapheme of several characters is never part of
            // a URL; one stand-in keeps the cell count right.
            let mut graphemes = cell.text.chars();
            let ch = match (graphemes.next(), graphemes.next()) {
                (None, _) => ' ',
                (Some(ch), None) => ch,
                (Some(_), Some(_)) => '\u{fffd}',
            };
            chars.push((ch, r, c, width));
        });
    }

    let target = chars
        .iter()
        .position(|&(_, r, c, w)| r == row && (c..c + w).contains(&col))?;
    let text: Vec<char> = chars.iter().map(|&(ch, ..)| ch).collect();
    let (start, end) = url_around(&text, target)?;

    let mut segments: Vec<LinkSegment> = Vec::new();
    for &(_, r, c, w) in &chars[start..end] {
        match segments.last_mut() {
            Some(seg) if seg.row == r => seg.cols = c + w - seg.col,
            _ => segments.push(LinkSegment {
                row: r,
                col: c,
                cols: w,
            }),
        }
    }
    Some(Link {
        url: text[start..end].iter().collect(),
        segments,
    })
}

/// Whether `row`'s last column holds a printed character: the row ran out of
/// width, so the next one continues it.
fn runs_to_the_edge(grid: &dyn GridView, row: u16, cols: u16) -> bool {
    let mut full = false;
    grid.visit_row(row, &mut |c, cell| {
        if c + 1 == cols {
            full = match cell.width {
                CellWidth::WideContinuation => true,
                _ => !cell.text.trim().is_empty(),
            };
        }
    });
    full
}

/// The `[start, end)` of the URL in `text` that covers index `at`.
fn url_around(text: &[char], at: usize) -> Option<(usize, usize)> {
    if !is_url_char(text[at]) {
        return None;
    }
    // The run of URL characters around `at`; the URL starts at a scheme in it.
    let mut run_start = at;
    while run_start > 0 && is_url_char(text[run_start - 1]) {
        run_start -= 1;
    }
    let mut run_end = at + 1;
    while run_end < text.len() && is_url_char(text[run_end]) {
        run_end += 1;
    }
    let run: String = text[run_start..run_end].iter().collect();
    let lower = run.to_ascii_lowercase();
    // Every scheme in the run starts a URL that ends where the next begins.
    let mut starts: Vec<usize> = ["http://", "https://"]
        .iter()
        .flat_map(|scheme| lower.match_indices(scheme).map(|(i, _)| i))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    // The run is ASCII, so its byte offsets are character offsets.
    for (n, &from) in starts.iter().enumerate() {
        let to = starts.get(n + 1).copied().unwrap_or(run.len());
        let to = from + trim_url_end(&run[from..to]);
        if (run_start + from..run_start + to).contains(&at) {
            let scheme = if lower[from..].starts_with("https://") {
                8
            } else {
                7
            };
            return (to > from + scheme).then_some((run_start + from, run_start + to));
        }
    }
    None
}

/// The length of `url` without what reads as the sentence around it: trailing
/// punctuation, and closing brackets that open nowhere in the URL.
fn trim_url_end(url: &str) -> usize {
    let mut end = url.len();
    loop {
        let Some(last) = url[..end].chars().last() else {
            return end;
        };
        let unbalanced =
            |open: char| url[..end].matches(open).count() < url[..end].matches(last).count();
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= 1;
    }
}

/// A character a URL may contain: printable ASCII that is not a space, a
/// quote, or a bracket URLs never carry bare.
fn is_url_char(ch: char) -> bool {
    ch.is_ascii_graphic() && !matches!(ch, '"' | '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeGrid;

    fn grid(rows: u16, cols: u16, lines: &[&str]) -> FakeGrid {
        let mut g = FakeGrid::new(rows, cols);
        for (r, line) in lines.iter().enumerate() {
            g.set_text(r as u16, 0, line);
        }
        g
    }

    fn url_at(g: &FakeGrid, row: u16, col: u16) -> Option<String> {
        link_at(g, row, col).map(|l| l.url)
    }

    #[test]
    fn a_url_is_found_from_any_of_its_cells_and_nowhere_else() {
        let g = grid(2, 60, &["See https://example.com/a?b=1#c for more."]);
        let link = link_at(&g, 0, 4).unwrap();
        assert_eq!(link.url, "https://example.com/a?b=1#c");
        assert_eq!(
            link.segments,
            [LinkSegment {
                row: 0,
                col: 4,
                cols: 27
            }]
        );
        assert_eq!(
            url_at(&g, 0, 30).as_deref(),
            Some("https://example.com/a?b=1#c")
        );
        assert_eq!(url_at(&g, 0, 3), None, "the space before it");
        assert_eq!(url_at(&g, 0, 32), None, "the word after it");
        assert_eq!(url_at(&g, 1, 0), None, "an empty row");
    }

    #[test]
    fn sentence_punctuation_and_markdown_brackets_are_not_part_of_it() {
        let g = grid(
            4,
            70,
            &[
                "Opened https://github.com/o/r/pull/96.",
                "[the PR](https://github.com/o/r/pull/96), and (https://x.io/a_(b)).",
                "'https://x.io/q' and <https://x.io/z>",
                "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            ],
        );
        assert_eq!(
            url_at(&g, 0, 10).as_deref(),
            Some("https://github.com/o/r/pull/96")
        );
        assert_eq!(
            url_at(&g, 1, 12).as_deref(),
            Some("https://github.com/o/r/pull/96")
        );
        assert_eq!(url_at(&g, 1, 50).as_deref(), Some("https://x.io/a_(b)"));
        assert_eq!(url_at(&g, 2, 3).as_deref(), Some("https://x.io/q"));
        assert_eq!(url_at(&g, 2, 25).as_deref(), Some("https://x.io/z"));
        assert_eq!(
            url_at(&g, 3, 0).as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        );
    }

    #[test]
    fn a_url_wrapped_onto_the_next_rows_is_read_whole() {
        // 20 columns: the URL fills row 0, all of row 1 and part of row 2.
        let g = grid(
            4,
            20,
            &[
                "Go: https://example.",
                "com/very/long/path/t",
                "o/page done",
                "next",
            ],
        );
        let link = link_at(&g, 1, 5).unwrap();
        assert_eq!(link.url, "https://example.com/very/long/path/to/page");
        assert_eq!(
            link.segments,
            [
                LinkSegment {
                    row: 0,
                    col: 4,
                    cols: 16
                },
                LinkSegment {
                    row: 1,
                    col: 0,
                    cols: 20
                },
                LinkSegment {
                    row: 2,
                    col: 0,
                    cols: 6
                },
            ]
        );
        assert_eq!(url_at(&g, 0, 4), Some(link.url.clone()));
        assert_eq!(url_at(&g, 2, 5), Some(link.url));
        assert_eq!(url_at(&g, 2, 6), None, "the space after it");
        assert_eq!(url_at(&g, 3, 0), None, "row 2 did not run to the edge");
    }

    #[test]
    fn a_box_border_ends_a_link_and_two_links_stay_apart() {
        let g = grid(
            2,
            40,
            &["│ https://a.io/x │", "https://a.io/1,https://b.io/2"],
        );
        assert_eq!(url_at(&g, 0, 5).as_deref(), Some("https://a.io/x"));
        assert_eq!(url_at(&g, 1, 2).as_deref(), Some("https://a.io/1"));
        assert_eq!(url_at(&g, 1, 20).as_deref(), Some("https://b.io/2"));
    }

    #[test]
    fn only_web_links_are_found() {
        let g = grid(
            3,
            40,
            &[
                "file:///etc/passwd javascript:alert(1)",
                "https:// ftp://x.io",
                "HTTPS://X.IO/A",
            ],
        );
        assert_eq!(url_at(&g, 0, 3), None);
        assert_eq!(url_at(&g, 0, 25), None);
        assert_eq!(url_at(&g, 1, 2), None, "a scheme with nothing after it");
        assert_eq!(url_at(&g, 1, 12), None);
        assert_eq!(url_at(&g, 2, 3).as_deref(), Some("HTTPS://X.IO/A"));
    }

    #[test]
    fn a_cell_outside_the_grid_has_no_link() {
        let g = grid(1, 10, &["https://a"]);
        assert_eq!(url_at(&g, 0, 10), None);
        assert_eq!(url_at(&g, 1, 0), None);
    }
}
