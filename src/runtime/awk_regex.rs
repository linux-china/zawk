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
use regex_automata::util::pool::Pool;
use regex_automata::{Anchored, Input, MatchKind};
use std::cell::{OnceCell, RefCell};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

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
pub(crate) fn compile(pat: &str) -> Result<AwkRegex, regex::Error> {
    RegexBuilder::new(&with_case(translate(pat).map_err(regex::Error::Syntax)?))
        .dot_matches_new_line(true)
        .build()
        .map(AwkRegex::new)
}

/// A compiled regex, with what it takes to report its leftmost-longest matches (see `find_at`),
/// so that searches need no lookup by pattern. It dereferences to the `Regex`, whose own methods
/// report leftmost-first matches.
pub struct AwkRegex {
    re: Regex,
    /// Built on the first search for the leftmost-longest match: `is_match` (`~`) does not need
    /// it. `None` when leftmost-first is already leftmost-longest (see `needs_longest`) or the DFA
    /// cannot be built.
    longest: OnceLock<Option<Longest>>,
    /// For the regexes of `RegexCache` (see `counted`): the memory of `longest`, charged to
    /// `regex_memory` until the regex is dropped.
    charged: Option<AtomicUsize>,
    /// When the pattern is literal text (see `literal_parts`): searches go through memchr
    /// instead of the regex engines.
    literal: Option<LiteralSearch<'static>>,
}

impl AwkRegex {
    /// Wraps `re` (in the syntax of `regex`).
    pub(crate) fn new(re: Regex) -> AwkRegex {
        // A pattern compiled under IGNORECASE starts with `(?i)`, so it is never literal here.
        let literal = literal_parts(re.as_str().as_bytes())
            .map(|(kind, lit)| LiteralSearch::new(kind, lit).into_owned());
        AwkRegex {
            re,
            longest: OnceLock::new(),
            charged: None,
            literal,
        }
    }

    /// Counts the memory of what is built for leftmost-longest matches against `regex_memory`,
    /// for regexes of a thread's `RegexCache`, which are dropped on the thread that built them.
    pub(crate) fn counted(mut self) -> AwkRegex {
        self.charged = Some(AtomicUsize::new(0));
        self
    }

    fn longest(&self) -> Option<&Longest> {
        self.longest
            .get_or_init(|| {
                let pat = self.re.as_str();
                if !needs_longest(pat) {
                    return None;
                }
                let Some(charged) = &self.charged else {
                    return Longest::new(pat);
                };
                let (l, measured) = super::regex_memory::measure(|| Longest::new(pat));
                if l.is_some() {
                    let cost = measured.unwrap_or_else(|| LONGEST_COST.cost(pat.len()));
                    super::regex_memory::make_room(cost);
                    super::regex_memory::charge(cost);
                    charged.store(cost, Ordering::Relaxed);
                }
                l
            })
            .as_ref()
    }
}

impl Drop for AwkRegex {
    fn drop(&mut self) {
        if let Some(charged) = &self.charged {
            super::regex_memory::release(charged.load(Ordering::Relaxed));
        }
    }
}

impl std::ops::Deref for AwkRegex {
    type Target = Regex;
    fn deref(&self) -> &Regex {
        &self.re
    }
}

impl std::fmt::Debug for AwkRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.re.fmt(f)
    }
}

/// A matcher that is cheap to build, for dynamic patterns that are used only a few times
/// (`$0 ~ ("^[a-z]+ " $9)`, `match($0, "^" $1 " [^ ]+")`, one pattern per record). `compile`
/// builds `regex`'s full set of engines (reverse NFA, one-pass DFA, a full DFA for small patterns,
/// prefilter), which takes 25–90µs; the Thompson NFA and PikeVM used here take 1–3µs, and match
/// short text about as fast. `RegexCache` switches a pattern to `compile` once it is reused.
///
/// Unicode classes (`[[:alpha:]]` is `\p{Alphabetic}`, `.` any character) make up most of the
/// build time of the NFA, so the engine is built for the characters of the text at hand (see
/// `restrict`): a match can only consume characters of the text, so the result is the same. The
/// engine for ASCII text is kept; for other text one is built per call.
///
/// Before any engine is used, the text is checked for the literals every match starts and ends
/// with (see `may_match`): a pattern built from the input mostly does not match it.
pub(crate) struct LightMatcher {
    hir: regex_syntax::hir::Hir,
    /// Whether leftmost-first matches can be shorter than leftmost-longest ones (see
    /// `needs_longest`).
    longest: bool,
    /// Literals one of which every match starts with, and whether matches start at the start of
    /// the text; `None` when unknown.
    prefixes: Option<(Vec<Vec<u8>>, bool)>,
    /// The same for the end of matches.
    suffixes: Option<(Vec<Vec<u8>>, bool)>,
    /// The engine for ASCII text, once built; `Some(None)` when it cannot be built. Boxed, as the
    /// registry of matchers keeps them in its table.
    ascii: Option<Option<Box<LightEngine>>>,
    /// How many times the matcher was used.
    pub(crate) uses: u32,
}

/// A PikeVM with its cache.
struct Pike {
    vm: PikeVM,
    cache: RefCell<regex_automata::nfa::thompson::pikevm::Cache>,
}

