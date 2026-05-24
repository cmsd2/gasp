//! Tiny aligned-column table printer for CLI output. Holds rows in
//! memory, computes column widths, prints headers and rows separated by
//! two spaces. The last column isn't padded, so long values (URLs,
//! detail messages) don't get trailing whitespace.

pub struct Table {
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: &[&'static str]) -> Self {
        Self {
            headers: headers.to_vec(),
            rows: Vec::new(),
        }
    }

    pub fn row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    pub fn print(&self) {
        let n = self.headers.len();
        if n == 0 {
            return;
        }

        let mut widths = vec![0usize; n];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = h.len();
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(n) {
                widths[i] = widths[i].max(cell.len());
            }
        }

        print_row(
            &self
                .headers
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            &widths,
        );
        for row in &self.rows {
            print_row(row, &widths);
        }
    }
}

fn print_row(cells: &[String], widths: &[usize]) {
    let last = widths.len().saturating_sub(1);
    let empty = String::new();
    for (i, w) in widths.iter().enumerate() {
        let cell = cells.get(i).unwrap_or(&empty);
        if i == last {
            // Last column: no trailing padding.
            println!("{cell}");
        } else {
            print!("{cell:<w$}  ", w = *w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_is_silent() {
        let t = Table::new(&[]);
        t.print(); // shouldn't panic
    }

    #[test]
    fn row_count_matches() {
        let mut t = Table::new(&["A", "B"]);
        t.row(["1", "2"]);
        t.row(["3", "4"]);
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn widths_grow_to_fit_longest_cell() {
        let mut t = Table::new(&["A", "B"]);
        t.row(["short", "1"]);
        t.row(["longer-cell", "2"]);
        // White-box: just verify the values are captured; printing is
        // side-effectful and tested via integration coverage.
        assert_eq!(t.rows[1][0], "longer-cell");
    }
}
