use std::io::IsTerminal;

/// ANSI styling, suppressed when output is redirected or `NO_COLOR` is set.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    enabled: bool,
}

impl Style {
    pub fn for_stdout() -> Self {
        Self {
            enabled: std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal(),
        }
    }

    pub const fn plain() -> Self {
        Self { enabled: false }
    }

    #[cfg(test)]
    const fn coloured() -> Self {
        Self { enabled: true }
    }

    pub fn heading(self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn name(self, text: &str) -> String {
        self.paint("36", text)
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        } else {
            text.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_styling_leaves_text_alone() {
        assert_eq!(Style::plain().heading("Images"), "Images");
        assert_eq!(Style::plain().name("debian"), "debian");
    }

    #[test]
    fn coloured_styling_wraps_and_resets() {
        let painted = Style::coloured().name("debian");
        assert!(painted.starts_with('\u{1b}'), "{painted:?}");
        assert!(painted.ends_with("\u{1b}[0m"), "{painted:?}");
        assert!(painted.contains("debian"));
    }

    #[test]
    fn every_style_resets_what_it_sets() {
        let style = Style::coloured();
        for painted in [style.heading("x"), style.dim("x"), style.name("x")] {
            assert_eq!(painted.matches("\u{1b}[0m").count(), 1, "{painted:?}");
        }
    }
}
