/// Left-aligned columns, sized to their contents.
pub fn render(headings: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let mut widths: Vec<usize> = headings
        .iter()
        .map(|heading| heading.chars().count())
        .collect();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            let width = cell.chars().count();
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
                let padding = widths.get(column).copied().unwrap_or(0) - cell.chars().count();
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

    #[test]
    fn wide_characters_are_counted_as_characters_not_bytes() {
        let output = render(&["NAME", "TAG"], &[row(&["naïve", "x"])]);
        assert_eq!(output[1], "naïve  x");
    }
}
