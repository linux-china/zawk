//! Compiling awk (POSIX ERE, with gawk's extensions) regular expressions with the `regex` crate.
//!
//! The syntaxes mostly agree; `translate` rewrites the parts that do not:
//!
//! * `.` matches any character, including a newline (set on the builder, see `compile`).
//! * A `{` that does not start a valid interval (`{n}`, `{n,}`, `{n,m}`) is a literal, as is one
//!   at the start of an expression.
//! * gawk's operators: `\y` (word boundary), `\<` and `\>` (start/end of a word), `` \` `` and
//!   `\'` (start/end of the string), and `\B`.
//! * Escapes: `\b` is a backspace (gawk uses `\y` for word boundaries), `\a`, `\v`, `\f`; octal
//!   (`\351`) and hex (`\xE9`) escapes are bytes, not code points (see `push_byte`); other
//!   letters that are not operators are literal characters.
//! * POSIX character classes (`[:alpha:]` etc.) match non-ASCII characters too, as in gawk under a
//!   UTF-8 locale (the `regex` crate's own versions are ASCII-only); see `posix_class`.
//! * In bracket expressions, `[`, `&`, `~` and `-` sequences that the `regex` crate treats as
//!   nested classes or set operations are literal characters.
//!
//! `regex` reports the leftmost-first match of an alternation (e.g. `x` for `x|xy` against
//! "xyz"), where POSIX awk reports the leftmost-longest one (`xy`). The `find_at`, `find_iter`
//! and `captures_at` helpers below restore the POSIX semantics for patterns with an alternation or
//! a repetition of a group (`(ab)?(abcd)?`); other patterns use `regex` directly.
use regex::bytes::{Captures, Regex, RegexBuilder};
use regex_automata::hybrid::dfa::{Cache, DFA};
use regex_automata::nfa::thompson::pikevm::PikeVM;
use regex_automata::{Anchored, Input, MatchKind};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

thread_local! {
    // gawk's IGNORECASE, set when the variable is assigned (see `Variables::store_int`). Worker
    // threads copy it from the main thread when they start (see `Core::shuttle`).
    static IGNORE_CASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Whether IGNORECASE was ever set on any thread. String comparisons and index() check this first,
// so programs that do not use IGNORECASE do not pay for the thread-local lookup.
static IGNORE_CASE_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set whether regexes compiled, string comparisons and index() on this thread ignore case
/// (gawk's IGNORECASE).
pub(crate) fn set_ignore_case(on: bool) {
    if on {
        IGNORE_CASE_USED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    IGNORE_CASE.with(|c| c.set(on));
}

/// Whether IGNORECASE is set on this thread.
#[inline]
pub(crate) fn ignore_case() -> bool {
    IGNORE_CASE_USED.load(std::sync::atomic::Ordering::Relaxed) && IGNORE_CASE.with(|c| c.get())
}

/// Prefix `pat` (in the syntax of `regex`) with `(?i)` when IGNORECASE is set. The flag is part of
/// the pattern text so that the regexes derived from `re.as_str()` below ignore case as well.
pub(crate) fn with_case(pat: String) -> String {
    if ignore_case() {
        format!("(?i){}", pat)
    } else {
        pat
    }
}

/// Compile an awk regular expression.
pub(crate) fn compile(pat: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&with_case(translate(pat).map_err(regex::Error::Syntax)?))
        .dot_matches_new_line(true)
        .build()
}

/// A matcher for `~` that is cheap to build, for dynamic patterns that are used only a few times
/// (`$0 ~ ("^[a-z]+ " $9)`, one pattern per record). `compile` builds `regex`'s full set of
/// engines, including a full DFA for small patterns, which takes several times longer (~18µs vs
/// ~3µs for `^[a-z]+ abc`) than the forward-only lazy DFA used here, with no capture groups,
/// reverse search or prefilter. `RegexCache::is_regex_match` switches a pattern to `compile` once
/// it is reused.
///
/// Unicode classes (`[[:alpha:]]` is `\p{Alphabetic}`, `.` any character) make up most of the
/// build time of the rest, so the engine is built for the characters of the text at hand (see
/// `restrict`): a match can only consume characters of the text, so the result is the same. The
/// engine for ASCII text is kept; for other text one is built per call.
pub(crate) struct LightMatcher {
    hir: regex_syntax::hir::Hir,
    /// The engine for ASCII text, once built; `Some(None)` when it cannot be built.
    ascii: Option<Option<LightEngine>>,
    /// How many times the matcher was used.
    pub(crate) uses: u32,
}

struct LightEngine {
    /// `None` when the lazy DFA cannot be built.
    dfa: Option<(DFA, Cache)>,
    nfa: regex_automata::nfa::thompson::NFA,
}

impl LightEngine {
    fn new(hir: &regex_syntax::hir::Hir) -> Option<LightEngine> {
        use regex_automata::nfa::thompson::{Compiler, Config, WhichCaptures};
        // The configuration of `regex::bytes::Regex` (see `compile`), minus capture groups.
        let nfa = Compiler::new()
            .configure(
                Config::new()
                    .utf8(false)
                    .which_captures(WhichCaptures::None)
                    .nfa_size_limit(Some(10 << 20)),
            )
            .build_from_hir(hir)
            .ok()?;
        let dfa = DFA::builder()
            .configure(DFA::config().unicode_word_boundary(true))
            .build_from_nfa(nfa.clone())
            .ok()
            .map(|dfa| {
                let cache = dfa.create_cache();
                (dfa, cache)
            });
        Some(LightEngine { dfa, nfa })
    }

    fn is_match(&mut self, hay: &[u8]) -> bool {
        let input = Input::new(hay).earliest(true);
        if let Some((dfa, cache)) = &mut self.dfa {
            if let Ok(m) = dfa.try_search_fwd(cache, &input) {
                return m.is_some();
            }
        }
        // The lazy DFA gave up (Unicode word boundaries next to non-ASCII text) or could not be
        // built.
        let pike = PikeVM::new_from_nfa(self.nfa.clone()).expect("PikeVM supports every NFA");
        pike.is_match(&mut pike.create_cache(), input)
    }
}

/// `hir` with every class restricted to `chars`; in text made of those characters (and bytes
/// that are not valid UTF-8) it matches the same as `hir`. When `chars` is ASCII, Unicode word
/// boundaries become ASCII ones too, which the lazy DFA handles without giving up. Byte classes
/// are left alone.
fn restrict(hir: &regex_syntax::hir::Hir, chars: &regex_syntax::hir::ClassUnicode, ascii: bool) -> regex_syntax::hir::Hir {
    use regex_syntax::hir::{Capture, Class, Hir, HirKind, Look, Repetition};
    match hir.kind() {
        HirKind::Class(Class::Unicode(c)) => {
            let mut c = c.clone();
            c.intersect(chars);
            Hir::class(Class::Unicode(c))
        }
        HirKind::Look(look) if ascii => Hir::look(match look {
            Look::WordUnicode => Look::WordAscii,
            Look::WordUnicodeNegate => Look::WordAsciiNegate,
            Look::WordStartUnicode => Look::WordStartAscii,
            Look::WordEndUnicode => Look::WordEndAscii,
            Look::WordStartHalfUnicode => Look::WordStartHalfAscii,
            Look::WordEndHalfUnicode => Look::WordEndHalfAscii,
            other => *other,
        }),
        HirKind::Repetition(r) => Hir::repetition(Repetition {
            sub: Box::new(restrict(&r.sub, chars, ascii)),
            ..r.clone()
        }),
        HirKind::Capture(c) => Hir::capture(Capture {
            sub: Box::new(restrict(&c.sub, chars, ascii)),
            ..c.clone()
        }),
        HirKind::Concat(hs) => Hir::concat(hs.iter().map(|h| restrict(h, chars, ascii)).collect()),
        HirKind::Alternation(hs) => Hir::alternation(hs.iter().map(|h| restrict(h, chars, ascii)).collect()),
        _ => hir.clone(),
    }
}

/// The ASCII characters.
fn ascii_chars() -> regex_syntax::hir::ClassUnicode {
    use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
    ClassUnicode::new([ClassUnicodeRange::new('\0', '\x7F')])
}

/// The ASCII characters and those of `hay`.
fn chars_of(hay: &[u8]) -> regex_syntax::hir::ClassUnicode {
    use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
    let mut chars = ascii_chars();
    chars.union(&ClassUnicode::new(
        hay.utf8_chunks()
            .flat_map(|c| c.valid().chars())
            .filter(|c| !c.is_ascii())
            .map(|c| ClassUnicodeRange::new(c, c)),
    ));
    chars
}

/// Whether compiling `hir` with all of Unicode could exceed `regex`'s size limit, which the
/// restricted engines of `LightMatcher` would not notice: counted repetitions multiply the size
/// of what they repeat.
fn may_be_too_big(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Hir, HirKind};
    fn walk(h: &Hir, times: u64) -> bool {
        match h.kind() {
            HirKind::Repetition(r) => {
                let n = times.saturating_mul(r.max.unwrap_or(r.min).max(1) as u64);
                n > 64 || walk(&r.sub, n)
            }
            HirKind::Capture(c) => walk(&c.sub, times),
            HirKind::Concat(hs) | HirKind::Alternation(hs) => hs.iter().any(|h| walk(h, times)),
            _ => false,
        }
    }
    walk(hir, 1)
}

