use std::io::{IsTerminal, Write};
use vm_core::store::Progress;

/// A single rewritten line on a terminal, a quiet counter anywhere else.
pub struct Bar {
    label: String,
    interactive: bool,
    last_shown: u64,
    finished: bool,
}

const STEP: u64 = 1 << 20;

impl Bar {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            interactive: std::io::stderr().is_terminal(),
            last_shown: 0,
            finished: false,
        }
    }

    pub fn update(&mut self, progress: Progress) {
        if !self.interactive || progress.received < self.last_shown.saturating_add(STEP) {
            return;
        }
        self.last_shown = progress.received;
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r{} {}", self.label, describe(progress));
        let _ = stderr.flush();
    }

    pub fn finish(&mut self, message: &str) {
        self.finished = true;
        let mut stderr = std::io::stderr();
        if self.interactive {
            let _ = write!(stderr, "\r\u{1b}[K");
        }
        let _ = writeln!(stderr, "{message}");
    }
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

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{bytes} {name}")
    } else {
        format!("{size:.1} {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_reported_whole_below_a_kibibyte() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
    }

    #[test]
    fn larger_sizes_step_up_through_the_units() {
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1024 * 1024), "1.0 MiB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

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
}
