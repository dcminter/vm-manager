//! Sizes as a person reads them. Documents carry the figures themselves; this
//! is only for the text output.

const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

pub fn human(bytes: u64) -> String {
    let (size, unit) = scale(bytes);
    render(size, unit, bytes)
}

/// Two figures against one scale, as in `1.4 of 12.0 GiB`.
///
/// The larger sets the unit so that the pair can be compared at a glance;
/// written each in its own, `900.0 MiB of 12.0 GiB` makes the reader do the
/// arithmetic the units were supposed to save them.
pub fn human_pair(used: u64, total: u64) -> String {
    let (_, unit) = scale(total);
    let divisor = divisor(unit);
    #[expect(clippy::cast_precision_loss, reason = "a figure for a person to read")]
    let at = |bytes: u64| bytes as f64 / divisor;
    if unit == 0 {
        return format!("{used} of {total} B");
    }
    // Far enough apart, the shared unit rounds the smaller figure away and
    // says nothing at all. Something is better read in its own unit than
    // reported as none of the other's.
    if used > 0 && at(used) < 0.05 {
        return format!("{} of {}", human(used), human(total));
    }
    format!(
        "{:.1} of {:.1} {}",
        at(used),
        at(total),
        UNITS.get(unit).copied().unwrap_or("B")
    )
}

/// The largest unit the figure is still at least one of, and how many.
fn scale(bytes: u64) -> (f64, usize) {
    #[expect(clippy::cast_precision_loss, reason = "a figure for a person to read")]
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    (size, unit)
}

const fn divisor(unit: usize) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "1024 to a small power")]
    let held = (1_u64 << (10 * unit)) as f64;
    held
}

/// Bytes are whole things and a fraction of one means nothing.
fn render(size: f64, unit: usize, bytes: u64) -> String {
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
    fn a_pair_is_written_once_against_the_unit_of_the_larger() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(human_pair(3 * gib / 2, 12 * gib), "1.5 of 12.0 GiB");
    }

    /// The point of the shared unit: the smaller figure keeps its proportion
    /// rather than being promoted into a unit of its own.
    #[test]
    fn a_much_smaller_figure_is_written_in_the_larger_unit() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(human_pair(900 * 1024 * 1024, 12 * gib), "0.9 of 12.0 GiB");
        assert_eq!(human_pair(0, 2 * gib), "0.0 of 2.0 GiB");
    }

    /// A new machine has written a few megabytes to a disk that spans tens of
    /// gigabytes, and `0.0 of 40.0 GiB` would be a figure that says nothing.
    #[test]
    fn a_figure_the_shared_unit_would_round_away_keeps_its_own() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(
            human_pair(18 * 1024 * 1024, 12 * gib),
            "18.0 MiB of 12.0 GiB"
        );
    }

    /// Nothing written is nothing, and it reads better against the total than
    /// alone.
    #[test]
    fn a_figure_that_really_is_nothing_keeps_the_shared_unit() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(human_pair(0, 12 * gib), "0.0 of 12.0 GiB");
    }

    #[test]
    fn a_pair_of_small_figures_stays_in_whole_bytes() {
        assert_eq!(human_pair(12, 500), "12 of 500 B");
    }
}
