use crate::common::{FileSpec, Result};
use command::CommandReader;
// Fixed-seed hasher: keeps `for (k in a)` order reproducible across runs and backends.
pub(crate) type HashMap<K, V> = hashbrown::HashMap<K, V, foldhash::fast::FixedState>;
use regex::bytes::Regex;
use awk_regex::AwkRegex;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::hash::Hash;
use std::io;
use std::iter::FromIterator;
use std::mem;
use std::rc::Rc;
use std::str;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) mod command;
pub(crate) mod awk_regex;
pub(crate) mod regex_memory;
pub(crate) mod compare;
pub(crate) mod numfmt;
pub mod float_parse;
pub mod printf;
pub mod splitter;
pub mod str_impl;
pub mod string_search;
pub mod utf8;
pub mod writers;
pub mod parquet_writer;
pub mod date_time;
pub mod encoding;
pub mod crypto;
pub mod str_escape;
pub mod math_util;
pub(crate) mod sorted_in;
pub(crate) mod array_stats;

pub mod json;
pub mod network;
pub mod kv;
pub mod s3;
pub mod http;
pub mod decompress;
pub mod os_util;
pub mod csv;
pub mod sqlite;
pub mod libsql;
pub mod mysql;
pub mod postgres;
pub mod logging;
pub(crate) mod tls;
pub mod string_util;
pub mod faker;
pub mod html;
pub mod config_util;
pub mod jev;
pub mod jev_functions;

use crate::pushdown::FieldSet;
use splitter::regex::RegexSplitter;

/// Shared tokio runtime for blocking on async builtins (s3, libsql, ...),
/// so per-record calls don't rebuild a runtime each time.
pub(crate) static TOKIO_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
});

pub(crate) use crate::builtins::Variables;
pub use command::run_command;
pub(crate) use command::run_command2;
pub(crate) use float_parse::{
    exact_int_literal, hextoi, parse_hex_literal, parse_int_literal, str_to_int, str_to_num, strtod,
};
pub(crate) use printf::FormatArg;
pub use splitter::{
    batch::{escape_csv, escape_tsv},
    ChainedReader, Line, LineReader,
};
pub use str_impl::{Str, UniqueStr};

/// How the memory of a compiled dynamic regex counts against `regex_memory`. Dynamic regexes can
/// come from input data (`$0 ~ $1`, `split($0, a, $2)`), so the caches are bounded by memory,
/// rather than by count so that programs matching against a few thousand distinct patterns
/// (e.g. from a lookup table) do not recompile on every record. Searching allocates the caches
/// of the regex engines, measured at up to about half the compiled size (more for Unicode
/// classes). Without the allocator's counters, a short pattern like `^us.r123$` is estimated
/// at about 16KB.
const REGEX_COST: EntryCost = EntryCost {
    measured_percent: 150,
    estimate: CostModel {
        base: 12 << 10,
        per_byte: 512,
    },
};

/// How the memory of a `LightMatcher` counts against `regex_memory`: measured while it is built
/// with the engine its first use builds (see `RegexCache::light_matcher`).
const LIGHT_COST: EntryCost = EntryCost {
    measured_percent: 100,
    estimate: CostModel {
        base: 4 << 10,
        per_byte: 256,
    },
};

pub struct RegexCache(
    Registry<AwkRegex>,
    // Field separators that are a single character, which awk treats literally.
    Registry<AwkRegex>,
    // The IGNORECASE setting the regexes in the first and last registries were compiled with,
    // and the registries for the other setting, swapped back in when IGNORECASE changes again:
    // a program that toggles IGNORECASE per record keeps both sets of regexes cached.
    (bool, Registry<AwkRegex>, Registry<awk_regex::LightMatcher>),
    // Dynamic patterns that have not been used often enough yet to be worth compiling into the
    // first registry (see `light_matcher`).
    Registry<awk_regex::LightMatcher>,
    // The eviction round of `regex_memory` the registries last took part in.
    u64,
);

/// How many times a dynamic pattern is used with the cheap matcher (see
/// `awk_regex::LightMatcher`) before compiling it fully: a full regex takes 10–50 times longer to
/// build, but its prefilter and full DFA make matching faster.
const LIGHT_USES: u32 = 4;

impl Default for RegexCache {
    fn default() -> Self {
        RegexCache(
            Registry::with_memory_limit(REGEX_COST),
            Registry::with_memory_limit(REGEX_COST),
            (
                false,
                Registry::with_memory_limit(REGEX_COST),
                Registry::with_memory_limit(LIGHT_COST),
            ),
            Registry::with_memory_limit(LIGHT_COST),
            regex_memory::round(),
        )
    }
}

/// A regex matching the single character `c` literally. IGNORECASE does not apply to it, as in
/// gawk.
fn literal_regex(c: &str) -> Result<AwkRegex> {
    match Regex::new(&regex::escape(c)) {
        Ok(r) => Ok(AwkRegex::new(r)),
        Err(e) => err!("{}", e),
    }
}

/// Whether `pat` is a single character (other than " "), which awk treats literally when used
/// as a field separator, in FS and in split().
fn is_literal_separator(pat: &Str) -> bool {
    pat.with_bytes(|bs| {
        bs != b" " && std::str::from_utf8(bs).is_ok_and(|s| s.chars().count() == 1)
    })
}

/// The blanks that separate fields under the default FS (`" "`). This is the same set the
/// main-input splitter uses (see `splitter::batch`), so a record splits the same way whether it
/// is read as main input, by `getline`, assigned to `$0` or passed to `split()`.
#[inline]
fn is_blank(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// The `(start, end)` byte ranges of the fields of `bs` under the default FS: the maximal runs
/// of non-blanks.
fn blank_fields(bs: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bs.len() && is_blank(bs[i]) {
            i += 1;
        }
        if i == bs.len() {
            return None;
        }
        let start = i;
        while i < bs.len() && !is_blank(bs[i]) {
            i += 1;
        }
        Some((start, i))
    })
}

/// Splits `s` by the default FS, pushing an empty string for fields projected away by
/// `used_fields`.
fn split_blanks<'a>(s: &Str<'a>, used_fields: &FieldSet, mut push: impl FnMut(Str<'a>)) {
    s.with_bytes(|bs| {
        for (i, (start, end)) in blank_fields(bs).enumerate() {
            push(if used_fields.get(i + 1) {
                s.slice_of(bs, start, end)
            } else {
                Str::default()
            });
        }
    })
}

/// The separator byte, when `pat` is a single ASCII character other than `" "` (one that awk
/// treats literally).
fn single_byte_separator(pat: &Str) -> Option<u8> {
    pat.with_bytes(|bs| match bs {
        [b] if *b != b' ' && b.is_ascii() => Some(*b),
        _ => None,
    })
}

/// Splits `s` at every occurrence of the byte `sep`, pushing an empty string for fields
/// projected away by `used_fields`. An empty `s` has no fields.
fn split_byte<'a>(s: &Str<'a>, sep: u8, used_fields: &FieldSet, mut push: impl FnMut(Str<'a>)) {
    s.with_bytes(|bs| {
        if bs.is_empty() {
            return;
        }
        let mut field = |i: usize, start: usize, end: usize| {
            push(if used_fields.get(i) {
                s.slice_of(bs, start, end)
            } else {
                Str::default()
            });
        };
        let mut prev = 0;
        let mut i = 1;
        for pos in memchr::memchr_iter(sep, bs) {
            field(i, prev, pos);
            prev = pos + 1;
            i += 1;
        }
        field(i, prev, bs.len());
    })
}

/// Splits `s` at the matches of `re`, as split() does for a regex separator.
fn split_with<'a>(re: &(impl awk_regex::Search + ?Sized), s: &Str<'a>, used_fields: &FieldSet, mut push: impl FnMut(Str<'a>)) {
    s.split(
        re,
        |s, _| {
            push(s);
            1
        },
        used_fields,
    )
}

type FieldsAndSeps<'a> = (Vec<Str<'a>>, Vec<(Int, Str<'a>)>);

/// The fields and separators of `split(s, arr, re, seps)` for a regex separator `re` (see
/// `RegexCache::split_seps`).
fn split_seps_with<'a>(re: &(impl awk_regex::Search + ?Sized), s: &Str<'a>) -> FieldsAndSeps<'a> {
    let mut flds = Vec::new();
    let mut seps = Vec::new();
    if s.is_empty() {
        return (flds, seps);
    }
    s.with_bytes(|bs| {
        let mut prev = 0;
        for (start, end) in awk_regex::find_iter(re, bs) {
            // As in split(), empty matches do not separate fields.
            if start == end {
                continue;
            }
            flds.push(s.slice(prev, start));
            seps.push((flds.len() as Int, s.slice(start, end)));
            prev = end;
        }
        flds.push(s.slice(prev, bs.len()));
    });
    (flds, seps)
}

/// The fields and separators of `patsplit(s, arr, re, seps)` (see `RegexCache::patsplit_seps`).
fn patsplit_seps_with<'a>(re: &(impl awk_regex::Search + ?Sized), s: &Str<'a>) -> FieldsAndSeps<'a> {
    let mut flds = Vec::new();
    let mut seps = Vec::new();
    if s.is_empty() {
        return (flds, seps);
    }
    s.with_bytes(|bs| {
        let mut prev = 0;
        for (start, end) in awk_regex::find_iter(re, bs) {
            seps.push((flds.len() as Int, s.slice(prev, start)));
            flds.push(s.slice(start, end));
            prev = end;
        }
        seps.push((flds.len() as Int, s.slice(prev, bs.len())));
    });
    (flds, seps)
}

