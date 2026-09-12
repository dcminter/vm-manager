/// A rendered document. Ordered so that output is stable between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(u64),
    String(String),
    List(Vec<Self>),
    Map(Vec<(String, Self)>),
}

impl Value {
    pub fn string(text: impl Into<String>) -> Self {
        Self::String(text.into())
    }

    pub fn map(fields: impl IntoIterator<Item = (&'static str, Self)>) -> Self {
        Self::Map(
            fields
                .into_iter()
                .map(|(key, value)| ((*key).to_owned(), value))
                .collect(),
        )
    }

    pub fn list(items: impl IntoIterator<Item = Self>) -> Self {
        Self::List(items.into_iter().collect())
    }

    pub fn strings(items: impl IntoIterator<Item = String>) -> Self {
        Self::List(items.into_iter().map(Self::String).collect())
    }
}

pub fn to_json(value: &Value) -> String {
    let mut text = String::new();
    write_json(value, 0, &mut text);
    text.push('\n');
    text
}

fn write_json(value: &Value, depth: usize, text: &mut String) {
    let pad = |depth: usize| "  ".repeat(depth);
    match value {
        Value::Null => text.push_str("null"),
        Value::Bool(held) => text.push_str(if *held { "true" } else { "false" }),
        Value::Integer(held) => text.push_str(&held.to_string()),
        Value::String(held) => text.push_str(&quote_json(held)),
        Value::List(items) if items.is_empty() => text.push_str("[]"),
        Value::List(items) => {
            text.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                text.push_str(&pad(depth + 1));
                write_json(item, depth + 1, text);
                if index + 1 < items.len() {
                    text.push(',');
                }
                text.push('\n');
            }
            text.push_str(&pad(depth));
            text.push(']');
        }
        Value::Map(fields) if fields.is_empty() => text.push_str("{}"),
        Value::Map(fields) => {
            text.push_str("{\n");
            for (index, (key, held)) in fields.iter().enumerate() {
                text.push_str(&pad(depth + 1));
                text.push_str(&quote_json(key));
                text.push_str(": ");
                write_json(held, depth + 1, text);
                if index + 1 < fields.len() {
                    text.push(',');
                }
                text.push('\n');
            }
            text.push_str(&pad(depth));
            text.push('}');
        }
    }
}

fn quote_json(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{08}' => quoted.push_str("\\b"),
            '\u{0c}' => quoted.push_str("\\f"),
            control if control < ' ' => {
                use std::fmt::Write as _;
                let _ = write!(quoted, "\\u{:04x}", control as u32);
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

pub fn to_yaml(value: &Value) -> String {
    let mut text = String::new();
    match value {
        Value::Map(fields) if !fields.is_empty() => write_yaml_map(fields, 0, &mut text),
        Value::List(items) if !items.is_empty() => write_yaml_list(items, 0, &mut text),
        scalar => {
            text.push_str(&yaml_scalar(scalar));
            text.push('\n');
        }
    }
    text
}

fn write_yaml_map(fields: &[(String, Value)], depth: usize, text: &mut String) {
    let pad = "  ".repeat(depth);
    for (key, value) in fields {
        text.push_str(&pad);
        text.push_str(&yaml_key(key));
        match value {
            Value::Map(nested) if !nested.is_empty() => {
                text.push_str(":\n");
                write_yaml_map(nested, depth + 1, text);
            }
            Value::List(items) if !items.is_empty() => {
                text.push_str(":\n");
                write_yaml_list(items, depth + 1, text);
            }
            scalar => {
                text.push_str(": ");
                text.push_str(&yaml_scalar(scalar));
                text.push('\n');
            }
        }
    }
}

fn write_yaml_list(items: &[Value], depth: usize, text: &mut String) {
    let pad = "  ".repeat(depth);
    for item in items {
        text.push_str(&pad);
        match item {
            Value::Map(fields) if !fields.is_empty() => {
                text.push_str("- ");
                let mut nested = String::new();
                write_yaml_map(fields, depth + 1, &mut nested);
                text.push_str(nested.trim_start());
            }
            Value::List(nested) if !nested.is_empty() => {
                text.push_str("-\n");
                write_yaml_list(nested, depth + 1, text);
            }
            scalar => {
                text.push_str("- ");
                text.push_str(&yaml_scalar(scalar));
                text.push('\n');
            }
        }
    }
}

fn yaml_key(key: &str) -> String {
    if needs_quoting(key) {
        quote_json(key)
    } else {
        key.to_owned()
    }
}

fn yaml_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(held) => if *held { "true" } else { "false" }.to_owned(),
        Value::Integer(held) => held.to_string(),
        Value::String(held) => {
            if needs_quoting(held) {
                quote_json(held)
            } else {
                held.clone()
            }
        }
        Value::List(_) => "[]".to_owned(),
        Value::Map(_) => "{}".to_owned(),
    }
}

