/// A document, with maps kept in insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(u64),
    /// Anything a JSON number can hold that an unsigned integer cannot.
    Number(f64),
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

/// JSON has no way to write an infinity or a NaN, so neither is emitted.
fn number(held: f64) -> String {
    if !held.is_finite() {
        return "null".to_owned();
    }
    if held.fract() == 0.0 && held.abs() < 9_007_199_254_740_992.0 {
        return format!("{held:.0}");
    }
    held.to_string()
}

/// Compact JSON with a trailing newline.
pub fn to_json_line(value: &Value) -> String {
    let mut text = String::new();
    write_json_line(value, &mut text);
    text.push('\n');
    text
}

fn write_json_line(value: &Value, text: &mut String) {
    match value {
        Value::List(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_json_line(item, text);
            }
            text.push(']');
        }
        Value::Map(fields) => {
            text.push('{');
            for (index, (key, held)) in fields.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                text.push_str(&quote_json(key));
                text.push(':');
                write_json_line(held, text);
            }
            text.push('}');
        }
        scalar => {
            let mut rendered = String::new();
            write_json(scalar, 0, &mut rendered);
            text.push_str(&rendered);
        }
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
        Value::Number(held) => text.push_str(&number(*held)),
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
        Value::Number(held) => number(*held),
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

/// Whether a YAML scalar must be quoted to avoid being read as another type.
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

/// A parse failure at a byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub at: usize,
    pub reason: &'static str,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.reason, self.at)
    }
}

/// The deepest nesting parsed, to bound recursion.
const MAX_DEPTH: usize = 64;

/// Reads one JSON document, allowing only trailing whitespace.
pub fn from_json(text: &str) -> Result<Value, Invalid> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let value = parse(bytes, &mut at, 0)?;
    skip_space(bytes, &mut at);
    if at != bytes.len() {
        return Err(Invalid {
            at,
            reason: "trailing input after the document",
        });
    }
    Ok(value)
}

fn parse(bytes: &[u8], at: &mut usize, depth: usize) -> Result<Value, Invalid> {
    if depth > MAX_DEPTH {
        return Err(Invalid {
            at: *at,
            reason: "nested beyond the permitted depth",
        });
    }
    skip_space(bytes, at);
    match bytes.get(*at) {
        None => Err(Invalid {
            at: *at,
            reason: "the document ends where a value was expected",
        }),
        Some(b'{') => parse_map(bytes, at, depth),
        Some(b'[') => parse_list(bytes, at, depth),
        Some(b'"') => parse_string(bytes, at).map(Value::String),
        Some(b't') => literal(bytes, at, b"true", Value::Bool(true)),
        Some(b'f') => literal(bytes, at, b"false", Value::Bool(false)),
        Some(b'n') => literal(bytes, at, b"null", Value::Null),
        Some(_) => parse_number(bytes, at),
    }
}

fn parse_map(bytes: &[u8], at: &mut usize, depth: usize) -> Result<Value, Invalid> {
    *at += 1;
    let mut fields = Vec::new();
    skip_space(bytes, at);
    if bytes.get(*at) == Some(&b'}') {
        *at += 1;
        return Ok(Value::Map(fields));
    }
    loop {
        skip_space(bytes, at);
        let key = parse_string(bytes, at)?;
        skip_space(bytes, at);
        if bytes.get(*at) != Some(&b':') {
            return Err(Invalid {
                at: *at,
                reason: "a field name is not followed by a colon",
            });
        }
        *at += 1;
        fields.push((key, parse(bytes, at, depth + 1)?));
        skip_space(bytes, at);
        match bytes.get(*at) {
            Some(b',') => *at += 1,
            Some(b'}') => {
                *at += 1;
                return Ok(Value::Map(fields));
            }
            _ => {
                return Err(Invalid {
                    at: *at,
                    reason: "an object is not closed",
                });
            }
        }
    }
}

fn parse_list(bytes: &[u8], at: &mut usize, depth: usize) -> Result<Value, Invalid> {
    *at += 1;
    let mut items = Vec::new();
    skip_space(bytes, at);
    if bytes.get(*at) == Some(&b']') {
        *at += 1;
        return Ok(Value::List(items));
    }
    loop {
        items.push(parse(bytes, at, depth + 1)?);
        skip_space(bytes, at);
        match bytes.get(*at) {
            Some(b',') => *at += 1,
            Some(b']') => {
                *at += 1;
                return Ok(Value::List(items));
            }
            _ => {
                return Err(Invalid {
                    at: *at,
                    reason: "an array is not closed",
                });
            }
        }
    }
}