/// Replaces the contents of `arr` with `flds` (indexed from 1) and of `seps` with `sep_list`;
/// returns the number of fields.
fn fill_seps_maps<'a, K1, K2>(
    flds: Vec<Str<'a>>,
    sep_list: Vec<(Int, Str<'a>)>,
    arr: &SharedMap<K1, Str<'a>>,
    seps: &SharedMap<K2, Str<'a>>,
) -> Int
where
    K1: Hash + Eq + From<Int>,
    K2: Hash + Eq + From<Int>,
{
    let n = flds.len() as Int;
    arr.clear();
    for (i, f) in flds.into_iter().enumerate() {
        arr.insert(K1::from(i as Int + 1), f);
    }
    seps.clear();
    for (i, sep) in sep_list {
        seps.insert(K2::from(i), sep);
    }
    n
}

impl RegexCache {
    /// Switch to the regexes compiled with the current IGNORECASE setting, if it changed since the
    /// last lookup, and take part in the eviction rounds of `regex_memory` that started since.
    fn sync(&mut self) {
        let ignore_case = awk_regex::ignore_case();
        if self.2.0 != ignore_case {
            std::mem::swap(&mut self.0, &mut self.2.1);
            std::mem::swap(&mut self.3, &mut self.2.2);
            self.2.0 = ignore_case;
        }
        let round = regex_memory::round();
        if self.4 != round {
            self.4 = round;
            self.0.sync_budget(round);
            self.1.sync_budget(round);
            self.2.1.sync_budget(round);
            self.2.2.sync_budget(round);
            self.3.sync_budget(round);
        }
    }
    pub(crate) fn with_regex<T>(&mut self, pat: &Str, mut f: impl FnMut(&AwkRegex) -> T) -> Result<T> {
        self.sync();
        self.0.get(
            pat,
            |s| match awk_regex::compile(s) {
                Ok(r) => Ok(r.counted()),
                Err(e) => err!("{}", awk_regex::compile_error(s, &e)),
            },
            // eta-expansion required to get this compiling..
            |x| f(x),
        )
    }
    /// Runs `f` with the regex that separates records for `rs`: a single-character `RS` is
    /// matched literally, as in POSIX awk and gawk; longer values are regexes.
    pub(crate) fn with_record_separator<T>(
        &mut self,
        rs: &Str,
        mut f: impl FnMut(&AwkRegex) -> T,
    ) -> Result<T> {
        let single_char =
            rs.with_bytes(|bs| std::str::from_utf8(bs).is_ok_and(|s| s.chars().count() == 1));
        if single_char {
            self.sync();
            self.1.get(rs, literal_regex, |x| f(x))
        } else {
            self.with_regex(rs, f)
        }
    }
    /// Runs `f` with a searcher for the dynamic pattern `pat`, to search `s` (and only `s`, see
    /// `light_matcher`).
    pub(crate) fn with_search<T>(
        &mut self,
        pat: &Str,
        s: &Str,
        f: impl FnOnce(&dyn awk_regex::Search) -> T,
    ) -> Result<T> {
        self.with_search_fallible(pat, s, |re| Ok(f(re)))
    }
    pub(crate) fn with_search_fallible<T>(
        &mut self,
        pat: &Str,
        s: &Str,
        f: impl FnOnce(&dyn awk_regex::Search) -> Result<T>,
    ) -> Result<T> {
        let mut f = Some(f);
        // Literal text (`gsub("eps", "E")`) needs no regex, cached or not.
        let literal = pat.with_bytes(|p| {
            let (kind, lit) = awk_regex::as_literal(p)?;
            Some(f.take().unwrap()(&awk_regex::LiteralSearch::new(kind, lit)))
        });
        if let Some(res) = literal {
            return res;
        }
        self.sync();
        if let Some(re) = self.0.lookup(pat) {
            return f.take().unwrap()(re);
        }
        let light = s.with_bytes(|bs| {
            let m = self.light_matcher(pat, bs)?;
            let f = &mut f;
            if !m.may_match(bs) {
                return Some(f.take().unwrap()(&awk_regex::NoMatch));
            }
            m.with_engine(bs, |e| f.take().unwrap()(e))
        });
        if let Some(res) = light {
            return res;
        }
        // `f` was not called.
        let f = f.take().unwrap();
        self.0.get_fallible(
            pat,
            |s| match awk_regex::compile(s) {
                Ok(r) => Ok(r.counted()),
                Err(e) => err!("{}", awk_regex::compile_error(s, &e)),
            },
            |re| f(re),
        )
    }

    /// The light matcher (see `awk_regex::LightMatcher`) for a dynamic pattern that is not
    /// compiled yet, with this use counted, to search `hay`; `None` once the pattern has been used
    /// `LIGHT_USES` times (it is then compiled fully), or when it does not parse (`compile` then
    /// reports the error). Patterns built from the input are often each used once
    /// (`$0 ~ ("^[a-z]+ " $9)`, `match($0, "^" $1)`), so a new pattern starts with the matcher
    /// that is cheap to build.
    fn light_matcher(&mut self, pat: &Str, hay: &[u8]) -> Option<&mut awk_regex::LightMatcher> {
        let uses = match self.3.lookup(pat) {
            Some(m) => m.uses,
            None => {
                // Measure the memory of the matcher with the engine its first use builds.
                let (m, measured) = regex_memory::measure(|| {
                    let mut m = pat.with_bytes(|bs| str::from_utf8(bs).ok().and_then(awk_regex::LightMatcher::new))?;
                    m.prepare(hay);
                    Some(m)
                });
                self.3.insert(pat, m?, measured);
                0
            }
        };
        if uses >= LIGHT_USES {
            self.3.remove(pat);
            return None;
        }
        let m = self.3.lookup(pat)?;
        m.uses += 1;
        Some(m)
    }

