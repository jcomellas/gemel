//! Unison's glob/regex engine used by ignore patterns.
//!
//! Ports Unison `src/ubase/rx.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Unison matches ignore patterns with its own engine: POSIX-style regular
//! expressions ([`Rx::regex`]) and shell globs ([`Rx::glob`], [`Rx::globx`]),
//! compiled to a DFA built lazily from Brzozowski derivatives. The engine is
//! ported as is (rather than translated to the `regex` crate), so that the
//! same patterns match the same names. Matching is on bytes.
//!
//! One deliberate difference concerns `^` and `$`, which match at line
//! boundaries. OCaml memoises derivatives and nullability in process-wide
//! tables whose keys leave out the line position, and keys DFA states by
//! expression alone, so a state reached after some input can inherit the
//! "beginning of line" position of an identical earlier state. Here the
//! position is part of every key. The visible effect, in the test vectors,
//! is on [`Rx::match_substring`] with a pattern starting with `^`: in OCaml
//! the `^` then matches after any prefix (so `^a` is found in `"ba"`).
//! Unison never calls `match_substring`, and `match_string` and
//! `match_prefix` agree with OCaml on every vector, anchored ones included.
//! Globs never use anchors.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

// ---- Character sets: sorted, disjoint, non-adjacent intervals ----

type CSet = Vec<(i32, i32)>;

fn cunion(l: &[(i32, i32)], l2: &[(i32, i32)]) -> CSet {
    match (l, l2) {
        (_, []) => l.to_vec(),
        ([], _) => l2.to_vec(),
        ([(c1, c2), r @ ..], [(d1, d2), r2 @ ..]) => {
            let (c1, c2, d1, d2) = (*c1, *c2, *d1, *d2);
            if c2 + 1 < d1 {
                [vec![(c1, c2)], cunion(r, l2)].concat()
            } else if d2 + 1 < c1 {
                [vec![(d1, d2)], cunion(l, r2)].concat()
            } else if c2 < d2 {
                cunion(r, &[&[(c1.min(d1), d2)], r2].concat())
            } else {
                cunion(&[&[(c1.min(d1), c2)], r].concat(), r2)
            }
        }
    }
}

fn cinter(l: &[(i32, i32)], l2: &[(i32, i32)]) -> CSet {
    match (l, l2) {
        (_, []) | ([], _) => Vec::new(),
        ([(c1, c2), r @ ..], [(d1, d2), r2 @ ..]) => {
            let (c1, c2, d1, d2) = (*c1, *c2, *d1, *d2);
            if c2 < d1 {
                cinter(r, l2)
            } else if d2 < c1 {
                cinter(l, r2)
            } else if c2 < d2 {
                [vec![(c1.max(d1), c2)], cinter(r, l2)].concat()
            } else {
                [vec![(c1.max(d1), d2)], cinter(l, r2)].concat()
            }
        }
    }
}

fn cnegate(mi: i32, ma: i32, l: &[(i32, i32)]) -> CSet {
    match l {
        [] => {
            if mi <= ma {
                vec![(mi, ma)]
            } else {
                Vec::new()
            }
        }
        [(c1, _), ..] if ma < *c1 => {
            if mi <= ma {
                vec![(mi, ma)]
            } else {
                Vec::new()
            }
        }
        [(c1, _), ..] if mi < *c1 => [vec![(mi, c1 - 1)], cnegate(*c1, ma, l)].concat(),
        [(_, c2), r @ ..] => cnegate(mi.max(c2 + 1), ma, r),
    }
}

fn csingle(c: u8) -> CSet {
    vec![(c.into(), c.into())]
}

fn cadd(c: u8, l: &[(i32, i32)]) -> CSet {
    cunion(&csingle(c), l)
}

fn cseq(c: u8, c2: u8) -> CSet {
    let (i, j) = (i32::from(c), i32::from(c2));
    if i <= j { vec![(i, j)] } else { vec![(j, i)] }
}

fn ctrans(o: i32, l: &[(i32, i32)]) -> CSet {
    l.iter()
        .filter(|&&(c1, c2)| !(c2 + o < 0 || c1 + o > 255))
        .map(|&(c1, c2)| (c1 + o, c2 + o))
        .collect()
}

const CANY: &[(i32, i32)] = &[(0, 255)];

// ---- Regular expressions as written ----

#[derive(Debug, Clone, PartialEq, Eq)]
enum Regexp {
    Set(CSet),
    Sequence(Vec<Regexp>),
    Alternative(Vec<Regexp>),
    Repeat(Box<Regexp>, i64, Option<i64>),
    BegOfLine,
    EndOfLine,
    Intersection(Vec<Regexp>),
    Difference(Box<Regexp>, Box<Regexp>),
}

fn uppercase() -> CSet {
    cunion(&cseq(b'A', b'Z'), &cunion(&cseq(192, 214), &cseq(216, 222)))
}

