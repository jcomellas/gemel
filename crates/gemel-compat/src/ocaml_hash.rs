//! Pre-OCaml-4.00 polymorphic hash (`Uutil.hash`, `Uutil.hash2`) for the value shapes Unison hashes.
//!
//! Ports Unison `src/hash_compat.c`, Unison `src/uutil.ml`.
//!
//! Derived from OCaml (via Unison), Copyright 1996 Institut National de Recherche en
//! Informatique et en Automatique, LGPL-2.1 with the OCaml linking exception.
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Unison hashes archives (`Update.checkArchive`), file properties and,
//! under case-insensitive modes, file names with OCaml 3.x's `Hashtbl.hash`,
//! kept alive in `hash_compat.c` because the results are compared between
//! replicas. The hash walks the *runtime representation* of a value, so this
//! module models that representation with [`Value`]; callers build the
//! `Value` that OCaml would have in memory for the corresponding OCaml type.
//!
//! # Building values
//!
//! - `int`, `bool`, `char` and constant constructors (`None`, `[]`, `false`,
//!   `IdIgnored`, …) are immediates: [`Value::Int`]. `true` is `Int(1)`; the
//!   nth constant constructor of a type is `Int(n)`.
//! - Non-constant constructors are blocks tagged with their index among the
//!   type's non-constant constructors: `Some x` is `Block(0, [x])`, and
//!   `x :: rest` is `Block(0, [x, rest])`.
//! - Tuples and records are `Block(0, fields)`. A constructor whose argument
//!   is a parenthesised tuple (`Loaded of (string * string)`) holds a single
//!   field pointing to a separate tuple block.
//! - `string` is [`Value::String`], `int64` is [`Value::Int64`] and `float`
//!   is [`Value::Double`].

/// The runtime shape of an OCaml value, as far as the hash can see it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An immediate: `int`, `char`, `bool` or a constant constructor.
    Int(i64),
    /// A `string` (a byte string; not necessarily UTF-8).
    String(Vec<u8>),
    /// A boxed `int64` (a custom block).
    Int64(i64),
    /// A boxed `float`.
    Double(f64),
    /// A structured block: tuple, record or non-constant constructor.
    Block(u8, Vec<Value>),
}

impl Value {
    /// `None`.
    pub const NONE: Value = Value::Int(0);

    /// `[]`.
    pub const NIL: Value = Value::Int(0);

    /// A string value.
    pub fn string(s: impl Into<Vec<u8>>) -> Value {
        Value::String(s.into())
    }

    /// `Some v`.
    pub fn some(v: Value) -> Value {
        Value::Block(0, vec![v])
    }

    /// `Some v` or `None`.
    pub fn option(v: Option<Value>) -> Value {
        v.map_or(Value::NONE, Value::some)
    }

    /// A tuple or record.
    pub fn tuple(fields: impl Into<Vec<Value>>) -> Value {
        Value::Block(0, fields.into())
    }

    /// A list, as nested cons cells.
    pub fn list(items: impl IntoIterator<Item = Value, IntoIter: DoubleEndedIterator>) -> Value {
        items
            .into_iter()
            .rev()
            .fold(Value::NIL, |rest, item| Value::Block(0, vec![item, rest]))
    }
}

const ALPHA: u64 = 65599;
const BETA: u64 = 19;
const MASK: u64 = 0x3FFF_FFFF;

struct State {
    accu: u64,
    count: i64,
    limit: i64,
}

impl State {
    fn combine(&mut self, new: u64) {
        self.accu = self.accu.wrapping_mul(ALPHA).wrapping_add(new);
    }

    fn combine_small(&mut self, new: u8) {
        self.accu = self.accu.wrapping_mul(BETA).wrapping_add(new.into());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.combine_small(b);
        }
    }

    // `hash_aux` in hash_compat.c. Recursion depth is bounded by `limit`.
    fn aux(&mut self, v: &Value) {
        self.limit -= 1;
        if self.count < 0 || self.limit < 0 {
            return;
        }
        self.count -= 1;
        match v {
            Value::Int(n) => self.combine(*n as u64),
            Value::String(s) => self.bytes(s),
            // Bytes in little-endian order on every platform.
            Value::Double(x) => self.bytes(&x.to_le_bytes()),
            Value::Int64(n) => self.combine(int64_custom_hash(*n)),
            Value::Block(tag, fields) => {
                self.combine_small(*tag);
                for field in fields.iter().rev() {
                    self.aux(field);
                }
            }
        }
    }

    fn finish(self) -> i64 {
        // The mask keeps the result positive and equal on 32- and 64-bit
        // platforms: the low 30 bits don't depend on the word size.
        (self.accu & MASK) as i64
    }
}

// The `hash` field of the OCaml runtime's custom operations for `int64`
// (runtime/ints.c), which hash_compat.c calls for custom blocks.
fn int64_custom_hash(n: i64) -> u64 {
    let lo = n as u32;
    let hi = (n >> 32) as u32;
    u64::from(hi ^ lo)
}

/// `hash_param count limit v` (`unsn_hash_univ_param`): hashes at most
/// `count` meaningful values while visiting at most `limit` nodes.
pub fn hash_param(count: i64, limit: i64, v: &Value) -> i64 {
    let mut state = State {
        accu: 0,
        count,
        limit,
    };
    state.aux(v);
    state.finish()
}

/// `Uutil.hash`: `hash_param 10 100`.
pub fn hash(v: &Value) -> i64 {
    hash_param(10, 100, v)
}

/// `Uutil.hash` of an OCaml `string`, without building a [`Value`].
pub fn hash_string(s: &[u8]) -> i64 {
    let mut state = State {
        accu: 0,
        count: 0,
        limit: 0,
    };
    state.bytes(s);
    state.finish()
}

/// `Uutil.hash` of an OCaml `int`, without building a [`Value`].
pub fn hash_int(n: i64) -> i64 {
    (n as u64 & MASK) as i64
}

/// `Uutil.hash2`: combines two hashes (or OCaml ints).
pub fn hash2(x: i64, y: i64) -> i64 {
    // OCaml ints wrap at 63 bits rather than 64, which doesn't affect the low
    // 30 bits kept by the mask.
    (17i64.wrapping_mul(x).wrapping_add(257i64.wrapping_mul(y))) & MASK as i64
}

/// `Uutil.Filesize.hash`.
pub fn hash_filesize(size: i64) -> i64 {
    hash2(size, ((size as u64) >> 31) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_paths_match_the_general_hash() {
        for s in [&b""[..], b"a", b"hello", b"\xff\xfe\x00"] {
            assert_eq!(hash_string(s), hash(&Value::string(s)));
        }
        for n in [0, 1, -1, 1 << 30, (1 << 62) - 1, -(1 << 62)] {
            assert_eq!(hash_int(n), hash(&Value::Int(n)));
        }
    }

    #[test]
    fn list_builds_cons_cells_in_order() {
        assert_eq!(
            Value::list([Value::Int(1), Value::Int(2)]),
            Value::Block(
                0,
                vec![
                    Value::Int(1),
                    Value::Block(0, vec![Value::Int(2), Value::NIL])
                ]
            )
        );
    }
}