    pub(crate) fn get_line<'a, LR: LineReader>(
        &mut self,
        file: &Str<'a>,
        pat: &Str<'a>,
        reg: &mut FileRead<LR>,
        is_file: bool,
    ) -> Result<Str<'a>> {
        // Keep the read state for the read-error check of this getline (see `FileRead::read_err`):
        // getline then looks the reader up once rather than twice.
        reg.last_read_state = None;
        let (line, state) = if is_file {
            reg.with_file(file, |reader| {
                let line = reader.read_record(pat, self)?;
                Ok((line, reader.read_state()))
            })?
        } else {
            reg.with_cmd(file, |reader| {
                let line = reader.read_record(pat, self)?;
                Ok((line, reader.read_state()))
            })?
        };
        reg.last_read_state = Some((is_file, state));
        Ok(line.upcast())
    }

    // This only gets used if getline is invoked explicitly without an input file argument.
    pub(crate) fn get_line_stdin<'a, LR: LineReader>(
        &mut self,
        pat: &Str<'a>,
        reg: &mut FileRead<LR>,
    ) -> Result<(/* file changed */ bool, Str<'a>)> {
        let (changed, line) = reg.stdin.read_record(pat, self)?;
        Ok((changed, line.upcast()))
    }
    pub(crate) fn get_line_stdin_reuse<LR: LineReader>(
        &mut self,
        pat: &Str,
        reg: &mut FileRead<LR>,
        old_line: &mut LR::Line,
    ) -> Result</*file changed */ bool> {
        // Read into the spare buffer (reusing its allocations), so that the current record is
        // still around if we reach the end of the input: awk keeps the last record ($0, NF and
        // the fields) in END.
        let spare = match &mut reg.spare_line {
            Some(spare) => spare,
            None => {
                let changed = reg.stdin.read_line_reuse(pat, self, old_line)?;
                reg.spare_line = Some(old_line.clone());
                return Ok(changed);
            }
        };
        mem::swap(old_line, spare);
        let changed = reg.stdin.read_line_reuse(pat, self, old_line)?;
        if reg.stdin.read_state() == 0 {
            // End of input: no new record was read.
            mem::swap(old_line, spare);
        }
        Ok(changed)
    }
    fn split_internal<'a>(
        &mut self,
        pat: &Str,
        s: &Str<'a>,
        used_fields: &FieldSet,
        mut push: impl FnMut(Str<'a>),
    ) -> Result<()> {
        if pat.is_empty() {
            // An empty separator splits into characters (as in gawk).
            s.with_bytes(|bs| {
                let mut push_field = |i: usize, start: usize, end: usize| {
                    if used_fields.get(i + 1) {
                        push(s.slice(start, end));
                    } else {
                        push(Str::default());
                    }
                };
                match std::str::from_utf8(bs) {
                    Ok(text) => {
                        for (i, (start, c)) in text.char_indices().enumerate() {
                            push_field(i, start, start + c.len_utf8());
                        }
                    }
                    // Not valid UTF-8: split into bytes.
                    Err(_) => (0..bs.len()).for_each(|i| push_field(i, i, i + 1)),
                }
            });
            return Ok(());
        }
        if pat == &Str::from(" ") {
            // The default FS: fields are separated by runs of blanks (see `is_blank`).
            split_blanks(s, used_fields, push);
            Ok(())
        } else if let Some(sep) = single_byte_separator(pat) {
            split_byte(s, sep, used_fields, push);
            Ok(())
        } else if is_literal_separator(pat) {
            self.sync();
            self.1.get(
                pat,
                literal_regex,
                |re| {
                    s.split(
                        re,
                        |s, _| {
                            push(s);
                            1
                        },
                        used_fields,
                    )
                },
            )
        } else {
            self.with_search(pat, s, |re| split_with(re, s, used_fields, &mut push))
        }
    }
    pub(crate) fn split_regex<'a>(
        &mut self,
        pat: &Str,
        s: &Str<'a>,
        used_fields: &FieldSet,
        v: &mut Vec<Str<'a>>,
    ) -> Result<()> {
        self.split_internal(pat, s, used_fields, |s| v.push(s))
    }

    pub(crate) fn split_regex_intmap<'a>(
        &mut self,
        pat: &Str<'a>,
        s: &Str<'a>,
        m: &IntMap<Str<'a>>,
    ) -> Result<()> {
        let mut i = 0i64;
        let mut m_b = m.0.borrow_mut();
        m_b.clear();
        self.split_internal(pat, s, &FieldSet::all(), |s| {
            i += 1;
            m_b.insert(i, s);
        })
    }

    pub(crate) fn split_regex_strmap<'a>(
        &mut self,
        pat: &Str<'a>,
        s: &Str<'a>,
        m: &StrMap<'a, Str<'a>>,
    ) -> Result<()> {
        let mut i = 0i64;
        let mut m_b = m.0.borrow_mut();
        m_b.clear();
        self.split_internal(pat, s, &FieldSet::all(), |s| {
            i += 1;
            m_b.insert(convert::<i64, Str<'_>>(i), s);
        })
    }

    /// `split_regex_intmap` with a constant-folded regex (never one of the separators that
    /// `split_internal` treats specially).
    pub(crate) fn split_const_intmap<'a>(re: &AwkRegex, s: &Str<'a>, m: &IntMap<Str<'a>>) {
        let mut i = 0i64;
        let mut m_b = m.0.borrow_mut();
        m_b.clear();
        split_with(re, s, &FieldSet::all(), |s| {
            i += 1;
            m_b.insert(i, s);
        })
    }

    /// `split_regex_strmap` with a constant-folded regex.
    pub(crate) fn split_const_strmap<'a>(re: &AwkRegex, s: &Str<'a>, m: &StrMap<'a, Str<'a>>) {
        let mut i = 0i64;
        let mut m_b = m.0.borrow_mut();
        m_b.clear();
        split_with(re, s, &FieldSet::all(), |s| {
            i += 1;
            m_b.insert(convert::<i64, Str<'_>>(i), s);
        })
    }

    /// `split(s, arr, pat, seps)`: the fields of `s` and the separators, as gawk fills `seps`:
    /// `seps[i]` is the separator between field i and field i+1. With the default `" "`, leading
    /// and trailing blanks are kept as `seps[0]` and `seps[n]`; an empty `pat` splits into
    /// characters separated by empty strings.
    pub(crate) fn split_seps<'a>(
        &mut self,
        pat: &Str,
        s: &Str<'a>,
    ) -> Result<(Vec<Str<'a>>, Vec<(Int, Str<'a>)>)> {
        let mut flds = Vec::new();
        let mut seps = Vec::new();
        if s.is_empty() {
            return Ok((flds, seps));
        }
        if pat.is_empty() {
            self.split_internal(pat, s, &FieldSet::all(), |f| flds.push(f))?;
            seps.extend((1..flds.len()).map(|i| (i as Int, Str::default())));
            return Ok((flds, seps));
        }
        if pat == &Str::from(" ") {
            s.with_bytes(|bs| {
                let mut prev = 0;
                for (start, end) in blank_fields(bs) {
                    if start > prev {
                        seps.push((flds.len() as Int, s.slice(prev, start)));
                    }
                    flds.push(s.slice(start, end));
                    prev = end;
                }
                if prev < bs.len() {
                    seps.push((flds.len() as Int, s.slice(prev, bs.len())));
                }
            });
            return Ok((flds, seps));
        }
        if is_literal_separator(pat) {
            self.sync();
            self.1.get(pat, literal_regex, |re| split_seps_with(re, s))
        } else {
            self.with_search(pat, s, |re| split_seps_with(re, s))
        }
    }

    /// `patsplit(s, arr, re, seps)`: the matches of `re` in `s` are the fields, and the text
    /// around them the separators, as gawk fills `seps`: `seps[0]` is the text before the first
    /// field and `seps[i]` the text after field i (both kept even when empty). An empty match is
    /// a field unless it directly follows another match.
    pub(crate) fn patsplit_seps<'a>(
        &mut self,
        pat: &Str,
        s: &Str<'a>,
    ) -> Result<(Vec<Str<'a>>, Vec<(Int, Str<'a>)>)> {
        if s.is_empty() {
            return Ok(Default::default());
        }
        self.with_search(pat, s, |re| patsplit_seps_with(re, s))
    }

    /// `split(s, arr, pat, seps)` (or `patsplit` when `patsplit` is set): replaces the contents
    /// of `arr` with the fields of `s` (indexed from 1) and of `seps` with the separators;
    /// returns the number of fields.
    pub(crate) fn split_seps_maps<'a, K1, K2>(
        &mut self,
        pat: &Str,
        s: &Str<'a>,
        arr: &SharedMap<K1, Str<'a>>,
        seps: &SharedMap<K2, Str<'a>>,
        patsplit: bool,
    ) -> Result<Int>
    where
        K1: Hash + Eq + From<Int>,
        K2: Hash + Eq + From<Int>,
    {
        let (flds, sep_list) =
            if patsplit { self.patsplit_seps(pat, s)? } else { self.split_seps(pat, s)? };
        Ok(fill_seps_maps(flds, sep_list, arr, seps))
    }

    /// `split_seps_maps` with a constant-folded regex (for split(), never one of the separators
    /// that `split_seps` treats specially).
    pub(crate) fn split_seps_maps_const<'a, K1, K2>(
        re: &AwkRegex,
        s: &Str<'a>,
        arr: &SharedMap<K1, Str<'a>>,
        seps: &SharedMap<K2, Str<'a>>,
        patsplit: bool,
    ) -> Int
    where
        K1: Hash + Eq + From<Int>,
        K2: Hash + Eq + From<Int>,
    {
        let (flds, sep_list) =
            if patsplit { patsplit_seps_with(re, s) } else { split_seps_with(re, s) };
        fill_seps_maps(flds, sep_list, arr, seps)
    }

    pub(crate) fn regex_const_match_loc(
        vars: &mut Variables,
        re: &(impl awk_regex::Search + ?Sized),
        s: &Str,
    ) -> Result<Int> {
        Self::store_match_loc(vars, s, |bs| awk_regex::find_at(re, bs, 0))
    }
    /// Sets RSTART and RLENGTH to the match `find` reports in `s` and returns RSTART.
    fn store_match_loc(
        vars: &mut Variables,
        s: &Str,
        find: impl FnOnce(&[u8]) -> Option<(usize, usize)>,
    ) -> Result<Int> {
        use crate::builtins::Variable;
        // RSTART and RLENGTH count chars, as `length()` and `substr()` do.
        let (start, len) = s.with_bytes(|bs| match find(bs) {
            Some((from, to)) => {
                let start = s.chars_before(bs, from) as Int;
                let len = str_impl::char_count(&bs[from..to]) as Int;
                (start + 1, len)
            }
            None => (0, -1),
        });
        vars.store_int(Variable::RSTART, start)?;
        vars.store_int(Variable::RLENGTH, len)?;
        Ok(start)
    }
    /// The leftmost match of `pat` in `s` when `pat` is literal text (see `awk_regex::as_literal`),
    /// found without compiling a regex; `None` when `pat` is not literal.
    fn find_literal(pat: &Str, s: &Str) -> Option<Option<(usize, usize)>> {
        pat.with_bytes(|p| {
            let (kind, lit) = awk_regex::as_literal(p)?;
            Some(s.with_bytes(|bs| awk_regex::find_literal(kind, lit, bs)))
        })
    }
    pub(crate) fn regex_match_loc(
        &mut self,
        vars: &mut Variables,
        pat: &Str,
        s: &Str,
    ) -> Result<Int> {
        if let Some(found) = Self::find_literal(pat, s) {
            return Self::store_match_loc(vars, s, |_| found);
        }
        self.with_search_fallible(pat, s, |re| Self::regex_const_match_loc(vars, re, s))
    }

    /// gawk's `match(s, re, arr)`: like `match(s, re)`, and also clears `arr` and passes the whole
    /// match (group 0) and each participating capture group to `f` as (group, text, start, len),
    /// with the start (1-based) and length counted in chars.
    fn regex_match_captures<'a>(
        &mut self,
        vars: &mut Variables,
        pat: &Str<'a>,
        s: &Str<'a>,
        mut f: impl FnMut(Int, Str<'a>, Int, Int),
    ) -> Result<Int> {
        self.with_search_fallible(pat, s, |re| {
            let groups: Vec<(Int, usize, usize)> = s.with_bytes(|bs| match awk_regex::find_at(re, bs, 0)
                .and_then(|(from, to)| awk_regex::captures_at(re, bs, from, to))
            {
                Some(caps) => caps
                    .iter()
                    .enumerate()
                    .filter_map(|(i, m)| m.map(|(from, to)| (i as Int, from, to)))
                    .collect(),
                None => Vec::new(),
            });
            let (start, len) = s.with_bytes(|bs| {
                for &(i, from, to) in groups.iter() {
                    let start = s.chars_before(bs, from) as Int + 1;
                    let len = str_impl::char_count(&bs[from..to]) as Int;
                    f(i, s.slice(from, to), start, len);
                }
                match groups.first() {
                    Some(&(_, from, to)) => (
                        s.chars_before(bs, from) as Int + 1,
                        str_impl::char_count(&bs[from..to]) as Int,
                    ),
                    None => (0, -1),
                }
            });
            use crate::builtins::Variable;
            vars.store_int(Variable::RSTART, start)?;
            vars.store_int(Variable::RLENGTH, len)?;
            Ok(start)
        })
    }

    /// `match(s, re, arr)` with an integer-keyed `arr`: `arr[n]` is the text of group `n`.
    pub(crate) fn regex_match_intmap<'a>(
        &mut self,
        vars: &mut Variables,
        pat: &Str<'a>,
        s: &Str<'a>,
        m: &IntMap<Str<'a>>,
    ) -> Result<Int> {
        m.clear();
        self.regex_match_captures(vars, pat, s, |i, text, _, _| m.insert(i, text))
    }

    /// `match(s, re, arr)` with a string-keyed `arr`: as gawk, `arr[n]` is the text of group `n`,
    /// and `arr[n, "start"]` and `arr[n, "length"]` its position.
    pub(crate) fn regex_match_strmap<'a>(
        &mut self,
        vars: &mut Variables,
        pat: &Str<'a>,
        s: &Str<'a>,
        m: &StrMap<'a, Str<'a>>,
    ) -> Result<Int> {
        m.clear();
        self.regex_match_captures(vars, pat, s, |i, text, start, len| {
            m.insert(Str::from(i), text);
            m.insert(Str::from(format!("{}\x1cstart", i)), Str::from(start));
            m.insert(Str::from(format!("{}\x1clength", i)), Str::from(len));
        })
    }

    pub(crate) fn regex_const_match(pat: &AwkRegex, s: &Str) -> bool {
        s.with_bytes(|bs| pat.is_match(bs))
    }

    pub(crate) fn is_regex_match(&mut self, pat: &Str, s: &Str) -> Result<bool> {
        if let Some(found) = Self::find_literal(pat, s) {
            return Ok(found.is_some());
        }
        // Patterns built from the input are often each used once (`$0 ~ ("^[a-z]+ " $9)`): see
        // `light_matcher`.
        self.sync();
        if let Some(re) = self.0.lookup(pat) {
            return Ok(Self::regex_const_match(re, s));
        }
        if let Some(res) = s.with_bytes(|bs| self.light_matcher(pat, bs)?.is_match(bs)) {
            return Ok(res);
        }
        self.with_regex(pat, |re| Self::regex_const_match(re, s))
    }
}