fn case_insens(r: &Regexp, upper: &CSet, lower: &CSet) -> Regexp {
    let ci = |r: &Regexp| case_insens(r, upper, lower);
    match r {
        Regexp::Set(s) => Regexp::Set(cunion(
            s,
            &cunion(
                &ctrans(32, &cinter(s, upper)),
                &ctrans(-32, &cinter(s, lower)),
            ),
        )),
        Regexp::Sequence(l) => Regexp::Sequence(l.iter().map(ci).collect()),
        Regexp::Alternative(l) => Regexp::Alternative(l.iter().map(ci).collect()),
        Regexp::Repeat(r, i, j) => Regexp::Repeat(Box::new(ci(r)), *i, *j),
        Regexp::BegOfLine | Regexp::EndOfLine => r.clone(),
        Regexp::Intersection(l) => Regexp::Intersection(l.iter().map(ci).collect()),
        Regexp::Difference(a, b) => Regexp::Difference(Box::new(ci(a)), Box::new(ci(b))),
    }
}

/// Why a pattern was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxError {
    /// Malformed pattern (`Rx.Parse_error`).
    ParseError,
    /// Collating elements and character classes (`[[:alpha:]]`, `[[=a=]]`),
    /// and multi-character collating symbols (`Rx.Not_supported`).
    NotSupported,
    /// A `globx` pattern ending in a backslash, on which OCaml's brace
    /// expansion raises `Invalid_argument`.
    InvalidArgument,
}

impl fmt::Display for RxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RxError::ParseError => "malformed pattern",
            RxError::NotSupported => "unsupported pattern syntax",
            RxError::InvalidArgument => "pattern ends with a backslash",
        })
    }
}

impl std::error::Error for RxError {}

type Result<T> = std::result::Result<T, RxError>;

/// A cursor over a pattern.
struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn eos(&self) -> bool {
        self.i == self.s.len()
    }
    fn test(&self, c: u8) -> bool {
        !self.eos() && self.s[self.i] == c
    }
    fn accept(&mut self, c: u8) -> bool {
        let r = self.test(c);
        if r {
            self.i += 1;
        }
        r
    }
    fn get(&mut self) -> u8 {
        let r = self.s[self.i];
        self.i += 1;
        r
    }
}

/// OCaml's 63-bit `int` arithmetic.
fn wrap63(n: i64) -> i64 {
    (n << 1) >> 1
}

/// `Rx.parse`: POSIX-style syntax (`|`, `*`, `+`, `?`, `{i}`, `{i,}`,
/// `{i,j}`, `[...]`, `.`, `^`, `$`, `\` escapes).
fn parse(s: &[u8]) -> Result<Regexp> {
    struct P<'a>(Cursor<'a>);

    impl P<'_> {
        fn regexp(&mut self) -> Result<Regexp> {
            let mut left = self.branch()?;
            while self.0.accept(b'|') {
                left = Regexp::Alternative(vec![left, self.branch()?]);
            }
            Ok(left)
        }
        fn branch(&mut self) -> Result<Regexp> {
            let mut left = self.piece()?;
            while !(self.0.eos() || self.0.test(b'|') || self.0.test(b')')) {
                left = Regexp::Sequence(vec![left, self.piece()?]);
            }
            Ok(left)
        }
        fn piece(&mut self) -> Result<Regexp> {
            let r = self.atom()?;
            if self.0.accept(b'*') {
                return Ok(Regexp::Repeat(Box::new(r), 0, None));
            }
            if self.0.accept(b'+') {
                return Ok(Regexp::Repeat(Box::new(r), 1, None));
            }
            if self.0.accept(b'?') {
                return Ok(Regexp::Alternative(vec![Regexp::Sequence(vec![]), r]));
            }
            if self.0.accept(b'{') {
                return match self.integer()? {
                    Some(i) => {
                        let j = if self.0.accept(b',') {
                            self.integer()?
                        } else {
                            Some(i)
                        };
                        if !self.0.accept(b'}') {
                            return Err(RxError::ParseError);
                        }
                        if j.is_some_and(|j| j < i) {
                            return Err(RxError::ParseError);
                        }
                        Ok(Regexp::Repeat(Box::new(r), i, j))
                    }
                    None => {
                        self.0.i -= 1;
                        Ok(r)
                    }
                };
            }
            Ok(r)
        }
        fn atom(&mut self) -> Result<Regexp> {
            let c = &mut self.0;
            if c.accept(b'.') {
                return Ok(Regexp::Set(CANY.to_vec()));
            }
            if c.accept(b'(') {
                let r = self.regexp()?;
                if !self.0.accept(b')') {
                    return Err(RxError::ParseError);
                }
                return Ok(r);
            }
            if c.accept(b'^') {
                return Ok(Regexp::BegOfLine);
            }
            if c.accept(b'$') {
                return Ok(Regexp::EndOfLine);
            }
            if c.accept(b'[') {
                return Ok(Regexp::Set(if self.0.accept(b'^') {
                    cnegate(0, 255, &self.bracket(Vec::new())?)
                } else {
                    self.bracket(Vec::new())?
                }));
            }
            if c.accept(b'\\') {
                if c.eos() {
                    return Err(RxError::ParseError);
                }
                return match c.get() {
                    ch @ (b'|' | b'(' | b')' | b'*' | b'+' | b'?' | b'[' | b'.' | b'^' | b'$'
                    | b'{' | b'\\') => Ok(Regexp::Set(csingle(ch))),
                    _ => Err(RxError::ParseError),
                };
            }
            if c.eos() {
                return Err(RxError::ParseError);
            }
            match c.get() {
                b'*' | b'+' | b'?' | b'{' | b'\\' => Err(RxError::ParseError),
                ch => Ok(Regexp::Set(csingle(ch))),
            }
        }
        fn integer(&mut self) -> Result<Option<i64>> {
            let c = &mut self.0;
            if c.eos() {
                return Ok(None);
            }
            let d = c.get();
            if !d.is_ascii_digit() {
                c.i -= 1;
                return Ok(None);
            }
            let mut i = i64::from(d - b'0');
            while !c.eos() {
                let d = c.get();
                if !d.is_ascii_digit() {
                    c.i -= 1;
                    break;
                }
                let next = wrap63(i.wrapping_mul(10).wrapping_add(i64::from(d - b'0')));
                if next < i {
                    return Err(RxError::ParseError);
                }
                i = next;
            }
            Ok(Some(i))
        }
        fn bracket(&mut self, mut s: CSet) -> Result<CSet> {
            loop {
                if !s.is_empty() && self.0.accept(b']') {
                    return Ok(s);
                }
                let c = self.char()?;
                if self.0.accept(b'-') {
                    if self.0.accept(b']') {
                        return Ok(cadd(c, &cadd(b'-', &s)));
                    }
                    let c2 = self.char()?;
                    s = cunion(&cseq(c, c2), &s);
                } else {
                    s = cadd(c, &s);
                }
            }
        }
        fn char(&mut self) -> Result<u8> {
            let c = &mut self.0;
            if c.eos() {
                return Err(RxError::ParseError);
            }
            let ch = c.get();
            if ch != b'[' {
                return Ok(ch);
            }
            if c.accept(b'=') || c.accept(b':') {
                return Err(RxError::NotSupported);
            }
            if c.accept(b'.') {
                if c.eos() {
                    return Err(RxError::ParseError);
                }
                let ch = c.get();
                if !c.accept(b'.') {
                    return Err(RxError::NotSupported);
                }
                if !c.accept(b']') {
                    return Err(RxError::ParseError);
                }
                return Ok(ch);
            }
            Ok(ch)
        }
    }

    let mut p = P(Cursor { s, i: 0 });
    let res = p.regexp()?;
    if !p.0.eos() {
        return Err(RxError::ParseError);
    }
    Ok(res)
}