impl Pike {
    fn from_nfa(nfa: regex_automata::nfa::thompson::NFA, kind: MatchKind) -> Pike {
        let vm = PikeVM::builder()
            .configure(PikeVM::config().match_kind(kind))
            .build_from_nfa(nfa)
            .expect("PikeVM supports every NFA");
        let cache = RefCell::new(vm.create_cache());
        Pike { vm, cache }
    }

    /// The capture groups of the first match of `input`.
    fn groups(&self, input: &Input) -> Option<Groups> {
        let mut caps = self.vm.create_captures();
        self.vm
            .search(&mut self.cache.borrow_mut(), input, &mut caps);
        caps.is_match().then(|| {
            let names = Groups::named(
                caps.group_info()
                    .pattern_names(regex_automata::PatternID::ZERO),
            );
            Groups(
                (0..caps.group_len())
                    .map(|i| caps.get_group(i).map(|s| (s.start, s.end)))
                    .collect(),
                names,
            )
        })
    }
}

thread_local! {
    /// The NFA compiler for `light_nfa`. A compiler allocates a cache for compiling non-ASCII
    /// classes that takes longer to set up (~9µs) than compiling a short pattern; reusing the
    /// compiler reuses it.
    static NFA_COMPILER: regex_automata::nfa::thompson::Compiler = {
        use regex_automata::nfa::thompson::{Compiler, Config};
        let mut c = Compiler::new();
        c.configure(Config::new().utf8(false).nfa_size_limit(Some(10 << 20)));
        c
    };
}

/// The NFA for `hir`, with the configuration of `regex::bytes::Regex` (see `compile`).
fn light_nfa(hir: &regex_syntax::hir::Hir) -> Option<regex_automata::nfa::thompson::NFA> {
    NFA_COMPILER.with(|c| {
        let nfa = c.build_from_hir(hir).ok();
        // The compiler keeps the states it built until the next build: build an empty pattern to
        // free them now, so that `regex_memory::measure` counts what the NFA keeps, rather than
        // that less what the previous build kept.
        let _ = c.build_from_hir(&regex_syntax::hir::Hir::empty());
        nfa
    })
}

/// The engines of a `LightMatcher` for some text, each built on first use.
pub(crate) struct LightEngine {
    /// The pattern, restricted to the characters of the text.
    hir: regex_syntax::hir::Hir,
    longest: bool,
    nfa: regex_automata::nfa::thompson::NFA,
    /// Leftmost-first matches, searching with `FIRST_CACHE`.
    first: PikeVM,
    /// Tells engines apart for `FIRST_CACHE`.
    id: u64,
    /// A lazy DFA for `is_match`, built on its second use: for a single search of short text,
    /// building it costs more than searching with `first`. `None` when it cannot be built.
    dfa: OnceCell<Option<(DFA, RefCell<Cache>)>>,
    is_match_uses: std::cell::Cell<u32>,
    /// Reporting all matches, so that an anchored search ends at the longest (see `Longest`).
    all: OnceCell<Pike>,
    /// For capture groups: of the pattern, and of the pattern anchored at the end (see
    /// `captures_at`; `None` when it cannot be built).
    groups: OnceCell<Pike>,
    exact: OnceCell<Option<Pike>>,
}

/// Numbers `LightEngine`s.
static ENGINE_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// The cache of the PikeVM `first` of `LightEngine`s, and the id of the engine it was last
    /// set up for. Most engines are used for a single search: resetting the cache for another
    /// engine keeps its allocations, which take longer to make (and free) than the search.
    static FIRST_CACHE: RefCell<Option<(u64, regex_automata::nfa::thompson::pikevm::Cache)>> =
        const { RefCell::new(None) };
}

impl LightEngine {
    fn new(hir: regex_syntax::hir::Hir, longest: bool) -> Option<LightEngine> {
        let nfa = light_nfa(&hir)?;
        Some(LightEngine {
            first: PikeVM::new_from_nfa(nfa.clone()).expect("PikeVM supports every NFA"),
            id: ENGINE_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            hir,
            longest,
            nfa,
            dfa: OnceCell::new(),
            is_match_uses: Default::default(),
            all: OnceCell::new(),
            groups: OnceCell::new(),
            exact: OnceCell::new(),
        })
    }

    fn is_match(&self, hay: &[u8]) -> bool {
        let input = Input::new(hay).earliest(true);
        let uses = self.is_match_uses.get();
        self.is_match_uses.set(uses + 1);
        if uses > 0 {
            let dfa = self.dfa.get_or_init(|| {
                DFA::builder()
                    .configure(DFA::config().unicode_word_boundary(true))
                    .build_from_nfa(self.nfa.clone())
                    .ok()
                    .map(|dfa| {
                        let cache = RefCell::new(dfa.create_cache());
                        (dfa, cache)
                    })
            });
            if let Some((dfa, cache)) = dfa {
                if let Ok(m) = dfa.try_search_fwd(&mut cache.borrow_mut(), &input) {
                    return m.is_some();
                }
            }
            // The lazy DFA gave up (Unicode word boundaries next to non-ASCII text) or could
            // not be built.
        }
        self.with_first_cache(|cache| self.first.is_match(cache, input))
    }

    fn with_first_cache<T>(
        &self,
        f: impl FnOnce(&mut regex_automata::nfa::thompson::pikevm::Cache) -> T,
    ) -> T {
        FIRST_CACHE.with_borrow_mut(|c| {
            let (id, cache) = c.get_or_insert_with(|| (self.id, self.first.create_cache()));
            if *id != self.id {
                cache.reset(&self.first);
                *id = self.id;
            }
            f(cache)
        })
    }
}