#[derive(Clone)]
pub(crate) struct FileWrite(writers::Registry);

impl Default for FileWrite {
    fn default() -> FileWrite {
        FileWrite::new(writers::default_factory())
    }
}

impl FileWrite {
    pub(crate) fn flush_all(&mut self) -> Result<()> {
        self.0.flush_all()
    }
    /// awk's `fflush(name)`: 0 on success, -1 (with a warning, as in gawk) if `name` is not
    /// open for output.
    pub(crate) fn fflush(&mut self, name: &Str) -> Result<Int> {
        if self.0.flush_named(name)? {
            return Ok(0);
        }
        eprintln_ignore!(
            "zawk: warning: fflush: `{}' is not an open file or pipe",
            name.as_str()
        );
        Ok(-1)
    }
    /// Check that the output of a redirection can be opened; see `Registry::check_output`.
    /// Callers treat an error as fatal, unlike errors from the write itself.
    pub(crate) fn check_output(&mut self, path: Option<(&Str, FileSpec)>) -> Result<()> {
        match path {
            Some((name, fspec)) => self.0.check_output(name, fspec),
            None => Ok(()),
        }
    }
    pub(crate) fn flush_stdout(&mut self) -> Result<()> {
        self.0.get_file(None)?.flush()
    }
    /// Close an output file or command. Returns the command's exit status, 0 for a file, or
    /// `None` if `path` was never opened for output.
    pub(crate) fn close(&mut self, path: &Str) -> Result<Option<Int>> {
        self.0.close(path)
    }
    pub(crate) fn new(ff: impl writers::FileFactory) -> FileWrite {
        FileWrite(writers::Registry::from_factory(ff))
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        self.0.destroy_and_flush_all_files()
    }

    /// Write the output of a `printf` (already formatted with `printf::format`).
    ///
    /// Formatting and writing are kept separate so that callers can report format errors while
    /// still exiting silently on write errors (e.g. a closed pipe).
    pub(crate) fn write_printf(&mut self, path: Option<(&Str, FileSpec)>, s: &Str) -> Result<()> {
        let (handle, fspec) = if let Some((out_file, fspec)) = path {
            (self.0.get_handle(Some(out_file), fspec)?, fspec)
        } else {
            (
                self.0.get_handle(None, FileSpec::default())?,
                FileSpec::default(),
            )
        };
        handle.write(s, fspec)
    }
    pub(crate) fn write_all(
        &mut self,
        ss: &[&Str],
        out_spec: Option<(&Str, FileSpec)>,
    ) -> Result<()> {
        if let Some((path, spec)) = out_spec {
            self.0.get_handle(Some(path), spec)?.write_all(ss, spec)
        } else {
            self.0
                .get_handle(None, FileSpec::default())?
                .write_all(ss, FileSpec::Append)
        }
    }
}

pub const CHUNK_SIZE: usize = 8 << 10;

#[derive(Default)]
pub(crate) struct Inputs {
    files: Registry<RegexSplitter<InputFile>>,
    commands: Registry<RegexSplitter<CommandReader>>,
}

pub(crate) struct FileRead<LR: LineReader = RegexSplitter<Box<dyn io::Read + Send>>> {
    pub(crate) inputs: Inputs,
    stdin: LR,
    // A second buffer for the main input's current record, see `get_line_stdin_reuse`. It is
    // initialized from the first record, so that it has the same field projection.
    spare_line: Option<LR::Line>,
    named_columns: Option<Vec<Str<'static>>>,
    used_fields: FieldSet,
    backup_used_fields: FieldSet,
    named_columns_applied: bool,
    // The read state of the last `getline < file` (is_file = true) or `cmd | getline`, which
    // the read-error check that follows it takes instead of looking the reader up a second time.
    last_read_state: Option<(/* is_file */ bool, Int)>,
}

// Set when -H applies to more than one input file, see `FileRead::update_named_columns`.
static MULTI_FILE_HEADERS: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_multi_file_headers(multi: bool) {
    MULTI_FILE_HEADERS.store(multi, Ordering::Relaxed);
}

impl<LR: LineReader> FileRead<LR> {
    pub(crate) fn try_resize(&self, size: usize) -> Vec<impl FnOnce() -> Option<Self> + Send + use<LR>> {
        self.stdin
            .request_handles(size)
            .into_iter()
            .map(|x| {
                let fields = self.used_fields.clone();
                move || {
                    let stdin = x();
                    if stdin.wait() {
                        Some(FileRead {
                            inputs: Default::default(),
                            spare_line: None,
                            named_columns: None,
                            used_fields: fields.clone(),
                            backup_used_fields: fields,
                            named_columns_applied: true,
                            last_read_state: None,
                            stdin,
                        })
                    } else {
                        None
                    }
                }
            })
            .collect()
    }

    /// Close an input file or command. Returns the command's exit status, 0 for a file, or
    /// `None` if `path` was not open for input.
    pub(crate) fn close(&mut self, path: &Str) -> Option<Int> {
        let file = self.inputs.files.remove(path);
        let cmd = self.inputs.commands.take(path).map(|mut c| c.inner_mut().close());
        cmd.or(if file { Some(0) } else { None })
    }

    pub(crate) fn new(
        stdin: LR,
        used_fields: FieldSet,
        named_columns: Option<Vec<&[u8]>>,
    ) -> FileRead<LR> {
        let backup_used_fields = used_fields;
        let used_fields = if named_columns.is_some() {
            // In header-parsing mode we parse all columns until `update_named_columns` is called
            // to ensure that we parse the entire header. Otherwise we just use the same field set
            // as before.
            FieldSet::all()
        } else {
            backup_used_fields.clone()
        };
        let mut res = FileRead {
            inputs: Default::default(),
            spare_line: None,
            stdin,
            used_fields,
            backup_used_fields,
            named_columns_applied: false,
            last_read_state: None,
            named_columns: named_columns
                .map(|cs| cs.into_iter().map(|s| Str::from(s).unmoor()).collect()),
        };
        res.stdin.set_used_fields(&res.used_fields);
        res
    }