// ---- Globs ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loc {
    Beg,
    BegAny,
    Mid,
}

fn glob_parse(init: Loc, s: &[u8]) -> Result<Regexp> {
    let gany = cnegate(0, 255, &csingle(b'/'));
    let notdot = cnegate(0, 255, &cunion(&csingle(b'.'), &csingle(b'/')));
    let dot = csingle(b'.');
    let any_name = || Regexp::Repeat(Box::new(Regexp::Set(gany.clone())), 0, None);
    // Something that doesn't start with a dot, or nothing.
    let beg_start = || {
        Regexp::Alternative(vec![
            Regexp::Sequence(vec![]),
            Regexp::Sequence(vec![Regexp::Set(notdot.clone()), any_name()]),
        ])
    };
    let beg_start2 = || Regexp::Sequence(vec![Regexp::Set(notdot.clone()), any_name()]);

    let mut c = Cursor { s, i: 0 };
    // `char`: a backslash escapes the next character.
    fn glob_char(c: &mut Cursor<'_>) -> Result<u8> {
        c.accept(b'\\');
        if c.eos() {
            return Err(RxError::ParseError);
        }
        Ok(c.get())
    }
    fn bracket(c: &mut Cursor<'_>, mut s: CSet) -> Result<CSet> {
        loop {
            if !s.is_empty() && c.accept(b']') {
                return Ok(s);
            }
            let ch = glob_char(c)?;
            if c.accept(b'-') {
                if c.accept(b']') {
                    return Ok(cadd(ch, &cadd(b'-', &s)));
                }
                let ch2 = glob_char(c)?;
                s = cunion(&cseq(ch, ch2), &s);
            } else {
                s = cadd(ch, &s);
            }
        }
    }

    let mut beg = init;
    let mut left = Regexp::Sequence(vec![]);
    while !c.eos() {
        let (piece, next) = if c.accept(b'*') {
            if beg != Loc::Mid {
                (Regexp::Sequence(vec![]), Loc::BegAny)
            } else {
                (any_name(), Loc::Mid)
            }
        } else if c.accept(b'?') {
            let p = match beg {
                Loc::Beg => Regexp::Set(notdot.clone()),
                Loc::BegAny => Regexp::Sequence(vec![Regexp::Set(notdot.clone()), any_name()]),
                Loc::Mid => Regexp::Set(gany.clone()),
            };
            (p, Loc::Mid)
        } else if c.accept(b'[') {
            let set = if c.accept(b'^') || c.accept(b'!') {
                cnegate(0, 255, &bracket(&mut c, Vec::new())?)
            } else {
                bracket(&mut c, Vec::new())?
            };
            let p = match beg {
                Loc::Beg => Regexp::Set(cinter(&notdot, &set)),
                Loc::BegAny => Regexp::Alternative(vec![
                    Regexp::Sequence(vec![beg_start(), Regexp::Set(cinter(&notdot, &set))]),
                    Regexp::Sequence(vec![beg_start2(), Regexp::Set(cinter(&dot, &set))]),
                ]),
                Loc::Mid => Regexp::Set(cinter(&gany, &set)),
            };
            (p, Loc::Mid)
        } else {
            let ch = glob_char(&mut c)?;
            let p = if beg != Loc::BegAny {
                Regexp::Set(csingle(ch))
            } else if ch == b'.' {
                Regexp::Sequence(vec![beg_start2(), Regexp::Set(csingle(ch))])
            } else {
                Regexp::Sequence(vec![beg_start(), Regexp::Set(csingle(ch))])
            };
            (p, if ch == b'/' { init } else { Loc::Mid })
        };
        left = Regexp::Sequence(vec![left, piece]);
        beg = next;
    }
    Ok(match beg {
        Loc::Mid | Loc::Beg => left,
        Loc::BegAny => Regexp::Sequence(vec![left, beg_start()]),
    })
}