impl LightMatcher {
    /// A matcher for the awk regex `pat`, or `None` when it does not parse (or may be too big,
    /// see `may_be_too_big`); the caller then goes through `compile`, which reports errors.
    pub(crate) fn new(pat: &str) -> Option<LightMatcher> {
        let hir = regex_syntax::ParserBuilder::new()
            .utf8(false)
            .dot_matches_new_line(true)
            .build()
            .parse(&with_case(translate(pat).ok()?))
            .ok()?;
        if may_be_too_big(&hir) {
            return None;
        }
        Some(LightMatcher {
            hir,
            ascii: None,
            uses: 0,
        })
    }

    /// Whether the pattern matches `hay`, or `None` when the engine for it cannot be built; the
    /// caller then goes through `compile`, which reports the error.
    pub(crate) fn is_match(&mut self, hay: &[u8]) -> Option<bool> {
        if hay.is_ascii() {
            let hir = &self.hir;
            let engine = self.ascii.get_or_insert_with(|| LightEngine::new(&restrict(hir, &ascii_chars(), true)));
            return engine.as_mut().map(|e| e.is_match(hay));
        }
        LightEngine::new(&restrict(&self.hir, &chars_of(hay), false)).map(|mut e| e.is_match(hay))
    }
}

/// Where a pattern without regex operators (see `as_literal`) has to match its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Literal {
    /// `lit`: anywhere.
    Contains,
    /// `^lit`
    Prefix,
    /// `lit$`
    Suffix,
    /// `^lit$`
    Exact,
}

/// When `pat` is literal text, optionally anchored with `^` and `$` (and IGNORECASE is off),
/// where it has to match and the text. Such patterns, which programs often build from their
/// input (`$2 ~ ("^" $1 "$")`), are matched without compiling (or caching) a regex.
pub(crate) fn as_literal(pat: &[u8]) -> Option<(Literal, &[u8])> {
    if ignore_case() || std::str::from_utf8(pat).is_err() {
        return None;
    }
    let (start, rest) = match pat.strip_prefix(b"^") {
        Some(rest) => (true, rest),
        None => (false, pat),
    };
    let (end, lit) = match rest.strip_suffix(b"$") {
        Some(lit) => (true, lit),
        None => (false, rest),
    };
    let is_meta = |b: &u8| {
        matches!(
            b,
            b'\\' | b'.' | b'[' | b']' | b'(' | b')' | b'*' | b'+' | b'?' | b'{' | b'}' | b'|' | b'^' | b'$'
        )
    };
    if lit.iter().any(is_meta) {
        return None;
    }
    let kind = match (start, end) {
        (false, false) => Literal::Contains,
        (true, false) => Literal::Prefix,
        (false, true) => Literal::Suffix,
        (true, true) => Literal::Exact,
    };
    Some((kind, lit))
}

/// The leftmost match of the literal pattern `(kind, lit)` (see `as_literal`) in `hay`, as
/// (start, end).
pub(crate) fn find_literal(kind: Literal, lit: &[u8], hay: &[u8]) -> Option<(usize, usize)> {
    let n = lit.len();
    match kind {
        Literal::Contains => memchr::memmem::find(hay, lit).map(|i| (i, i + n)),
        Literal::Prefix => hay.starts_with(lit).then_some((0, n)),
        Literal::Suffix => hay.ends_with(lit).then(|| (hay.len() - n, hay.len())),
        Literal::Exact => (hay == lit).then_some((0, n)),
    }
}

/// The syntax error in `pat`, if any, e.g. "unclosed group".
pub(crate) fn syntax_error(pat: &str) -> Option<String> {
    let translated = match translate(pat) {
        Ok(t) => t,
        Err(e) => return Some(e),
    };
    regex_syntax::ast::parse::Parser::new()
        .parse(&translated)
        .err()
        .map(|e| e.kind().to_string())
}