impl Search for LightEngine {
    fn search_at(&self, hay: &[u8], start: usize) -> Option<(usize, usize)> {
        let m =
            self.with_first_cache(|cache| self.first.find(cache, Input::new(hay).range(start..)))?;
        if !self.longest {
            return Some((m.start(), m.end()));
        }
        let all = self
            .all
            .get_or_init(|| Pike::from_nfa(self.nfa.clone(), MatchKind::All));
        let input = Input::new(hay).range(m.start()..).anchored(Anchored::Yes);
        let end = all
            .vm
            .find(&mut all.cache.borrow_mut(), input)
            .map_or(m.end(), |l| l.end().max(m.end()));
        Some((m.start(), end))
    }

    fn groups_at(&self, hay: &[u8], from: usize, to: usize) -> Option<Groups> {
        use regex_syntax::hir::{Hir, Look};
        if self.longest {
            let exact = self.exact.get_or_init(|| {
                light_nfa(&Hir::concat(vec![self.hir.clone(), Hir::look(Look::End)]))
                    .map(|nfa| Pike::from_nfa(nfa, MatchKind::LeftmostFirst))
            });
            if let Some(exact) = exact {
                // As in `captures_at`: the text before `from` stays visible to look-behind.
                let input = Input::new(&hay[..to]).range(from..).anchored(Anchored::Yes);
                if let Some(g) = exact.groups(&input) {
                    return Some(g);
                }
            }
        }
        let groups = self
            .groups
            .get_or_init(|| Pike::from_nfa(self.nfa.clone(), MatchKind::LeftmostFirst));
        groups.groups(&Input::new(hay).range(from..))
    }
}