    pub(crate) fn update_named_columns<'a>(&mut self, fi: &StrMap<'a, Int>) {
        let referenced_fi = self.backup_used_fields.has_fi();
        let have_columns = self.named_columns.is_some();

        // if we referenced FI, but we weren't able to analyze the columns accessed through FI,
        // then keep the blanket used-fields set; we can't say anything more about them.
        if referenced_fi && !have_columns {
            return;
        }
        // With several input files, the header of each file is read with the used-field set in
        // effect, and may name the columns in a different order: keep parsing all columns.
        if referenced_fi && MULTI_FILE_HEADERS.load(Ordering::Relaxed) {
            return;
        }
        // The header of a later input file: the used-field set is already in place.
        if self.named_columns_applied {
            return;
        }
        self.named_columns_applied = true;

        // Switch back to the original used-field set.
        mem::swap(&mut self.used_fields, &mut self.backup_used_fields);

        // We didn't use FI to reference columns, perhaps just using -H to trim the header.
        //
        // NB: We could optimize for this case, but given that we only ever read a single line of
        // input that's probably more trouble than it's worth.
        if !referenced_fi {
            return;
        }

        // We failed the initial check, and referenced_fi is true, so we must have columns.
        let cols = self.named_columns.as_ref().unwrap();

        // Merge in the named column indexes into our used-field list. Names missing from the
        // header are skipped rather than looked up: `get` would insert them into FI, and
        // `$FI["typo"]` would then read the whole record instead of "".
        for c in cols.iter() {
            let c_borrow: &Str<'a> = c.upcast_ref();
            if fi.contains(c_borrow) {
                self.used_fields.set(fi.get(c_borrow) as usize)
            }
        }
        self.stdin.set_used_fields(&self.used_fields)
    }

    pub(crate) fn stdin_filename(&self) -> Str<'static> {
        self.stdin.filename()
    }

    pub(crate) fn read_err_stdin(&mut self) -> Int {
        self.stdin.read_state()
    }

    // A getline lowers to a read followed by its read-error check on the same file or command,
    // so the check takes the state that the read left (see `RegexCache::get_line`), and only
    // looks the reader up if the read failed before reaching it.
    pub(crate) fn read_err(&mut self, path: &Str) -> Result<Int> {
        if let Some((true, state)) = self.last_read_state.take() {
            return Ok(state);
        }
        self.with_file(path, |reader| Ok(reader.read_state()))
    }
    pub(crate) fn read_err_cmd(&mut self, cmd: &Str) -> Result<Int> {
        if let Some((false, state)) = self.last_read_state.take() {
            return Ok(state);
        }
        self.with_cmd(cmd, |reader| Ok(reader.read_state()))
    }

    pub(crate) fn next_file(&mut self) -> Result<()> {
        let _ = self.stdin.next_file()?;
        Ok(())
    }

    fn with_cmd<R>(
        &mut self,
        cmd: &Str,
        f: impl FnMut(&mut RegexSplitter<CommandReader>) -> Result<R>,
    ) -> Result<R> {
        let check_utf8 = self.stdin.check_utf8();
        self.inputs.commands.get_fallible(
            cmd,
            |s| match command::command_for_read(s.as_bytes()) {
                Ok(r) => Ok(RegexSplitter::new(
                    r,
                    CHUNK_SIZE,
                    cmd.clone().unmoor(),
                    check_utf8,
                )),
                Err(e) => err!("failed to create command for reading: {}", e),
            },
            f,
        )
    }

    fn with_file<R>(
        &mut self,
        path: &Str,
        f: impl FnMut(&mut RegexSplitter<InputFile>) -> Result<R>,
    ) -> Result<R> {
        let check_utf8 = self.stdin.check_utf8();
        self.inputs.files.get_fallible(
            path,
            |s| {
                Ok(RegexSplitter::new(
                    InputFile::open(s),
                    CHUNK_SIZE,
                    path.clone().unmoor(),
                    check_utf8,
                ))
            },
            f,
        )
    }
}

/// A file read by `getline < file`. In awk, failing to open the file is not fatal: getline
/// returns -1. We model that as a reader whose reads fail, which leaves the reader in its error
/// state just like any other read error (e.g. reading a directory).
pub(crate) enum InputFile {
    // A local file, an `s3://` object or an HTTP(S) URL, transparently decompressed.
    Open(Box<dyn io::Read + Send>),
    Stdin(io::Stdin),
    // The error from opening the file: its OS error code if it has one, else its kind.
    Failed(io::ErrorKind, Option<i32>),
}

impl InputFile {
    fn open(path: &str) -> InputFile {
        // As in gawk, `getline < "-"` reads standard input.
        if path == "-" {
            return InputFile::Stdin(io::stdin());
        }
        let opened = if s3::is_s3_url(path) {
            decompress::maybe_decompress(path, s3::S3Reader::new(path))
        } else if http::is_http_url(path) {
            http::open(path).and_then(|resp| decompress::maybe_decompress(path, resp))
        } else {
            File::open(path).and_then(|f| decompress::maybe_decompress(path, f))
        };
        match opened {
            Ok(r) => InputFile::Open(r),
            Err(e) => {
                set_errno_io(&e);
                InputFile::Failed(e.kind(), e.raw_os_error())
            }
        }
    }
}

impl io::Read for InputFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let res = match self {
            InputFile::Open(r) => r.read(buf),
            InputFile::Stdin(r) => r.read(buf),
            InputFile::Failed(_, Some(code)) => Err(io::Error::from_raw_os_error(*code)),
            InputFile::Failed(kind, None) => Err((*kind).into()),
        };
        if let Err(e) = &res {
            if e.kind() != io::ErrorKind::Interrupted {
                set_errno_io(e);
            }
        }
        res
    }
}

/// Estimated memory of a cached value, from the length of its key.
#[derive(Clone, Copy)]
pub(crate) struct CostModel {
    pub(crate) base: usize,
    pub(crate) per_byte: usize,
}

impl CostModel {
    pub(crate) fn cost(&self, key_len: usize) -> usize {
        self.base + self.per_byte * key_len
    }
}

/// A cache keyed by strings with a memory budget, for caches whose keys can come from input data
/// and that `Registry` cannot serve (`Send` values behind a global lock, or keys built from
/// several arguments). Once inserting an entry would take the estimated memory of the entries
/// past the budget, the least recently used half is evicted (see `Registry::evict_lru`).
pub(crate) struct StrLru<V> {
    // Each value is paired with the `clock` value of its last use and its memory.
    cached: HashMap<String, (V, u64, usize)>,
    // The memory budget, or `None` for a cache counted against `regex_memory`.
    budget: Option<usize>,
    cost: CostModel,
    // The memory of the entries.
    used: usize,
    clock: u64,
}

impl<V> StrLru<V> {
    pub(crate) fn new(budget: usize, cost: CostModel) -> Self {
        StrLru {
            cached: HashMap::default(),
            budget: Some(budget),
            cost,
            used: 0,
            clock: 0,
        }
    }
    /// A cache counted against `regex_memory`, whose entries are measured when built (see
    /// `insert_measured`), or estimated with `cost` when the allocator does not tell.
    pub(crate) fn with_regex_memory(cost: CostModel) -> Self {
        StrLru {
            budget: None,
            ..StrLru::new(0, cost)
        }
    }
    pub(crate) fn get(&mut self, key: &str) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        self.cached.get_mut(key).map(|(v, tick, _)| {
            *tick = clock;
            &*v
        })
    }
    pub(crate) fn insert(&mut self, key: String, v: V) -> &V {
        self.insert_measured(key, v, None)
    }
    /// Inserts `v`, which took `measured` bytes to build (if known; see
    /// `regex_memory::measure`).
    pub(crate) fn insert_measured(&mut self, key: String, v: V, measured: Option<usize>) -> &V {
        let cost = measured.unwrap_or_else(|| self.cost.cost(key.len()));
        if let Some((_, _, old)) = self.cached.remove(&key) {
            self.used -= old;
            if self.budget.is_none() {
                regex_memory::release(old);
            }
        }
        let full = match self.budget {
            Some(budget) => self.used + cost > budget,
            None => regex_memory::make_room(cost),
        };
        if full {
            self.evict_lru();
        }
        if self.budget.is_none() {
            regex_memory::charge(cost);
        }
        self.clock += 1;
        self.used += cost;
        &self.cached.entry(key).insert((v, self.clock, cost)).into_mut().0
    }
    /// The value for `key`, created by `new` when it is not cached.
    pub(crate) fn get_or_try_insert<E>(
        &mut self,
        key: &str,
        new: impl FnOnce() -> std::result::Result<V, E>,
    ) -> std::result::Result<&V, E> {
        if self.cached.contains_key(key) {
            return Ok(self.get(key).unwrap());
        }
        let v = new()?;
        Ok(self.insert(key.to_owned(), v))
    }
    /// Evicts the least recently used half of the entries.
    pub(crate) fn evict_lru(&mut self) {
        let mut ticks: Vec<u64> = self.cached.values().map(|(_, tick, _)| *tick).collect();
        if ticks.is_empty() {
            return;
        }
        let mid = ticks.len() / 2;
        let (_, &mut threshold, _) = ticks.select_nth_unstable(mid);
        self.cached.retain(|_, (_, tick, _)| *tick > threshold);
        let used = self.cached.values().map(|(_, _, cost)| *cost).sum();
        if self.budget.is_none() {
            regex_memory::release(self.used - used);
        }
        self.used = used;
    }
}

/// A borrowed key for looking up a `Registry`: it hashes and compares like the `Str` with the
/// same bytes, so a lookup needs neither a clone of the key nor an `unmoor` (which copies a
/// literal).
struct BytesKey<'a>(&'a [u8]);

impl Hash for BytesKey<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // As in `Hash for Str`.
        self.0.hash(state)
    }
}

impl hashbrown::Equivalent<Str<'static>> for BytesKey<'_> {
    fn equivalent(&self, key: &Str<'static>) -> bool {
        key.with_bytes(|bs| bs == self.0)
    }
}