/// A one-line message for an error compiling `pat`: `invalid regex /(/: unclosed group`.
pub(crate) fn compile_error(pat: &str, err: &regex::Error) -> String {
    let reason = syntax_error(pat).unwrap_or_else(|| match err {
        regex::Error::CompiledTooBig(_) => "regex too big".to_string(),
        _ => err.to_string(),
    });
    format!("invalid regex /{}/: {}", pat, reason)
}

/// What it takes to turn a leftmost-first match into the leftmost-longest one.
struct Longest {
    /// A lazy DFA reporting all matches, so that an anchored forward search ends at the longest.
    /// With Unicode word boundaries (`\y`, `\<`, `\>`, `\B`) it gives up on non-ASCII bytes.
    dfa: DFA,
    cache: RefCell<Cache>,
    /// The same as a PikeVM, for when the lazy DFA gives up; built on first use.
    pike: OnceCell<(PikeVM, RefCell<regex_automata::nfa::thompson::pikevm::Cache>)>,
    nfa: regex_automata::nfa::thompson::NFA,
    /// The pattern anchored at the end, `(?:pat)\z`, to get capture groups of a given span.
    exact: OnceCell<Option<Regex>>,
}

/// Memory budget, in bytes, for the patterns kept in `LONGEST`. Like `RegexCache` (see
/// `REGEX_CACHE_BUDGET`), patterns can come from input data, so once the estimated size of the
/// cache reaches this budget the least recently used half of it is evicted.
const LONGEST_BUDGET: usize = 64 << 20;

/// Estimated memory of the longest-match machinery for a pattern of the given length in bytes.
const LONGEST_COST: super::CostModel = super::CostModel {
    base: 8 << 10,
    per_byte: 512,
};

thread_local! {
    /// The longest-match machinery per pattern.
    static LONGEST: RefCell<super::StrLru<Option<Rc<Longest>>>> =
        RefCell::new(super::StrLru::new(LONGEST_BUDGET, LONGEST_COST));
}

/// Whether the leftmost-first match of `pat` (in the syntax of `regex`) can be shorter than the
/// leftmost-longest one: the pattern has an alternation, or a repetition of something other than a
/// single character (`(ab)?(abcd)?`) or that is not greedy. Patterns that fail to parse are
/// assumed to need it.
fn needs_longest(pat: &str) -> bool {
    use regex_syntax::hir::{Hir, HirKind};
    fn one_char(h: &Hir) -> bool {
        match h.kind() {
            HirKind::Class(_) => true,
            HirKind::Literal(lit) => std::str::from_utf8(&lit.0).map_or(lit.0.len() == 1, |s| s.chars().count() == 1),
            HirKind::Capture(c) => one_char(&c.sub),
            _ => false,
        }
    }
    fn walk(h: &Hir) -> bool {
        match h.kind() {
            HirKind::Alternation(_) => true,
            HirKind::Repetition(r) => !r.greedy || !one_char(&r.sub),
            HirKind::Capture(c) => walk(&c.sub),
            HirKind::Concat(hs) => hs.iter().any(walk),
            _ => false,
        }
    }
    regex_syntax::ParserBuilder::new()
        .utf8(false)
        .dot_matches_new_line(true)
        .build()
        .parse(pat)
        .map_or(true, |h| walk(&h))
}

/// The longest-match machinery for `re`, or `None` when leftmost-first is already leftmost-longest
/// (see `needs_longest`) or the DFA cannot be built.
fn longest(re: &Regex) -> Option<Rc<Longest>> {
    let pat = re.as_str();
    LONGEST.with(|cell| {
        if let Some(l) = cell.borrow_mut().get(pat) {
            return l.clone();
        }
        if !needs_longest(pat) {
            cell.borrow_mut().insert(pat.to_owned(), None);
            return None;
        }
        let l = Longest::new(pat).map(Rc::new);
        cell.borrow_mut().insert(pat.to_owned(), l.clone());
        l
    })
}

impl Longest {
    fn new(pat: &str) -> Option<Longest> {
        use regex_automata::nfa::thompson::{Config, WhichCaptures, NFA};
        let nfa = NFA::compiler()
            .syntax(
                regex_automata::util::syntax::Config::new()
                    .utf8(false)
                    .dot_matches_new_line(true),
            )
            .configure(Config::new().utf8(false).which_captures(WhichCaptures::Implicit))
            .build(pat)
            .ok()?;
        let dfa = DFA::builder()
            .configure(DFA::config().match_kind(MatchKind::All).unicode_word_boundary(true))
            .build_from_nfa(nfa.clone())
            .ok()?;
        let cache = RefCell::new(dfa.create_cache());
        Some(Longest { dfa, cache, pike: OnceCell::new(), nfa, exact: OnceCell::new() })
    }
}

/// The end of the longest match of `l` starting at `start`.
fn longest_end(l: &Longest, hay: &[u8], start: usize) -> Option<usize> {
    let input = Input::new(hay).range(start..).anchored(Anchored::Yes);
    if let Ok(m) = l.dfa.try_search_fwd(&mut l.cache.borrow_mut(), &input) {
        return m.map(|m| m.offset());
    }
    // The lazy DFA gave up (Unicode word boundaries next to non-ASCII text): the PikeVM, also
    // reporting all matches, ends at the longest one too.
    let (pike, cache) = l.pike.get_or_init(|| {
        let pike = PikeVM::builder()
            .configure(PikeVM::config().match_kind(MatchKind::All))
            .build_from_nfa(l.nfa.clone())
            .expect("PikeVM supports every NFA");
        let cache = RefCell::new(pike.create_cache());
        (pike, cache)
    });
    pike.find(&mut cache.borrow_mut(), input).map(|m| m.end())
}

/// The leftmost-longest match of `re` in `hay` starting at or after `start`, as (start, end).
pub(crate) fn find_at(re: &Regex, hay: &[u8], start: usize) -> Option<(usize, usize)> {
    let m = re.find_at(hay, start)?;
    let (from, to) = (m.start(), m.end());
    match longest(re) {
        Some(l) => Some((from, longest_end(&l, hay, from).map_or(to, |e| e.max(to)))),
        None => Some((from, to)),
    }
}

/// What it takes to tell whether a match of a pattern can still change when more text is appended
/// (see `may_extend`).
struct Extender {
    /// A lazy DFA reporting all matches; an anchored search from a position is alive at the end of
    /// the text while the text from that position is the prefix of a possible match.
    dfa: DFA,
    cache: RefCell<Cache>,
    /// The maximum length of a match, if bounded.
    max_len: Option<usize>,
    /// The bytes an anchored search can start with without dying at once, whatever precedes it;
    /// only positions holding one of them need a search (see `may_extend`).
    first: Box<[bool; 256]>,
    /// The bytes of `first`, when there are at most three, to find them with memchr.
    few_first: Option<Vec<u8>>,
}