/// `explode`: expands `{a,b}` alternatives (nested braces allowed).
fn explode(s: &[u8]) -> Result<Vec<Vec<u8>>> {
    fn mul(l: &[Vec<u8>], l2: &[Vec<u8>]) -> Vec<Vec<u8>> {
        l.iter()
            .flat_map(|a| l2.iter().map(move |b| [&a[..], &b[..]].concat()))
            .collect()
    }
    // `String.sub str s (i - s)`, which fails past the end.
    fn sub(str: &[u8], s: usize, i: usize) -> Result<Vec<u8>> {
        str.get(s..i)
            .map(<[u8]>::to_vec)
            .ok_or(RxError::InvalidArgument)
    }
    fn expl(
        str: &[u8],
        inner: bool,
        mut s: usize,
        mut i: usize,
        mut acc: Vec<Vec<u8>>,
        mut beg: Vec<Vec<u8>>,
    ) -> Result<(Vec<Vec<u8>>, usize)> {
        let l = str.len();
        loop {
            if i >= l {
                if inner {
                    return Err(RxError::ParseError);
                }
                return Ok((mul(&beg, &[sub(str, s, i)?]), i));
            }
            match str[i] {
                b'\\' => i += 2,
                b'{' => {
                    let (t, i2) = expl(str, true, i + 1, i + 1, Vec::new(), vec![Vec::new()])?;
                    beg = mul(&beg, &mul(&[sub(str, s, i)?], &t));
                    s = i2;
                    i = i2;
                }
                b',' if inner => {
                    let mut new_acc = mul(&beg, &[sub(str, s, i)?]);
                    new_acc.extend(acc);
                    acc = new_acc;
                    beg = vec![Vec::new()];
                    s = i + 1;
                    i += 1;
                }
                b'}' if inner => {
                    let mut out = mul(&beg, &[sub(str, s, i)?]);
                    out.extend(acc);
                    return Ok((out, i + 1));
                }
                _ => i += 1,
            }
        }
    }
    let (mut l, _) = expl(s, false, 0, 0, Vec::new(), vec![Vec::new()])?;
    l.reverse();
    Ok(l)
}

// ---- Derivatives ----

/// A set of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Bytes([u64; 4]);

impl Bytes {
    fn from_cset(s: &[(i32, i32)]) -> Self {
        let mut b = [0u64; 4];
        for &(lo, hi) in s {
            for c in lo.max(0)..=hi.min(255) {
                b[c as usize / 64] |= 1 << (c % 64);
            }
        }
        Bytes(b)
    }
    fn contains(self, c: u8) -> bool {
        self.0[usize::from(c) / 64] & (1 << (c % 64)) != 0
    }
}

type Term = Arc<Node>;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Node {
    Cst(Bytes),
    Alt(Vec<Term>),
    Seq(Vec<Term>),
    Rep(Term, i64, Option<i64>),
    Bol,
    Eol,
    Int(Vec<Term>),
    Dif(Term, Term),
}

/// The smart constructors of rx.ml, which keep terms in a canonical form
/// (flattened, sorted, deduplicated alternatives) so that the DFA is finite.
struct Builder {
    epsilon: Term,
    empty: Term,
}

impl Builder {
    fn new() -> Self {
        Builder {
            epsilon: Arc::new(Node::Seq(Vec::new())),
            empty: Arc::new(Node::Alt(Vec::new())),
        }
    }

    fn make(&self, n: Node) -> Term {
        Arc::new(n)
    }

    fn is_empty(t: &Term) -> bool {
        matches!(&**t, Node::Alt(l) if l.is_empty())
    }

    fn is_epsilon(t: &Term) -> bool {
        matches!(&**t, Node::Seq(l) if l.is_empty())
    }

    fn seq2(&self, x: &Term, y: &Term) -> Term {
        if Self::is_empty(x) || Self::is_empty(y) {
            return self.empty.clone();
        }
        if Self::is_epsilon(x) {
            return y.clone();
        }
        if Self::is_epsilon(y) {
            return x.clone();
        }
        match (&**x, &**y) {
            (Node::Seq(r), Node::Seq(s)) => self.make(Node::Seq([&r[..], &s[..]].concat())),
            (Node::Seq(r), _) => self.make(Node::Seq([&r[..], std::slice::from_ref(y)].concat())),
            (_, Node::Seq(s)) => self.make(Node::Seq([std::slice::from_ref(x), &s[..]].concat())),
            _ => self.make(Node::Seq(vec![x.clone(), y.clone()])),
        }
    }

    /// `seq'`.
    fn seq_list(&self, l: &[Term]) -> Term {
        match l {
            [] => self.epsilon.clone(),
            [x] => x.clone(),
            _ => self.make(Node::Seq(l.to_vec())),
        }
    }

