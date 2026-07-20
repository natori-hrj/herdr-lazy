//! A minimal JSON reader — just enough to consume `herdr plugin list --json`.
//!
//! Why hand-rolled: the payload is one known shape from one known producer, and reading it
//! needs a few string fields. serde + serde_json would be ~40 crates for that. The project
//! takes a dependency only where std genuinely cannot do the job (see `ui`, which needs raw
//! terminal mode); this is not one of those places.
//!
//! Why a real parser rather than scanning for `"plugin_id":"..."`: the payload nests
//! objects that reuse key names — every entry in `actions[]` has its own `id`, and
//! `build[]`/`actions[]` both carry `command`. Substring scanning reads those as if they
//! belonged to the plugin. Parsing structurally is the only way to ask for *this* object's
//! field and get the right answer.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(BTreeMap<String, Value>),
}

impl Value {
    /// Field of an object, or None for any other shape.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.get(key),
            _ => None,
        }
    }

    /// Follow a chain of object keys: `v.path(&["result", "plugins"])`.
    pub fn path(&self, keys: &[&str]) -> Option<&Value> {
        keys.iter().try_fold(self, |v, k| v.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Convenience: a string field of this object.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }
}

pub fn parse(input: &str) -> Result<Value, String> {
    let bytes: Vec<char> = input.chars().collect();
    let mut p = Parser { b: &bytes, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing input at char {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [char],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        match self.bump() {
            Some(got) if got == c => Ok(()),
            Some(got) => Err(format!(
                "expected `{}`, found `{}` at {}",
                c,
                got,
                self.i - 1
            )),
            None => Err(format!("expected `{}`, found end of input", c)),
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(&word.chars().collect::<Vec<_>>()[..]) {
            self.i += word.chars().count();
            Ok(v)
        } else {
            Err(format!("invalid literal at {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Value::Str(self.string()?)),
            Some('t') => self.literal("true", Value::Bool(true)),
            Some('f') => self.literal("false", Value::Bool(false)),
            Some('n') => self.literal("null", Value::Null),
            Some(_) => self.number(),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect('{')?;
        let mut map = BTreeMap::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Value::Obj(map));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(':')?;
            self.ws();
            let v = self.value()?;
            map.insert(k, v);
            self.ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => return Ok(Value::Obj(map)),
                _ => return Err(format!("malformed object near {}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(Value::Arr(items)),
                _ => return Err(format!("malformed array near {}", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".to_string()),
                Some('"') => return Ok(s),
                Some('\\') => match self.bump() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('/') => s.push('/'),
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('b') => s.push('\u{8}'),
                    Some('f') => s.push('\u{c}'),
                    Some('u') => s.push(self.unicode_escape()?),
                    other => return Err(format!("bad escape `\\{:?}`", other)),
                },
                Some(c) => s.push(c),
            }
        }
    }

    /// `\uXXXX`, including surrogate pairs. Lone surrogates become U+FFFD rather than
    /// failing the whole parse — we would rather read a plugin list with one odd name
    /// than reject it outright.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) {
            // High surrogate: expect a following `\uXXXX` low surrogate.
            if self.peek() == Some('\\') && self.b.get(self.i + 1) == Some(&'u') {
                self.i += 2;
                let lo = self.hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return Ok(char::from_u32(c).unwrap_or('\u{FFFD}'));
                }
            }
            return Ok('\u{FFFD}');
        }
        Ok(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut n = 0u32;
        for _ in 0..4 {
            let c = self.bump().ok_or("truncated \\u escape")?;
            let d = c.to_digit(16).ok_or(format!("bad hex digit `{}`", c))?;
            n = n * 16 + d;
        }
        Ok(n)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()
            || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E')
        {
            self.i += 1;
        }
        let s: String = self.b[start..self.i].iter().collect();
        s.parse::<f64>()
            .map(Value::Num)
            .map_err(|_| format!("bad number `{}`", s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact payload shape herdr 0.7.4 returns for a linked local plugin.
    const REAL_LIST: &str = r#"{"id":"cli:plugin","result":{"plugins":[{"actions":[{"command":["target/release/herdr-lazy","init"],"contexts":["workspace"],"id":"init","title":"Lazy: install curated defaults"}],"build":[{"command":["cargo","build","--release"]}],"description":"Be lazy","enabled":true,"manifest_path":"/p/herdr-plugin.toml","min_herdr_version":"0.7.0","name":"herdr-lazy","platforms":["linux"],"plugin_id":"natori.lazy","plugin_root":"/p","source":{"kind":"local"},"version":"0.1.0"}],"type":"plugin_list"}}"#;

    #[test]
    fn reads_plugin_fields_from_real_payload() {
        let v = parse(REAL_LIST).expect("should parse");
        let plugins = v.path(&["result", "plugins"]).unwrap().as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].str_field("plugin_id"), Some("natori.lazy"));
        assert_eq!(plugins[0].str_field("name"), Some("herdr-lazy"));
        assert_eq!(plugins[0].get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(
            plugins[0].path(&["source", "kind"]).unwrap().as_str(),
            Some("local")
        );
    }

    /// The bug that motivated a real parser: `actions[].id` must not be mistaken for the
    /// plugin's own `plugin_id`/`id`, which naive substring scanning does.
    #[test]
    fn nested_ids_do_not_leak_into_plugin_fields() {
        let v = parse(REAL_LIST).unwrap();
        let p = &v.path(&["result", "plugins"]).unwrap().as_array().unwrap()[0];
        assert_eq!(p.str_field("id"), None, "plugin has no bare `id` field");
        let action = &p.get("actions").unwrap().as_array().unwrap()[0];
        assert_eq!(action.str_field("id"), Some("init"));
    }

    #[test]
    fn empty_plugin_list() {
        let v =
            parse(r#"{"id":"cli:plugin","result":{"plugins":[],"type":"plugin_list"}}"#).unwrap();
        assert!(v
            .path(&["result", "plugins"])
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn scalars_and_escapes() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse(" true ").unwrap(), Value::Bool(true));
        assert_eq!(parse("-12.5e2").unwrap(), Value::Num(-1250.0));
        assert_eq!(
            parse(r#""a\"b\\c\nd""#).unwrap(),
            Value::Str("a\"b\\c\nd".to_string())
        );
        // Literal (unescaped) multi-byte input, at 3 and 4 bytes per character: the parser
        // walks `chars()`, so anything indexing by byte would split these and panic.
        assert_eq!(parse(r#""あ""#).unwrap(), Value::Str("あ".to_string()));
        assert_eq!(parse(r#""🚀""#).unwrap(), Value::Str("🚀".to_string()));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{").is_err());
        assert!(parse(r#"{"a":1,}"#).is_err());
        assert!(parse(r#"{"a":1} junk"#).is_err());
        assert!(parse(r#""unterminated"#).is_err());
    }
}