pub(crate) struct Registry<T> {
    // TODO(ezr): we could potentially increase speed here if we did pointer equality (and
    // length) for lookups.
    // We could be fine having duplicates for Regex. We could also also intern strings
    // as we go by swapping out one Rc for another as we encounter them. That would keep the
    // fast path fast, but we would have to make sure we weren't keeping any Refs alive.
    // Each value is paired with the `clock` value of its last use and its memory as counted
    // against `regex_memory` (0 when `limit` is not set).
    cached: HashMap<Str<'static>, (T, u64, usize)>,
    // When set, the entries count against the budget of `regex_memory`, and the least recently
    // used half of the registry is evicted in each eviction round.
    limit: Option<EntryCost>,
    // The memory of the entries, when `limit` is set.
    used: usize,
    // The eviction round of `regex_memory` the registry last took part in.
    round: u64,
    clock: u64,
}
impl<T> Default for Registry<T> {
    fn default() -> Self {
        Registry {
            cached: Default::default(),
            limit: None,
            used: 0,
            round: 0,
            clock: 0,
        }
    }
}

impl<T> Drop for Registry<T> {
    fn drop(&mut self) {
        if self.limit.is_some() {
            regex_memory::release(self.used);
        }
    }
}

/// How a registry counted against `regex_memory` accounts for the memory of an entry.
#[derive(Clone, Copy)]
pub(crate) struct EntryCost {
    /// The percentage of the memory measured while building an entry (see `Registry::get`) to
    /// count: a compiled regex allocates its search caches when first used.
    measured_percent: usize,
    /// The estimate, from the length of the key, when the allocator does not tell.
    estimate: CostModel,
}

impl<T> Registry<T> {
    fn with_memory_limit(cost: EntryCost) -> Self {
        Registry {
            cached: Default::default(),
            limit: Some(cost),
            used: 0,
            round: regex_memory::round(),
            clock: 0,
        }
    }
    /// The memory to count for an entry for a key of `key_len` bytes, of which building took
    /// `measured` bytes (if known), not counting `measured_percent`. A measured entry also counts
    /// its key and its slot in the table (twice, as the table can be half empty).
    fn cost(&self, key_len: usize, measured: Option<usize>) -> usize {
        let slot = 2 * mem::size_of::<(Str<'static>, (T, u64, usize))>() + key_len;
        self.limit.map_or(0, |limit| measured.map_or_else(|| limit.estimate.cost(key_len), |m| m + slot))
    }
    fn remove(&mut self, s: &Str) -> bool {
        self.take(s).is_some()
    }
    fn take(&mut self, s: &Str) -> Option<T> {
        let (t, _, cost) = s.with_bytes(|bs| self.cached.remove(&BytesKey(bs)))?;
        self.used -= cost;
        if self.limit.is_some() {
            regex_memory::release(cost);
        }
        Some(t)
    }
    /// Evicts the least recently used half of the entries. Evicting in batches keeps cache hits
    /// down to a counter update while making eviction amortized O(1) per insertion.
    fn evict_lru(&mut self) {
        let mut ticks: Vec<u64> = self.cached.values().map(|(_, tick, _)| *tick).collect();
        if ticks.is_empty() {
            return;
        }
        let mid = ticks.len() / 2;
        let (_, &mut threshold, _) = ticks.select_nth_unstable(mid);
        // Ticks are unique, so this keeps exactly the entries more recent than the median.
        self.cached.retain(|_, (_, tick, _)| *tick > threshold);
        let used = self.cached.values().map(|(_, _, cost)| *cost).sum();
        regex_memory::release(self.used - used);
        self.used = used;
    }
    /// Takes part in the eviction rounds of `regex_memory` that started since the last call.
    fn sync_budget(&mut self, round: u64) {
        if self.limit.is_some() && self.round != round {
            self.round = round;
            self.evict_lru();
        }
    }
    /// The entry for `s`, if any, marked as just used.
    fn lookup(&mut self, s: &Str) -> Option<&mut T> {
        self.clock += 1;
        let clock = self.clock;
        let (val, tick, _) = s.with_bytes(|bs| self.cached.get_mut(&BytesKey(bs)))?;
        *tick = clock;
        Some(val)
    }
    /// Adds an entry for `s`, which must not have one yet, that took `measured` bytes to build
    /// (if known; see `regex_memory::measure`).
    fn insert(&mut self, s: &Str, val: T, measured: Option<usize>) {
        let cost = self.cost(s.len(), measured);
        if self.limit.is_some() {
            if regex_memory::make_room(cost) {
                self.sync_budget(regex_memory::round());
            }
            regex_memory::charge(cost);
        }
        self.cached.insert(s.clone().unmoor(), (val, self.clock, cost));
        self.used += cost;
    }
    fn get<R>(
        &mut self,
        s: &Str,
        new: impl FnMut(&str) -> Result<T>,
        getter: impl FnOnce(&mut T) -> R,
    ) -> Result<R> {
        self.get_fallible(s, new, |t| Ok(getter(t)))
    }
    fn get_fallible<R>(
        &mut self,
        s: &Str,
        mut new: impl FnMut(&str) -> Result<T>,
        getter: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R> {
        self.clock += 1;
        let clock = self.clock;
        // Look up with the borrowed bytes: only a miss pays for an owned key.
        let hit = s.with_bytes(|bs| self.cached.get_mut(&BytesKey(bs)));
        if let Some((val, tick, _)) = hit {
            *tick = clock;
            return getter(val);
        }
        let percent = self.limit.map(|limit| limit.measured_percent);
        let (val, res, measured) = s.with_bytes(|raw_str| {
            let s = match str::from_utf8(raw_str) {
                Ok(s) => s,
                Err(e) => return err!("invalid UTF-8 for file or regex: {}", e),
            };
            let (val, measured) = match percent {
                Some(percent) => {
                    let (val, measured) = regex_memory::measure(|| new(s));
                    (val, measured.map(|m| m * percent / 100))
                }
                None => (new(s), None),
            };
            let mut val = val?;
            let res = getter(&mut val);
            Ok((val, res, measured))
        })?;
        self.insert(s, val, measured);
        res
    }
}

pub(crate) struct _Carrier;

pub(crate) trait Convert<S, T> {
    fn convert(s: S) -> T;
}

impl Convert<Float, Int> for _Carrier {
    fn convert(f: Float) -> Int {
        f as Int
    }
}
impl Convert<Int, Float> for _Carrier {
    fn convert(i: Int) -> Float {
        i as Float
    }
}

// See str_impl.rs for how these first two are implemented.
impl<'a> Convert<Int, Str<'a>> for _Carrier {
    fn convert(i: Int) -> Str<'a> {
        i.into()
    }
}
impl<'a> Convert<Float, Str<'a>> for _Carrier {
    fn convert(f: Float) -> Str<'a> {
        f.into()
    }
}
impl<'a> Convert<Str<'a>, Float> for _Carrier {
    fn convert(s: Str<'a>) -> Float {
        s.with_bytes(str_to_num)
    }
}
impl<'a> Convert<Str<'a>, Int> for _Carrier {
    fn convert(s: Str<'a>) -> Int {
        s.with_bytes(str_to_int)
    }
}
impl<'b, 'a> Convert<&'b Str<'a>, Float> for _Carrier {
    fn convert(s: &'b Str<'a>) -> Float {
        s.with_bytes(str_to_num)
    }
}
impl<'b, 'a> Convert<&'b Str<'a>, Int> for _Carrier {
    fn convert(s: &'b Str<'a>) -> Int {
        s.with_bytes(str_to_int)
    }
}

/// Report the failure of a stdlib function (network, database, crypto, ...) on stderr, e.g.
/// `zawk: warning: jwt: unsupported algorithm "NOPE"`. The function then returns an empty value
/// instead of aborting the program. Each distinct message is reported once, so that a function
/// failing on every record does not flood stderr.
pub(crate) fn stdlib_warning(func: &str, msg: impl std::fmt::Display) {
    static WARNED: LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(Default::default);
    let msg = format!("zawk: warning: {}: {}", func, msg);
    let first = WARNED.lock().map_or(true, |mut warned| warned.insert(msg.clone()));
    if first {
        eprintln_ignore!("{}", msg);
    }
}

/// The value of awk's `close`: the exit status of an output or input command (0 for a file), and
/// -1 if nothing named that way was open.
pub(crate) fn close_result(output: Option<Int>, input: Option<Int>) -> Int {
    output.or(input).unwrap_or_else(|| {
        set_errno("close of redirection that was never opened".into());
        -1
    })
}

