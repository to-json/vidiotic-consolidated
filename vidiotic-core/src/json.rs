//! Just enough JSON to emit an object correctly.
//!
//! Both browser shells hand the page a state object — `/play`'s `engine_state`,
//! `/chop`'s `state_json` and its export plan — and both built them by
//! interpolating into one long `format!` next to a local `json_escape` the author
//! had to remember to call. That works exactly as long as everybody remembers.
//! A single unescaped field turns the readout into a parse error, and the readout
//! is how the smoke suites see anything at all, so the failure arrives as "the
//! test can't read the state" rather than as "this string had a quote in it".
//!
//! Two things this fixes beyond the missing-`escape` hazard:
//!
//! - **Non-finite floats.** `NaN` and `inf` are not JSON, and `format!("{}", …)`
//!   writes them out as `NaN`/`inf`, which `JSON.parse` rejects. These are
//!   reachable: `/chop` publishes `in_frame as f64 / fps`, and a source that
//!   reports no frame rate makes that infinite. [`Obj::num`] writes `null`.
//! - **Separators.** Commas between members were a property of the format string,
//!   so adding a field meant getting the punctuation right by eye.
//!
//! Not a JSON library and not a serializer: no parsing, no derives, no `Value`
//! tree. `vidiotic-core` already hosts [`crate::bundle`] on the same argument —
//! both shells need it and neither can depend on the other.

/// A JSON string body (no surrounding quotes), with everything a JSON string
/// may not contain literally replaced.
///
/// The control range goes out as `\uXXXX` rather than the short escapes for the
/// few that have them; both are valid and one rule is easier to be sure of.
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                use std::fmt::Write as _;
                // Infallible into a String; the `write!` is only for the hex.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// `s` as a complete JSON string, quotes included.
#[must_use]
pub fn string(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

/// A finite number, or `null`. See the module note on why that is not pedantry.
#[must_use]
pub fn num(v: f64) -> String {
    if v.is_finite() {
        v.to_string()
    } else {
        "null".to_owned()
    }
}

/// A JSON array of values that are *already* JSON — what [`Obj::finish`] and
/// [`string`] return.
#[must_use]
pub fn arr<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut out = String::from("[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&item);
    }
    out.push(']');
    out
}

/// A JSON object, built a member at a time.
///
/// Every setter escapes what needs escaping and writes its own separator, so a
/// new field cannot arrive unescaped or with the punctuation wrong.
#[derive(Debug, Default)]
pub struct Obj {
    buf: String,
}

impl Obj {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn key(&mut self, key: &str) {
        if !self.buf.is_empty() {
            self.buf.push(',');
        }
        self.buf.push_str(&string(key));
        self.buf.push(':');
    }

    /// A string member; the value is escaped.
    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.buf.push_str(&string(value));
        self
    }

    /// An integer member.
    pub fn int(&mut self, key: &str, value: i64) -> &mut Self {
        self.key(key);
        self.buf.push_str(&value.to_string());
        self
    }

    /// A floating-point member, `null` if not finite.
    pub fn num(&mut self, key: &str, value: f64) -> &mut Self {
        self.key(key);
        self.buf.push_str(&num(value));
        self
    }

    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.key(key);
        self.buf.push_str(if value { "true" } else { "false" });
        self
    }

    /// A member whose value is already JSON — a nested [`Obj::finish`], an
    /// [`arr`], or the literal `null`. The one setter that trusts its caller,
    /// and the reason it exists is nesting rather than convenience.
    pub fn raw(&mut self, key: &str, json: &str) -> &mut Self {
        self.key(key);
        self.buf.push_str(json);
        self
    }

    /// A member that is an object or `null`.
    pub fn opt(&mut self, key: &str, json: Option<String>) -> &mut Self {
        self.raw(key, json.as_deref().unwrap_or("null"))
    }

    #[must_use]
    pub fn finish(&self) -> String {
        format!("{{{}}}", self.buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn strings_survive_what_breaks_a_literal() {
        assert_eq!(string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(string(r"a\b"), r#""a\\b""#);
        // Control characters go out as \uXXXX rather than the short escapes.
        // Four hex digits exactly, so the following `b` is not swallowed.
        assert_eq!(string("a\nb"), r#""a\u000ab""#);
        assert_eq!(string("a\tb"), r#""a\u0009b""#);
        // A filename is the field that actually carries these.
        assert_eq!(string(r#"my "clip"\2.mov"#), r#""my \"clip\"\\2.mov""#);
    }

    #[test]
    fn non_finite_numbers_become_null() {
        // `format!("{}", …)` would write NaN/inf, which JSON.parse rejects.
        // Reachable: /chop publishes `frames / fps`.
        assert_eq!(num(f64::NAN), "null");
        assert_eq!(num(f64::INFINITY), "null");
        assert_eq!(num(f64::NEG_INFINITY), "null");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(29.97), "29.97");
    }

    #[test]
    fn an_object_punctuates_itself() {
        assert_eq!(Obj::new().finish(), "{}");
        let mut o = Obj::new();
        o.str("name", "a")
            .int("frames", 12)
            .num("fps", 30.0)
            .bool("playing", false)
            .raw("crop", "null");
        assert_eq!(
            o.finish(),
            r#"{"name":"a","frames":12,"fps":30,"playing":false,"crop":null}"#
        );
    }

    #[test]
    fn arrays_and_nesting_compose() {
        let mut inner = Obj::new();
        inner.int("index", 0).str("name", "one");
        let mut outer = Obj::new();
        outer.raw("spans", &arr([inner.finish()]));
        assert_eq!(outer.finish(), r#"{"spans":[{"index":0,"name":"one"}]}"#);

        assert_eq!(arr(Vec::<String>::new()), "[]");
        assert_eq!(arr(["a", "b"].iter().map(|s| string(s))), r#"["a","b"]"#);
    }

    #[test]
    fn opt_writes_null_for_none() {
        let mut o = Obj::new();
        o.opt("clip", None).opt("other", Some(string("x")));
        assert_eq!(o.finish(), r#"{"clip":null,"other":"x"}"#);
    }

    /// A key is escaped like any other string. Nothing in this repo uses an
    /// exotic key, and that is the point of not having to check.
    #[test]
    fn keys_are_escaped_too() {
        let mut o = Obj::new();
        o.int(r#"a"b"#, 1);
        assert_eq!(o.finish(), r#"{"a\"b":1}"#);
    }
}