    fn alt_merge(&self, r: &[Term], s: &[Term]) -> Vec<Term> {
        match (r, s) {
            ([], _) => s.to_vec(),
            (_, []) => r.to_vec(),
            ([a, r2 @ ..], [b, s2 @ ..]) => {
                if let (Node::Seq(xa), Node::Seq(yb)) = (&**a, &**b)
                    && let (Some((x, m)), Some((y, n))) = (xa.split_first(), yb.split_first())
                    && x == y
                {
                    let merged = self.seq2(x, &self.alt2(&self.seq_list(m), &self.seq_list(n)));
                    return self.alt_merge(&[&[merged], r2].concat(), s2);
                }
                match a.cmp(b) {
                    std::cmp::Ordering::Equal => [vec![a.clone()], self.alt_merge(r2, s2)].concat(),
                    std::cmp::Ordering::Less => [vec![a.clone()], self.alt_merge(r2, s)].concat(),
                    std::cmp::Ordering::Greater => {
                        [vec![b.clone()], self.alt_merge(r, s2)].concat()
                    }
                }
            }
        }
    }

    fn alt2(&self, x: &Term, y: &Term) -> Term {
        let c = x.cmp(y);
        if c.is_eq() {
            return x.clone();
        }
        match (&**x, &**y) {
            (Node::Alt(r), _) if r.is_empty() => y.clone(),
            (_, Node::Alt(s)) if s.is_empty() => x.clone(),
            (Node::Alt(r), Node::Alt(s)) => self.make(Node::Alt(self.alt_merge(r, s))),
            (Node::Alt(r), _) if r.len() == 1 && r[0] == *y => y.clone(),
            (_, Node::Alt(s)) if s.len() == 1 && s[0] == *x => x.clone(),
            (Node::Alt(r), _) => self.make(Node::Alt(self.alt_merge(r, std::slice::from_ref(y)))),
            (_, Node::Alt(s)) => self.make(Node::Alt(self.alt_merge(std::slice::from_ref(x), s))),
            (Node::Seq(r), Node::Seq(s)) if !r.is_empty() && !s.is_empty() && r[0] == s[0] => self
                .seq2(
                    &r[0],
                    &self.alt2(&self.seq_list(&r[1..]), &self.seq_list(&s[1..])),
                ),
            _ => self.make(Node::Alt(if c.is_lt() {
                vec![x.clone(), y.clone()]
            } else {
                vec![y.clone(), x.clone()]
            })),
        }
    }

    fn alt(&self, l: Vec<Term>) -> Term {
        l.iter()
            .rev()
            .fold(self.empty.clone(), |acc, t| self.alt2(t, &acc))
    }

    fn rep(&self, x: &Term, i: i64, j: Option<i64>) -> Term {
        if Self::is_empty(x) && i > 0 {
            return self.empty.clone();
        }
        if Self::is_empty(x) || Self::is_epsilon(x) {
            return self.epsilon.clone();
        }
        match (i, j) {
            (_, Some(0)) => self.epsilon.clone(),
            (0, Some(1)) => self.alt2(&self.epsilon, x),
            (1, Some(1)) => x.clone(),
            _ => self.make(Node::Rep(x.clone(), i, j)),
        }
    }

    fn int2(&self, x: &Term, y: &Term) -> Term {
        let c = x.cmp(y);
        if c.is_eq() {
            return x.clone();
        }
        match (&**x, &**y) {
            (Node::Int(r), _) if r.is_empty() => y.clone(),
            (_, Node::Int(s)) if s.is_empty() => x.clone(),
            (Node::Int(r), Node::Int(s)) => self.make(Node::Int(self.alt_merge(r, s))),
            (Node::Int(r), _) if r.len() == 1 && r[0] == *y => y.clone(),
            (_, Node::Int(s)) if s.len() == 1 && s[0] == *x => x.clone(),
            (Node::Int(r), _) => self.make(Node::Int(self.alt_merge(r, std::slice::from_ref(y)))),
            (_, Node::Int(s)) => self.make(Node::Int(self.alt_merge(std::slice::from_ref(x), s))),
            _ => self.make(Node::Int(if c.is_lt() {
                vec![x.clone(), y.clone()]
            } else {
                vec![y.clone(), x.clone()]
            })),
        }
    }

    /// `int`: note that, as in OCaml, the fold starts from `empty`, so an
    /// intersection always contains `empty` and matches nothing.
    fn int(&self, l: Vec<Term>) -> Term {
        l.iter()
            .rev()
            .fold(self.empty.clone(), |acc, t| self.int2(t, &acc))
    }

    fn dif(&self, x: &Term, y: &Term) -> Term {
        if x == y {
            return self.empty.clone();
        }
        match (&**x, &**y) {
            (Node::Dif(x1, y1), _) => self.dif(x1, &self.alt2(y1, y)),
            (Node::Alt(l), _) if l.is_empty() => self.empty.clone(),
            (_, Node::Alt(l)) if l.is_empty() => x.clone(),
            _ => self.make(Node::Dif(x.clone(), y.clone())),
        }
    }

