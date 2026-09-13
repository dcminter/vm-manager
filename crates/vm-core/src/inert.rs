//! Replayed console output with nothing left in it that acts on a terminal.

const ESCAPE: u8 = 0x1b;
const BELL: u8 = 0x07;
const CANCEL: u8 = 0x18;
const SUBSTITUTE: u8 = 0x1a;
const DELETE: u8 = 0x7f;

/// The longest colour sequence kept; anything longer is dropped.
const LONGEST_COLOUR: usize = 64;

/// Whether colour sequences survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Keep,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Intermediate,
    Control,
    Text,
    TextEscape,
}

/// Removes queries, cursor movement, resets and control bytes from console output, read by read.
#[derive(Debug, Clone)]
pub struct Filter {
    colour: Colour,
    state: State,
    held: Vec<u8>,
}

impl Filter {
    #[must_use]
    pub const fn new(colour: Colour) -> Self {
        Self {
            colour,
            state: State::Ground,
            held: Vec::new(),
        }
    }

    /// What of `bytes` is kept, given what earlier reads left unfinished.
    pub fn apply(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut kept = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            self.step(byte, &mut kept);
        }
        kept
    }

    fn step(&mut self, byte: u8, kept: &mut Vec<u8>) {
        if matches!(byte, CANCEL | SUBSTITUTE) {
            self.state = State::Ground;
            return;
        }
        match self.state {
            State::Ground => match byte {
                ESCAPE => self.state = State::Escape,
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | DELETE => {}
                _ => kept.push(byte),
            },
            State::Escape => {
                self.state = match byte {
                    b'[' => {
                        self.held.clear();
                        State::Control
                    }
                    b']' | b'P' | b'X' | b'^' | b'_' => State::Text,
                    0x20..=0x2f => State::Intermediate,
                    0x00..=0x1f => State::Escape,
                    _ => State::Ground,
                }
            }
            State::Intermediate => {
                self.state = match byte {
                    ESCAPE => State::Escape,
                    0x00..=0x2f => State::Intermediate,
                    _ => State::Ground,
                };
            }
            State::Control => match byte {
                ESCAPE => self.state = State::Escape,
                0x20..=0x3f => self.held.push(byte),
                0x40..=0x7e => {
                    self.state = State::Ground;
                    if byte == b'm' && self.keeps_colour() {
                        kept.extend_from_slice(b"\x1b[");
                        kept.extend_from_slice(&self.held);
                        kept.push(b'm');
                    }
                }
                0x00..=0x1f => {}
                _ => self.state = State::Ground,
            },
            State::Text => match byte {
                ESCAPE => self.state = State::TextEscape,
                BELL => self.state = State::Ground,
                _ => {}
            },
            State::TextEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                } else {
                    self.state = State::Escape;
                    self.step(byte, kept);
                }
            }
        }
    }

    fn keeps_colour(&self) -> bool {
        self.colour == Colour::Keep
            && self.held.len() <= LONGEST_COLOUR
            && self
                .held
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
    }
}

/// What of a whole console output is kept.
#[must_use]
pub fn inert(bytes: &[u8], colour: Colour) -> Vec<u8> {
    Filter::new(colour).apply(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(text: &str, colour: Colour) -> String {
        String::from_utf8(inert(text.as_bytes(), colour)).unwrap_or_default()
    }

    #[test]
    fn text_and_line_endings_pass_through() {
        let text = "[    0.000000] Linux\r\n\tlogin: é\n";
        assert_eq!(kept(text, Colour::Drop), text);
    }

    #[test]
    fn cursor_and_status_queries_are_removed() {
        assert_eq!(
            kept("localhost:~# \x1b[6nuname\r\n", Colour::Keep),
            "localhost:~# uname\r\n"
        );
        assert_eq!(
            kept(
                "\x1b[32766;32766H\x1b[6n\x1b[5n\x1b[c\x1b[>c\x1b[18t\x1b[?1$pok",
                Colour::Keep
            ),
            "ok"
        );
    }

    #[test]
    fn resets_clears_and_mode_changes_are_removed() {
        assert_eq!(
            kept(
                "\x1b[!p\x1b[?7h\x1b[H\x1b[J\x1bc\x1b(B\x1b7\x1b8done",
                Colour::Keep
            ),
            "done"
        );
    }

    #[test]
    fn colour_is_kept_or_dropped_as_asked() {
        let text = "[\x1b[0;32m  OK  \x1b[0m] Started \x1b[0;1;39mjournal\x1b[0m.";
        assert_eq!(kept(text, Colour::Keep), text);
        assert_eq!(kept(text, Colour::Drop), "[  OK  ] Started journal.");
        assert_eq!(
            kept("\x1b[38:2:255:0:0mred", Colour::Keep),
            "\x1b[38:2:255:0:0mred"
        );
    }

    #[test]
    fn a_colour_sequence_with_other_parameters_is_not_colour() {
        assert_eq!(kept("\x1b[?1mx\x1b[>4;2my", Colour::Keep), "xy");
    }

    #[test]
    fn operating_system_and_device_strings_end_at_either_terminator() {
        assert_eq!(kept("a\x1b]104\x07b", Colour::Keep), "ab");
        assert_eq!(kept("a\x1b]11;?\x1b\\b", Colour::Keep), "ab");
        assert_eq!(kept("a\x1bP$qm\x1b\\b", Colour::Keep), "ab");
        assert_eq!(kept("a\x1b_hidden\x07b", Colour::Keep), "ab");
    }

    #[test]
    fn an_escape_inside_a_string_that_does_not_end_it_starts_a_sequence() {
        assert_eq!(kept("\x1b]0;title\x1b[31mred", Colour::Keep), "\x1b[31mred");
    }

    #[test]
    fn control_bytes_other_than_tabs_and_line_endings_are_removed() {
        assert_eq!(
            kept("\x07\x07bell\x05\x08\x00\x7f\n", Colour::Keep),
            "bell\n"
        );
    }

    #[test]
    fn cancel_abandons_a_sequence() {
        assert_eq!(kept("\x1b[6\x18n", Colour::Keep), "n");
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_recognised() {
        let mut filter = Filter::new(Colour::Keep);
        let mut joined = filter.apply(b"prompt# \x1b");
        joined.extend(filter.apply(b"[6"));
        joined.extend(filter.apply(b"n\x1b[3"));
        joined.extend(filter.apply(b"2mgreen\x1b]104"));
        joined.extend(filter.apply(b"\x07 done"));
        assert_eq!(joined, b"prompt# \x1b[32mgreen done");
    }

    #[test]
    fn an_overlong_colour_sequence_is_dropped() {
        let long = format!("\x1b[{}mx", "1;".repeat(40));
        assert_eq!(kept(&long, Colour::Keep), "x");
    }

    #[test]
    fn filtering_twice_changes_nothing_more() {
        let once = inert(b"\x1b[6n\x1b[0;32mOK\x1b[0m\x07\r\n", Colour::Keep);
        assert_eq!(inert(&once, Colour::Keep), once);
    }
}