/// `hir` with every class restricted to `chars`; in text made of those characters (and bytes
/// that are not valid UTF-8) it matches the same as `hir`. When `chars` is ASCII, Unicode word
/// boundaries become ASCII ones too, which the lazy DFA handles without giving up. Byte classes
/// are left alone.
fn restrict(
    hir: &regex_syntax::hir::Hir,
    chars: &regex_syntax::hir::ClassUnicode,
    ascii: bool,
) -> regex_syntax::hir::Hir {
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
        HirKind::Alternation(hs) => {
            Hir::alternation(hs.iter().map(|h| restrict(h, chars, ascii)).collect())
        }
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

/// Literals one of which every match of `hir` starts with (or ends with, for
/// `ExtractKind::Suffix`), when there are a few, none empty.
fn required_literals(
    hir: &regex_syntax::hir::Hir,
    kind: regex_syntax::hir::literal::ExtractKind,
) -> Option<Vec<Vec<u8>>> {
    let mut extractor = regex_syntax::hir::literal::Extractor::new();
    extractor.kind(kind).limit_total(64);
    let seq = extractor.extract(hir);
    let lits = seq.literals()?;
    if lits.len() > 8 || lits.iter().any(|l| l.is_empty()) {
        return None;
    }
    Some(lits.iter().map(|l| l.as_bytes().to_vec()).collect())
}

impl LightMatcher {
    /// A matcher for the awk regex `pat`, or `None` when it does not parse (or may be too big,
    /// see `may_be_too_big`); the caller then goes through `compile`, which reports errors.
    pub(crate) fn new(pat: &str) -> Option<LightMatcher> {
        use regex_syntax::hir::{Look, literal::ExtractKind};
        let hir = regex_syntax::ParserBuilder::new()
            .utf8(false)
            .dot_matches_new_line(true)
            .build()
            .parse(&with_case(translate(pat).ok()?))
            .ok()?;
        if may_be_too_big(&hir) {
            return None;
        }
        let props = hir.properties();
        let prefixes = required_literals(&hir, ExtractKind::Prefix)
            .map(|lits| (lits, props.look_set_prefix().contains(Look::Start)));
        let suffixes = required_literals(&hir, ExtractKind::Suffix)
            .map(|lits| (lits, props.look_set_suffix().contains(Look::End)));
        Some(LightMatcher {
            longest: hir_needs_longest(&hir),
            hir,
            prefixes,
            suffixes,
            ascii: None,
            uses: 0,
        })
    }

    /// Whether the pattern can match `hay`, judging by the literals its matches start and end
    /// with; `false` means that it does not match.
    pub(crate) fn may_match(&self, hay: &[u8]) -> bool {
        let prefix = |(lits, start): &(Vec<Vec<u8>>, bool)| {
            lits.iter().any(|l| {
                if *start {
                    hay.starts_with(l)
                } else {
                    memchr::memmem::find(hay, l).is_some()
                }
            })
        };
        let suffix = |(lits, end): &(Vec<Vec<u8>>, bool)| {
            lits.iter().any(|l| {
                if *end {
                    hay.ends_with(l)
                } else {
                    memchr::memmem::find(hay, l).is_some()
                }
            })
        };
        self.prefixes.as_ref().is_none_or(prefix) && self.suffixes.as_ref().is_none_or(suffix)
    }

    /// Builds what the first use of the matcher on `hay` needs, so that its memory can be
    /// measured with the matcher's (see `RegexCache::light_matcher`).
    pub(crate) fn prepare(&mut self, hay: &[u8]) {
        if hay.is_ascii() && self.may_match(hay) {
            self.ascii_engine();
        }
    }

    fn ascii_engine(&mut self) -> Option<&LightEngine> {
        let (hir, longest) = (&self.hir, self.longest);
        self.ascii
            .get_or_insert_with(|| {
                LightEngine::new(restrict(hir, &ascii_chars(), true), longest).map(Box::new)
            })
            .as_deref()
    }

    /// Runs `f` with the engine for `hay` (and other text made of the same characters), or
    /// returns `None` when it cannot be built; the caller then goes through `compile`, which
    /// reports the error.
    pub(crate) fn with_engine<T>(
        &mut self,
        hay: &[u8],
        f: impl FnOnce(&LightEngine) -> T,
    ) -> Option<T> {
        if hay.is_ascii() {
            return self.ascii_engine().map(f);
        }
        LightEngine::new(restrict(&self.hir, &chars_of(hay), false), self.longest).map(|e| f(&e))
    }

    /// Whether the pattern matches `hay`, or `None` when the engine for it cannot be built.
    pub(crate) fn is_match(&mut self, hay: &[u8]) -> Option<bool> {
        if !self.may_match(hay) {
            return Some(false);
        }
        self.with_engine(hay, |e| e.is_match(hay))
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
    if ignore_case() {
        return None;
    }
    literal_parts(pat)
}

/// `as_literal` regardless of IGNORECASE; also holds for patterns in the syntax of `regex`, whose
/// operators include all of awk's. A group around the text, as the lexer puts around single
/// letters (see `lexer::parse_regex_literal`), does not change what it matches.
fn literal_parts(pat: &[u8]) -> Option<(Literal, &[u8])> {
    if std::str::from_utf8(pat).is_err() {
        return None;
    }
    let pat = pat
        .strip_prefix(b"(?:")
        .and_then(|p| p.strip_suffix(b")"))
        .unwrap_or(pat);
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
            b'\\'
                | b'.'
                | b'['
                | b']'
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b'?'
                | b'{'
                | b'}'
                | b'|'
                | b'^'
                | b'$'
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

/// A literal pattern (see `as_literal`) to search with `find_at`, `find_iter` and `captures_at`:
/// memchr's substring search instead of a regex engine.
pub(crate) struct LiteralSearch<'p> {
    kind: Literal,
    finder: memchr::memmem::Finder<'p>,
}

impl<'p> LiteralSearch<'p> {
    pub(crate) fn new(kind: Literal, lit: &'p [u8]) -> LiteralSearch<'p> {
        LiteralSearch {
            kind,
            finder: memchr::memmem::Finder::new(lit),
        }
    }

    fn into_owned(self) -> LiteralSearch<'static> {
        LiteralSearch {
            kind: self.kind,
            finder: self.finder.into_owned(),
        }
    }
}

impl Search for LiteralSearch<'_> {
    fn search_at(&self, hay: &[u8], start: usize) -> Option<(usize, usize)> {
        let lit = self.finder.needle();
        let n = lit.len();
        // `^` and `$` anchor at the ends of `hay`, not at `start` (as with `Regex::find_at`).
        match self.kind {
            Literal::Contains => self
                .finder
                .find(hay.get(start..)?)
                .map(|i| (start + i, start + i + n)),
            Literal::Prefix => (start == 0 && hay.starts_with(lit)).then_some((0, n)),
            Literal::Suffix => {
                (hay.len() >= start + n && hay.ends_with(lit)).then(|| (hay.len() - n, hay.len()))
            }
            Literal::Exact => (start == 0 && hay == lit).then_some((0, n)),
        }
    }

    fn groups_at(&self, _: &[u8], from: usize, to: usize) -> Option<Groups> {
        Some(Groups(vec![Some((from, to))], Vec::new()))
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

/// Creates the caches of a `Pool`, one per thread searching with it.
type CacheFn<T> = Box<dyn Fn() -> T + Send + Sync + UnwindSafe + RefUnwindSafe>;

/// What it takes to turn a leftmost-first match into the leftmost-longest one. Constant regexes
/// are shared by the threads of parallel programs, so the search caches are pooled.
struct Longest {
    /// A lazy DFA reporting all matches, so that an anchored forward search ends at the longest.
    /// With Unicode word boundaries (`\y`, `\<`, `\>`, `\B`) it gives up on non-ASCII bytes.
    dfa: DFA,
    cache: Pool<Cache, CacheFn<Cache>>,
    /// The same as a PikeVM, for when the lazy DFA gives up; built on first use.
    pike: OnceLock<(
        PikeVM,
        Pool<
            regex_automata::nfa::thompson::pikevm::Cache,
            CacheFn<regex_automata::nfa::thompson::pikevm::Cache>,
        >,
    )>,
    nfa: regex_automata::nfa::thompson::NFA,
    /// The pattern anchored at the end, `(?:pat)\z`, to get capture groups of a given span.
    exact: OnceLock<Option<Regex>>,
}

/// Estimated memory of the longest-match or extension-check machinery for a pattern of the given
/// length in bytes, when the allocator does not tell (see `regex_memory::measure`).
const LONGEST_COST: super::CostModel = super::CostModel {
    base: 8 << 10,
    per_byte: 512,
};

/// Evicts the least recently used half of the caches here, for an eviction round of
/// `regex_memory`. A cache in use is skipped: it is the one adding an entry, which evicts by
/// itself.
pub(crate) fn evict_caches() {
    EXTENDERS.with(|cell| {
        cell.try_borrow_mut()
            .map(|mut cache| cache.evict_lru())
            .ok()
    });
}

/// Whether the leftmost-first match of `pat` (in the syntax of `regex`) can be shorter than the
/// leftmost-longest one: the pattern has an alternation, or a repetition of something other than a
/// single character (`(ab)?(abcd)?`) or that is not greedy. Patterns that fail to parse are
/// assumed to need it.
fn needs_longest(pat: &str) -> bool {
    regex_syntax::ParserBuilder::new()
        .utf8(false)
        .dot_matches_new_line(true)
        .build()
        .parse(pat)
        .map_or(true, |h| hir_needs_longest(&h))
}

/// `needs_longest` for the parsed pattern.
fn hir_needs_longest(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Hir, HirKind};
    fn one_char(h: &Hir) -> bool {
        match h.kind() {
            HirKind::Class(_) => true,
            HirKind::Literal(lit) => {
                std::str::from_utf8(&lit.0).map_or(lit.0.len() == 1, |s| s.chars().count() == 1)
            }
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
    walk(hir)
}

impl Longest {
    fn new(pat: &str) -> Option<Longest> {
        use regex_automata::nfa::thompson::{Config, NFA, WhichCaptures};
        let nfa = NFA::compiler()
            .syntax(
                regex_automata::util::syntax::Config::new()
                    .utf8(false)
                    .dot_matches_new_line(true),
            )
            .configure(
                Config::new()
                    .utf8(false)
                    .which_captures(WhichCaptures::Implicit),
            )
            .build(pat)
            .ok()?;
        let dfa = DFA::builder()
            .configure(
                DFA::config()
                    .match_kind(MatchKind::All)
                    .unicode_word_boundary(true),
            )
            .build_from_nfa(nfa.clone())
            .ok()?;
        let cache = {
            let dfa = dfa.clone();
            Pool::new(Box::new(move || dfa.create_cache()) as CacheFn<Cache>)
        };
        Some(Longest {
            dfa,
            cache,
            pike: OnceLock::new(),
            nfa,
            exact: OnceLock::new(),
        })
    }
}

/// The end of the longest match of `l` starting at `start`.
fn longest_end(l: &Longest, hay: &[u8], start: usize) -> Option<usize> {
    let input = Input::new(hay).range(start..).anchored(Anchored::Yes);
    if let Ok(m) = l.dfa.try_search_fwd(&mut l.cache.get(), &input) {
        return m.map(|m| m.offset());
    }
    // The lazy DFA gave up (Unicode word boundaries next to non-ASCII text): the PikeVM, also
    // reporting all matches, ends at the longest one too.
    let (pike, cache) = l.pike.get_or_init(|| {
        let pike = PikeVM::builder()
            .configure(PikeVM::config().match_kind(MatchKind::All))
            .build_from_nfa(l.nfa.clone())
            .expect("PikeVM supports every NFA");
        let cache = {
            let pike = pike.clone();
            Pool::new(Box::new(move || pike.create_cache()) as CacheFn<_>)
        };
        (pike, cache)
    });
    pike.find(&mut cache.get(), input).map(|m| m.end())
}

/// The capture groups of a match, as byte ranges of the text; `None` for groups that did not
/// participate. Also the names of the named groups (`(?P<n>...)`), as (group, name), empty for
/// patterns without any.
pub(crate) struct Groups(Vec<Option<(usize, usize)>>, Vec<(usize, Box<str>)>);

impl Groups {
    fn named<'n>(names: impl Iterator<Item = Option<&'n str>>) -> Vec<(usize, Box<str>)> {
        names
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| (i, n.into())))
            .collect()
    }
    /// The range of group `i` (0 for the whole match).
    pub(crate) fn get(&self, i: usize) -> Option<(usize, usize)> {
        self.0.get(i).copied().flatten()
    }
    /// The number of groups, including group 0.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = Option<(usize, usize)>> + '_ {
        self.0.iter().copied()
    }
    /// The name of group `i`, if it has one.
    pub(crate) fn name(&self, i: usize) -> Option<&str> {
        self.1.iter().find(|(g, _)| *g == i).map(|(_, n)| &**n)
    }
}