thread_local! {
    // gawk's ERRNO: the message of the last failed `getline` or `close`. It is not cleared when
    // a later call succeeds. Each worker of a parallel program has its own.
    static ERRNO: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn errno() -> String {
    ERRNO.with(|e| e.borrow().clone())
}

pub(crate) fn set_errno(msg: String) {
    ERRNO.with(|e| *e.borrow_mut() = msg);
}

/// Set ERRNO from an I/O error, as the C library's `strerror` describes it (without Rust's
/// " (os error N)" suffix), e.g. "No such file or directory".
pub(crate) fn set_errno_io(e: &io::Error) {
    let mut msg = e.to_string();
    if let Some(code) = e.raw_os_error() {
        if let Some(stripped) = msg.strip_suffix(&format!(" (os error {})", code)) {
            msg = stripped.to_string();
        }
    }
    set_errno(msg);
}

/// Largest result a stdlib function may build: a string of this many bytes (`repeat`) or chars
/// (`pad`, `pad_start`, `pad_end`), or an array of this many elements (`seq`). It matches the
/// printf limit on field widths, so that a typo such as `repeat("ab", 1e12)` is a fatal error
/// instead of an attempt to allocate terabytes (which gets the process killed).
pub(crate) const MAX_RESULT_LEN: usize = 64 << 20;

/// Fail unless `len`, the size of the result `func` is about to build, is within
/// [`MAX_RESULT_LEN`].
pub(crate) fn check_result_len(func: &str, what: &str, len: Float) -> Result<()> {
    if len > MAX_RESULT_LEN as Float {
        return err!("{}: {} {} is too large (maximum {})", func, what, len, MAX_RESULT_LEN);
    }
    Ok(())
}

/// Error reported when integer addition or subtraction leaves the 64-bit range.
pub(crate) const INT_OVERFLOW_MSG: &str = "integer overflow: result exceeds the 64-bit integer range";

pub(crate) trait Inc {
    fn inc_int(&mut self, by: Int);
    fn inc_float(&mut self, by: Float);
}

impl Inc for Int {
    fn inc_int(&mut self, by: Int) {
        *self += by;
    }
    fn inc_float(&mut self, by: Float) {
        *self += by as Int;
    }
}

impl Inc for Float {
    fn inc_int(&mut self, by: Int) {
        *self += by as Float;
    }
    fn inc_float(&mut self, by: Float) {
        *self += by;
    }
}

impl<'a> Inc for Str<'a> {
    fn inc_int(&mut self, by: Int) {
        *self = convert::<_, Self>(convert::<_, Int>(self as &_) + by);
    }
    fn inc_float(&mut self, by: Float) {
        *self = convert::<_, Self>(convert::<_, Float>(self as &_) + by);
    }
}

pub(crate) fn convert<S, T>(s: S) -> T
where
    _Carrier: Convert<S, T>,
{
    _Carrier::convert(s)
}

// AWK arrays are inherently shared and mutable, so we have to do this, even if it is a code smell.
// NB These are repr(transparent) because we pass them around as void* when compiling with LLVM.
#[repr(transparent)]
#[derive(Debug)]
pub(crate) struct SharedMap<K, V>(pub(crate) Rc<RefCell<HashMap<K, V>>>);

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> SharedMap<K, V> {
        SharedMap(Rc::new(RefCell::new(Default::default())))
    }
}

impl<K, V> Clone for SharedMap<K, V> {
    fn clone(&self) -> Self {
        SharedMap(self.0.clone())
    }
}

impl<K: Hash + Eq, V> SharedMap<K, V> {
    pub(crate) fn len(&self) -> usize {
        self.0.borrow().len()
    }
    pub(crate) fn insert(&self, k: K, v: V) {
        self.borrow_mut().insert(k, v);
    }
    pub(crate) fn delete(&self, k: &K) {
        self.borrow_mut().remove(k);
    }
    pub(crate) fn iter<F, R>(&self, f: F) -> R
    where
        F: FnOnce(hashbrown::hash_map::Iter<K, V>) -> R,
    {
        f(self.0.borrow().iter())
    }
    pub(crate) fn clear(&self) {
        self.borrow_mut().clear();
    }
}

impl<K: Hash + Eq + Clone, V: Inc + Default + Clone> SharedMap<K, V> {
    pub(crate) fn inc_int(&self, k: &K, by: Int) -> V {
        self.with_inserted(k, |kref| {
            kref.inc_int(by);
            kref.clone()
        })
    }

    pub(crate) fn inc_float(&self, k: &K, by: Float) -> V {
        self.with_inserted(k, |kref| {
            kref.inc_float(by);
            kref.clone()
        })
    }

    fn with_inserted<R>(&self, k: &K, f: impl FnOnce(&mut V) -> R) -> R {
        let mut slf = self.0.borrow_mut();
        if let Some(k) = slf.get_mut(k) {
            f(k)
        } else {
            f(slf.entry(k.clone()).or_insert(Default::default()))
        }
    }
}

// When sending SharedMaps across threads we have to clone them and clone their contents, as Rc is
// not thread-safe (and we don't want to pay the cost of Arc clones during normal execution).
pub(crate) struct Shuttle<T>(T);
impl<'a> From<Shuttle<HashMap<Int, UniqueStr<'a>>>> for IntMap<Str<'a>> {
    fn from(sh: Shuttle<HashMap<Int, UniqueStr<'a>>>) -> Self {
        SharedMap(Rc::new(RefCell::new(
            sh.0.into_iter().map(|(x, y)| (x, y.into_str())).collect(),
        )))
    }
}

impl<'a> From<Shuttle<HashMap<UniqueStr<'a>, Int>>> for StrMap<'a, Int> {
    fn from(sh: Shuttle<HashMap<UniqueStr<'a>, Int>>) -> Self {
        SharedMap(Rc::new(RefCell::new(
            sh.0.into_iter().map(|(x, y)| (x.into_str(), y)).collect(),
        )))
    }
}

impl<'a> From<Shuttle<HashMap<UniqueStr<'a>, UniqueStr<'a>>>> for StrMap<'a, Str<'a>> {
    fn from(sh: Shuttle<HashMap<UniqueStr<'a>, UniqueStr<'a>>>) -> Self {
        SharedMap(Rc::new(RefCell::new(
            sh.0.into_iter().map(|(x, y)| (x.into_str(), y.into_str())).collect(),
        )))
    }
}

impl<K, V> SharedMap<K, V> {
    fn borrow_mut(&self) -> impl std::ops::DerefMut<Target = HashMap<K, V>> + '_ {
        // Unlike the full std::collections APIs, we are careful not to hand out any references
        // internal to a SharedMap from a public function. That means that functions which mutate
        // the map are "Cell"-like, in that they swap out values or drop them in, but never hold
        // onto a mutable reference that could be aliased down the line.
        //
        // Still, we do the checked mutable borrow in debug builds to catch future violations of
        // this invariant.
        #[cfg(debug_assertions)]
        {
            self.0.borrow_mut()
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { &mut *self.0.as_ptr() }
        }
    }
}

impl<K: Hash + Eq, V: Clone> SharedMap<K, V> {
    pub(crate) fn contains(&self, k: &K) -> bool {
        #[cfg(debug_assertions)]
        {
            self.0.borrow().get(k).is_some()
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { &mut *self.0.as_ptr() }.get(k).is_some()
        }
    }
}

/// The value of a missing array element.
pub(crate) trait Uninit {
    fn uninit() -> Self;
}

impl Uninit for Int {
    fn uninit() -> Int {
        0
    }
}

impl Uninit for Float {
    fn uninit() -> Float {
        0.0
    }
}

impl<'a> Uninit for Str<'a> {
    fn uninit() -> Str<'a> {
        Str::uninit()
    }
}

impl<K: Hash + Eq + Clone, V: Clone + Uninit> SharedMap<K, V> {
    pub(crate) fn get(&self, k: &K) -> V {
        self.borrow_mut()
            .raw_entry_mut()
            .from_key(k)
            .or_insert_with(|| (k.clone(), V::uninit()))
            .1
            .clone()
    }
}

impl<'a> IntMap<Str<'a>> {
    pub(crate) fn shuttle(&self) -> Shuttle<HashMap<Int, UniqueStr<'a>>> {
        Shuttle(
            self.0
                .borrow()
                .iter()
                .map(|(x, y)| (*x, UniqueStr::from(y.clone())))
                .collect(),
        )
    }
}

impl<'a> StrMap<'a, Int> {
    pub(crate) fn shuttle(&self) -> Shuttle<HashMap<UniqueStr<'a>, Int>> {
        Shuttle(
            self.0
                .borrow()
                .iter()
                .map(|(x, y)| (UniqueStr::from(x.clone()), *y))
                .collect(),
        )
    }
}

impl<'a> StrMap<'a, Str<'a>> {
    pub(crate) fn shuttle(&self) -> Shuttle<HashMap<UniqueStr<'a>, UniqueStr<'a>>> {
        Shuttle(
            self.0
                .borrow()
                .iter()
                .map(|(x, y)| (UniqueStr::from(x.clone()), UniqueStr::from(y.clone())))
                .collect(),
        )
    }
}

impl<K: Hash + Eq + Clone, V> SharedMap<K, V> {
    pub(crate) fn to_vec(&self) -> Vec<K> {
        self.0.borrow().keys().cloned().collect()
    }
}

impl<K: Hash + Eq, V> From<HashMap<K, V>> for SharedMap<K, V> {
    fn from(m: HashMap<K, V>) -> SharedMap<K, V> {
        SharedMap(Rc::new(RefCell::new(m)))
    }
}

impl<K: Hash + Eq, V> FromIterator<(K, V)> for SharedMap<K, V> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = (K, V)>,
    {
        SharedMap(Rc::new(RefCell::new(
            iter.into_iter().collect::<HashMap<K, V>>(),
        )))
    }
}

pub(crate) type Int = i64;
pub(crate) type Float = f64;
pub(crate) type IntMap<V> = SharedMap<Int, V>;

impl<'a> IntMap<Str<'a>> {
    /// Undo [`Str::as_text`] on the values of a map built from `as_text` input.
    pub(crate) fn from_text(self) -> Self {
        for v in self.borrow_mut().values_mut() {
            *v = v.unescape_text();
        }
        self
    }
}

impl<'a, V> StrMap<'a, V> {
    /// Undo [`Str::as_text`] on the keys of a map built from `as_text` input, and on the values
    /// with `value`.
    pub(crate) fn from_text(self, value: impl Fn(V) -> V) -> Self {
        let entries: Vec<_> = self.borrow_mut().drain().collect();
        let mut map = self.borrow_mut();
        for (k, v) in entries {
            map.insert(k.unescape_text(), value(v));
        }
        drop(map);
        self
    }
}

