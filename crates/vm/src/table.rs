/// How wide a cell is on the screen.
///
/// A styled cell carries escape sequences that take no space, and counting
/// them pads the column by the length of the colour rather than the text.
fn width(cell: &str) -> usize {
    let mut count = 0;
    let mut escaped = false;
    for held in cell.chars() {
        if escaped {
            // A CSI sequence runs until a letter; nothing here writes any
            // other kind.
            escaped = !held.is_ascii_alphabetic();
        } else if held == '\u{1b}' {
            escaped = true;
        } else {
            count += 1;
        }
    }
    count
}

/// Left-aligned columns, sized to their contents.
pub fn render(headings: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let mut widths: Vec<usize> = headings.iter().map(|heading| width(heading)).collect();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            let width = width(cell);
            match widths.get_mut(column) {
                Some(held) if *held < width => *held = width,
                Some(_) => {}
                None => widths.push(width),
            }
        }
    }
    let line = |cells: &[String]| {
        let mut text = String::new();
        for (column, cell) in cells.iter().enumerate() {
            let last = column + 1 == cells.len();
            text.push_str(cell);
            if !last {
                let padding = widths
                    .get(column)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(width(cell));
                text.push_str(&" ".repeat(padding + 2));
            }
        }
        text
    };
    let mut output = vec![line(
        &headings.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>(),
    )];
    output.extend(rows.iter().map(|row| line(row)));
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|cell| (*cell).to_owned()).collect()
    }

    #[test]
    fn columns_are_padded_to_the_widest_cell() {
        let rows = vec![
            row(&["debian", "trixie"]),
            row(&["a-much-longer-name", "13"]),
        ];
        let output = render(&["NAME", "TAG"], &rows);
        assert_eq!(output[0], "NAME                TAG");
        assert_eq!(output[1], "debian              trixie");
        assert_eq!(output[2], "a-much-longer-name  13");
    }

    #[test]
    fn the_heading_sets_the_floor_for_its_column() {
        let output = render(&["REPOSITORY", "TAG"], &[row(&["x", "y"])]);
        assert_eq!(output[1], "x           y");
    }

    #[test]
    fn the_last_column_is_not_padded() {
        let output = render(&["A", "B"], &[row(&["x", "y"])]);
        assert!(!output[1].ends_with(' '), "{:?}", output[1]);
    }

    #[test]
    fn headings_alone_still_render() {
        assert_eq!(render(&["NAME"], &[]), vec!["NAME".to_owned()]);
    }

    /// A coloured cell is as wide as the text a reader sees, not as wide as
    /// the escape sequences that colour it.
    #[test]
    fn styling_takes_up_no_width() {
        assert_eq!(width("\u{1b}[36mq1\u{1b}[0m"), 2);
        assert_eq!(width("q1"), 2);
        let coloured = "\u{1b}[36mq1\u{1b}[0m".to_owned();
        let output = render(&["NAME", "IMAGE"], &[vec![coloured, "debian".to_owned()]]);
        assert_eq!(output[0], "NAME  IMAGE");
        // Four spaces: the column is as wide as its heading, and the cell it
        // holds is two characters of text however many bytes it took.
        assert!(
            output[1].ends_with("q1\u{1b}[0m    debian"),
            "{:?}",
            output[1]
        );
    }

    #[test]
    fn wide_characters_are_counted_as_characters_not_bytes() {
        let output = render(&["NAME", "TAG"], &[row(&["naïve", "x"])]);
        assert_eq!(output[1], "naïve  x");
    }
}