/// What `find_at`, `find_iter` and `captures_at` search with: a compiled `Regex`, or the engine
/// of a `LightMatcher` (only for the text it was built for).
pub(crate) trait Search {
    /// The leftmost-longest match in `hay` starting at or after `start`, as (start, end).
    fn search_at(&self, hay: &[u8], start: usize) -> Option<(usize, usize)>;
    /// The capture groups for the match `from..to` of `hay` (as found by `search_at`).
    fn groups_at(&self, hay: &[u8], from: usize, to: usize) -> Option<Groups>;
}

/// A pattern known not to match the text at hand (see `LightMatcher::may_match`).
pub(crate) struct NoMatch;

impl Search for NoMatch {
    fn search_at(&self, _: &[u8], _: usize) -> Option<(usize, usize)> {
        None
    }
    fn groups_at(&self, _: &[u8], _: usize, _: usize) -> Option<Groups> {
        None
    }
}

impl Search for AwkRegex {
    fn search_at(&self, hay: &[u8], start: usize) -> Option<(usize, usize)> {
        if let Some(lit) = &self.literal {
            return lit.search_at(hay, start);
        }
        let m = self.re.find_at(hay, start)?;
        let (from, to) = (m.start(), m.end());
        match self.longest() {
            Some(l) => Some((from, longest_end(l, hay, from).map_or(to, |e| e.max(to)))),
            None => Some((from, to)),
        }
    }

