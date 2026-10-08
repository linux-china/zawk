//! Comparisons involving strings, following awk's "strnum" rules.
//!
//! In awk, strings that come from input (fields, getline, split, ARGV, ENVIRON, -v assignments)
//! are "strnums": if they look like a number, they compare numerically with numbers and other
//! numeric strnums. Other strings (string constants, results of concatenation or string
//! functions) always compare as strings; a number compared with such a string is converted to a
//! string first. An uninitialized value compares as "" with strings and as 0 with numbers.
//!
//! Whether a string operand may be a strnum is decided statically (see `strnum_analysis`), and
//! passed to the functions here as a `StrKind`.
use crate::runtime::{convert, Float, Int, Str};

/// How a string operand of a comparison behaves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StrKind {
    /// Always compares as a string.
    Str = 0,
    /// May come from input: numeric if it looks like a number.
    Strnum = 1,
    /// An uninitialized value (converted to ""): compares as 0 with numeric values.
    Uninit = 2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum CmpOp {
    Lt = 0,
    Lte = 1,
    Gt = 2,
    Gte = 3,
    Eq = 4,
}

impl CmpOp {
    fn from_code(c: Int) -> CmpOp {
        match c {
            0 => CmpOp::Lt,
            1 => CmpOp::Lte,
            2 => CmpOp::Gt,
            3 => CmpOp::Gte,
            _ => CmpOp::Eq,
        }
    }
    fn apply<T: PartialOrd + ?Sized>(self, l: &T, r: &T) -> bool {
        match self {
            CmpOp::Lt => l < r,
            CmpOp::Lte => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Gte => l >= r,
            CmpOp::Eq => l == r,
        }
    }
}

impl StrKind {
    fn from_code(c: Int) -> StrKind {
        match c {
            0 => StrKind::Str,
            1 => StrKind::Strnum,
            _ => StrKind::Uninit,
        }
    }
    /// The numeric value of an operand of this kind, if it compares numerically.
    fn numeric_value(self, bs: &[u8]) -> Option<Float> {
        match self {
            StrKind::Str => None,
            StrKind::Strnum => looks_numeric(bs),
            StrKind::Uninit => Some(0.0),
        }
    }
}

/// Pack the parameters of a comparison into a single integer, so they can be passed as an
/// immediate to generated code.
pub(crate) fn encode(op: CmpOp, k1: StrKind, k2: StrKind) -> Int {
    op as Int | (k1 as Int) << 4 | (k2 as Int) << 8
}

fn decode(code: Int) -> (CmpOp, StrKind, StrKind) {
    (
        CmpOp::from_code(code & 0xf),
        StrKind::from_code((code >> 4) & 0xf),
        StrKind::from_code((code >> 8) & 0xf),
    )
}