/// The bytes that the anchored searches of `dfa` can consume first without dying or quitting,
/// in any of the contexts a start state depends on (the text start, a line terminator, a word
/// or non-word byte before it).
fn first_bytes(dfa: &DFA) -> Box<[bool; 256]> {
    let mut cache = dfa.create_cache();
    let mut first = Box::new([false; 256]);
    for behind in [None, Some(b'\n'), Some(b'\r'), Some(b'a'), Some(b' '), Some(0x80)] {
        let config = regex_automata::util::start::Config::new()
            .anchored(Anchored::Yes)
            .look_behind(behind);
        // Contexts that quit (non-ASCII bytes with Unicode word boundaries) never get a
        // search going either (see `alive_at_end`).
        let Ok(start) = dfa.start_state(&mut cache, &config) else {
            continue;
        };
        for b in 0..=255u8 {
            if let Ok(sid) = dfa.next_state(&mut cache, start, b) {
                first[b as usize] |= !sid.is_dead() && !sid.is_quit();
            }
        }
    }
    first
}

thread_local! {
    /// The extension-check machinery per pattern (record separators, so few of them).
    static EXTENDERS: RefCell<super::StrLru<Option<Rc<Extender>>>> =
        RefCell::new(super::StrLru::new(LONGEST_BUDGET, LONGEST_COST));
    /// The pattern last looked up in `EXTENDERS` and its entry: a reader asks for the same
    /// separator once per record.
    static LAST_EXTENDER: RefCell<Option<(String, Option<Rc<Extender>>)>> = const { RefCell::new(None) };
}

/// The extension-check machinery for `re`, or `None` when the DFA cannot be built.
fn extender(re: &Regex) -> Option<Rc<Extender>> {
    let pat = re.as_str();
    if let Some(x) = LAST_EXTENDER.with_borrow(|last| last.as_ref().filter(|(p, _)| p == pat).map(|(_, x)| x.clone())) {
        return x;
    }
    let x = extender_uncached(pat);
    LAST_EXTENDER.with_borrow_mut(|last| *last = Some((pat.to_owned(), x.clone())));
    x
}

fn extender_uncached(pat: &str) -> Option<Rc<Extender>> {
    EXTENDERS.with(|cell| {
        if let Some(x) = cell.borrow_mut().get(pat) {
            return x.clone();
        }
        let max_len = regex_syntax::ParserBuilder::new()
            .utf8(false)
            .dot_matches_new_line(true)
            .build()
            .parse(pat)
            .ok()
            .and_then(|h| h.properties().maximum_len());
        let x = DFA::builder()
            .configure(DFA::config().match_kind(MatchKind::All).unicode_word_boundary(true))
            .syntax(
                regex_automata::util::syntax::Config::new()
                    .utf8(false)
                    .dot_matches_new_line(true),
            )
            .thompson(regex_automata::nfa::thompson::Config::new().utf8(false))
            .build(pat)
            .ok()
            .map(|dfa| {
                let first = first_bytes(&dfa);
                let few: Vec<u8> = (0..=255u8).filter(|&b| first[b as usize]).collect();
                let few_first = (few.len() <= 3).then_some(few);
                let cache = RefCell::new(dfa.create_cache());
                Rc::new(Extender { dfa, cache, max_len, first, few_first })
            });
        cell.borrow_mut().insert(pat.to_owned(), x.clone());
        x
    })
}

/// Whether an anchored search for `x` from `from` is still alive at the end of `hay`, i.e. more
/// text could complete or lengthen a match starting there.
fn alive_at_end(x: &Extender, cache: &mut Cache, hay: &[u8], from: usize) -> bool {
    let input = Input::new(hay).range(from..).anchored(Anchored::Yes);
    let Ok(mut sid) = x.dfa.start_state_forward(cache, &input) else {
        return false;
    };
    for &b in &hay[from..] {
        sid = match x.dfa.next_state(cache, sid, b) {
            Ok(sid) => sid,
            Err(_) => return false,
        };
        // A quit state (Unicode word boundaries next to non-ASCII text) is taken as dead: the
        // match is then kept, rather than reading on, possibly to the end of the input.
        if sid.is_dead() || sid.is_quit() {
            return false;
        }
    }
    true
}

/// Whether the leftmost-longest match of `re` in `hay` starting at `start` (as found by
/// `find_at`) can change when more text is appended to `hay`: lengthen (`a|abc` matching `a` of
/// "ab", `中+` matching "中" before the first byte of another "中", `x\>` before the end), or be
/// preceded by a match starting earlier (`abcd|c` matching `c` of "abc"). Used by readers
/// that search a buffer of the input before the rest of it is read.
pub(crate) fn may_extend(re: &Regex, hay: &[u8], start: usize) -> bool {
    let Some(x) = extender(re) else {
        return false;
    };
    // A search from `from` can only reach the end of `hay` if a match can be that long.
    let lo = x.max_len.map_or(0, |m| hay.len().saturating_sub(m));
    if lo > start {
        return false;
    }
    let mut cache = x.cache.borrow_mut();
    let mut alive = |from: usize| alive_at_end(&x, &mut cache, hay, from);
    let window = &hay[lo..=start];
    match x.few_first.as_deref() {
        Some([]) => false,
        Some(&[a]) => memchr::memchr_iter(a, window).any(|i| alive(lo + i)),
        Some(&[a, b]) => memchr::memchr2_iter(a, b, window).any(|i| alive(lo + i)),
        Some(&[a, b, c]) => memchr::memchr3_iter(a, b, c, window).any(|i| alive(lo + i)),
        _ => (lo..=start).any(|from| x.first[hay[from] as usize] && alive(from)),
    }
}

/// The position just past the character at `pos`: a whole UTF-8 sequence when one starts there,
/// otherwise a single byte. Used to step over an empty match without splitting a character.
pub(crate) fn next_char(hay: &[u8], pos: usize) -> usize {
    let n = match hay.get(pos) {
        Some(&b) if b >= 0xF0 => 4,
        Some(&b) if b >= 0xE0 => 3,
        Some(&b) if b >= 0xC0 => 2,
        _ => 1,
    };
    if n > 1 && hay.get(pos..pos + n).is_some_and(|c| std::str::from_utf8(c).is_ok()) {
        pos + n
    } else {
        pos + 1
    }
}