fn parse_string(bytes: &[u8], at: &mut usize) -> Result<String, Invalid> {
    if bytes.get(*at) != Some(&b'"') {
        return Err(Invalid {
            at: *at,
            reason: "a string was expected",
        });
    }
    *at += 1;
    let mut out = String::new();
    loop {
        let Some(byte) = bytes.get(*at).copied() else {
            return Err(Invalid {
                at: *at,
                reason: "a string is not closed",
            });
        };
        *at += 1;
        match byte {
            b'"' => return Ok(out),
            b'\\' => out.push(escape(bytes, at)?),
            control if control < 0x20 => {
                return Err(Invalid {
                    at: *at - 1,
                    reason: "a string holds an unescaped control character",
                });
            }
            _ => {
                // The input is valid UTF-8, so multi-byte sequences are copied as is.
                let start = *at - 1;
                let mut end = *at;
                while end < bytes.len() && bytes[end] & 0xC0 == 0x80 {
                    end += 1;
                }
                match std::str::from_utf8(&bytes[start..end]) {
                    Ok(text) => out.push_str(text),
                    Err(_) => {
                        return Err(Invalid {
                            at: start,
                            reason: "a string is not valid text",
                        });
                    }
                }
                *at = end;
            }
        }
    }
}

fn escape(bytes: &[u8], at: &mut usize) -> Result<char, Invalid> {
    let Some(byte) = bytes.get(*at).copied() else {
        return Err(Invalid {
            at: *at,
            reason: "an escape is not completed",
        });
    };
    *at += 1;
    Ok(match byte {
        b'"' => '"',
        b'\\' => '\\',
        b'/' => '/',
        b'b' => '\u{08}',
        b'f' => '\u{0c}',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'u' => return unicode_escape(bytes, at),
        _ => {
            return Err(Invalid {
                at: *at - 1,
                reason: "an escape names no character",
            });
        }
    })
}

/// Reads a `\u` escape, combining surrogate pairs.
fn unicode_escape(bytes: &[u8], at: &mut usize) -> Result<char, Invalid> {
    let first = hex4(bytes, at)?;
    if (0xD800..0xDC00).contains(&first) {
        if bytes.get(*at) != Some(&b'\\') || bytes.get(*at + 1) != Some(&b'u') {
            return Err(Invalid {
                at: *at,
                reason: "a surrogate is not followed by its pair",
            });
        }
        *at += 2;
        let second = hex4(bytes, at)?;
        if !(0xDC00..0xE000).contains(&second) {
            return Err(Invalid {
                at: *at,
                reason: "a surrogate pair is malformed",
            });
        }
        let combined = 0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
        return char::from_u32(combined).ok_or(Invalid {
            at: *at,
            reason: "a surrogate pair names no character",
        });
    }
    char::from_u32(first).ok_or(Invalid {
        at: *at,
        reason: "an escape names no character",
    })
}

fn hex4(bytes: &[u8], at: &mut usize) -> Result<u32, Invalid> {
    let Some(digits) = bytes.get(*at..*at + 4) else {
        return Err(Invalid {
            at: *at,
            reason: "an escape is not completed",
        });
    };
    let mut value = 0u32;
    for digit in digits {
        let Some(held) = char::from(*digit).to_digit(16) else {
            return Err(Invalid {
                at: *at,
                reason: "an escape is not hexadecimal",
            });
        };
        value = value * 16 + held;
    }
    *at += 4;
    Ok(value)
}

/// Non-negative integers stay exact; other numbers become floats.
fn parse_number(bytes: &[u8], at: &mut usize) -> Result<Value, Invalid> {
    let start = *at;
    while let Some(byte) = bytes.get(*at) {
        if byte.is_ascii_digit() || b"+-.eE".contains(byte) {
            *at += 1;
        } else {
            break;
        }
    }
    let text = std::str::from_utf8(&bytes[start..*at]).unwrap_or("");
    if text.is_empty() {
        return Err(Invalid {
            at: start,
            reason: "a value was expected",
        });
    }
    if let Ok(whole) = text.parse::<u64>() {
        return Ok(Value::Integer(whole));
    }
    text.parse::<f64>().map(Value::Number).map_err(|_| Invalid {
        at: start,
        reason: "a number is not readable",
    })
}

fn literal(bytes: &[u8], at: &mut usize, word: &[u8], value: Value) -> Result<Value, Invalid> {
    if bytes.get(*at..*at + word.len()) == Some(word) {
        *at += word.len();
        Ok(value)
    } else {
        Err(Invalid {
            at: *at,
            reason: "a value was expected",
        })
    }
}

