use crate::style::Style;
use crate::value::{Value, to_json, to_yaml};
use clap::ValueEnum;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Tables and prose, coloured when the terminal allows
    Text,
    Json,
    Yaml,
}

impl Format {
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// Anything a command can return. The text rendering and the document are
/// separate so that no command writes directly to the terminal.
pub trait Report {
    fn to_value(&self) -> Value;
    fn render_text(&self, style: Style) -> Vec<String>;
}

pub fn emit(report: &dyn Report, format: Format, style: Style) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    match format {
        Format::Text => {
            for line in report.render_text(style) {
                writeln!(stdout, "{line}")?;
            }
        }
        Format::Json => write!(stdout, "{}", to_json(&report.to_value()))?,
        Format::Yaml => write!(stdout, "{}", to_yaml(&report.to_value()))?,
    }
    stdout.flush()
}

/// Errors are reported in the requested format too, so a script that asked for
/// JSON is never handed a bare sentence.
pub fn emit_error(error: &vm_core::Error, format: Format) {
    let mut stderr = io::stderr();
    let _ = match format {
        Format::Text => writeln!(stderr, "vm: {error}"),
        Format::Json => write!(stderr, "{}", to_json(&error_document(error))),
        Format::Yaml => write!(stderr, "{}", to_yaml(&error_document(error))),
    };
}

fn error_document(error: &vm_core::Error) -> Value {
    Value::map([(
        "error",
        Value::map([
            ("kind", Value::string(error.kind())),
            ("message", Value::string(error.to_string())),
        ]),
    )])
}

/// A closed downstream is how `| head` ends, not a failure to report.
pub fn is_closed_pipe(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::BrokenPipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Report for Fake {
        fn to_value(&self) -> Value {
            Value::map([("name", Value::string("debian"))])
        }

        fn render_text(&self, _: Style) -> Vec<String> {
            vec!["debian".to_owned()]
        }
    }

    #[test]
    fn only_the_text_format_is_text() {
        assert!(Format::Text.is_text());
        assert!(!Format::Json.is_text());
        assert!(!Format::Yaml.is_text());
    }

    #[test]
    fn a_report_renders_both_ways_from_one_value() {
        let report = Fake;
        assert_eq!(
            report.render_text(Style::plain()),
            vec!["debian".to_owned()]
        );
        assert_eq!(to_yaml(&report.to_value()), "name: debian\n");
    }

    #[test]
    fn an_error_document_carries_a_kind_and_a_message() {
        let error = vm_core::Error::UnknownImage {
            reference: "ubuntu:noble".to_owned(),
        };
        let rendered = to_yaml(&error_document(&error));
        assert!(rendered.contains("kind: unknown-image"), "{rendered}");
        assert!(rendered.contains("ubuntu:noble"), "{rendered}");
    }

    #[test]
    fn an_error_document_is_a_single_json_object() {
        let error = vm_core::Error::NoImageStore;
        let rendered = to_json(&error_document(&error));
        assert!(rendered.starts_with("{\n  \"error\": {"), "{rendered}");
        assert!(
            rendered.contains(r#""kind": "no-image-store""#),
            "{rendered}"
        );
    }

    #[test]
    fn a_closed_pipe_is_recognised_and_other_failures_are_not() {
        assert!(is_closed_pipe(&io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(!is_closed_pipe(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }
}