impl<'a> StrMap<'a, Str<'a>> {
    pub(crate) fn from_text_all(self) -> Self {
        self.from_text(|v| v.unescape_text())
    }
}
pub(crate) type StrMap<'a, V> = SharedMap<Str<'a>, V>;

pub(crate) struct Iter<S> {
    cur: Cell<usize>,
    items: Vec<S>,
}

impl<S> Default for Iter<S> {
    fn default() -> Iter<S> {
        None.into_iter().collect()
    }
}

impl<S> FromIterator<S> for Iter<S> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = S>,
    {
        Iter {
            cur: Cell::new(0),
            items: Vec::from_iter(iter),
        }
    }
}

impl<S> Iter<S> {
    pub(crate) fn has_next(&self) -> bool {
        self.cur.get() < self.items.len()
    }
    pub(crate) unsafe fn get_next(&self) -> &S {
        debug_assert!(self.has_next());
        let cur = self.cur.get();
        let res = self.items.get_unchecked(cur);
        self.cur.set(cur + 1);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_lru_is_bounded() {
        const COST: CostModel = CostModel { base: 100, per_byte: 1 };
        let budget = 100 * COST.cost(8);
        let mut lru = StrLru::new(budget, COST);
        lru.insert("hot".to_owned(), 0);
        for i in 0..10_000 {
            assert_eq!(lru.get("hot"), Some(&0));
            let key = format!("k{:07}", i);
            assert_eq!(lru.get_or_try_insert(&key, || Ok::<_, ()>(i)), Ok(&i));
            assert!(lru.used <= budget);
            assert!(lru.cached.len() <= 100);
        }
        // A frequently used key survives evictions; reinserting a key does not count it twice.
        assert_eq!(lru.get("hot"), Some(&0));
        let used = lru.used;
        lru.insert("hot".to_owned(), 1);
        assert_eq!((lru.used, lru.get("hot")), (used, Some(&1)));
        assert_eq!(lru.get_or_try_insert("bad", || Err("parse")), Err("parse"));
        assert_eq!(lru.get("bad"), None);
    }

    // Room for a few hundred short patterns.
    const TEST_BUDGET: usize = 2 << 20;

    /// A regex cache with `TEST_BUDGET` as the memory budget of this thread's regex caches.
    fn test_cache() -> RegexCache {
        regex_memory::set_budget(TEST_BUDGET);
        RegexCache::default()
    }

    #[test]
    fn regex_cache_is_bounded() {
        let mut rc = test_cache();
        for i in 0..(TEST_BUDGET / REGEX_COST.estimate.cost(4) * 3) {
            let pat = Str::from(format!("x{}", i));
            assert!(rc.with_regex(&pat, |re| re.is_match(b"x0")).is_ok());
            assert!(regex_memory::used() <= TEST_BUDGET);
        }
        // Entries still hit after the cache has been cleared.
        let pat = Str::from("x0".to_string());
        assert!(rc.with_regex(&pat, |re| re.is_match(b"x0")).unwrap());
    }

    #[test]
    fn regex_cache_counts_measured_memory() {
        // `[[:alpha:]]` compiles to `\p{Alphabetic}`, about 100KB: the budget holds a few dozen
        // such regexes, whatever the length of the pattern, rather than the ~70 short patterns
        // the estimate from its length would allow.
        let mut rc = test_cache();
        let hay = Str::from("中文 abc");
        for i in 0..200 {
            let pat = Str::from(format!("[[:alpha:]]+x{}", i));
            assert!(!rc.with_regex(&pat, |re| re.is_match(b"x")).unwrap());
            assert!(!rc.is_regex_match(&Str::from(format!("[[:alpha:]]+y{}", i)), &hay).unwrap());
            assert!(regex_memory::used() <= TEST_BUDGET);
        }
        if regex_memory::measure(|| vec![0u8; 1024]).1.is_some() {
            assert!(rc.0.cached.len() < 40, "{} regexes cached", rc.0.cached.len());
        }
        // The longest-match machinery, built on the first search, counts against the budget too.
        for i in 0..200 {
            let pat = Str::from(format!("[[:alpha:]]+(a|ab){}", i));
            rc.with_regex(&pat, |re| awk_regex::find_at(re, b"ab0", 0)).unwrap();
            assert!(regex_memory::used() <= TEST_BUDGET);
        }
    }

    #[test]
    fn regex_cache_keeps_recently_used() {
        let mut rc = test_cache();
        let hot = Str::from("hot".to_string());
        for i in 0..(TEST_BUDGET / REGEX_COST.estimate.cost(4) * 3) {
            assert!(rc.with_regex(&hot, |re| re.is_match(b"hot")).unwrap());
            let pat = Str::from(format!("x{}", i));
            assert!(rc.with_regex(&pat, |re| re.is_match(b"x0")).is_ok());
            // A frequently used regex survives evictions.
            assert!(rc.0.cached.contains_key(&hot.clone().unmoor()));
        }
    }

    #[test]
    fn regex_cache_keeps_both_ignorecase_settings() {
        let mut rc = test_cache();
        let pat = Str::from("ab+c");
        let s = Str::from("xABBC");
        for _ in 0..3 {
            for ic in [false, true] {
                awk_regex::set_ignore_case(ic);
                assert_eq!(rc.with_regex(&pat, |re| re.is_match(b"xABBC")).unwrap(), ic);
                assert_eq!(rc.is_regex_match(&pat, &s).unwrap(), ic);
            }
        }
        // Toggling IGNORECASE swaps the caches instead of recompiling: each setting keeps its
        // compiled regex.
        assert_eq!((rc.0.cached.len(), rc.2.1.cached.len()), (1, 1));
        awk_regex::set_ignore_case(false);
    }

    #[test]
    fn literal_patterns_skip_the_cache() {
        let mut rc = test_cache();
        let cases: &[(&str, &str, bool)] = &[
            ("user1", "a user12", true),
            ("^user1", "user12", true),
            ("^user1", "a user1", false),
            ("user1$", "a user1", true),
            ("user1$", "user12", false),
            ("^user1$", "user1", true),
            ("^user1$", "user12", false),
            ("", "x", true),
            ("^$", "", true),
            ("^$", "x", false),
            ("^", "x", true),
            ("$", "x", true),
        ];
        for &(pat, s, want) in cases {
            let got = rc.is_regex_match(&Str::from(pat), &Str::from(s)).unwrap();
            assert_eq!(got, want, "{:?} ~ {:?}", s, pat);
        }
        assert!(rc.0.cached.is_empty() && rc.3.cached.is_empty());
        // Patterns with operators still go through a regex engine: the cheap one for the first
        // `LIGHT_USES` uses, then a fully compiled regex.
        let pat = Str::from("^u.er$");
        for _ in 0..LIGHT_USES {
            assert!(rc.is_regex_match(&pat, &Str::from("user")).unwrap());
            assert_eq!((rc.0.cached.len(), rc.3.cached.len()), (0, 1));
        }
        assert!(!rc.is_regex_match(&pat, &Str::from("users")).unwrap());
        assert_eq!((rc.0.cached.len(), rc.3.cached.len()), (1, 0));
        assert!(rc.is_regex_match(&pat, &Str::from("uber")).unwrap());
        // Invalid patterns report the error of `compile`.
        let err = rc.is_regex_match(&Str::from("a("), &Str::from("a")).unwrap_err();
        assert!(format!("{}", err).contains("invalid regex /a(/"), "{}", err);
    }

    #[test]
    fn searches_start_with_light_matchers() {
        let mut rc = test_cache();
        let find = |rc: &mut RegexCache, pat: &str, s: &str| {
            let s = Str::from(s);
            rc.with_search(&Str::from(pat), &s, |re| {
                s.with_bytes(|bs| {
                    let spans: Vec<_> = awk_regex::find_iter(re, bs).collect();
                    let groups = spans.first().and_then(|&(from, to)| awk_regex::captures_at(re, bs, from, to));
                    (spans, groups.map(|g| g.iter().collect::<Vec<_>>()))
                })
            })
            .unwrap()
        };
        // The same results from the light matcher (ASCII and other text, and text its literals
        // rule out), and from the compiled regex once the pattern has been used `LIGHT_USES` times.
        let cases: &[(&str, &str, Vec<(usize, usize)>, Option<Vec<Option<(usize, usize)>>>)] = &[
            ("(x|xy)(z?)", "a xyz xz", vec![(2, 5), (6, 8)], Some(vec![Some((2, 5)), Some((2, 4)), Some((4, 5))])),
            ("(x|xy)(z?)", "é xyz xz", vec![(3, 6), (7, 9)], Some(vec![Some((3, 6)), Some((3, 5)), Some((5, 6))])),
            ("(x|xy)(z?)", "abc", vec![], None),
            ("^(a+)(b)?", "aab", vec![(0, 3)], Some(vec![Some((0, 3)), Some((0, 2)), Some((2, 3))])),
        ];
        for (pat, s, spans, groups) in cases {
            for i in 0..=LIGHT_USES {
                assert_eq!(&find(&mut rc, pat, s), &(spans.clone(), groups.clone()), "/{}/ on {:?}", pat, s);
                assert_eq!(rc.0.cached.len() == 1, i == LIGHT_USES, "/{}/ use {}", pat, i + 1);
            }
            rc.0.cached.clear();
        }
        let err = rc.with_search(&Str::from("a("), &Str::from("a"), |_| ()).unwrap_err();
        assert!(format!("{}", err).contains("invalid regex /a(/"), "{}", err);
    }
}