/// Plain YAML scalars are only safe when they cannot be read as something
/// else, so anything ambiguous is emitted double-quoted.
fn needs_quoting(text: &str) -> bool {
    const RESERVED: [char; 14] = [
        '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '%', '@',
    ];
    const WORDS: [&str; 10] = [
        "true", "false", "null", "yes", "no", "on", "off", "y", "n", "~",
    ];
    if text.is_empty() {
        return true;
    }
    if text.trim() != text {
        return true;
    }
    if text
        .chars()
        .any(|character| character.is_control() || character == '"' || character == '\'')
    {
        return true;
    }
    if text.contains(": ") || text.contains(" #") {
        return true;
    }
    if text.starts_with(|character: char| RESERVED.contains(&character)) {
        return true;
    }
    if WORDS.iter().any(|word| word.eq_ignore_ascii_case(text)) {
        return true;
    }
    text.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        Value::map([
            ("name", Value::string("debian")),
            ("tag", Value::string("trixie")),
            ("size", Value::Integer(339_738_624)),
            ("seedable", Value::Bool(true)),
            ("note", Value::Null),
            (
                "aliases",
                Value::strings(["13".to_owned(), "latest".to_owned()]),
            ),
        ])
    }

    #[test]
    fn json_renders_every_scalar_in_its_own_form() {
        let text = to_json(&sample());
        assert!(text.contains(r#""name": "debian""#), "{text}");
        assert!(text.contains(r#""size": 339738624"#), "{text}");
        assert!(text.contains(r#""seedable": true"#), "{text}");
        assert!(text.contains(r#""note": null"#), "{text}");
    }

    #[test]
    fn json_separates_members_with_commas_but_not_the_last() {
        let text = to_json(&Value::map([
            ("a", Value::Integer(1)),
            ("b", Value::Integer(2)),
        ]));
        assert_eq!(text, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
    }

    #[test]
    fn json_renders_empty_collections_inline() {
        assert_eq!(to_json(&Value::list([])), "[]\n");
        assert_eq!(to_json(&Value::map([])), "{}\n");
    }

    #[test]
    fn json_nests_lists_of_maps() {
        let value = Value::list([Value::map([("a", Value::Integer(1))])]);
        assert_eq!(to_json(&value), "[\n  {\n    \"a\": 1\n  }\n]\n");
    }

    #[test]
    fn json_escapes_the_characters_the_grammar_forbids() {
        let value = Value::string("quote \" slash \\ newline \n tab \t");
        assert_eq!(
            to_json(&value).trim_end(),
            r#""quote \" slash \\ newline \n tab \t""#
        );
    }

    #[test]
    fn json_escapes_other_control_characters_as_hex() {
        let bell = String::from(char::from(7u8));
        let escape = char::from(92u8);
        let expected = format!("\"{escape}u0007\"");
        assert_eq!(to_json(&Value::String(bell)).trim_end(), expected);
    }

    #[test]
    fn json_leaves_printable_unicode_alone() {
        let accented = "cafe\u{e9}";
        assert_eq!(
            to_json(&Value::string(accented)).trim_end(),
            format!("\"{accented}\"")
        );
    }

    #[test]
    fn json_output_ends_with_exactly_one_newline() {
        let text = to_json(&sample());
        assert!(text.ends_with("}\n"));
        assert!(!text.ends_with("}\n\n"));
    }

    #[test]
    fn yaml_renders_a_map_as_indented_pairs() {
        let text = to_yaml(&Value::map([
            ("a", Value::Integer(1)),
            ("b", Value::string("x")),
        ]));
        assert_eq!(text, "a: 1\nb: x\n");
    }

    #[test]
    fn yaml_indents_nested_maps() {
        let value = Value::map([("outer", Value::map([("inner", Value::string("value"))]))]);
        assert_eq!(to_yaml(&value), "outer:\n  inner: value\n");
    }

    #[test]
    fn yaml_renders_lists_with_dashes() {
        let value = Value::map([("aliases", Value::strings(["13".to_owned(), "x".to_owned()]))]);
        assert_eq!(to_yaml(&value), "aliases:\n  - \"13\"\n  - x\n");
    }

    #[test]
    fn yaml_puts_the_first_field_of_a_list_item_on_the_dash_line() {
        let value = Value::list([Value::map([
            ("name", Value::string("debian")),
            ("tag", Value::string("trixie")),
        ])]);
        assert_eq!(to_yaml(&value), "- name: debian\n  tag: trixie\n");
    }

    #[test]
    fn yaml_quotes_anything_that_could_read_as_another_type() {
        for text in [
            "true", "False", "null", "yes", "no", "on", "~", "13", "1.5", "-2",
        ] {
            let rendered = to_yaml(&Value::map([("k", Value::string(text))]));
            assert_eq!(
                rendered,
                format!("k: \"{text}\"\n"),
                "{text} should be quoted"
            );
        }
    }

    #[test]
    fn yaml_quotes_strings_that_would_break_the_syntax() {
        let cases = [
            "",
            " padded ",
            "has: colon",
            "trailing #comment",
            "- leading dash",
            "{brace",
        ];
        for text in cases {
            let rendered = to_yaml(&Value::map([("k", Value::string(text))]));
            assert!(
                rendered.starts_with("k: \""),
                "{text:?} should be quoted, got {rendered:?}"
            );
        }
    }

    #[test]
    fn yaml_leaves_ordinary_prose_unquoted() {
        let value = Value::map([(
            "description",
            Value::string("Debian 13 (Trixie) cloud image"),
        )]);
        assert_eq!(
            to_yaml(&value),
            "description: Debian 13 (Trixie) cloud image\n"
        );
    }

    #[test]
    fn yaml_escapes_control_characters_by_quoting() {
        let rendered = to_yaml(&Value::map([("k", Value::string("two\nlines"))]));
        assert_eq!(rendered, "k: \"two\\nlines\"\n");
    }

    #[test]
    fn yaml_renders_empty_collections_inline() {
        let value = Value::map([("a", Value::list([])), ("b", Value::map([]))]);
        assert_eq!(to_yaml(&value), "a: []\nb: {}\n");
    }

    #[test]
    fn yaml_renders_a_bare_scalar_on_its_own() {
        assert_eq!(to_yaml(&Value::Integer(7)), "7\n");
        assert_eq!(to_yaml(&Value::Null), "null\n");
    }

    #[test]
    fn a_url_is_not_quoted_despite_its_colon() {
        let value = Value::map([("url", Value::string("https://example.test/a.qcow2"))]);
        assert_eq!(to_yaml(&value), "url: https://example.test/a.qcow2\n");
    }

    #[test]
    fn an_absent_value_renders_as_null_in_both_formats() {
        let value = Value::map([("size", Value::Null)]);
        assert!(to_json(&value).contains(r#""size": null"#));
        assert_eq!(
            to_yaml(&value),
            "size: null
"
        );
    }
}