/// All successive non-overlapping leftmost-longest matches of `re` in `hay`, as (start, end).
/// As with `Regex::find_iter`, an empty match right after the previous match is skipped.
pub(crate) fn find_iter<'h>(
    re: &'h Regex,
    hay: &'h [u8],
) -> impl Iterator<Item = (usize, usize)> + 'h {
    let mut pos = 0;
    let mut last_end = None;
    std::iter::from_fn(move || loop {
        if pos > hay.len() {
            return None;
        }
        let (from, to) = find_at(re, hay, pos)?;
        if from == to && last_end == Some(to) {
            pos = next_char(hay, to);
            continue;
        }
        pos = to;
        last_end = Some(to);
        return Some((from, to));
    })
}

/// The capture groups of `re` for the match `from..to` of `hay` (as found by `find_at`).
pub(crate) fn captures_at<'h>(re: &Regex, hay: &'h [u8], from: usize, to: usize) -> Option<Captures<'h>> {
    if let Some(l) = longest(re) {
        let exact = l.exact.get_or_init(|| {
            RegexBuilder::new(&format!("(?:{})\\z", re.as_str()))
                .dot_matches_new_line(true)
                .build()
                .ok()
        });
        if let Some(exact) = exact {
            // Text before `from` stays visible to look-behind assertions such as `\b`.
            if let Some(c) = exact.captures_at(&hay[..to], from) {
                if c.get(0).map(|m| m.start()) == Some(from) {
                    return Some(c);
                }
            }
        }
    }
    re.captures_at(hay, from)
}

fn push_literal(out: &mut String, c: char) {
    if c.is_ascii_punctuation() {
        out.push('\\');
    }
    out.push(c);
}

/// The value of the octal escape (up to three digits) at `chars[*i]`.
fn parse_octal(chars: &[char], i: &mut usize) -> u8 {
    let mut value = 0u32;
    let mut n = 0;
    while n < 3 && *i < chars.len() && ('0'..='7').contains(&chars[*i]) {
        value = value * 8 + chars[*i].to_digit(8).unwrap();
        *i += 1;
        n += 1;
    }
    (value & 0xff) as u8
}

/// The value of the hex escape (one or two digits, as in gawk) at `chars[*i]`, just after `\x`.
fn parse_hex(chars: &[char], i: &mut usize) -> Option<u8> {
    let mut value = 0u32;
    let mut n = 0;
    while n < 2 && *i < chars.len() && chars[*i].is_ascii_hexdigit() {
        value = value * 16 + chars[*i].to_digit(16).unwrap();
        *i += 1;
        n += 1;
    }
    (n > 0).then_some(value as u8)
}

/// Push the byte escape `b` (from `\351` or `\xE9`). As in gawk, it matches a byte, not the
/// character U+00E9: ASCII values are the same either way, the others are matched with Unicode
/// mode off. Inside a bracket expression the escape is written as `\xE9`, and
/// `translate_bracket` moves it out of the Unicode part of the class.
fn push_byte(out: &mut String, b: u8, in_bracket: bool) {
    if b < 0x80 {
        out.push_str(&format!("\\x{{{:x}}}", b));
    } else if in_bracket {
        out.push_str(&format!("\\x{:02X}", b));
    } else {
        out.push_str(&format!("(?-u:\\x{:02X})", b));
    }
}

/// The byte escape (`\351` or `\xE9`) at `chars[*i]` (a backslash), if any, consuming it.
fn byte_escape(chars: &[char], i: &mut usize) -> Option<u8> {
    if chars.get(*i) != Some(&'\\') {
        return None;
    }
    let mut j = *i + 1;
    let b = match chars.get(j) {
        Some('0'..='7') => parse_octal(chars, &mut j),
        Some('x') if chars.get(j + 1) != Some(&'{') => {
            j += 1;
            parse_hex(chars, &mut j)?
        }
        _ => return None,
    };
    *i = j;
    Some(b)
}

/// Copy the `{...}` at `chars[*i]` to `out`, up to and including the closing brace.
fn push_braced(out: &mut String, chars: &[char], i: &mut usize) {
    while let Some(&c) = chars.get(*i) {
        out.push(c);
        *i += 1;
        if c == '}' {
            break;
        }
    }
}

/// Translate the escape sequence at `chars[*i]` (just after a backslash).
fn translate_escape(out: &mut String, chars: &[char], i: &mut usize, in_bracket: bool) {
    let c = match chars.get(*i) {
        Some(c) => *c,
        None => {
            // A trailing backslash is a literal backslash.
            out.push_str("\\\\");
            return;
        }
    };
    *i += 1;
    match c {
        'y' if !in_bracket => out.push_str(r"\b"),
        '<' if !in_bracket => out.push_str(r"\b{start}"),
        '>' if !in_bracket => out.push_str(r"\b{end}"),
        '`' if !in_bracket => out.push_str(r"\A"),
        '\'' if !in_bracket => out.push_str(r"\z"),
        'B' if !in_bracket => out.push_str(r"\B"),
        'b' => out.push_str(r"\x08"),
        'a' => out.push_str(r"\x07"),
        'v' => out.push_str(r"\x0B"),
        'f' => out.push_str(r"\x0C"),
        'n' | 't' | 'r' | 'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
            out.push('\\');
            out.push(c);
        }
        // Unicode classes and code points, e.g. \pL, \p{Greek} or \x{e9}, as supported by `regex`.
        // The braces are copied as is, rather than being taken for an interval.
        'p' | 'P' => {
            out.push('\\');
            out.push(c);
            if chars.get(*i) == Some(&'{') {
                push_braced(out, chars, i);
            }
        }
        'x' if chars.get(*i) == Some(&'{') => {
            out.push_str("\\x");
            push_braced(out, chars, i);
        }
        // gawk's hex escapes (one or two digits) are bytes, like octal escapes.
        'x' => match parse_hex(chars, i) {
            Some(b) => push_byte(out, b, in_bracket),
            None => out.push('x'),
        },
        '0'..='7' => {
            *i -= 1;
            let b = parse_octal(chars, i);
            push_byte(out, b, in_bracket);
        }
        // Other escaped characters are literal (gawk warns about unknown escapes).
        c => push_literal(out, c),
    }
}