/// Parse `bs` as a number if it looks like one by awk's rules: optional surrounding blanks, an
/// optional sign, and a decimal number with an optional exponent (or, as in gawk, a signed "inf"
/// or "nan"). Hexadecimal and other forms do not count.
pub(crate) fn looks_numeric(bs: &[u8]) -> Option<Float> {
    let is_blank = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');
    let start = bs.iter().position(|b| !is_blank(b))?;
    let end = bs.iter().rposition(|b| !is_blank(b))? + 1;
    let s = &bs[start..end];
    let (sign, body) = match s.first() {
        Some(b'+') | Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    if sign && (body.eq_ignore_ascii_case(b"inf") || body.eq_ignore_ascii_case(b"nan")) {
        return std::str::from_utf8(s).ok()?.parse().ok();
    }
    let digits = |bs: &[u8]| bs.iter().take_while(|b| b.is_ascii_digit()).count();
    let int_digits = digits(body);
    let mut i = int_digits;
    let mut frac_digits = 0;
    if body.get(i) == Some(&b'.') {
        frac_digits = digits(&body[i + 1..]);
        i += 1 + frac_digits;
    }
    if int_digits + frac_digits == 0 {
        return None;
    }
    if matches!(body.get(i), Some(b'e') | Some(b'E')) {
        let mut j = i + 1;
        if matches!(body.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        let exp_digits = digits(&body[j..]);
        if exp_digits == 0 {
            return None;
        }
        i = j + exp_digits;
    }
    if i != body.len() {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

/// Compare two strings; `code` is `encode(op, kind of l, kind of r)`.
pub(crate) fn cmp_str_str(l: &Str, r: &Str, code: Int) -> Int {
    let (op, lk, rk) = decode(code);
    l.with_bytes(|lb| {
        r.with_bytes(|rb| {
            // Only parse when both sides may compare numerically.
            if lk != StrKind::Str && rk != StrKind::Str {
                if let (Some(lf), Some(rf)) = (lk.numeric_value(lb), rk.numeric_value(rb)) {
                    return op.apply(&lf, &rf);
                }
            }
            op.apply(lb, rb)
        })
    }) as Int
}

/// Compare a string with a number; `code` is `str_num_code(..)`.
pub(crate) fn cmp_str_num(s: &Str, n: Float, code: Int) -> Int {
    let (op, sk, _) = decode(code);
    let str_on_right = code & (1 << 12) != 0;
    s.with_bytes(|sb| {
        if let Some(sf) = sk.numeric_value(sb) {
            return if str_on_right {
                op.apply(&n, &sf)
            } else {
                op.apply(&sf, &n)
            };
        }
        let ns: Str = convert::<Float, Str>(n);
        ns.with_bytes(|nb| {
            if str_on_right {
                op.apply(nb, sb)
            } else {
                op.apply(sb, nb)
            }
        })
    }) as Int
}

/// Flag for `cmp_str_num` codes: the string is the right operand.
const STR_ON_RIGHT: Int = 1 << 12;

/// The `code` argument of `cmp_str_num`.
pub(crate) fn str_num_code(op: CmpOp, s_kind: StrKind, str_on_right: bool) -> Int {
    encode(op, s_kind, StrKind::Str) | if str_on_right { STR_ON_RIGHT } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_strings() {
        for (s, v) in [
            ("10", 10.0),
            (" +10 ", 10.0),
            ("-1.5", -1.5),
            (".5", 0.5),
            ("5.", 5.0),
            ("1e3", 1000.0),
            ("1E-2\n", 0.01),
        ] {
            assert_eq!(looks_numeric(s.as_bytes()), Some(v), "{:?}", s);
        }
        assert!(looks_numeric(b"+inf").unwrap().is_infinite());
        for s in ["", " ", "abc", "10abc", "0x1A", "1e", ".", "+", "inf", "1 2", "--1"] {
            assert_eq!(looks_numeric(s.as_bytes()), None, "{:?}", s);
        }
    }

    #[test]
    fn comparisons() {
        use CmpOp::*;
        use StrKind::{Strnum, Uninit};
        let s = |x: &'static str| Str::from(x);
        #[allow(non_upper_case_globals)]
        const Plain: StrKind = StrKind::Str;
        // Two numeric strnums compare numerically; otherwise as strings.
        assert_eq!(cmp_str_str(&s("10"), &s("9"), encode(Gt, Strnum, Strnum)), 1);
        assert_eq!(cmp_str_str(&s("10"), &s("9"), encode(Gt, Strnum, Plain)), 0);
        assert_eq!(cmp_str_str(&s("abc"), &s("9"), encode(Gt, Strnum, Strnum)), 1);
        assert_eq!(cmp_str_str(&s("1e1"), &s("10"), encode(Eq, Strnum, Strnum)), 1);
        // Uninitialized values are "" or 0.
        assert_eq!(cmp_str_str(&s(""), &s(""), encode(Eq, Uninit, Plain)), 1);
        assert_eq!(cmp_str_str(&s(""), &s("0"), encode(Eq, Uninit, Strnum)), 1);
        assert_eq!(cmp_str_str(&s(""), &s("0"), encode(Eq, Uninit, Plain)), 0);
        // Strings and numbers.
        assert_eq!(cmp_str_num(&s("10"), 9.0, str_num_code(Gt, Strnum, false)), 1);
        assert_eq!(cmp_str_num(&s("abc"), 5.0, str_num_code(Gt, Strnum, false)), 1);
        assert_eq!(cmp_str_num(&s("10"), 9.0, str_num_code(Gt, Plain, false)), 0);
        // 9 < "10"
        assert_eq!(cmp_str_num(&s("10"), 9.0, str_num_code(Lt, Strnum, true)), 1);
    }
}