    fn translate(&self, r: &Regexp) -> Term {
        match r {
            Regexp::Set(s) => self.make(Node::Cst(Bytes::from_cset(s))),
            Regexp::Alternative(l) => self.alt(l.iter().map(|r| self.translate(r)).collect()),
            Regexp::Sequence(_) => self.trans_seq(r, self.epsilon.clone()),
            Regexp::Repeat(r, i, j) => self.rep(&self.translate(r), *i, *j),
            Regexp::BegOfLine => self.make(Node::Bol),
            Regexp::EndOfLine => self.make(Node::Eol),
            Regexp::Intersection(l) => self.int(l.iter().map(|r| self.translate(r)).collect()),
            Regexp::Difference(a, b) => self.dif(&self.translate(a), &self.translate(b)),
        }
    }

    fn trans_seq(&self, r: &Regexp, rem: Term) -> Term {
        match r {
            Regexp::Sequence(l) => l.iter().rev().fold(rem, |rem, r| self.trans_seq(r, rem)),
            _ => self.seq2(&self.translate(r), &rem),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Pos {
    Bol,
    Other,
}

const NEVER: i64 = 0;
const ALWAYS: i64 = -1;
const WHEN_EOL: i64 = 2;

/// `combine`: folds `op` over `f` of each element, stopping at `bot`.
fn combine(
    top: i64,
    bot: i64,
    op: fn(i64, i64) -> i64,
    f: impl Fn(&Term) -> i64,
    l: &[Term],
) -> i64 {
    let mut v = top;
    for a in l {
        let c = f(a);
        if c == bot {
            return c;
        }
        v = op(v, c);
    }
    v
}

/// Whether `x` matches the empty string at `pos`: [`NEVER`], [`ALWAYS`], or
/// with bit [`WHEN_EOL`] set, at the end of a line.
fn contains_epsilon(pos: Pos, x: &Term) -> i64 {
    match &**x {
        Node::Cst(_) => NEVER,
        Node::Alt(l) => combine(NEVER, ALWAYS, |a, b| a | b, |y| contains_epsilon(pos, y), l),
        Node::Seq(l) | Node::Int(l) => {
            combine(ALWAYS, NEVER, |a, b| a & b, |y| contains_epsilon(pos, y), l)
        }
        Node::Rep(_, 0, _) => ALWAYS,
        Node::Rep(y, _, _) => contains_epsilon(pos, y),
        Node::Bol => {
            if pos == Pos::Bol {
                ALWAYS
            } else {
                NEVER
            }
        }
        Node::Eol => WHEN_EOL,
        Node::Dif(y, z) => contains_epsilon(pos, y) & !contains_epsilon(pos, z),
    }
}

impl Builder {
    fn delta_seq(&self, pos: Pos, c: u8, l: &[Term]) -> Term {
        let Some((x, r)) = l.split_first() else {
            return self.empty.clone();
        };
        let rdx = self.seq2(&self.delta(pos, c, x), &self.seq_list(r));
        let eps = contains_epsilon(pos, x);
        if eps & ALWAYS == ALWAYS || (eps & WHEN_EOL == WHEN_EOL && c == b'\n') {
            self.alt2(&rdx, &self.delta_seq(pos, c, r))
        } else {
            rdx
        }
    }

    /// The derivative of `x` by `c`.
    fn delta(&self, pos: Pos, c: u8, x: &Term) -> Term {
        match &**x {
            Node::Cst(s) => {
                if s.contains(c) {
                    self.epsilon.clone()
                } else {
                    self.empty.clone()
                }
            }
            Node::Alt(l) => self.alt(l.iter().map(|y| self.delta(pos, c, y)).collect()),
            Node::Seq(l) => self.delta_seq(pos, c, l),
            Node::Rep(y, 0, None) => self.seq2(&self.delta(pos, c, y), x),
            Node::Rep(y, i, None) => self.seq2(&self.delta(pos, c, y), &self.rep(y, i - 1, None)),
            Node::Rep(y, 0, Some(j)) => {
                self.seq2(&self.delta(pos, c, y), &self.rep(y, 0, Some(j - 1)))
            }
            Node::Rep(y, i, Some(j)) => {
                self.seq2(&self.delta(pos, c, y), &self.rep(y, i - 1, Some(j - 1)))
            }
            Node::Eol | Node::Bol => self.empty.clone(),
            Node::Int(l) => self.int(l.iter().map(|y| self.delta(pos, c, y)).collect()),
            Node::Dif(y, z) => self.dif(&self.delta(pos, c, y), &self.delta(pos, c, z)),
        }
    }
}

// ---- The lazily built DFA ----

const UNKNOWN: u32 = u32::MAX;

struct State {
    desc: Term,
    pos: Pos,
    is_final: bool,
    valid: bool,
    next: Box<[u32; 256]>,
}

struct Dfa {
    builder: Builder,
    states: Vec<State>,
    index: HashMap<(Term, Pos), u32>,
}

impl Dfa {
    fn new(r: &Regexp) -> Self {
        let builder = Builder::new();
        let init = builder.translate(r);
        let mut dfa = Dfa {
            builder,
            states: Vec::new(),
            index: HashMap::new(),
        };
        dfa.find_state(Pos::Bol, init);
        dfa
    }

    fn find_state(&mut self, pos: Pos, desc: Term) -> u32 {
        if let Some(&i) = self.index.get(&(desc.clone(), pos)) {
            return i;
        }
        let i = u32::try_from(self.states.len()).expect("fewer than 2^32 DFA states");
        self.states.push(State {
            valid: !Builder::is_empty(&desc),
            is_final: contains_epsilon(pos, &desc) != 0,
            desc: desc.clone(),
            pos,
            next: Box::new([UNKNOWN; 256]),
        });
        self.index.insert((desc, pos), i);
        i
    }

    fn next(&mut self, st: u32, c: u8) -> u32 {
        let n = self.states[st as usize].next[usize::from(c)];
        if n != UNKNOWN {
            return n;
        }
        let State { desc, pos, .. } = &self.states[st as usize];
        let desc = self.builder.delta(*pos, c, desc);
        let pos = if c == b'\n' { Pos::Bol } else { Pos::Other };
        let n = self.find_state(pos, desc);
        self.states[st as usize].next[usize::from(c)] = n;
        n
    }

    /// `match_str`.
    fn matches(&mut self, s: &[u8]) -> bool {
        let mut st = 0;
        for &c in s {
            st = self.next(st, c);
            if !self.states[st as usize].valid {
                return false;
            }
        }
        self.states[st as usize].is_final
    }

    /// `match_pref`: the longest matching prefix of `s[p..]`.
    fn match_prefix(&mut self, s: &[u8], p: usize) -> Option<usize> {
        let mut longest = self.states[0].is_final.then_some(p);
        let mut st = 0;
        for (i, &c) in s.iter().enumerate().skip(p) {
            st = self.next(st, c);
            let state = &self.states[st as usize];
            if !state.valid {
                break;
            }
            if state.is_final {
                longest = Some(i + 1);
            }
        }
        longest.map(|l| l - p)
    }
}

// ---- The public type ----

/// A compiled pattern (`Rx.t`). Matching builds the DFA on demand; a value
/// can be shared between threads.
pub struct Rx {
    def: Regexp,
    full: OnceLock<Mutex<Dfa>>,
    sub: OnceLock<Mutex<Dfa>>,
}

impl Clone for Rx {
    fn clone(&self) -> Self {
        Rx::wrap(self.def.clone())
    }
}

impl fmt::Debug for Rx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Rx").field(&self.def).finish()
    }
}

impl PartialEq for Rx {
    fn eq(&self, other: &Self) -> bool {
        self.def == other.def
    }
}

impl Rx {
    fn wrap(def: Regexp) -> Rx {
        Rx {
            def,
            full: OnceLock::new(),
            sub: OnceLock::new(),
        }
    }