/// The Unicode-aware equivalent of the POSIX character class `[:name:]`, for use inside a
/// bracket expression. `digit` and `xdigit` stay ASCII, like glibc's in a UTF-8 locale, and (also
/// like glibc) other decimal digits such as `٣` count as `alpha`. Returns None for unknown names.
fn posix_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => r"\p{Alphabetic}[\p{Nd}--0-9]",
        "alnum" => r"\p{Alphabetic}\p{Nd}",
        "upper" => r"\p{Uppercase}",
        "lower" => r"\p{Lowercase}",
        "digit" => "0-9",
        "xdigit" => "0-9A-Fa-f",
        "space" => r"\p{White_Space}",
        "blank" => r"\t\p{Zs}",
        "punct" => r"\p{P}\p{S}",
        "cntrl" => r"\p{Cc}",
        "graph" => r"[^\p{C}\p{Z}]",
        "print" => r"[^\p{C}\p{Zl}\p{Zp}]",
        "word" => r"\w",
        _ => return None,
    })
}

/// Whether `chars[i..]` starts with a valid interval: `{n}`, `{n,}` or `{n,m}`.
fn interval_len(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == start {
        return None;
    }
    if chars.get(j) == Some(&',') {
        j += 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
    }
    if chars.get(j) == Some(&'}') {
        Some(j + 1 - i)
    } else {
        None
    }
}

/// Translate the bracket expression starting at `chars[*i]` (a `[`). Returns false if it is not
/// terminated, in which case nothing is consumed, and an error for an unknown class like `[:foo:]`.
fn translate_bracket(out: &mut String, chars: &[char], i: &mut usize) -> Result<bool, String> {
    let mut j = *i + 1;
    let mut res = String::from("[");
    // Byte escapes of non-ASCII values (and ranges of them), matched with Unicode mode off.
    let mut bytes = String::new();
    let negated = chars.get(j) == Some(&'^');
    if negated {
        res.push('^');
        j += 1;
    }
    // A `]` first in the list is a literal.
    if chars.get(j) == Some(&']') {
        res.push_str(r"\]");
        j += 1;
    }
    loop {
        let c = match chars.get(j) {
            Some(c) => *c,
            None => return Ok(false),
        };
        match c {
            ']' => {
                res.push(']');
                j += 1;
                break;
            }
            '[' => match chars.get(j + 1) {
                // Character classes like [:alpha:].
                Some(':') => {
                    let end = (j + 2..chars.len().saturating_sub(1))
                        .find(|&k| chars[k] == ':' && chars[k + 1] == ']');
                    match end {
                        Some(end) => {
                            let name: String = chars[j + 2..end].iter().collect();
                            match posix_class(&name) {
                                Some(class) => res.push_str(class),
                                None => return Err(format!("invalid character class name [:{}:]", name)),
                            }
                            j = end + 2;
                        }
                        None => {
                            res.push_str(r"\[");
                            j += 1;
                        }
                    }
                }
                // Collating symbols and equivalence classes of one character, e.g. [.-.].
                Some('.') | Some('=') if chars.get(j + 3) == Some(&chars[j + 1]) && chars.get(j + 4) == Some(&']') => {
                    push_literal(&mut res, chars[j + 2]);
                    j += 5;
                }
                _ => {
                    res.push_str(r"\[");
                    j += 1;
                }
            },
            '\\' => {
                if let Some(k) = high_byte_item(&mut bytes, chars, j) {
                    j = k;
                    continue;
                }
                j += 1;
                translate_escape(&mut res, chars, &mut j, /*in_bracket=*/ true);
            }
            // Set operators in the `regex` crate; literal characters in awk.
            '&' | '~' => {
                res.push('\\');
                res.push(c);
                j += 1;
            }
            '-' if chars.get(j + 1) == Some(&'-') => {
                res.push_str(r"\-");
                j += 1;
            }
            c => {
                res.push(c);
                j += 1;
            }
        }
    }
    if bytes.is_empty() {
        out.push_str(&res);
    } else {
        let body = &res[if negated { 2 } else { 1 }..res.len() - 1];
        let neg = if negated { "^" } else { "" };
        let byte_safe = body.is_ascii()
            && !["\\p", "\\P", "\\w", "\\W", "\\d", "\\D", "\\s", "\\S"].iter().any(|e| body.contains(e));
        if byte_safe {
            // ASCII is the same with Unicode mode on or off, so the whole class can be bytes.
            out.push_str(&format!("(?-u:[{}{}{}])", neg, body, bytes));
        } else if !negated {
            out.push_str(&format!("(?:{}|(?-u:[{}]))", res, bytes));
        } else {
            // A negated class mixing Unicode classes and bytes cannot be split into an
            // alternation; fall back to treating the bytes as code points.
            out.push_str(&format!("[^{}{}]", body, bytes));
        }
    }
    *i = j;
    Ok(true)
}

/// If the bracket expression item at `chars[j]` (a backslash) is a byte escape of a non-ASCII
/// value, or a range with such an end (`\200-\377`), push it to `bytes` in the syntax of `regex`
/// and return the index after it.
fn high_byte_item(bytes: &mut String, chars: &[char], j: usize) -> Option<usize> {
    let mut k = j;
    let lo = byte_escape(chars, &mut k)?;
    let mut hi = None;
    if chars.get(k) == Some(&'-') && chars.get(k + 1).is_some_and(|&c| c != ']') {
        let mut m = k + 1;
        hi = match byte_escape(chars, &mut m) {
            Some(b) => Some(b),
            None if chars[m] != '\\' && chars[m].is_ascii() => {
                m += 1;
                Some(chars[m - 1] as u8)
            }
            None => None,
        };
        if hi.is_some() {
            k = m;
        }
    }
    if lo < 0x80 && hi.map_or(true, |h| h < 0x80) {
        return None;
    }
    bytes.push_str(&format!("\\x{:02X}", lo));
    if let Some(h) = hi {
        bytes.push_str(&format!("-\\x{:02X}", h));
    }
    Some(k)
}

