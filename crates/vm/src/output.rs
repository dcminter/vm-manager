use crate::style::Style;
use clap::ValueEnum;
use std::io::{self, Write};
use vm_core::value::{Value, to_json, to_yaml};

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

/// Anything a command returns, rendered as text or as a document.
pub trait Report {
    fn to_value(&self) -> Value;
    fn render_text(&self, style: Style) -> Vec<String>;

    /// Whether the command did everything it was asked to.
    fn succeeded(&self) -> bool {
        true
    }

    /// Errors to write to standard error when rendering as text.
    fn complaints(&self) -> Vec<String> {
        Vec::new()
    }
}

pub fn emit(report: &dyn Report, format: Format, style: Style) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    match format {
        Format::Text => {
            for line in report.render_text(style) {
                writeln!(stdout, "{line}")?;
            }
            stdout.flush()?;
            let mut stderr = io::stderr();
            for line in report.complaints() {
                writeln!(stderr, "{line}")?;
            }
        }
        Format::Json => write!(stdout, "{}", to_json(&report.to_value()))?,
        Format::Yaml => write!(stdout, "{}", to_yaml(&report.to_value()))?,
    }
    stdout.flush()
}

/// What a command given several names did to each, carrying on past failures.
pub struct Batch {
    pub outcomes: Vec<vm_core::Result<Box<dyn Report>>>,
}

impl Batch {
    /// Acts on each name; a single name is reported alone.
    pub fn each(
        names: &[String],
        mut operate: impl FnMut(&str) -> vm_core::Result<Box<dyn Report>>,
    ) -> vm_core::Result<Box<dyn Report>> {
        match names {
            [name] => operate(name),
            _ => Ok(Box::new(Self {
                outcomes: names.iter().map(|name| operate(name)).collect(),
            })),
        }
    }
}

impl Report for Batch {
    fn to_value(&self) -> Value {
        Value::list(self.outcomes.iter().map(|outcome| match outcome {
            Ok(report) => report.to_value(),
            Err(error) => error_document(error),
        }))
    }

    fn render_text(&self, style: Style) -> Vec<String> {
        self.outcomes
            .iter()
            .flatten()
            .flat_map(|report| report.render_text(style))
            .collect()
    }

    fn succeeded(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome.as_ref().is_ok_and(|report| report.succeeded()))
    }

    fn complaints(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .map(|error| format!("vm: {error}"))
            .collect()
    }
}

/// Writes an error in the requested format.
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
    #![allow(clippy::unwrap_used)]

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

    fn fake() -> Box<dyn Report> {
        Box::new(Fake)
    }

    #[test]
    fn one_name_reports_as_the_command_always_has() {
        let report = Batch::each(&["one".to_owned()], |_| Ok(fake())).unwrap();
        assert_eq!(to_yaml(&report.to_value()), "name: debian\n");
        let error = Batch::each(&["one".to_owned()], |name| {
            Err(vm_core::Error::UnknownInstance {
                name: name.to_owned(),
            })
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), "unknown-instance");
    }

    #[test]
    fn several_names_carry_on_past_a_failure_and_report_it() {
        let names = ["one".to_owned(), "two".to_owned(), "three".to_owned()];
        let mut seen = Vec::new();
        let report = Batch::each(&names, |name| {
            seen.push(name.to_owned());
            if name == "two" {
                Err(vm_core::Error::UnknownInstance {
                    name: name.to_owned(),
                })
            } else {
                Ok(fake())
            }
        })
        .unwrap();
        assert_eq!(seen, names);
        assert!(!report.succeeded());
        assert_eq!(
            report.render_text(Style::plain()),
            ["debian".to_owned(), "debian".to_owned()]
        );
        let complaints = report.complaints();
        assert_eq!(complaints.len(), 1);
        assert!(
            complaints[0].starts_with("vm: ") && complaints[0].contains("two"),
            "{complaints:?}"
        );
        let document = to_json(&report.to_value());
        assert!(document.starts_with('['), "{document}");
        assert_eq!(
            document.matches("\"name\": \"debian\"").count(),
            2,
            "{document}"
        );
        assert!(
            document.contains("\"kind\": \"unknown-instance\""),
            "{document}"
        );
    }

    #[test]
    fn several_names_that_all_succeed_succeed() {
        let names = ["one".to_owned(), "two".to_owned()];
        let report = Batch::each(&names, |_| Ok(fake())).unwrap();
        assert!(report.succeeded());
        assert!(report.complaints().is_empty());
    }

    #[test]
    fn a_closed_pipe_is_recognised_and_other_failures_are_not() {
        assert!(is_closed_pipe(&io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(!is_closed_pipe(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }
}