    /// `Rx.rx`: a POSIX-style regular expression.
    #[doc(alias = "rx")]
    pub fn regex(s: impl AsRef<[u8]>) -> Result<Rx> {
        parse(s.as_ref()).map(Rx::wrap)
    }

    /// `Rx.glob`: a shell glob, where a leading dot must be matched
    /// explicitly.
    pub fn glob(s: impl AsRef<[u8]>) -> Result<Rx> {
        Rx::glob_with(true, s)
    }

    /// `Rx.glob'`: `nodot` says whether a dot at the start of a name must be
    /// matched explicitly.
    pub fn glob_with(nodot: bool, s: impl AsRef<[u8]>) -> Result<Rx> {
        glob_parse(if nodot { Loc::Beg } else { Loc::Mid }, s.as_ref()).map(Rx::wrap)
    }

    /// `Rx.globx`: a glob that also understands `{a,b}`.
    pub fn globx(s: impl AsRef<[u8]>) -> Result<Rx> {
        Rx::globx_with(true, s)
    }

    /// `Rx.globx'`.
    pub fn globx_with(nodot: bool, s: impl AsRef<[u8]>) -> Result<Rx> {
        let parts = explode(s.as_ref())?;
        let globs = parts
            .iter()
            .map(|p| Rx::glob_with(nodot, p))
            .collect::<Result<Vec<_>>>()?;
        Ok(Rx::alt(&globs))
    }

    /// `Rx.str`: matches `s` literally.
    pub fn str(s: impl AsRef<[u8]>) -> Rx {
        Rx::wrap(Regexp::Sequence(
            s.as_ref()
                .iter()
                .map(|&c| Regexp::Set(csingle(c)))
                .collect(),
        ))
    }

    /// `Rx.alt`.
    pub fn alt(l: &[Rx]) -> Rx {
        Rx::wrap(Regexp::Alternative(
            l.iter().map(|r| r.def.clone()).collect(),
        ))
    }

    /// `Rx.seq`.
    pub fn seq(l: &[Rx]) -> Rx {
        Rx::wrap(Regexp::Sequence(l.iter().map(|r| r.def.clone()).collect()))
    }

    /// `Rx.empty`: matches nothing.
    pub fn empty() -> Rx {
        Rx::alt(&[])
    }

    /// `Rx.epsilon`: matches the empty string.
    pub fn epsilon() -> Rx {
        Rx::seq(&[])
    }

    /// `Rx.rep`: between `i` and `j` (or unboundedly many) repetitions.
    ///
    /// # Panics
    ///
    /// If `j < i`, like OCaml's `invalid_arg`.
    pub fn rep(&self, i: u32, j: Option<u32>) -> Rx {
        assert!(j.is_none_or(|j| j >= i), "Rx.rep");
        Rx::wrap(Regexp::Repeat(
            Box::new(self.def.clone()),
            i.into(),
            j.map(i64::from),
        ))
    }