    fn groups_at(&self, hay: &[u8], from: usize, to: usize) -> Option<Groups> {
        if let Some(lit) = &self.literal {
            return lit.groups_at(hay, from, to);
        }
        let groups = |c: Captures| {
            Groups(
                c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect(),
                Groups::named(self.re.capture_names()),
            )
        };
        if let Some(l) = self.longest() {
            let exact = l.exact.get_or_init(|| {
                RegexBuilder::new(&format!("(?:{})\\z", self.as_str()))
                    .dot_matches_new_line(true)
                    .build()
                    .ok()
            });
            if let Some(exact) = exact {
                // Text before `from` stays visible to look-behind assertions such as `\b`.
                if let Some(c) = exact.captures_at(&hay[..to], from) {
                    if c.get(0).map(|m| m.start()) == Some(from) {
                        return Some(groups(c));
                    }
                }
            }
        }
        self.re.captures_at(hay, from).map(groups)
    }
}

/// The leftmost-longest match of `re` in `hay` starting at or after `start`, as (start, end).
pub(crate) fn find_at<S: Search + ?Sized>(
    re: &S,
    hay: &[u8],
    start: usize,
) -> Option<(usize, usize)> {
    re.search_at(hay, start)
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
    for behind in [
        None,
        Some(b'\n'),
        Some(b'\r'),
        Some(b'a'),
        Some(b' '),
        Some(0x80),
    ] {
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
        RefCell::new(super::StrLru::with_regex_memory(LONGEST_COST));
    /// The pattern last looked up in `EXTENDERS` and its entry: a reader asks for the same
    /// separator once per record.
    static LAST_EXTENDER: RefCell<Option<(String, Option<Rc<Extender>>)>> = const { RefCell::new(None) };
}

/// The extension-check machinery for `re`, or `None` when the DFA cannot be built.
fn extender(re: &Regex) -> Option<Rc<Extender>> {
    let pat = re.as_str();
    if let Some(x) = LAST_EXTENDER.with_borrow(|last| {
        last.as_ref()
            .filter(|(p, _)| p == pat)
            .map(|(_, x)| x.clone())
    }) {
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
        let (x, measured) = super::regex_memory::measure(|| {
            let max_len = regex_syntax::ParserBuilder::new()
                .utf8(false)
                .dot_matches_new_line(true)
                .build()
                .parse(pat)
                .ok()
                .and_then(|h| h.properties().maximum_len());
            DFA::builder()
                .configure(
                    DFA::config()
                        .match_kind(MatchKind::All)
                        .unicode_word_boundary(true),
                )
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
                    Rc::new(Extender {
                        dfa,
                        cache,
                        max_len,
                        first,
                        few_first,
                    })
                })
        });
        cell.borrow_mut()
            .insert_measured(pat.to_owned(), x.clone(), measured);
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
    if n > 1
        && hay
            .get(pos..pos + n)
            .is_some_and(|c| std::str::from_utf8(c).is_ok())
    {
        pos + n
    } else {
        pos + 1
    }
}

/// All successive non-overlapping leftmost-longest matches of `re` in `hay`, as (start, end).
/// As with `Regex::find_iter`, an empty match right after the previous match is skipped.
pub(crate) fn find_iter<'h, S: Search + ?Sized>(
    re: &'h S,
    hay: &'h [u8],
) -> impl Iterator<Item = (usize, usize)> + 'h {
    let mut pos = 0;
    let mut last_end = None;
    std::iter::from_fn(move || {
        loop {
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
        }
    })
}

/// The capture groups of `re` for the match `from..to` of `hay` (as found by `find_at`).
pub(crate) fn captures_at<S: Search + ?Sized>(
    re: &S,
    hay: &[u8],
    from: usize,
    to: usize,
) -> Option<Groups> {
    re.groups_at(hay, from, to)
}

