//! Lightweight output tree for fit statistics.
//!
//! Same shape and text as building a `serde_json::Value` with `json!`, rounding every float to 6 decimals and
//! serialising compactly with sorted keys, without the BTreeMap and String-key cost. `J::into_value` gives the
//! `serde_json::Value` for the library API; `J::to_json` writes the text directly.
use serde_json::{Map, Number, Value};
use std::borrow::Cow;

#[derive(Debug, Clone)]
pub enum J {
    Null,
    Bool(bool),
    U(u64),
    I(i64),
    /// always finite (non-finite floats become Null, as `serde_json::to_value` does)
    F(f64),
    S(Cow<'static, str>),
    A(Vec<J>),
    /// entries in insertion order; keys are unique; written sorted by key (BTreeMap order)
    O(Vec<(Cow<'static, str>, J)>),
}

#[inline]
fn round6(v: f64) -> f64 {
    if v.is_finite() { (v * 1e6).round() / 1e6 } else { v }
}

impl J {
    pub fn obj() -> J {
        J::O(Vec::new())
    }
    /// insert or replace (like `Map::insert`)
    pub fn insert(&mut self, k: impl Into<Cow<'static, str>>, v: J) {
        let k = k.into();
        if let J::O(o) = self {
            if let Some(e) = o.iter_mut().find(|e| e.0 == k) {
                e.1 = v;
            } else {
                o.push((k, v));
            }
        } else {
            panic!("J::insert on a non-object");
        }
    }
    /// append without the duplicate check (caller guarantees a new key)
    #[inline]
    pub fn push_new(&mut self, k: impl Into<Cow<'static, str>>, v: J) {
        if let J::O(o) = self {
            o.push((k.into(), v));
        } else {
            panic!("J::push_new on a non-object");
        }
    }

    /// any serde-serialisable value (same JSON as `json!` gives it)
    pub fn ser<T: serde::Serialize + ?Sized>(v: &T) -> J {
        J::from(serde_json::to_value(v).unwrap_or(Value::Null))
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let J::Bool(b) = self { Some(*b) } else { None }
    }

    /// `serde_json::Value` with floats rounded to 6 decimals (= `tidy(json!(...))`)
    pub fn into_value(self) -> Value {
        match self {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(b),
            J::U(u) => Value::Number(u.into()),
            J::I(i) => Value::Number(i.into()),
            J::F(f) => Number::from_f64(round6(f)).map(Value::Number).unwrap_or(Value::Null),
            J::S(s) => Value::String(s.into_owned()),
            J::A(a) => Value::Array(a.into_iter().map(J::into_value).collect()),
            J::O(o) => {
                let mut m = Map::new();
                for (k, v) in o {
                    m.insert(k.into_owned(), v.into_value());
                }
                Value::Object(m)
            }
        }
    }

    /// compact JSON text with sorted keys and 6-decimal floats (= `to_string(&self.into_value())`)
    pub fn to_json(mut self) -> String {
        let mut out = Vec::with_capacity(16 * 1024);
        self.write(&mut out);
        // only valid UTF-8 is written
        String::from_utf8(out).unwrap()
    }

    fn write(&mut self, out: &mut Vec<u8>) {
        use serde_json::ser::Formatter;
        match self {
            J::Null => out.extend_from_slice(b"null"),
            J::Bool(true) => out.extend_from_slice(b"true"),
            J::Bool(false) => out.extend_from_slice(b"false"),
            J::U(u) => {
                let _ = serde_json::ser::CompactFormatter.write_u64(out, *u);
            }
            J::I(i) => {
                let _ = serde_json::ser::CompactFormatter.write_i64(out, *i);
            }
            J::F(f) => {
                let r = round6(*f);
                if r.is_finite() {
                    let _ = serde_json::ser::CompactFormatter.write_f64(out, r);
                } else {
                    out.extend_from_slice(b"null");
                }
            }
            J::S(Cow::Borrowed(s)) => write_static_str(out, s),
            J::S(s) => write_str(out, s),
            J::A(a) => {
                out.push(b'[');
                for (n, v) in a.iter_mut().enumerate() {
                    if n > 0 {
                        out.push(b',');
                    }
                    v.write(out);
                }
                out.push(b']');
            }
            J::O(o) => {
                o.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                out.push(b'{');
                for (n, (k, v)) in o.iter_mut().enumerate() {
                    if n > 0 {
                        out.push(b',');
                    }
                    match k {
                        Cow::Borrowed(k) => write_static_str(out, k),
                        Cow::Owned(k) => write_str(out, k),
                    }
                    out.push(b':');
                    v.write(out);
                }
                out.push(b'}');
            }
        }
    }
}

/// string literals of this crate (keys and enum-like values) never need escaping (checked in debug builds)
#[inline]
fn write_static_str(out: &mut Vec<u8>, s: &str) {
    debug_assert!(!s.bytes().any(|b| b < 0x20 || b == b'"' || b == b'\\'));
    out.push(b'"');
    out.extend_from_slice(s.as_bytes());
    out.push(b'"');
}

/// JSON string with serde_json's escaping
fn write_str(out: &mut Vec<u8>, s: &str) {
    if !s.bytes().any(|b| b < 0x20 || b == b'"' || b == b'\\') {
        out.push(b'"');
        out.extend_from_slice(s.as_bytes());
        out.push(b'"');
    } else {
        let _ = serde_json::to_writer(&mut *out, s);
    }
}

impl std::ops::Index<&str> for J {
    type Output = J;
    fn index(&self, k: &str) -> &J {
        static NULL: J = J::Null;
        match self {
            J::O(o) => o.iter().find(|e| e.0 == k).map(|e| &e.1).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

/// like `Value`'s IndexMut: a missing key is inserted as Null
impl std::ops::IndexMut<&str> for J {
    fn index_mut(&mut self, k: &str) -> &mut J {
        match self {
            J::O(o) => {
                let p = match o.iter().position(|e| e.0 == k) {
                    Some(p) => p,
                    None => {
                        o.push((Cow::Owned(k.to_string()), J::Null));
                        o.len() - 1
                    }
                };
                &mut o[p].1
            }
            _ => panic!("J index on a non-object"),
        }
    }
}

// ---- conversions (same JSON as serde_json::to_value)
impl From<f64> for J {
    #[inline]
    fn from(v: f64) -> J {
        if v.is_finite() { J::F(v) } else { J::Null }
    }
}
impl From<f32> for J {
    #[inline]
    fn from(v: f32) -> J {
        J::from(v as f64)
    }
}
macro_rules! from_uint { ($($t:ty),*) => { $(impl From<$t> for J { #[inline] fn from(v: $t) -> J { J::U(v as u64) } })* } }
macro_rules! from_int { ($($t:ty),*) => { $(impl From<$t> for J { #[inline] fn from(v: $t) -> J { if v < 0 { J::I(v as i64) } else { J::U(v as u64) } } })* } }
from_uint!(u8, u16, u32, u64, usize);
from_int!(i8, i16, i32, i64, isize);
impl From<bool> for J {
    #[inline]
    fn from(v: bool) -> J {
        J::Bool(v)
    }
}
impl From<&'static str> for J {
    #[inline]
    fn from(v: &'static str) -> J {
        J::S(Cow::Borrowed(v))
    }
}
impl From<String> for J {
    #[inline]
    fn from(v: String) -> J {
        J::S(Cow::Owned(v))
    }
}
impl From<&String> for J {
    #[inline]
    fn from(v: &String) -> J {
        J::S(Cow::Owned(v.clone()))
    }
}
impl<T: Into<J>> From<Option<T>> for J {
    #[inline]
    fn from(v: Option<T>) -> J {
        match v {
            Some(x) => x.into(),
            None => J::Null,
        }
    }
}
impl<T: Into<J>> From<Vec<T>> for J {
    #[inline]
    fn from(v: Vec<T>) -> J {
        J::A(v.into_iter().map(Into::into).collect())
    }
}
impl From<&Vec<String>> for J {
    fn from(v: &Vec<String>) -> J {
        J::A(v.iter().map(J::from).collect())
    }
}
impl From<Value> for J {
    fn from(v: Value) -> J {
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(b),
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    J::U(u)
                } else if let Some(i) = n.as_i64() {
                    J::I(i)
                } else {
                    J::from(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => J::S(Cow::Owned(s)),
            Value::Array(a) => J::A(a.into_iter().map(J::from).collect()),
            Value::Object(o) => J::O(o.into_iter().map(|(k, v)| (Cow::Owned(k), J::from(v))).collect()),
        }
    }
}

/// `json!`-style constructor producing a [`J`] (adapted from serde_json's `json!` macro, MIT/Apache-2.0)
#[macro_export]
#[doc(hidden)]
macro_rules! jx {
    ($($json:tt)+) => { $crate::jx_internal!($($json)+) };
}

#[macro_export]
#[doc(hidden)]
macro_rules! jx_internal {
    (@array [$($elems:expr,)*]) => { vec![$($elems,)*] };
    (@array [$($elems:expr),*]) => { vec![$($elems),*] };
    (@array [$($elems:expr,)*] null $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!(null)] $($rest)*) };
    (@array [$($elems:expr,)*] true $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!(true)] $($rest)*) };
    (@array [$($elems:expr,)*] false $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!(false)] $($rest)*) };
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!([$($array)*])] $($rest)*) };
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!({$($map)*})] $($rest)*) };
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!($next),] $($rest)*) };
    (@array [$($elems:expr,)*] $last:expr) => { $crate::jx_internal!(@array [$($elems,)* $crate::jx_internal!($last)]) };
    (@array [$($elems:expr),*] , $($rest:tt)*) => { $crate::jx_internal!(@array [$($elems,)*] $($rest)*) };

    (@object $object:ident () () ()) => {};
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        $object.push((std::borrow::Cow::from($($key)+), $value));
        $crate::jx_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        $object.push((std::borrow::Cow::from($($key)+), $value));
    };
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!(null)) $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!(true)) $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!(false)) $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!([$($array)*])) $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!({$($map)*})) $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!($value)) , $($rest)*); };
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => { $crate::jx_internal!(@object $object [$($key)+] ($crate::jx_internal!($value))); };
    (@object $object:ident ($key:tt) (: $($rest:tt)*) (: $($copy:tt)*)) => {};
    (@object $object:ident ($($key:tt)+) (: $($unexpected:tt)+) $copy:tt) => {};
    (@object $object:ident ($($key:tt)*) (: $($rest:tt)*) $copy:tt) => {};
    (@object $object:ident ($($key:tt)*) (($key2:expr) : $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object ($key2) (: $($rest)*) (: $($rest)*)); };
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => { $crate::jx_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*)); };

    (null) => { $crate::jout::J::Null };
    (true) => { $crate::jout::J::Bool(true) };
    (false) => { $crate::jout::J::Bool(false) };
    ([]) => { $crate::jout::J::A(vec![]) };
    ([ $($tt:tt)+ ]) => { $crate::jout::J::A($crate::jx_internal!(@array [] $($tt)+)) };
    ({}) => { $crate::jout::J::O(Vec::new()) };
    ({ $($tt:tt)+ }) => {
        $crate::jout::J::O({
            let mut object: Vec<(std::borrow::Cow<'static, str>, $crate::jout::J)> = Vec::with_capacity(8);
            $crate::jx_internal!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };
    ($other:expr) => { $crate::jout::J::from($other) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jx;

    #[test]
    fn text_matches_serde_json() {
        let mk = || {
            let mut o = jx!({"b": 1.23456789, "a": [1, -2, 3u64, null, true, String::from("x\"y\n\u{1}")], "c": {"z": f64::NAN, "y": -0.0000001, "x": 1e300},
                            "d": Some(2.5), "e": None::<f64>, "f": 1e-7, "g": 0.1 + 0.2});
            o["h"] = jx!(7usize);
            o["b"] = jx!(2.0);
            o.insert("i", jx!(-5i32));
            o
        };
        assert_eq!(mk().to_json(), serde_json::to_string(&mk().into_value()).unwrap());
    }

    /// `&'static str` values are written without escaping: every string literal in stats.rs (the only module that
    /// builds J trees) must be escape-free, i.e. contain no backslash, quote (raw strings) or line break
    #[test]
    fn stats_literals_need_no_escaping() {
        let src = include_str!("stats.rs");
        assert!(!src.contains('\\'), "stats.rs: a backslash would need escaping");
        assert!(!src.contains("r#\"") && !src.contains("'\"'"), "stats.rs: raw strings / quote chars not supported by this check");
        let mut in_str = false;
        for c in src.chars() {
            if c == '"' {
                in_str = !in_str;
            } else if in_str && (c as u32) < 0x20 {
                panic!("stats.rs: control character / line break inside a string literal");
            }
        }
    }
}
