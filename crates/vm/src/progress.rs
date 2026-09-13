use crate::style::Style;
use crate::units::human;
use std::io::{IsTerminal, Write};
use vm_core::import;
use vm_core::machines::Event;
use vm_core::store::Progress;

/// A single rewritten line on a terminal, a quiet counter anywhere else.
pub struct Bar {
    label: String,
    interactive: bool,
    drawn: bool,
    last_shown: u64,
    last_percent: Option<u8>,
}

const STEP: u64 = 1 << 20;

impl Bar {
    pub fn new(label: &str, wanted: bool) -> Self {
        Self {
            label: label.to_owned(),
            interactive: wanted && std::io::stderr().is_terminal(),
            drawn: false,
            last_shown: 0,
            last_percent: None,
        }
    }

    /// Work measured in bytes, as a download is.
    pub fn update(&mut self, progress: Progress) {
        if !self.interactive || progress.received < self.last_shown.saturating_add(STEP) {
            return;
        }
        self.last_shown = progress.received;
        self.draw(&describe(progress));
    }

    /// Labels the current pass, restarting the figure when the label changes.
    pub fn naming(&mut self, label: &str) {
        if self.label != label {
            label.clone_into(&mut self.label);
            self.last_percent = None;
        }
    }

    /// Reports progress known only as a percentage.
    pub fn portion(&mut self, percent: u8) {
        if !self.interactive || self.last_percent == Some(percent) {
            return;
        }
        self.last_percent = Some(percent);
        self.draw(&format!("{}  {percent:>3}%", blocks(percent)));
    }

    fn draw(&mut self, text: &str) {
        self.drawn = true;
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r{} {text}", self.label);
        let _ = stderr.flush();
    }

    /// Removes the progress line so the report that follows starts clean.
    pub fn clear(&self) {
        if self.interactive && self.drawn {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r\u{1b}[K");
            let _ = stderr.flush();
        }
    }
}

/// A bar of a fixed width, so that the line it sits on never reflows.
const WIDTH: usize = 24;

fn blocks(percent: u8) -> String {
    let filled = (usize::from(percent.min(100)) * WIDTH).div_euclid(100);
    format!(
        "[{}{}]",
        "#".repeat(filled),
        " ".repeat(WIDTH.saturating_sub(filled))
    )
}

fn describe(progress: Progress) -> String {
    progress.total.map_or_else(
        || human(progress.received),
        |total| {
            let percent = progress
                .received
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100);
            format!(
                "{percent:>3}%  {} of {}",
                human(progress.received),
                human(total)
            )
        },
    )
}

/// Draws what an operation reports: a bar for its passes, a line for what it starts.
pub fn observer(text: bool, style: Style) -> impl FnMut(Event) {
    let mut bar = Bar::new("", text);
    move |event| match event {
        Event::Fetching(url) => {
            bar.clear();
            if text {
                eprintln!("Fetching {}", style.name(&url));
            }
        }
        Event::Received(progress) => {
            bar.naming("  ");
            bar.update(progress);
        }
        Event::Converting(percent) => {
            bar.naming("  converting");
            bar.portion(percent);
        }
        Event::Cloning(stage, percent) => {
            bar.naming(&pass(stage.label()));
            bar.portion(percent);
        }
        Event::Shutdown { name, seconds } => {
            bar.clear();
            if text {
                eprintln!("Waiting for {name} to shut down, up to {seconds}s");
            }
        }
        Event::Exporting(progress) => {
            bar.naming("  exporting ");
            bar.update(progress);
        }
        Event::Importing {
            from_url,
            event: import::Event::Receiving(progress),
        } => {
            bar.naming(&pass(if from_url { "fetching" } else { "reading" }));
            bar.update(progress);
        }
        Event::Importing {
            event: import::Event::Converting(percent),
            ..
        } => {
            bar.naming(&pass("converting"));
            bar.portion(percent);
        }
        Event::Importing {
            event: import::Event::Hashing(percent),
            ..
        } => {
            bar.naming(&pass("verifying"));
            bar.portion(percent);
        }
    }
}

/// A pass label padded so the figures beside it stay put.
fn pass(name: &str) -> String {
    format!("  {name:<10}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_total_is_reported_as_a_percentage() {
        let text = describe(Progress {
            received: 512,
            total: Some(1024),
        });
        assert!(text.contains("50%"), "{text}");
        assert!(text.contains("512 B of 1.0 KiB"), "{text}");
    }

    #[test]
    fn an_unknown_total_reports_only_what_has_arrived() {
        let text = describe(Progress {
            received: 2048,
            total: None,
        });
        assert_eq!(text, "2.0 KiB");
    }

    #[test]
    fn a_zero_total_does_not_divide_by_zero() {
        assert!(
            describe(Progress {
                received: 0,
                total: Some(0)
            })
            .contains("100%")
        );
    }

    #[test]
    fn naming_a_new_pass_starts_the_figure_again() {
        let mut bar = Bar::new("first", false);
        bar.last_percent = Some(100);
        bar.naming("first");
        assert_eq!(bar.last_percent, Some(100), "an unchanged name changed it");
        bar.naming("second");
        assert_eq!(bar.last_percent, None);
        assert_eq!(bar.label, "second");
    }

    #[test]
    fn a_bar_fills_from_empty_to_full() {
        assert_eq!(blocks(0), format!("[{}]", " ".repeat(WIDTH)));
        assert_eq!(blocks(100), format!("[{}]", "#".repeat(WIDTH)));
        assert_eq!(blocks(50).matches('#').count(), WIDTH / 2);
    }

    #[test]
    fn every_bar_is_the_same_width() {
        for percent in 0..=100 {
            assert_eq!(blocks(percent).chars().count(), WIDTH + 2, "{percent}");
        }
    }

    /// Nothing can be more than finished, whatever it says.
    #[test]
    fn a_percentage_beyond_a_hundred_does_not_overrun() {
        assert_eq!(blocks(200), blocks(100));
    }
}