    /// `Rx.rep0`.
    pub fn rep0(&self) -> Rx {
        self.rep(0, None)
    }

    /// `Rx.rep1`.
    pub fn rep1(&self) -> Rx {
        self.rep(1, None)
    }

    /// `Rx.opt`.
    pub fn opt(&self) -> Rx {
        Rx::alt(&[Rx::epsilon(), self.clone()])
    }

    /// `Rx.bol`: the beginning of a line.
    pub fn bol() -> Rx {
        Rx::wrap(Regexp::BegOfLine)
    }

    /// `Rx.eol`: the end of a line.
    pub fn eol() -> Rx {
        Rx::wrap(Regexp::EndOfLine)
    }

    /// `Rx.any`: any byte.
    pub fn any() -> Rx {
        Rx::wrap(Regexp::Set(CANY.to_vec()))
    }

    /// `Rx.notnl`: any byte but a newline.
    pub fn notnl() -> Rx {
        Rx::wrap(Regexp::Set(cnegate(0, 255, &csingle(b'\n'))))
    }

    /// `Rx.set`: any of the bytes of `s`.
    pub fn set(s: impl AsRef<[u8]>) -> Rx {
        Rx::wrap(Regexp::Set(
            s.as_ref()
                .iter()
                .fold(Vec::new(), |acc, &c| cunion(&csingle(c), &acc)),
        ))
    }

    /// `Rx.inter`. As in OCaml, the result matches nothing (see
    /// `Builder::int`); Unison doesn't use it.
    pub fn inter(l: &[Rx]) -> Rx {
        Rx::wrap(Regexp::Intersection(
            l.iter().map(|r| r.def.clone()).collect(),
        ))
    }

    /// `Rx.diff`: matches what `self` matches and `other` doesn't.
    pub fn diff(&self, other: &Rx) -> Rx {
        Rx::wrap(Regexp::Difference(
            Box::new(self.def.clone()),
            Box::new(other.def.clone()),
        ))
    }

    /// `Rx.case_insensitive`: folds ASCII and Latin-1 letters.
    pub fn case_insensitive(&self) -> Rx {
        let upper = uppercase();
        let lower = ctrans(32, &upper);
        Rx::wrap(case_insens(&self.def, &upper, &lower))
    }

    fn full(&self) -> &Mutex<Dfa> {
        self.full.get_or_init(|| Mutex::new(Dfa::new(&self.def)))
    }

    fn lock(m: &Mutex<Dfa>) -> std::sync::MutexGuard<'_, Dfa> {
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `Rx.match_string`: whether the whole of `s` matches.
    pub fn match_string(&self, s: impl AsRef<[u8]>) -> bool {
        Self::lock(self.full()).matches(s.as_ref())
    }

    /// `Rx.match_substring`: whether some substring of `s` matches.
    pub fn match_substring(&self, s: impl AsRef<[u8]>) -> bool {
        let dfa = self.sub.get_or_init(|| {
            let anything = Regexp::Repeat(Box::new(Regexp::Set(CANY.to_vec())), 0, None);
            Mutex::new(Dfa::new(&Regexp::Sequence(vec![
                anything.clone(),
                self.def.clone(),
                anything,
            ])))
        });
        Self::lock(dfa).matches(s.as_ref())
    }

    /// `Rx.match_prefix`: the length of the longest match starting at `p`.
    ///
    /// # Panics
    ///
    /// If `p > s.len()`, like OCaml's `invalid_arg`.
    pub fn match_prefix(&self, s: impl AsRef<[u8]>, p: usize) -> Option<usize> {
        let s = s.as_ref();
        assert!(p <= s.len(), "Rx.rep");
        Self::lock(self.full()).match_prefix(s, p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_hide_dot_files() {
        let g = Rx::glob("*.o").unwrap();
        assert!(g.match_string("a.o"));
        assert!(!g.match_string(".o"));
        assert!(!g.match_string("x/a.o"));
        assert!(Rx::glob_with(false, "*.o").unwrap().match_string(".o"));
        assert!(Rx::globx("*.{c,h}").unwrap().match_string("x.h"));
    }

    #[test]
    fn regexps_and_prefixes() {
        let r = Rx::regex("(a|ab)(c|bcd)").unwrap();
        assert!(r.match_string("abcd"));
        assert!(!r.match_string("abcde"));
        assert_eq!(r.match_prefix("abcdz", 0), Some(4));
        assert_eq!(Rx::regex("a*").unwrap().match_prefix("baa", 1), Some(2));
        assert_eq!(Rx::regex("a{3,2}"), Err(RxError::ParseError));
        assert_eq!(Rx::regex("[[:alpha:]]"), Err(RxError::NotSupported));
    }

    #[test]
    fn anchors_match_at_line_boundaries() {
        let r = Rx::regex("a$\n^b").unwrap();
        assert!(r.match_string("a\nb"));
        assert!(Rx::regex("^a$").unwrap().match_string("a"));
        assert!(!Rx::regex("a^b").unwrap().match_string("ab"));
    }

    #[test]
    fn intersection_matches_nothing_as_in_ocaml() {
        assert!(!Rx::inter(&[Rx::str("a")]).match_string("a"));
        assert!(Rx::any().rep0().diff(&Rx::str("a")).match_string("b"));
    }
}