/// Push `c` as a literal. Only meta characters are escaped: `regex` rejects escapes of other
/// characters such as `<` and `>`.
fn push_literal(out: &mut String, c: char) {
    if regex_syntax::is_meta_character(c) {
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
                                None => {
                                    return Err(format!(
                                        "invalid character class name [:{}:]",
                                        name
                                    ));
                                }
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
                Some('.') | Some('=')
                    if chars.get(j + 3) == Some(&chars[j + 1])
                        && chars.get(j + 4) == Some(&']') =>
                {
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
            && !["\\p", "\\P", "\\w", "\\W", "\\d", "\\D", "\\s", "\\S"]
                .iter()
                .any(|e| body.contains(e));
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
        assert_eq!(
            translate("[[:alpha:]_]").unwrap(),
            r"[\p{Alphabetic}[\p{Nd}--0-9]_]"
        );
        assert_eq!(translate("[^[:digit:]]").unwrap(), "[^0-9]");
        assert_eq!(translate("[a&&b]").unwrap(), r"[a\&\&b]");
        assert_eq!(translate("(?:.)").unwrap(), "(?:.)");
        assert_eq!(translate(r"\351").unwrap(), r"(?-u:\xE9)");
        assert_eq!(translate(r"\xc3\xa9").unwrap(), r"(?-u:\xC3)(?-u:\xA9)");
        assert_eq!(translate(r"\x41\xg").unwrap(), r"\x{41}xg");
        assert_eq!(translate(r"\x{e9}").unwrap(), r"\x{e9}");
        assert_eq!(translate(r"\p{Greek}+").unwrap(), r"\p{Greek}+");
        assert_eq!(
            translate(r"[\P{Greek}\x{e9}]").unwrap(),
            r"[\P{Greek}\x{e9}]"
        );
        assert_eq!(translate(r"\pL").unwrap(), r"\pL");
        assert!(translate("[[:foo:]]").is_err());
        assert_eq!(
            syntax_error("[[:foo:]]").unwrap(),
            "invalid character class name [:foo:]"
        );
        assert!(compile("a|[^[:nope:]]").is_err());
        assert_eq!(translate(r"[a\351]").unwrap(), r"(?-u:[a\xE9])");
        assert_eq!(translate(r"[^\200-\377]").unwrap(), r"(?-u:[^\x80-\xFF])");
        assert_eq!(translate(r"[é\351]").unwrap(), r"(?:[é]|(?-u:[\xE9]))");
        assert_eq!(translate(r"[\<\>]").unwrap(), "[<>]");
        assert_eq!(translate(r"[\-\]\&]").unwrap(), r"[\-\]\&]");
        assert_eq!(translate(r"\!\/").unwrap(), "!/");
    }

    #[test]
    fn byte_escapes() {
        assert!(is_match(r"\303\251", "é"));
        assert!(is_match(r"\xc3\xa9", "é"));
        assert!(!is_match(r"^\351", "é"));
        assert!(compile(r"^\351$").unwrap().is_match(b"\xe9"));
        assert!(
            compile(r"^[\200-\377]+$")
                .unwrap()
                .is_match("中文".as_bytes())
        );
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
        assert!(is_match(r"[\<]", "<"));
        assert!(is_match(r"[\>]", ">"));
        assert!(!is_match(r"[\<\>]", "a"));
        assert!(is_match("[[.<.]]", "<"));
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
        assert_eq!(caps.get(1), Some((0, 2)));
        assert_eq!(caps.get(2), Some((2, 3)));
        // Repeated groups choose between alternatives too.
        assert_eq!(spans("(ab)?(abcd)?", "abcd")[0], (0, 4));
        assert_eq!(spans("x(y)?(yz)?", "xyz"), vec![(0, 3)]);
        assert_eq!(spans("(a|ab)(c|bcd)", "abcd"), vec![(0, 4)]);
        let re = compile("(ab)?(abcd)?").unwrap();
        let caps = captures_at(&re, b"abcd", 0, 4).unwrap();
        assert!(caps.get(1).is_none());
        assert_eq!(caps.get(2), Some((0, 4)));
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
            "^[a-z]+ ab",
            "[[:alpha:]]+ x$",
            "[[:upper:]]",
            "[^a]b",
            "^.$",
            "^..$",
            "\\<caf\\>",
            "\\yab\\y",
            "\\Bé",
            "(a|ab)(c|bcd)",
            "[[:space:]]+",
            "\\303\\251",
            "^\\351",
            "\\p{Greek}+",
            "[^[:alnum:]]",
            "x*",
            "^$",
            "[é-ë]",
            "数据|data",
            "\\w+\\s\\w+$",
            "a{2,3}",
            ".{100}",
        ];
        let hays: [&[u8]; 14] = [
            b"",
            b"ab",
            b"xab",
            b"abc x",
            b"caf\xc3\xa9",
            b"caf",
            b"na\xc3\xafve caf\xc3\xa9 ab",
            b"\xce\xb1\xce\xb2",
            b"\xe6\x95\xb0\xe6\x8d\xae",
            b"Ab",
            b"\xe9",
            b"\xe2\x80\x83 x",
            b"aa",
            b"word word",
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
                    assert_eq!(
                        light.is_match(hay),
                        Some(re.is_match(hay)),
                        "{:?} ~ /{}/ ic={}",
                        hay,
                        pat,
                        ic
                    );
                }
            }
        }
        set_ignore_case(false);
        assert!(LightMatcher::new("(").is_none());
        assert!(LightMatcher::new("[[:foo:]]").is_none());
    }

    #[test]
    fn light_search_agrees_with_compile() {
        let pats = [
            "^[a-z]+ ab",
            "[[:alpha:]]+",
            "(a|ab)(c|bcd)",
            "x|xy",
            "(x|xy)(z?)",
            "(ab)?(abcd)?",
            "\\<(a|ab)",
            "(a|ab)\\B",
            "\\y(a|ab)",
            "\\<(中|中文)",
            "(x|xy)\\y",
            "x*",
            "y|",
            "[^ ]+",
            "(.)(.)",
            "^$",
            "$",
            "数据|data",
            "(é+)(b)?",
            "a{2,3}",
            "\\351",
            "(foo)?bar$",
            "^(user[0-9]+) ([^ ]+)",
        ];
        let hays: [&[u8]; 14] = [
            b"",
            b"abcd",
            b"xyz",
            b"ab c",
            b"abc",
            b"\xc3\xa9 ab",
            b"\xe8\xaf\xb4 \xe4\xb8\xad\xe6\x96\x87",
            b"\xc3\xa9 xy \xc3\xa9",
            b"\xc3\xa9\xc3\xa9b aaa",
            b"foobar",
            b"user12 x\xe4\xb8\xad y",
            b"\xe9a",
            b"\xe6\x95\xb0\xe6\x8d\xae data",
            b"aaaa",
        ];
        for ic in [false, true] {
            set_ignore_case(ic);
            for pat in pats {
                let re = compile(pat).unwrap();
                let mut light = LightMatcher::new(pat).unwrap();
                for hay in hays {
                    let want: Vec<_> = find_iter(&re, hay).collect();
                    let groups = |s: &dyn Search,
                                  spans: &[(usize, usize)]|
                     -> Vec<Vec<Option<(usize, usize)>>> {
                        spans
                            .iter()
                            .map(|&(f, t)| captures_at(s, hay, f, t).unwrap().iter().collect())
                            .collect()
                    };
                    let want_groups = groups(&re, &want);
                    let ctx = format!("/{}/ on {:?} ic={}", pat, hay, ic);
                    if !light.may_match(hay) {
                        assert!(want.is_empty(), "{}", ctx);
                        continue;
                    }
                    light
                        .with_engine(hay, |e| {
                            let got: Vec<_> = find_iter(e, hay).collect();
                            assert_eq!(got, want, "{}", ctx);
                            assert_eq!(groups(e, &got), want_groups, "{}", ctx);
                        })
                        .unwrap();
                }
            }
        }
        set_ignore_case(false);
    }

    #[test]
    fn light_matcher_required_literals() {
        let may = |pat: &str, hay: &str| LightMatcher::new(pat).unwrap().may_match(hay.as_bytes());
        assert!(may("^user1 [^ ]+ foo", "user1 x foo"));
        assert!(!may("^user1 [^ ]+ foo", "xuser1 x foo"));
        assert!(!may("^user1 [^ ]+ foo", "user1 x fo"));
        assert!(!may("bar$", "bar x"));
        assert!(may("(foo|bar)x", "a barx"));
        assert!(!may("(foo|bar)x", "a bazx"));
        assert!(may("[a-z]+", "1"));
        assert!(may("\\<abc", "abc"));
        set_ignore_case(true);
        assert!(may("^user", "USER"));
        set_ignore_case(false);
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
            .flat_map(|n| {
                (0..1 << n).map(move |m| {
                    (0..n)
                        .map(|i| if m >> i & 1 == 1 { 'b' } else { 'a' })
                        .collect()
                })
            })
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
                            assert_eq!(
                                longest_end(&l, hay.as_bytes(), m.start()),
                                Some(m.end()),
                                "/{}/ {:?}",
                                pat,
                                hay
                            );
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
    fn longest_is_built_on_first_search() {
        let re = compile("hot|hotter").unwrap();
        assert!(re.is_match(b"hot") && re.longest.get().is_none());
        assert_eq!(find_at(&re, b"hotter", 0), Some((0, 6)));
        assert!(re.longest.get().unwrap().is_some());
        let re = compile("ho+t").unwrap();
        assert_eq!(find_at(&re, b"hotter", 0), Some((0, 3)));
        assert!(re.longest.get().unwrap().is_none());
        // Literal patterns are searched with memchr, without the regex engines.
        let re = compile("hot").unwrap();
        assert_eq!(find_at(&re, b"hotter", 0), Some((0, 3)));
        assert!(re.longest.get().is_none());
    }

    /// Constant regexes are shared by the threads of parallel programs.
    #[test]
    fn awk_regex_is_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AwkRegex>();
        let re = std::sync::Arc::new(compile(r"a|ab\>").unwrap());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let re = re.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        assert_eq!(find_at(&*re, b"x ab", 0), Some((2, 4)));
                        assert_eq!(find_at(&*re, "中 ab".as_bytes(), 0), Some((4, 6)));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn literal_search_matches_regex() {
        let pats = [
            "a", "(?:a)", "ab", "^a", "a$", "^ab$", "", "^", "$", "^$", "é", " ", "(?: )",
        ];
        let hays = [
            "", "a", "ab", "aab", "abab", "ba", "xaby", "aaa", "é", "aéa", "a b", "  ",
        ];
        for pat in pats {
            let (kind, lit) = as_literal(pat.as_bytes()).unwrap();
            let lit = LiteralSearch::new(kind, lit);
            let re = Regex::new(pat).unwrap();
            for hay in hays {
                let hay = hay.as_bytes();
                for start in 0..=hay.len() {
                    let want = re.find_at(hay, start).map(|m| (m.start(), m.end()));
                    assert_eq!(
                        lit.search_at(hay, start),
                        want,
                        "{:?} in {:?} from {}",
                        pat,
                        hay,
                        start
                    );
                }
                let all: Vec<_> = find_iter(&lit, hay).collect();
                let mut full = AwkRegex::new(re.clone());
                full.literal = None;
                let want: Vec<_> = find_iter(&full, hay).collect();
                assert_eq!(all, want, "{:?} in {:?}", pat, hay);
            }
        }
        for pat in [
            ".",
            "a.",
            "a|b",
            "(?:a)|(?:b)",
            "(?:.)",
            "(?i)a",
            "a*",
            "[a]",
            "\\a",
        ] {
            assert!(compile(pat).unwrap().literal.is_none(), "{}", pat);
        }
        assert!(compile("(?:x)").unwrap().literal.is_some());
        assert!(compile("abc").unwrap().literal.is_some());
    }
}