fn skip_space(bytes: &[u8], at: &mut usize) {
    while matches!(bytes.get(*at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *at += 1;
    }
}

/// Field lookup.
impl Value {
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Map(fields) => fields
                .iter()
                .find(|(held, _)| held == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(held) => Some(held),
            _ => None,
        }
    }

    pub const fn as_integer(&self) -> Option<u64> {
        match self {
            Self::Integer(held) => Some(*held),
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

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

    #[test]
    fn the_scalars_read_back_as_themselves() {
        assert_eq!(from_json("null").unwrap(), Value::Null);
        assert_eq!(from_json("true").unwrap(), Value::Bool(true));
        assert_eq!(from_json("false").unwrap(), Value::Bool(false));
        assert_eq!(from_json("  42  ").unwrap(), Value::Integer(42));
        assert_eq!(from_json("\"text\"").unwrap(), Value::string("text"));
    }

    #[test]
    fn a_whole_number_keeps_its_exact_value() {
        let large = u64::MAX;
        assert_eq!(
            from_json(&large.to_string()).unwrap(),
            Value::Integer(large),
            "a float would have rounded it"
        );
    }

    #[test]
    fn anything_an_unsigned_integer_cannot_hold_becomes_a_number() {
        assert_eq!(from_json("-1").unwrap(), Value::Number(-1.0));
        assert_eq!(from_json("1.5").unwrap(), Value::Number(1.5));
        assert_eq!(from_json("2e3").unwrap(), Value::Number(2000.0));
    }

    #[test]
    fn a_whole_float_is_written_back_without_a_point() {
        assert_eq!(to_json(&Value::Number(-1.0)), "-1\n");
        assert_eq!(to_json(&Value::Number(1.5)), "1.5\n");
    }

    #[test]
    fn a_number_json_cannot_write_becomes_null() {
        assert_eq!(to_json(&Value::Number(f64::NAN)), "null\n");
        assert_eq!(to_json(&Value::Number(f64::INFINITY)), "null\n");
    }

    #[test]
    fn an_object_keeps_the_order_it_arrived_in() {
        let value = from_json(r#"{"b": 1, "a": 2}"#).unwrap();
        let Value::Map(fields) = &value else {
            panic!("expected a map, got {value:?}")
        };
        assert_eq!(fields[0].0, "b");
        assert_eq!(fields[1].0, "a");
    }

    #[test]
    fn nesting_is_read_through() {
        let value = from_json(r#"{"a": [1, {"b": null}]}"#).unwrap();
        assert_eq!(
            value,
            Value::map([(
                "a",
                Value::list([Value::Integer(1), Value::map([("b", Value::Null)])])
            )])
        );
    }

    #[test]
    fn empty_containers_are_read() {
        assert_eq!(from_json("{}").unwrap(), Value::Map(Vec::new()));
        assert_eq!(from_json("[]").unwrap(), Value::List(Vec::new()));
        assert_eq!(from_json(" [ ] ").unwrap(), Value::List(Vec::new()));
    }

    #[test]
    fn escapes_are_unescaped() {
        let value = from_json(r#""a\nb\tc\"d\\e\/f""#).unwrap();
        assert_eq!(value, Value::string("a\nb\tc\"d\\e/f"));
    }

    #[test]
    fn a_unicode_escape_becomes_its_character() {
        assert_eq!(from_json(r#""\u00e9""#).unwrap(), Value::string("\u{e9}"));
    }

    #[test]
    fn a_surrogate_pair_becomes_one_character() {
        assert_eq!(
            from_json(r#""\ud83d\udca9""#).unwrap(),
            Value::string("\u{1f4a9}")
        );
    }

    #[test]
    fn a_lone_surrogate_is_refused() {
        assert!(from_json(r#""\ud83d""#).is_err());
        assert!(from_json(r#""\ud83dx""#).is_err());
    }

    #[test]
    fn text_beyond_ascii_survives_the_round_trip() {
        let value = Value::string("héllo — ☃");
        let written = to_json(&value);
        assert_eq!(from_json(&written).unwrap(), value);
    }

    #[test]
    fn everything_this_emits_can_be_read_back() {
        let value = Value::map([
            ("name", Value::string("debian")),
            ("size", Value::Integer(339_738_624)),
            ("ratio", Value::Number(0.25)),
            ("held", Value::Bool(true)),
            ("missing", Value::Null),
            ("tags", Value::strings(vec!["a".to_owned(), "b".to_owned()])),
            ("empty", Value::List(Vec::new())),
        ]);
        assert_eq!(from_json(&to_json(&value)).unwrap(), value);
    }

    #[test]
    fn malformed_documents_are_refused() {
        for text in [
            "",
            "{",
            "[1, 2",
            "{\"a\" 1}",
            "{\"a\": }",
            "{a: 1}",
            "\"unterminated",
            "tru",
            "[1] [2]",
            "{\"a\": 1,}",
        ] {
            assert!(from_json(text).is_err(), "{text} should be refused");
        }
    }

    #[test]
    fn an_unescaped_control_character_is_refused() {
        let text = format!("\"a{}b\"", char::from(1u8));
        assert!(from_json(&text).is_err());
    }

    #[test]
    fn nesting_beyond_the_limit_is_refused_rather_than_recursed_into() {
        let deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
        let error = from_json(&deep).unwrap_err();
        assert_eq!(error.reason, "nested beyond the permitted depth");
    }

    #[test]
    fn a_field_can_be_read_out_of_a_map() {
        let value = from_json(r#"{"return": {"status": "running", "count": 3}}"#).unwrap();
        let held = value.get("return").unwrap();
        assert_eq!(held.get("status").unwrap().as_str(), Some("running"));
        assert_eq!(held.get("count").unwrap().as_integer(), Some(3));
        assert!(value.get("absent").is_none());
        assert!(Value::Null.get("anything").is_none());
    }
}