/// Translate an awk regular expression into the syntax of the `regex` crate. Errors that `regex`
/// would not report, like an unknown character class name, are returned as a message.
pub(crate) fn translate(pat: &str) -> Result<String, String> {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(pat.len() + 8);
    // Whether we are at the start of an expression (where a repetition operator is a literal).
    let mut at_start = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
                translate_escape(&mut out, &chars, &mut i, /*in_bracket=*/ false);
                at_start = false;
            }
            '[' => {
                if !translate_bracket(&mut out, &chars, &mut i)? {
                    out.push_str(r"\[");
                    i += 1;
                }
                at_start = false;
            }
            '{' => {
                match interval_len(&chars, i) {
                    Some(len) if !at_start => {
                        out.extend(&chars[i..i + len]);
                        i += len;
                    }
                    _ => {
                        out.push_str(r"\{");
                        i += 1;
                    }
                }
                at_start = false;
            }
            '}' => {
                out.push_str(r"\}");
                i += 1;
                at_start = false;
            }
            '(' => {
                out.push('(');
                i += 1;
                // Pass through groups like `(?:...)` (zawk generates them; gawk has none).
                if chars.get(i) == Some(&'?') {
                    out.push('?');
                    i += 1;
                }
                at_start = true;
            }
            '|' => {
                out.push('|');
                i += 1;
                at_start = true;
            }
            '^' => {
                out.push('^');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
                at_start = false;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_match(pat: &str, s: &str) -> bool {
        compile(pat).unwrap().is_match(s.as_bytes())
    }

    #[test]
    fn translation() {
        assert_eq!(translate(r"\yfoo\y").unwrap(), r"\bfoo\b");
        assert_eq!(translate(r"\<a\>").unwrap(), r"\b{start}a\b{end}");
        assert_eq!(translate("a{").unwrap(), r"a\{");
        assert_eq!(translate("{").unwrap(), r"\{");
        assert_eq!(translate("a{,}").unwrap(), r"a\{,\}");
        assert_eq!(translate("a{2}b{1,}c{1,3}").unwrap(), "a{2}b{1,}c{1,3}");
        assert_eq!(translate(r"\101").unwrap(), r"\x{41}");
        assert_eq!(translate("[[]").unwrap(), r"[\[]");
        assert_eq!(translate("[]a]").unwrap(), r"[\]a]");
        assert_eq!(translate("[^]a]").unwrap(), r"[^\]a]");
        assert_eq!(translate("[[:alpha:]_]").unwrap(), r"[\p{Alphabetic}[\p{Nd}--0-9]_]");
        assert_eq!(translate("[^[:digit:]]").unwrap(), "[^0-9]");
        assert_eq!(translate("[a&&b]").unwrap(), r"[a\&\&b]");
        assert_eq!(translate("(?:.)").unwrap(), "(?:.)");
        assert_eq!(translate(r"\351").unwrap(), r"(?-u:\xE9)");
        assert_eq!(translate(r"\xc3\xa9").unwrap(), r"(?-u:\xC3)(?-u:\xA9)");
        assert_eq!(translate(r"\x41\xg").unwrap(), r"\x{41}xg");
        assert_eq!(translate(r"\x{e9}").unwrap(), r"\x{e9}");
        assert_eq!(translate(r"\p{Greek}+").unwrap(), r"\p{Greek}+");
        assert_eq!(translate(r"[\P{Greek}\x{e9}]").unwrap(), r"[\P{Greek}\x{e9}]");
        assert_eq!(translate(r"\pL").unwrap(), r"\pL");
        assert!(translate("[[:foo:]]").is_err());
        assert_eq!(syntax_error("[[:foo:]]").unwrap(), "invalid character class name [:foo:]");
        assert!(compile("a|[^[:nope:]]").is_err());
        assert_eq!(translate(r"[a\351]").unwrap(), r"(?-u:[a\xE9])");
        assert_eq!(translate(r"[^\200-\377]").unwrap(), r"(?-u:[^\x80-\xFF])");
        assert_eq!(translate(r"[é\351]").unwrap(), r"(?:[é]|(?-u:[\xE9]))");
    }

    #[test]
    fn byte_escapes() {
        assert!(is_match(r"\303\251", "é"));
        assert!(is_match(r"\xc3\xa9", "é"));
        assert!(!is_match(r"^\351", "é"));
        assert!(compile(r"^\351$").unwrap().is_match(b"\xe9"));
        assert!(compile(r"^[\200-\377]+$").unwrap().is_match("中文".as_bytes()));
        assert!(!is_match(r"[\200-\377]", "abc"));
        assert!(is_match(r"^[^\200-\377]+$", "abc"));
        assert!(is_match(r"^[[:alpha:]\351]$", "é"));
        assert!(compile(r"^[[:alpha:]\351]$").unwrap().is_match(b"\xe9"));
        assert!(is_match(r"^\x{e9}$", "é"));
        assert!(is_match(r"\101\x42", "AB"));
    }

    #[test]
    fn matching() {
        assert!(is_match("a.b", "a\nb"));
        assert!(is_match("{", "{"));
        assert!(is_match("a{", "xa{"));
        assert!(is_match("a{,}", "a{,}"));
        assert!(!is_match("^a{2}$", "a"));
        assert!(is_match(r"\yfoo\y", "a foo b"));
        assert!(!is_match(r"\yfoo\y", "afoob"));
        assert!(is_match(r"\<foo\>", "foo"));
        assert!(!is_match(r"\<oo", "foo"));
        assert!(is_match(r"\`ab\'", "ab"));
        assert!(!is_match(r"\`b", "ab"));
        assert!(is_match("[[]", "["));
        assert!(is_match("[]]", "]"));
        assert!(is_match("[a&]", "&"));
        assert!(is_match(r"\101", "A"));
        assert!(is_match(r"\q", "q"));
        assert!(is_match("[[:digit:]]+", "x12"));
    }

    #[test]
    fn leftmost_longest() {
        let spans = |pat: &str, s: &str| -> Vec<(usize, usize)> {
            find_iter(&compile(pat).unwrap(), s.as_bytes()).collect()
        };
        assert_eq!(spans("a|ab", "abcd"), vec![(0, 2)]);
        assert_eq!(spans("x|xy", "xyz"), vec![(0, 2)]);
        assert_eq!(spans("o|oo", "foobar"), vec![(1, 3)]);
        assert_eq!(spans("y|", "x"), vec![(0, 0), (1, 1)]);
        assert_eq!(spans("ab", "abab"), vec![(0, 2), (2, 4)]);
        let re = compile("(x|xy)(z?)").unwrap();
        let caps = captures_at(&re, b"xyz", 0, 3).unwrap();
        assert_eq!(&caps[1], b"xy");
        assert_eq!(&caps[2], b"z");
        // Repeated groups choose between alternatives too.
        assert_eq!(spans("(ab)?(abcd)?", "abcd")[0], (0, 4));
        assert_eq!(spans("x(y)?(yz)?", "xyz"), vec![(0, 3)]);
        assert_eq!(spans("(a|ab)(c|bcd)", "abcd"), vec![(0, 4)]);
        let re = compile("(ab)?(abcd)?").unwrap();
        let caps = captures_at(&re, b"abcd", 0, 4).unwrap();
        assert!(caps.get(1).is_none());
        assert_eq!(&caps[2], b"abcd");
        // Word boundaries, in ASCII text (lazy DFA) and next to non-ASCII text (PikeVM).
        assert_eq!(spans("\\<(a|ab)", "ab c"), vec![(0, 2)]);
        assert_eq!(spans("(a|ab)\\B", "abc"), vec![(0, 2)]);
        assert_eq!(spans("\\y(a|ab)", "é ab"), vec![(3, 5)]);
        assert_eq!(spans("(a|ab)\\B", "abé"), vec![(0, 2)]);
        assert_eq!(spans("\\<(中|中文)", "说 中文"), vec![(4, 10)]);
        assert_eq!(spans("(x|xy)\\y", "é xy é"), vec![(3, 5)]);
    }

    #[test]
    fn light_matcher_agrees_with_compile() {
        let pats = [
            "^[a-z]+ ab", "[[:alpha:]]+ x$", "[[:upper:]]", "[^a]b", "^.$", "^..$", "\\<caf\\>",
            "\\yab\\y", "\\Bé", "(a|ab)(c|bcd)", "[[:space:]]+", "\\303\\251", "^\\351", "\\p{Greek}+",
            "[^[:alnum:]]", "x*", "^$", "[é-ë]", "数据|data", "\\w+\\s\\w+$", "a{2,3}", ".{100}",
        ];
        let hays: [&[u8]; 14] = [
            b"", b"ab", b"xab", b"abc x", b"caf\xc3\xa9", b"caf", b"na\xc3\xafve caf\xc3\xa9 ab", b"\xce\xb1\xce\xb2",
            b"\xe6\x95\xb0\xe6\x8d\xae", b"Ab", b"\xe9", b"\xe2\x80\x83 x", b"aa", b"word word",
        ];
        for ic in [false, true] {
            set_ignore_case(ic);
            for pat in pats {
                let re = compile(pat).unwrap();
                let Some(mut light) = LightMatcher::new(pat) else {
                    assert_eq!(pat, ".{100}");
                    continue;
                };
                for hay in hays {
                    assert_eq!(light.is_match(hay), Some(re.is_match(hay)), "{:?} ~ /{}/ ic={}", hay, pat, ic);
                }
            }
        }
        set_ignore_case(false);
        assert!(LightMatcher::new("(").is_none());
        assert!(LightMatcher::new("[[:foo:]]").is_none());
    }

    #[test]
    fn empty_matches_step_over_whole_chars() {
        let re = compile("x*").unwrap();
        let spans: Vec<_> = find_iter(&re, "éb".as_bytes()).collect();
        assert_eq!(spans, vec![(0, 0), (2, 2), (3, 3)]);
        let spans: Vec<_> = find_iter(&re, "中".as_bytes()).collect();
        assert_eq!(spans, vec![(0, 0), (3, 3)]);
        // Invalid UTF-8 advances one byte at a time.
        let spans: Vec<_> = find_iter(&re, b"\xffa").collect();
        assert_eq!(spans, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(next_char("é".as_bytes(), 0), 2);
        assert_eq!(next_char(b"\xc3", 0), 1);
        assert_eq!(next_char(b"", 0), 1);
    }

    #[test]
    fn needs_longest_from_hir() {
        assert!(needs_longest("a|ab"));
        assert!(needs_longest("(ab)?(abcd)?"));
        assert!(needs_longest("(?:ab)*a"));
        assert!(!needs_longest("[|]"));
        assert!(!needs_longest(r"\|"));
        assert!(!needs_longest("a|b"));
        assert!(!needs_longest("x?y*[a-z]+(é)?(.){2,3}"));
    }

    /// Patterns that `needs_longest` lets through use leftmost-first directly: check against the
    /// longest match from the all-matches DFA on every short haystack.
    #[test]
    fn leftmost_first_is_longest_when_skipped() {
        let atoms = ["a", "b", "a?", "b?", "a*", "b+", "[ab]?", "(a)?", ".?"];
        let hays: Vec<String> = (0..4)
            .flat_map(|n| (0..1 << n).map(move |m| (0..n).map(|i| if m >> i & 1 == 1 { 'b' } else { 'a' }).collect()))
            .collect();
        for x in atoms {
            for y in atoms {
                for z in atoms {
                    let pat = format!("{}{}{}", x, y, z);
                    assert!(!needs_longest(&pat), "{}", pat);
                    let re = compile(&pat).unwrap();
                    let l = Longest::new(&pat).unwrap();
                    for hay in &hays {
                        if let Some(m) = re.find(hay.as_bytes()) {
                            assert_eq!(longest_end(&l, hay.as_bytes(), m.start()), Some(m.end()), "/{}/ {:?}", pat, hay);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unicode_posix_classes() {
        assert!(is_match("^[[:alpha:]]+$", "héllo中文"));
        assert!(!is_match("[[:alpha:]]", "123 !"));
        assert!(is_match("^[[:alnum:]]+$", "é9"));
        assert!(is_match("^[[:upper:]]$", "É"));
        assert!(is_match("^[[:lower:]]$", "é"));
        assert!(!is_match("[[:digit:]]", "٣"));
        assert!(is_match("[[:alpha:]]", "٣"));
        assert!(is_match("^[[:space:]]$", "\u{3000}"));
        assert!(is_match("^[[:blank:]]$", "\t"));
        assert!(is_match("^[[:punct:]]+$", "，。!$"));
        assert!(is_match("^[[:graph:]]+$", "中!"));
        assert!(!is_match("[[:graph:]]", " \t"));
        assert!(is_match("^[[:print:]]+$", "中 a"));
        assert!(!is_match("[[:print:]]", "\n"));
        assert!(is_match("^[^[:alpha:]]+$", "123"));
        assert!(!is_match("[^[:alpha:]]", "中文"));
    }

    #[test]
    fn longest_cache_is_bounded() {
        const BUDGET: usize = 2 << 20;
        LONGEST.with(|cell| cell.borrow_mut().budget = BUDGET);
        let hot = compile("hot|hotter").unwrap();
        for i in 0..(BUDGET / LONGEST_COST.cost(8) * 3) {
            assert_eq!(find_at(&hot, b"hotter", 0), Some((0, 6)));
            let re = compile(&format!("zz|x{}", i)).unwrap();
            assert_eq!(find_at(&re, format!("x{}", i).as_bytes(), 0).map(|(f, _)| f), Some(0));
            LONGEST.with(|cell| {
                let cache = cell.borrow();
                assert!(cache.used <= BUDGET);
                // A frequently used pattern survives evictions.
                assert!(cache.cached.contains_key(hot.as_str()));
            });
        }
        // Evicted patterns are rebuilt and still match leftmost-longest.
        let re = compile("x|x0").unwrap();
        assert_eq!(find_at(&re, b"x0", 0), Some((0, 2)));
    }
}
