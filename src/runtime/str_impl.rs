/// Custom string implementation.
///
/// There is a lot of unsafe code here. Many of the features here can and were implementable in
/// terms of safe code using enums, and various components of the standard library. We moved to
/// this representation because it significantly improved some benchmarks in terms of time and
/// space, and it also makes for more ergonomic interop with LLVM.
///
/// TODO explain more about what is going on here.
use crate::pushdown::FieldSet;
use crate::runtime::{strtoi, Float, Int, SharedMap, IntMap};

use regex::bytes::{Captures, Regex};
use smallvec::SmallVec;

use std::alloc::{alloc_zeroed, dealloc, realloc, Layout};
use std::borrow::Cow;
use std::cell::{Cell, UnsafeCell};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::marker::PhantomData;

#[cfg(feature = "unstable")]
use std::intrinsics::likely;

#[cfg(not(feature = "unstable"))]
fn likely(b: bool) -> bool {
    b
}

use std::{mem};
use std::ptr;
use std::rc::Rc;
use std::slice;
use std::str;
use unicode_segmentation::UnicodeSegmentation;
use url::Url;
use crate::runtime;
use crate::runtime::str_escape::escape;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(usize)]
enum StrTag {
    Inline = 0,
    Literal = 1,
    Shared = 2,
    Concat = 3,
    Boxed = 4,
}

const NUM_VARIANTS: usize = 5;

impl StrTag {
    fn forced(self) -> bool {
        use StrTag::*;
        match self {
            Literal | Boxed | Inline => true,
            Concat | Shared => false,
        }
    }
}

// Why the repr(C)? We may rely on the lengths coming first.

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(transparent)]
struct Inline(u128);

const MAX_INLINE_SIZE: usize = 15;

impl Default for Inline {
    fn default() -> Inline {
        Inline(StrTag::Inline as u128)
    }
}

impl Inline {
    unsafe fn from_raw(ptr: *const u8, len: usize) -> Inline {
        debug_assert!(len <= MAX_INLINE_SIZE);
        if len > MAX_INLINE_SIZE {
            std::hint::unreachable_unchecked();
        }
        let mut res = ((len << 3) | StrTag::Inline as usize) as u128;
        ptr::copy_nonoverlapping(ptr, (&mut res as *mut u128 as *mut u8).offset(1), len);
        Inline(res)
    }
    unsafe fn from_unchecked(bs: &[u8]) -> Inline {
        Self::from_raw(bs.as_ptr(), bs.len())
    }
    fn len(&self) -> usize {
        (self.0 as usize & 0xFF) >> 3
    }
    fn bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts((self as *const Inline as *const u8).offset(1), self.len()) }
    }
}

#[derive(Clone)]
#[repr(C)]
struct Literal<'a> {
    ptr: *const u8,
    len: u64,
    _marker: PhantomData<&'a ()>,
}

#[derive(Clone, Debug)]
#[repr(C)]
struct Boxed {
    buf: Buf,
    len: u64,
}

#[derive(Clone, Debug)]
#[repr(C)]
struct Shared {
    buf: Buf,
    start: u32,
    end: u32,
}

#[derive(Clone, Debug)]
struct ConcatInner<'a> {
    left: Str<'a>,
    right: Str<'a>,
}

#[derive(Clone)]
#[repr(C)]
struct Concat<'a> {
    inner: Rc<ConcatInner<'a>>,
    len: u64,
}

impl<'a> Concat<'a> {
    // unsafe because len must be left.len() + right.len(). It must also be greater than
    // MAX_INLINE_LEN.
    unsafe fn new(len: u64, left: Str<'a>, right: Str<'a>) -> Concat<'a> {
        debug_assert!(len > MAX_INLINE_SIZE as u64);
        debug_assert_eq!(len, (left.bytes_len() + right.bytes_len()) as u64);
        Concat {
            len,
            inner: Rc::new(ConcatInner { left, right }),
        }
    }
    fn left(&self) -> Str<'a> {
        self.inner.left.clone()
    }
    fn right(&self) -> Str<'a> {
        self.inner.right.clone()
    }
}

#[derive(PartialEq, Eq)]
#[repr(C)]
struct StrRep<'a> {
    hi: usize,
    low: u64,
    _marker: PhantomData<&'a ()>,
}

impl<'a> Default for StrRep<'a> {
    fn default() -> StrRep<'a> {
        Inline::default().into()
    }
}

impl<'a> StrRep<'a> {
    fn get_tag(&self) -> StrTag {
        use StrTag::*;
        let tag = self.hi & 0x7;
        debug_assert!(tag < NUM_VARIANTS);
        match tag {
            0 => Inline,
            1 => Literal,
            2 => Shared,
            3 => Concat,
            4 => Boxed,
            _ => unreachable!(),
        }
    }
}

macro_rules! impl_tagged_from {
    ($from:ty, $tag:expr) => {
        impl<'a> From<$from> for StrRep<'a> {
            fn from(s: $from) -> StrRep<'a> {
                let mut rep = unsafe { mem::transmute::<$from, StrRep>(s) };
                rep.hi |= ($tag as usize);
                rep
            }
        }
    };
}

impl_tagged_from!(Shared, StrTag::Shared);
impl_tagged_from!(Concat<'a>, StrTag::Concat);
impl_tagged_from!(Boxed, StrTag::Boxed);
// Unlike the other variants, `Inline` always has the tag in place, so we can just cast it
// directly.
impl<'a> From<Inline> for StrRep<'a> {
    fn from(i: Inline) -> StrRep<'a> {
        unsafe { mem::transmute::<Inline, StrRep>(i) }
    }
}

impl<'a> From<Literal<'a>> for StrRep<'a> {
    fn from(s: Literal<'a>) -> StrRep<'a> {
        if s.len <= MAX_INLINE_SIZE as u64 {
            unsafe { Inline::from_raw(s.ptr, s.len as usize).into() }
        } else if s.ptr as usize & 0x7 == 0 {
            let mut rep = unsafe { mem::transmute::<Literal<'a>, StrRep>(s) };
            rep.hi |= StrTag::Literal as usize;
            rep
        } else {
            let buf = unsafe { Buf::read_from_raw(s.ptr, s.len as usize) };
            Boxed { len: s.len, buf }.into()
        }
    }
}

impl<'a> StrRep<'a> {
    /// The heap buffer behind a boxed or shared string.
    fn buf(&mut self) -> Option<Buf> {
        match self.get_tag() {
            StrTag::Boxed => Some(unsafe { self.view_as(|b: &Boxed| b.buf.clone()) }),
            StrTag::Shared => Some(unsafe { self.view_as(|s: &Shared| s.buf.clone()) }),
            _ => None,
        }
    }
    fn len(&mut self) -> usize {
        match self.get_tag() {
            StrTag::Boxed | StrTag::Literal | StrTag::Concat => self.low as usize,
            StrTag::Shared => unsafe {
                self.view_as(|s: &Shared| s.end as usize - s.start as usize)
            },
            StrTag::Inline => unsafe { self.view_as_inline(Inline::len) },
        }
    }
    unsafe fn view_as_inline<R>(&self, f: impl FnOnce(&Inline) -> R) -> R {
        f(mem::transmute::<&StrRep<'a>, &Inline>(self))
    }
    unsafe fn view_as<T, R>(&mut self, f: impl FnOnce(&T) -> R) -> R {
        let old = self.hi;
        self.hi = old & !0x7;
        let res = f(mem::transmute::<&mut StrRep<'a>, &T>(self));
        self.hi = old;
        res
    }
    unsafe fn drop_as<T>(&mut self) {
        let old = self.hi;
        self.hi = old & !0x7;
        ptr::drop_in_place(mem::transmute::<&mut StrRep<'a>, *mut T>(self));
    }

    unsafe fn copy(&self) -> StrRep<'a> {
        StrRep {
            low: self.low,
            hi: self.hi,
            _marker: PhantomData,
        }
    }

    // drop_with_tag is a parallel implementation of drop given an explicit tag. It is used in
    // conjunction with the LLVM-native "fast path" for dropping strings. See the gen_drop_str
    // function in llvm/builtin_functions.rs for more context.
    //
    // drop_with_tag must not be called with an Inline or Literal tag.
    unsafe fn drop_with_tag(&mut self, tag: u64) {
        // Debug-asserts are here to ensure that we catch any perturbing of the tag values getting
        // out of sync with this function.
        debug_assert_eq!(tag, self.get_tag() as u64);
        match tag {
            2 => {
                debug_assert_eq!(tag, StrTag::Shared as u64);
                self.drop_as::<Shared>();
            }
            3 => {
                debug_assert_eq!(tag, StrTag::Concat as u64);
                self.drop_as::<Concat>();
            }
            4 => {
                debug_assert_eq!(tag, StrTag::Boxed as u64);
                self.drop_as::<Boxed>();
            }
            _ => unreachable!(),
        }
    }
}

impl<'a> Drop for StrRep<'a> {
    fn drop(&mut self) {
        // Drop shows up on a lot of profiles. It doesn't appear as though drop is particularly
        // slow (efforts to do drops in batches, keeping the batch in thread-local storage, were
        // slightly slower), just that in short scripts there are just a lot of strings.
        let tag = self.get_tag();
        unsafe {
            match tag {
                StrTag::Inline | StrTag::Literal => {}
                StrTag::Shared => self.drop_as::<Shared>(),
                StrTag::Boxed => self.drop_as::<Boxed>(),
                StrTag::Concat => self.drop_as::<Concat>(),
            }
        };
    }
}

/// A Str that is either trivially copyable or holds the sole reference to some heap-allocated
/// memory. We also ensure no non-static Literal variants are active in the string, as we intend to
/// send this across threads, and non-static lifetimes are cumbersome in that context.
#[derive(Default, Debug, Hash, PartialEq, Eq)]
pub struct UniqueStr<'a>(Str<'a>);

unsafe impl<'a> Send for UniqueStr<'a> {}

impl<'a> Clone for UniqueStr<'a> {
    fn clone(&self) -> UniqueStr<'a> {
        UniqueStr(self.clone_str())
    }
}

impl<'a> UniqueStr<'a> {
    pub fn into_str(self) -> Str<'a> {
        self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The bytes of this string. The returned slice borrows `self`: Inline strings store their
    /// bytes inside the `Str` itself, so the slice must not outlive (or survive a move of) `self`.
    /// Callers that need the bytes for longer should copy them.
    pub fn bytes(&self) -> &[u8] {
        unsafe { &*self.0.get_bytes() }
    }
    pub fn clone_str(&self) -> Str<'a> {
        let rep = unsafe { self.0.rep_mut() };
        match rep.get_tag() {
            StrTag::Inline | StrTag::Literal => self.0.clone(),
            StrTag::Boxed => unsafe {
                rep.view_as(|b: &Boxed| {
                    let bs = b.buf.as_bytes();
                    Str::from_rep(
                        Boxed {
                            buf: Buf::read_from_raw(bs.as_ptr(), bs.len()),
                            len: bs.len() as u64,
                        }
                            .into(),
                    )
                })
            },
            StrTag::Shared | StrTag::Concat => unreachable!(),
        }
    }
}

impl<'a> From<Str<'a>> for UniqueStr<'a> {
    fn from(s: Str<'a>) -> UniqueStr<'a> {
        unsafe {
            let rep = s.rep_mut();
            match rep.get_tag() {
                StrTag::Inline | StrTag::Literal => return UniqueStr(s),
                StrTag::Shared | StrTag::Concat => s.force(),
                StrTag::Boxed => {}
            };
            debug_assert_eq!(StrTag::Boxed, rep.get_tag());
            // We have a box in place, check its refcount
            if let Some(boxed) = rep.view_as(|b: &Boxed| {
                if b.buf.refcount() == 1 {
                    None
                } else {
                    // Copy a new buffer.
                    let bs = b.buf.as_bytes();
                    debug_assert_eq!(bs.len() as u64, b.len);
                    Some(Boxed {
                        buf: Buf::read_from_raw(bs.as_ptr(), bs.len()),
                        len: bs.len() as u64,
                    })
                }
            }) {
                UniqueStr(Str::from_rep(boxed.into()))
            } else {
                UniqueStr(s)
            }
        }
    }
}

// Why UnsafeCell? We want something that won't increase the size of StrRep, but we also need to
// mutate it in-place. We can *almost* just use Cell here, but we cannot implement Clone behind
// cell.
#[derive(Default)]
#[repr(transparent)]
pub struct Str<'a>(UnsafeCell<StrRep<'a>>);

impl<'a> Str<'a> {
    pub fn is_empty(&self) -> bool {
        unsafe { mem::transmute::<&Str, &Inline>(self) == &Inline::default() }
    }
    unsafe fn rep(&self) -> &StrRep<'a> {
        &*self.0.get()
    }

    // We have rep_mut() for the same reasons that Rust has UnsafeCell: to allow
    // for otpimizations on Str that leverage interipr mutability.
    //
    // Callers _must_ ensure they are not aliasing mutable references this way.
    // In practice this is a murky question given the fact that all strings are
    // "logically" immutable and reference counted.
    #[allow(clippy::mut_from_ref)]
    unsafe fn rep_mut(&self) -> &mut StrRep<'a> {
        &mut *self.0.get()
    }

    pub(crate) unsafe fn drop_with_tag(&self, tag: u64) {
        self.rep_mut().drop_with_tag(tag)
    }
    // We rely on string literals having trivial drops for LLVM codegen, as they may be dropped
    // repeatedly.
    pub fn drop_is_trivial(&self) -> bool {
        match unsafe { self.rep() }.get_tag() {
            StrTag::Literal | StrTag::Inline => true,
            StrTag::Shared | StrTag::Concat | StrTag::Boxed => false,
        }
    }

    // leaks `self` unless you transmute it back. This is used in LLVM codegen
    pub fn into_bits(self) -> u128 {
        unsafe { mem::transmute::<Str<'a>, u128>(self) }
    }

    pub fn split(
        &self,
        pat: &Regex,
        // We want to accommodate functions that skip based on empty fields, like Awk whitespace
        // splitting. As a result, we pass down the field, and whether or not it was empty (emptiness
        // checks for the string itself are insufficient if used_fields projects some fields away),
        // the pattern returns the number of fields added to the output.
        mut push: impl FnMut(Str<'a>, bool /*is_empty*/) -> usize,
        used_fields: &FieldSet,
    ) {
        if self.is_empty() {
            return;
        }
        self.with_bytes(|s| {
            let mut prev = 0;
            let mut cur_field = 1;
            for (start, end) in super::awk_regex::find_iter(pat, s) {
                // As in gawk, a separator must match at least one character: empty matches
                // (e.g. of `x*` or `^`) do not split.
                if start == end {
                    continue;
                }
                let is_empty = prev == start;
                cur_field += if used_fields.get(cur_field) {
                    push(self.slice(prev, start), is_empty)
                } else {
                    push(Str::default(), is_empty)
                };
                prev = end;
            }
            let is_empty = prev == s.len();
            if used_fields.get(cur_field) {
                push(self.slice(prev, s.len()), is_empty);
            } else {
                push(Str::default(), is_empty);
            }
        });
    }

    pub fn join_slice<'b>(&self, inps: &[Str]) -> Str<'b> {
        // We've noticed that performance of `join_slice` is very sensitive to the number of
        // `realloc` calls that happen when pushing onto DynamicBufHeap, so we spend the extra time
        // of computing the size of the joined string ahead of time exactly.
        let mut sv = SmallVec::<[&[u8]; 16]>::with_capacity(inps.len());
        let sep_bytes: &[u8] = unsafe { &*self.get_bytes() };
        let mut size = 0;
        for (i, inp) in inps.iter().enumerate() {
            let inp_bytes = unsafe { &*inp.get_bytes() };
            sv.push(inp_bytes);
            size += inp_bytes.len();
            if i < inps.len() - 1 {
                size += sep_bytes.len()
            }
        }
        let mut buf = DynamicBufHeap::new(size);
        for (i, inp) in sv.into_iter().enumerate() {
            buf.write_all(inp).unwrap();
            if i < inps.len() - 1 {
                buf.write_all(sep_bytes).unwrap();
            }
        }
        buf.into_str()
    }

    pub fn join(&self, mut ss: impl Iterator<Item=Str<'a>>) -> Str<'a> {
        let mut res = if let Some(s) = ss.next() {
            s
        } else {
            return Default::default();
        };
        for s in ss {
            res = Str::concat(res.clone(), Str::concat(self.clone(), s.clone()));
        }
        res
    }

    // TODO: SIMD implementations of to_upper and to_lower aren't too difficult to write;
    // it's probably worth specializing these implementations with those if possible.

    /// awk's `tolower`. Despite the name, non-ASCII UTF-8 text is converted too (see
    /// `map_case`).
    pub fn to_lower_ascii<'b>(&self) -> Str<'b> {
        self.map_case(|b| b.to_ascii_lowercase(), char::to_lowercase)
    }

    pub fn fend<'b>(&self) -> Str<'b> {
        let mut context = fend_core::Context::new();
        let expr = self.to_string();
        return match fend_core::evaluate(&expr, &mut context) {
            Ok(result) => { Str::from(result.get_main_result().to_string()) }
            Err(error) => {
                crate::runtime::stdlib_warning("fend", format!("{:?}: {}", expr, error));
                Str::default()
            }
        };
    }

    pub(crate) fn chars<'b>(&self) -> runtime::IntMap<Str<'b>> {
        let map: IntMap<Str> = IntMap::default();
        self.with_bytes(|bs| {
            let (mut i, mut start) = (1, 0);
            while start < bs.len() {
                let w = if characters_as_bytes() { 1 } else { char_width(&bs[start..]) };
                map.insert(i, Str::from_bytes(&bs[start..start + w]));
                i += 1;
                start += w;
            }
        });
        map
    }
    pub(crate) fn url<'b>(&self) -> runtime::StrMap<'b, Str<'b>> {
        let mut map = hashbrown::HashMap::new();
        let mut url_text = self.to_string();
        if url_text.starts_with("jdbc:") {
            url_text = url_text.replace("jdbc:", "jdbc+");
        }
        if let Ok(url) = &Url::parse(&url_text) {
            if url.scheme() != "" {
                map.insert(Str::from("schema"), Str::from(url.scheme().to_string()));
            }
            if url.username() != "" {
                map.insert(Str::from("user"), Str::from(url.username().to_string()));
            }
            if let Some(password) = url.password() {
                map.insert(Str::from("password"), Str::from(password.to_string()));
            }
            if let Some(host) = url.host_str() {
                map.insert(Str::from("host"), Str::from(host.to_string()));
            }
            if let Some(port) = url.port() {
                map.insert(Str::from("port"), Str::from(port.to_string()));
            }
            if url.path() != "" {
                if let Ok(path) = urlencoding::decode(url.path()) {
                    map.insert(Str::from("path"), Str::from(path.to_string()));
                }
            }
            if let Some(query) = url.query() {
                map.insert(Str::from("query"), Str::from(query.to_string()));
            }
            if let Some(fragment) = url.fragment() {
                map.insert(Str::from("fragment"), Str::from(fragment.to_string()));
            }
        }
        return SharedMap::from(map);
    }

    /// Trim whitespace (`pat == " "`) or any char of `pat` from both ends. Works on bytes, so
    /// invalid UTF-8 (GBK, Latin-1, binary) is kept as is; it never matches `pat`.
    pub fn trim<'b>(&self, pat: &Str<'b>) -> Str<'b> {
        let pat = pat.as_str();
        let chars: Vec<char> = pat.chars().collect();
        let matches = |c: char| if pat == " " { c.is_whitespace() } else { chars.contains(&c) };
        self.with_bytes(|bs| {
            let mut start = 0;
            while start < bs.len() {
                let w = char_width(&bs[start..]);
                match str::from_utf8(&bs[start..start + w]) {
                    Ok(ch) if ch.chars().all(&matches) => start += w,
                    _ => break,
                }
            }
            let mut end = bs.len();
            while end > start {
                let mut from = end - 1;
                while from > start && end - from < 4 && bs[from] & 0xC0 == 0x80 {
                    from -= 1;
                }
                match str::from_utf8(&bs[from..end]) {
                    Ok(ch) if ch.chars().count() == 1 && ch.chars().all(&matches) => end = from,
                    _ => break,
                }
            }
            Buf::read_from_bytes(&bs[start..end]).into_str()
        })
    }

    pub fn capitalize<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let mut chars = src.chars();
        let result = match chars.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        };
        Str::from_text(result)
    }

    pub(crate) fn lines(&self) -> IntMap<Str<'a>> {
        let map: IntMap<Str> = IntMap::default();
        let src = self.as_text();
        let mut i: i64 = 1;
        for line in src.lines() {
            if !line.trim().is_empty() {
              map.insert(i, Str::from_text(line));
              i = i + 1;
            }
        }
        map
    }

    pub fn uncapitalize<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let mut chars = src.chars();
        let result = match chars.next() {
            None => String::new(),
            Some(f) => f.to_lowercase().collect::<String>() + chars.as_str(),
        };
        Str::from_text(result)
    }

    pub fn default_if_empty(&self, default: &Str<'a>) -> Str<'a> {
        if self.as_text().trim().is_empty() {
            return default.clone();
        }
        self.clone()
    }

    /// Byte-wise `starts_with`; false when either string is empty.
    pub fn has_prefix(&self, prefix: &Str) -> bool {
        self.with_bytes(|t| prefix.with_bytes(|p| !t.is_empty() && !p.is_empty() && t.starts_with(p)))
    }

    /// Byte-wise `ends_with`; false when either string is empty.
    pub fn has_suffix(&self, suffix: &Str) -> bool {
        self.with_bytes(|t| suffix.with_bytes(|p| !t.is_empty() && !p.is_empty() && t.ends_with(p)))
    }

    /// Byte-wise substring test; false when either string is empty.
    pub fn has_substr(&self, child: &Str) -> bool {
        self.with_bytes(|t| child.with_bytes(|c| !t.is_empty() && !c.is_empty() && memchr::memmem::find(t, c).is_some()))
    }

    pub fn append_if_missing(&self, suffix: &Str<'a>) -> Str<'a> {
        if !self.with_bytes(|text| suffix.with_bytes(|suffix| text.ends_with(suffix))) {
            return Str::concat(self.clone(), suffix.clone());
        }
        self.clone()
    }

    pub fn prepend_if_missing(&self, prefix: &Str<'a>) -> Str<'a> {
        if !self.with_bytes(|text| prefix.with_bytes(|prefix| text.starts_with(prefix))) {
            return Str::concat(prefix.clone(), self.clone());
        }
        self.clone()
    }

    pub fn remove_if_begin(&self, prefix: &Str<'a>) -> Str<'a> {
        let (len, prefix_len) = (self.bytes_len(), prefix.bytes_len());
        if self.with_bytes(|text| prefix.with_bytes(|prefix| text.starts_with(prefix))) {
            return self.slice(prefix_len, len);
        }
        self.clone()
    }

    pub fn remove_if_end(&self, suffix: &Str<'a>) -> Str<'a> {
        let (len, suffix_len) = (self.bytes_len(), suffix.bytes_len());
        if self.with_bytes(|text| suffix.with_bytes(|suffix| text.ends_with(suffix))) {
            return self.slice(0, len - suffix_len);
        }
        self.clone()
    }

    pub fn quote(&self) -> Str<'a> {
        self.wrap(b'\'')
    }

    pub fn double_quote(&self) -> Str<'a> {
        self.wrap(b'"')
    }

    /// `q` + self + `q`, unless self is already wrapped in `q`.
    fn wrap(&self, q: u8) -> Str<'a> {
        self.with_bytes(|bs| {
            if bs.len() >= 2 && bs[0] == q && bs[bs.len() - 1] == q {
                return self.clone();
            }
            Buf::read_from_bytes(&[&[q], bs, &[q]].concat()).into_str()
        })
    }

    pub fn camel_case<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let result = inflector::cases::camelcase::to_camel_case(&src);
        Str::from_text(result)
    }

    pub fn kebab_case<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let result = inflector::cases::kebabcase::to_kebab_case(&src);
        Str::from_text(result)
    }

    pub fn snake_case<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let result = inflector::cases::snakecase::to_snake_case(&src);
        Str::from_text(result)
    }

    pub fn title_case<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let result = inflector::cases::titlecase::to_title_case(&src);
        Str::from_text(result)
    }

    /// Substring of up to `r` chars starting at char `l` (0-based). The result shares the
    /// underlying buffer. Each invalid UTF-8 sequence counts as one char (as in [`Str::len`]) and
    /// is preserved as-is. Non-ASCII scans stop at char `l + r` and resume from the last position
    /// found in the same buffer, so `for (i...) substr(s, i, 1)` stays linear.
    pub fn sub_str(&self, l: usize, r: usize) -> Str<'a> {
        let bs = unsafe { &*self.get_bytes() };
        let end = l.saturating_add(r);
        let range = if characters_as_bytes() || bs[..end.min(bs.len())].is_ascii() {
            (l < bs.len()).then(|| (l, end.min(bs.len())))
        } else {
            let buf = unsafe { self.rep_mut() }.buf();
            SUBSTR_CURSOR.with(|cursor| {
                let mut cursor = cursor.borrow_mut();
                let (ci, bo) = match &*cursor {
                    Some(c) if c.ptr == bs.as_ptr() && c.len == bs.len() => {
                        if c.char_ix <= l {
                            (c.char_ix, c.byte_off)
                        } else if c.char_ix - l <= l {
                            char_back(bs, c.char_ix, c.byte_off, l).map_or((0, 0), |bo| (l, bo))
                        } else {
                            (0, 0)
                        }
                    }
                    _ => (0, 0),
                };
                let range = char_byte_range(bs, ci, bo, l, r);
                if let (Some(buf), Some((from, _))) = (buf, range) {
                    *cursor = Some(SubstrCursor {
                        _buf: buf,
                        ptr: bs.as_ptr(),
                        len: bs.len(),
                        char_ix: l,
                        byte_off: from,
                    });
                }
                range
            })
        };
        match range {
            Some((from, to)) if from < to => self.slice(from, to),
            _ => Str::default(),
        }
    }

    /// index start from 0
    pub fn char_at(&self, index: usize) -> Str<'a> {
        self.sub_str(index, 1)
    }

    pub(crate) fn words<'b>(&self) -> IntMap<Str<'b>> {
        let result: IntMap<Str> = IntMap::default();
        let mut index: i64 = 1;
        for word in self.as_text().unicode_words() {
            result.insert(index, Str::from_text(word));
            index = index + 1;
        }
        result
    }

    /// Mask the middle of the string with `*`. Lengths are counted in chars, not bytes.
    pub fn mask<'b>(&self) -> Str<'b> {
        let src = self.as_text();
        let src_len = src.chars().count();
        if src_len == 0 {
            return Str::from("".to_string());
        } else if src_len == 1 {
            return Str::from("*");
        } else if src_len <= 3 {
            let first = src.chars().next().unwrap();
            let result = format!("{}{}", first, "*".repeat(src_len - 1));
            return Str::from_text(result);
        }
        let mask_len = src_len / 3;
        let prefix_len = if mask_len > 2 { 2 } else { 0 };
        let mut result = String::new();
        // for loop for src with index and char
        for (i, c) in src.chars().enumerate() {
            if i < prefix_len || i >= prefix_len + mask_len {
                result.push(c);
            } else {
                result.push('*');
            }
        }
        Str::from_text(result)
    }

    /// Repeat the string `n` times; a negative `n` yields an empty string. A result longer than
    /// [`MAX_RESULT_LEN`](crate::runtime::MAX_RESULT_LEN) bytes is an error.
    pub fn repeat<'b>(&self, n: Int) -> crate::common::Result<Str<'b>> {
        let n = n.max(0);
        crate::runtime::check_result_len("repeat", "result length", self.bytes_len() as Float * n as Float)?;
        Ok(self.with_bytes(|bs| Buf::read_from_bytes(&bs.repeat(n as usize)).into_str()))
    }

    pub fn escape<'b>(&self, format: &Str<'b>) -> Str<'b> {
        Str::from_text(escape(&format.as_str(), &self.as_text()))
    }

    /// Truncate the string to at most `len` chars, ending with `place_holder` when truncated.
    /// If `place_holder` does not fit in `len` chars, the string is cut to `len` chars without
    /// it. A negative `len` is treated as 0.
    pub fn truncate<'b>(&self, len: Int, place_holder: &Str<'b>) -> Str<'b> {
        let max_len = len.max(0) as usize;
        let src_len = self.with_bytes(char_count);
        let place_holder_len = place_holder.with_bytes(char_count);
        let (keep, suffix) = if src_len <= max_len {
            (src_len, None)
        } else if place_holder_len >= max_len {
            (max_len, None)
        } else {
            (max_len - place_holder_len, Some(place_holder))
        };
        let text = self.sub_str(0, keep);
        text.with_bytes(|head| match suffix {
            None => Buf::read_from_bytes(head).into_str(),
            Some(ph) => ph.with_bytes(|tail| Buf::read_from_bytes(&[head, tail].concat()).into_str()),
        })
    }

    /// awk's `toupper`. Despite the name, non-ASCII UTF-8 text is converted too (see
    /// `map_case`).
    pub fn to_upper_ascii<'b>(&self) -> Str<'b> {
        self.map_case(|b| b.to_ascii_uppercase(), char::to_uppercase)
    }

    /// Case conversion for toupper/tolower. ASCII strings take the fast byte-wise path. Otherwise
    /// each character is converted when its mapping is a single character, like C's
    /// towupper/towlower used by gawk (so "ß" is unchanged rather than becoming "SS"); bytes that
    /// are not valid UTF-8 are kept as is.
    fn map_case<'b, I: Iterator<Item = char> + ExactSizeIterator>(
        &self,
        ascii: impl FnMut(u8) -> u8,
        unicode: impl Fn(char) -> I,
    ) -> Str<'b> {
        if self.with_bytes(|bs| bs.is_ascii()) {
            return self.map_bytes(ascii);
        }
        self.with_bytes(|bs| {
            let mut out = DynamicBufHeap::new(bs.len());
            for chunk in bs.utf8_chunks() {
                for c in chunk.valid().chars() {
                    let mut mapped = unicode(c);
                    let c = match (mapped.len(), mapped.next()) {
                        (1, Some(m)) => m,
                        _ => c,
                    };
                    let mut buf = [0u8; 4];
                    out.write_all(c.encode_utf8(&mut buf).as_bytes()).unwrap();
                }
                out.write_all(chunk.invalid()).unwrap();
            }
            out.into_str()
        })
    }

    fn map_bytes<'b>(&self, mut f: impl FnMut(u8) -> u8) -> Str<'b> {
        self.with_bytes(|bs| {
            if bs.len() <= MAX_INLINE_SIZE {
                let mut buf = SmallVec::<[u8; MAX_INLINE_SIZE]>::with_capacity(bs.len());
                for b in bs {
                    buf.push(f(*b))
                }
                unsafe { Str::from_rep(Inline::from_unchecked(buf.as_slice()).into()) }
            } else {
                let mut buf = DynamicBufHeap::new(bs.len());
                for b in bs {
                    buf.push_byte(f(*b))
                }
                buf.into_str()
            }
        })
    }

    pub fn subst_first(&self, pat: &Regex, subst: &Str<'a>) -> (Str<'a>, bool) {
        self.with_bytes(|s| {
            subst.with_bytes(|subst| {
                if let Some((start, end)) = super::awk_regex::find_at(pat, s, 0) {
                    let mut buf = DynamicBuf::new(s.len());
                    buf.write_all(&s[0..start]).unwrap();
                    process_match(&s[start..end], subst, &mut buf).unwrap();
                    buf.write_all(&s[end..s.len()]).unwrap();
                    (buf.into_str(), true)
                } else {
                    (self.clone(), false)
                }
            })
        })
    }

    pub fn subst_all(&self, pat: &Regex, subst: &Str<'a>) -> (Str<'a>, Int) {
        self.with_bytes(|s| {
            subst.with_bytes(|subst| {
                let mut buf = DynamicBuf::new(0);
                let mut prev = 0;
                let mut count = 0;
                let mut bound = CharBoundary::new(s);
                for (start, end) in super::awk_regex::find_iter(pat, s) {
                    if bound.splits_char(start, end) {
                        continue;
                    }
                    buf.write_all(&s[prev..start]).unwrap();
                    process_match(&s[start..end], subst, &mut buf).unwrap();
                    prev = end;
                    count += 1;
                }
                if count == 0 {
                    (self.clone(), count)
                } else {
                    buf.write_all(&s[prev..s.len()]).unwrap();
                    (buf.into_str(), count)
                }
            })
        })
    }

    pub fn gen_subst_dynamic(&self, pat: &Regex, subst: &Str<'a>, how: &Str<'a>) -> Str<'a> {
        how.with_bytes(|how| {
            if !how.is_empty() && matches!(how[0], b'g' | b'G') {
                self.gen_subst_all(pat, subst)
            } else {
                // this silently ignores strings that cannot be parsed and treats them as "1"
                let which = strtoi(how);
                let which = std::cmp::max(1, which);
                self.gen_subst_n(pat, subst, which)
            }
        })
    }

    pub fn gen_subst_all(&self, pat: &Regex, subst: &Str<'a>) -> Str<'a> {
        self.with_bytes(|s| {
            subst.with_bytes(|subst| {
                let mut buf = DynamicBuf::new(0);
                let mut prev = 0;
                let mut count = 0;
                let mut bound = CharBoundary::new(s);
                for (start, end) in super::awk_regex::find_iter(pat, s) {
                    if bound.splits_char(start, end) {
                        continue;
                    }
                    let Some(c) = super::awk_regex::captures_at(pat, s, start, end) else {
                        continue;
                    };
                    buf.write_all(&s[prev..start]).unwrap();
                    process_match_gen(c, subst, &mut buf).unwrap();
                    prev = end;
                    count += 1;
                }
                if count == 0 {
                    self.clone()
                } else {
                    buf.write_all(&s[prev..s.len()]).unwrap();
                    buf.into_str()
                }
            })
        })
    }

    /// Handle the general substitution for a case of integer value in "how"
    /// Will replace match number `which` (indexed from 1)
    pub fn gen_subst_n(&self, pat: &Regex, subst: &Str<'a>, which: Int) -> Str<'a> {
        self.with_bytes(|s| {
            subst.with_bytes(|subst| {
                // Match against the whole string (not a suffix) so that anchors and word
                // boundaries keep their context.
                let mut bound = CharBoundary::new(s);
                let nth = super::awk_regex::find_iter(pat, s)
                    .filter(|&(start, end)| !bound.splits_char(start, end))
                    .nth(which as usize - 1);
                if let Some((start, end, c)) = nth.and_then(|(start, end)| {
                    super::awk_regex::captures_at(pat, s, start, end).map(|c| (start, end, c))
                }) {
                    let mut buf = DynamicBuf::new(s.len());
                    buf.write_all(&s[0..start]).unwrap();
                    process_match_gen(c, subst, &mut buf).unwrap();
                    buf.write_all(&s[end..]).unwrap();
                    buf.into_str()
                } else {
                    self.clone()
                }
            })
        })
    }

    pub fn bytes_len(&self) -> usize {
        unsafe { self.rep_mut() }.len()
    }

    /// Length in chars; each invalid UTF-8 sequence counts as one char.
    pub fn len(&self) -> usize {
        self.with_bytes(char_count)
    }

    pub fn concat(left: Str<'a>, right: Str<'a>) -> Str<'a> {
        if left.is_empty() {
            mem::forget(left);
            return right;
        }
        if right.is_empty() {
            mem::forget(right);
            return left;
        }
        let llen = left.bytes_len();
        let rlen = right.bytes_len();
        let new_len = llen + rlen;
        if new_len <= MAX_INLINE_SIZE {
            let mut b = DynamicBuf::new(0);
            unsafe {
                b.write_all(&*left.get_bytes()).unwrap();
                b.write_all(&*right.get_bytes()).unwrap();
                b.into_str()
            }
        } else {
            // TODO: we can add another case here. If `left` is boxed and has a refcount of 1, we
            // can move it into a dynamicbuf and push `right` onto it, avoiding the heap
            // allocation. We _only_ want to do this if we reevaluate the `realloc` that DynamicBuf
            // does when you convert it back into a string, though. We would have to keep a
            // capacity around as well as a length.
            let concat = unsafe { Concat::new(new_len as u64, left, right) };
            Str::from_rep(concat.into())
        }
    }

    fn from_rep(rep: StrRep<'a>) -> Str<'a> {
        Str(UnsafeCell::new(rep))
    }

    // This helper method assumes:
    // * that from and to cannot overflow when moved to u32s/shared/etc.
    // * that any CONCATs have been forced away.
    // * to - from > MAX_INLINE_SIZE
    unsafe fn slice_nooverflow(&self, from: usize, to: usize) -> Str<'a> {
        let rep = self.rep_mut();
        let tag = rep.get_tag();
        let new_rep = match tag {
            StrTag::Shared => rep.view_as(|s: &Shared| {
                let start = s.start + from as u32;
                let end = s.start + to as u32;
                Shared {
                    start,
                    end,
                    buf: s.buf.clone(),
                }
                    .into()
            }),
            StrTag::Boxed => rep.view_as(|b: &Boxed| {
                Shared {
                    start: from as u32,
                    end: to as u32,
                    buf: b.buf.clone(),
                }
                    .into()
            }),
            StrTag::Literal => rep.view_as(|l: &Literal| {
                let new_ptr = l.ptr.add(from);
                let new_len = (to - from) as u64;
                Literal {
                    len: new_len,
                    ptr: new_ptr,
                    _marker: PhantomData,
                }
                    .into()
            }),
            StrTag::Inline | StrTag::Concat => unreachable!(),
        };
        Str::from_rep(new_rep)
    }

    unsafe fn slice_internal(&self, from: usize, to: usize) -> Str<'a> {
        assert!(from <= to);
        if from == to {
            return Default::default();
        }
        let len = self.bytes_len();
        assert!(
            to <= len,
            "invalid args to slice: range [{},{}) with len {}",
            from,
            to,
            len
        );
        let new_len = to - from;
        if new_len <= MAX_INLINE_SIZE {
            return Str::from_rep(Inline::from_unchecked(&(&(*self.get_bytes()))[from..to]).into());
        }
        let tag = self.rep().get_tag();
        let u32_max = u32::MAX as usize;
        let mut may_overflow = to > u32_max || from > u32_max;
        if !may_overflow && tag == StrTag::Shared {
            // If we are taking a slice of an existing slice, then we can overflow by adding the
            // starts and ends together.
            may_overflow = self.rep_mut().view_as(|s: &Shared| {
                (s.start as usize + from) > u32_max || (s.start as usize + to) > u32_max
            });
        }
        // Slices of literals are addressed with 64 bits.
        may_overflow = may_overflow && tag != StrTag::Literal;
        if may_overflow {
            // uncommon case: we cannot represent a Shared value. We need to copy and box the value
            // instead.
            // TODO: We can optimize cases when we are getting suffixes of Literal values
            // by creating new ones with offset pointers. This doesn't seem worth optimizing right
            // now, but we may want to in the future.
            self.force();
            let rep = self.rep_mut();
            let tag = rep.get_tag();
            // All other variants ruled out by how large `self` is and the fact that we
            // just called `force`
            debug_assert_eq!(tag, StrTag::Boxed);
            return Str::from_rep(rep.view_as(|b: &Boxed| {
                let buf = Buf::read_from_raw(b.buf.as_ptr().add(from), new_len);
                Boxed {
                    len: new_len as u64,
                    buf,
                }
                    .into()
            }));
        }

        // Force concat up here so we don't have to worry about aliasing `rep` in slice_nooverflow.
        if let StrTag::Concat = tag {
            self.force()
        }
        self.slice_nooverflow(from, to)
    }

    pub fn slice(&self, from: usize, to: usize) -> Str<'a> {
        // TODO: consider returning a result here so we can error out in a more graceful way.
        {
            let bs = unsafe { &*self.get_bytes() };
            assert!(
                (from == to && to == bs.len()) || from < bs.len(),
                "internal error: invalid index len={}, from={}, to={}",
                bs.len(),
                from,
                to,
            );
            assert!(to <= bs.len(), "internal error: invalid index");
        }
        unsafe { self.slice_internal(from, to) }
    }

    // Why is [with_bytes] safe and [force] unsafe? Let's go case-by-case for the state of `self`
    // EMPTY:  no data is passed into `f`.
    // BOXED:  The function signature ensures that no string references can "escape" `f`, and `self`
    //         will persist for the function body, which will keep the underlying buffer alive.
    // CONCAT: We `force` these strings, so they will be BOXED.
    // SHARED: This one is tricky. It may seem to be covered by the BOXED case, but the difference
    //         is that shared strings give up there references to the underlying buffer if they get
    //         forced. So if we did s.with_bytes(|x| { /* force s */; *x}), then *x is a
    //         use-after-free!
    //
    //         This is why [force] is unsafe. As written, no safe method will force a SHARED Str.
    //         If we add force to a public API (e.g. for garbage collection), we'll need to ensure
    //         that we don't call with_bytes around it, or clone the string before forcing.

    #[allow(clippy::never_loop)]
    unsafe fn force(&self) {
        let (tag, len) = {
            let rep = self.rep_mut();
            (rep.get_tag(), rep.len())
        };
        if tag.forced() {
            return;
        }
        let mut whead = 0;
        let mut res = UniqueBuf::new(len);
        macro_rules! push_bytes {
            ($slice:expr, [$from:expr, $to:expr]) => {{
                let from = $from;
                let slen = $to - from;
                push_bytes!(&$slice[$from], slen);
            }};
            ($at:expr, $len:expr) => {{
                let slen = $len;
                debug_assert!((len - whead) >= slen);
                let head = &mut res.as_mut_bytes()[whead];
                ptr::copy_nonoverlapping($at, head, slen);
                whead += slen;
            }};
        }
        let mut todos = SmallVec::<[Str<'a>; 16]>::new();
        let mut cur: Str<'a> = self.clone();
        let new_rep: StrRep<'a> = 'outer: loop {
            let rep = cur.rep_mut();
            let tag = rep.get_tag();
            cur = loop {
                match tag {
                    StrTag::Inline => rep.view_as_inline(|i| {
                        push_bytes!(i.bytes(), [0, i.len()]);
                    }),
                    StrTag::Literal => rep.view_as(|l: &Literal| {
                        push_bytes!(l.ptr, l.len as usize);
                    }),
                    StrTag::Boxed => rep.view_as(|b: &Boxed| {
                        push_bytes!(b.buf.as_bytes(), [0, b.len as usize]);
                    }),
                    StrTag::Shared => rep.view_as(|s: &Shared| {
                        push_bytes!(s.buf.as_bytes(), [s.start as usize, s.end as usize]);
                    }),
                    StrTag::Concat => {
                        break rep.view_as(|c: &Concat| {
                            todos.push(c.right());
                            c.left()
                        });
                    }
                }
                if let Some(c) = todos.pop() {
                    break c;
                }
                break 'outer Boxed {
                    len: len as u64,
                    buf: res.into_buf(),
                }
                    .into();
            };
        };
        *self.rep_mut() = new_rep;
    }

    // Avoid using this function; subsequent immutable calls to &self can invalidate the pointer.
    pub fn get_bytes(&self) -> *const [u8] {
        let rep = unsafe { self.rep_mut() };
        let tag = rep.get_tag();
        unsafe {
            match tag {
                StrTag::Inline => rep.view_as_inline(|i| i.bytes() as *const _),
                StrTag::Literal => rep.view_as(|lit: &Literal| {
                    std::ptr::slice_from_raw_parts(lit.ptr, lit.len as usize) as *const _
                }),
                StrTag::Shared => rep.view_as(|s: &Shared| {
                    &s.buf.as_bytes()[s.start as usize..s.end as usize] as *const _
                }),
                StrTag::Boxed => rep.view_as(|b: &Boxed| b.buf.as_bytes() as *const _),
                StrTag::Concat => {
                    self.force();
                    self.get_bytes()
                }
            }
        }
    }

    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let raw = self.get_bytes();
        unsafe { f(&*raw) }
    }

    pub fn unmoor(self) -> Str<'static> {
        let rep = unsafe { self.rep_mut() };
        let tag = rep.get_tag();
        if let StrTag::Literal = tag {
            let new_rep = unsafe {
                rep.view_as(|lit: &Literal| {
                    let buf = Buf::read_from_raw(lit.ptr, lit.len as usize);
                    Boxed { len: lit.len, buf }.into()
                })
            };
            *rep = new_rep;
        }
        unsafe { mem::transmute::<Str<'a>, Str<'static>>(self) }
    }

    /// Text view of the bytes: borrowed when they are valid UTF-8, otherwise invalid sequences
    /// are replaced with U+FFFD, so non UTF-8 input (Latin-1, GBK, binary) never panics.
    pub fn as_str(&self) -> Cow<'a, str> {
        let bs: &'a [u8] = unsafe { &*self.get_bytes() };
        String::from_utf8_lossy(bs)
    }

    /// Lossless text view of the bytes, for stdlib functions that need a `&str`. Valid UTF-8 is
    /// borrowed; otherwise each byte that is not valid UTF-8 becomes a placeholder char (see
    /// [`escape_byte`]) that [`Str::from_text`] turns back into the original byte. Use
    /// [`Str::as_str`] only where the text leaves zawk (paths, URLs, SQL, JSON).
    pub fn as_text(&self) -> Cow<'a, str> {
        let bs: &'a [u8] = unsafe { &*self.get_bytes() };
        match str::from_utf8(bs) {
            Ok(s) if !bs.contains(&ESCAPE_LEAD) => Cow::Borrowed(s),
            _ => Cow::Owned(escape_invalid_utf8(bs)),
        }
    }

    /// An owned copy of `bs`.
    pub fn from_bytes<'b>(bs: &[u8]) -> Str<'b> {
        Buf::read_from_bytes(bs).into_str()
    }

    /// [`Str::from_text`] on a string already holding the text.
    pub fn unescape_text(&self) -> Str<'a> {
        if !self.with_bytes(|bs| bs.contains(&ESCAPE_LEAD)) {
            return self.clone();
        }
        Str::from_text(self.as_str().into_owned())
    }

    /// The inverse of [`Str::as_text`]: placeholder chars become the bytes they stand for.
    pub fn from_text<'b>(s: impl Into<String>) -> Str<'b> {
        let s = s.into();
        if !s.as_bytes().contains(&ESCAPE_LEAD) {
            return Str::from(s);
        }
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            match unescape_byte(c) {
                Some(b) => out.push(b),
                None => out.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes()),
            }
        }
        Buf::read_from_bytes(&out).into_str()
    }
}

/// [`Str::as_text`] maps byte `b` (always >= 0x80: ASCII is valid UTF-8) to `ESCAPE_BASE + b`,
/// i.e. U+10FF80..=U+10FFFF in the last private use plane, whose UTF-8 encoding starts with
/// [`ESCAPE_LEAD`]. Real chars in that range are escaped byte by byte too, so the round trip is
/// lossless.
const ESCAPE_BASE: u32 = 0x10FF00;
const ESCAPE_LEAD: u8 = 0xF4;

fn escape_byte(b: u8) -> char {
    char::from_u32(ESCAPE_BASE + b as u32).unwrap()
}

fn unescape_byte(c: char) -> Option<u8> {
    let c = c as u32;
    (c >= ESCAPE_BASE + 0x80 && c <= ESCAPE_BASE + 0xFF).then(|| (c - ESCAPE_BASE) as u8)
}

fn escape_invalid_utf8(bs: &[u8]) -> String {
    let mut out = String::with_capacity(bs.len() + 8);
    for chunk in bs.utf8_chunks() {
        for c in chunk.valid().chars() {
            if unescape_byte(c).is_some() {
                out.extend(c.encode_utf8(&mut [0u8; 4]).bytes().map(escape_byte));
            } else {
                out.push(c);
            }
        }
        out.extend(chunk.invalid().iter().copied().map(escape_byte));
    }
    out
}

impl<'a> Clone for Str<'a> {
    fn clone(&self) -> Str<'a> {
        let rep = unsafe { self.rep_mut() };
        let tag = rep.get_tag();
        let cloned_rep: StrRep<'a> = unsafe {
            match tag {
                StrTag::Literal | StrTag::Inline => rep.copy(),
                StrTag::Shared => rep.view_as(|s: &Shared| s.clone()).into(),
                StrTag::Boxed => rep.view_as(|b: &Boxed| b.clone()).into(),
                StrTag::Concat => rep.view_as(|c: &Concat<'a>| c.clone()).into(),
            }
        };
        Str(UnsafeCell::new(cloned_rep))
    }
}

impl<'a> PartialEq for Str<'a> {
    fn eq(&self, other: &Str<'a>) -> bool {
        // If the bits are the same, then the strings are equal.
        if unsafe { self.rep() == other.rep() } {
            return true;
        }
        // TODO: we could intern these strings if they wind up equal.
        self.with_bytes(|bs1| other.with_bytes(|bs2| bs1 == bs2))
    }
}

impl<'a> Eq for Str<'a> {}

impl<'a> Hash for Str<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.with_bytes(|bs| bs.hash(state))
    }
}

impl<'a> From<&'a str> for Str<'a> {
    fn from(s: &'a str) -> Str<'a> {
        s.as_bytes().into()
    }
}

impl<'a> From<&'a [u8]> for Str<'a> {
    fn from(bs: &'a [u8]) -> Str<'a> {
        if bs.is_empty() {
            Default::default()
        } else if bs.len() <= MAX_INLINE_SIZE {
            Str::from_rep(unsafe { Inline::from_raw(bs.as_ptr(), bs.len()).into() })
        } else if bs.as_ptr() as usize & 0x7 != 0 {
            // Strings are not guaranteed to be word aligned. Copy over strings that aren't. This
            // is more important for tests; most of the places that literals can come from in an
            // awk program will hand out aligned pointers.
            let buf = Buf::read_from_bytes(bs);
            let boxed = Boxed {
                len: bs.len() as u64,
                buf,
            };
            Str::from_rep(boxed.into())
        } else {
            let literal = Literal {
                len: bs.len() as u64,
                ptr: bs.as_ptr(),
                _marker: PhantomData,
            };
            Str::from_rep(literal.into())
        }
    }
}

impl<'a> From<String> for Str<'a> {
    fn from(s: String) -> Str<'a> {
        if s.is_empty() {
            return Default::default();
        }
        let buf = Buf::read_from_bytes(s.as_bytes());
        let boxed = Boxed {
            len: s.len() as u64,
            buf,
        };
        Str::from_rep(boxed.into())
    }
}

// For numbers, we are careful to check if a number only requires 15 digits or fewer to be
// represented. This allows us to trigger the "Inline" variant and avoid a heap allocation,
// sometimes at the expenseof a small copy.

impl<'a> From<Int> for Str<'a> {
    fn from(i: Int) -> Str<'a> {
        let mut itoabuf = itoa::Buffer::new();
        let s = itoabuf.format(i);
        Buf::read_from_bytes(s.as_bytes()).into_str()
    }
}

// Converting a number to a string uses CONVFMT, as in awk; see runtime::numfmt.
impl<'a> From<Float> for Str<'a> {
    fn from(f: Float) -> Str<'a> {
        crate::runtime::numfmt::with_number_bytes(f, false, |bs| {
            Buf::read_from_bytes(bs).into_str()
        })
    }
}

impl<'a> Str<'a> {
    /// A number as written by `print`, which uses OFMT rather than CONVFMT.
    pub fn from_float_output(f: Float) -> Str<'a> {
        crate::runtime::numfmt::with_number_bytes(f, true, |bs| {
            Buf::read_from_bytes(bs).into_str()
        })
    }
}

impl Str<'static> {
    // Why have this? Parts of the runtime hold onto a Str<'static> to avoid adding a lifetime
    // parameter to the struct.
    pub fn upcast<'a>(self) -> Str<'a> {
        unsafe { mem::transmute::<Str<'static>, Str<'a>>(self) }
    }
    pub fn upcast_ref<'a>(&self) -> &Str<'a> {
        unsafe { mem::transmute::<&Str<'static>, &Str<'a>>(self) }
    }
}

#[repr(C)]
struct BufHeader {
    size: usize,
    // We only have "strong counts"
    count: Cell<usize>,
}

#[repr(transparent)]
pub struct UniqueBuf(*mut BufHeader);

unsafe impl Send for UniqueBuf {}

pub struct DynamicBufHeap {
    data: UniqueBuf,
    write_head: usize,
}

impl DynamicBufHeap {
    pub fn new(size: usize) -> DynamicBufHeap {
        DynamicBufHeap {
            data: UniqueBuf::new(size),
            write_head: 0,
        }
    }
    fn size(&self) -> usize {
        unsafe { (*self.data.0).size }
    }
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        self.data.as_mut_bytes()
    }
    pub fn write_head(&self) -> usize {
        self.write_head
    }
    pub fn into_buf(self) -> Buf {
        self.data.into_buf()
    }

    pub(crate) fn into_str<'a>(mut self) -> Str<'a> {
        // Shrink the buffer to fit.
        unsafe { self.realloc(self.write_head) };
        self.data.into_buf().into_str()
    }
    unsafe fn realloc(&mut self, new_cap: usize) {
        let cap = self.size();
        if cap == new_cap {
            return;
        }
        let new_buf = realloc(
            self.data.0 as *mut u8,
            UniqueBuf::layout(cap),
            UniqueBuf::layout(new_cap).size(),
        ) as *mut BufHeader;
        (*new_buf).size = new_cap;
        self.data.0 = new_buf;
    }

    fn push_byte(&mut self, b: u8) {
        let cap = self.size();
        debug_assert!(
            cap >= self.write_head,
            "cap={}, write_head={}",
            cap,
            self.write_head
        );
        let remaining = cap - self.write_head;
        unsafe {
            if remaining == 0 {
                let new_cap = std::cmp::max(cap + 1, cap * 2);
                self.realloc(new_cap);
            }
            *self.data.as_mut_ptr().add(self.write_head) = b;
        };
        self.write_head += 1;
    }
}

impl Write for DynamicBufHeap {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let cap = self.size();
        debug_assert!(
            cap >= self.write_head,
            "cap={}, write_head={}",
            cap,
            self.write_head
        );
        let remaining = cap - self.write_head;
        unsafe {
            if remaining < buf.len() {
                let new_cap = std::cmp::max(cap + buf.len(), cap * 2);
                self.realloc(new_cap);
                ptr::copy(
                    buf.as_ptr(),
                    self.data.as_mut_ptr().add(self.write_head),
                    buf.len(),
                );
                // NB: even after copying, there may be uninitialized memory at the tail of the
                // buffer. We enforce that this memory is never read by doing a realloc(write_head)
                // before moving this into a Buf. Before then, we don't read the underlying data at
                // all.
            } else {
                ptr::copy(
                    buf.as_ptr(),
                    self.data.as_mut_ptr().add(self.write_head),
                    buf.len(),
                )
            }
        };
        self.write_head += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub enum DynamicBuf {
    Inline(smallvec::SmallVec<[u8; MAX_INLINE_SIZE]>),
    Heap(DynamicBufHeap),
}

impl Default for DynamicBuf {
    fn default() -> DynamicBuf {
        DynamicBuf::Inline(Default::default())
    }
}

impl DynamicBuf {
    pub fn new(size: usize) -> DynamicBuf {
        if size <= MAX_INLINE_SIZE {
            DynamicBuf::Inline(Default::default())
        } else {
            DynamicBuf::Heap(DynamicBufHeap::new(size))
        }
    }
    pub fn into_str<'a>(self) -> Str<'a> {
        match self {
            // Safety: the 'unchecked' here refers to `sv` needing to fit within
            // the inline size. But DynamicBuf::Inline and Inline's definition
            // of the max size is the same (MAX_INLINE_SIZE).
            DynamicBuf::Inline(sv) => unsafe {
                Str::from_rep(Inline::from_unchecked(&sv[..]).into())
            },
            DynamicBuf::Heap(dbuf) => dbuf.into_str(),
        }
    }
}

impl Write for DynamicBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            DynamicBuf::Inline(sv) => {
                let new_len = sv.len() + buf.len();
                if sv.len() + buf.len() > MAX_INLINE_SIZE {
                    let mut heap = DynamicBufHeap::new(new_len);
                    heap.write_all(&sv[..]).unwrap();
                    heap.write_all(buf).unwrap();
                    *self = DynamicBuf::Heap(heap);
                } else {
                    sv.extend(buf.iter().cloned());
                }
                Ok(buf.len())
            }
            DynamicBuf::Heap(dbuf) => dbuf.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[repr(transparent)]
pub struct Buf(*const BufHeader);

impl Clone for Buf {
    fn clone(&self) -> Buf {
        let header: &BufHeader = unsafe { &(*self.0) };
        let cur = header.count.get();
        header.count.set(cur + 1);
        Buf(self.0)
    }
}

impl Drop for UniqueBuf {
    fn drop(&mut self) {
        let header: &mut BufHeader = unsafe { &mut (*self.0) };
        debug_assert_eq!(header.count.get(), 1);
        unsafe { dealloc(self.0 as *mut u8, UniqueBuf::layout(header.size)) }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        let header: &BufHeader = unsafe { &(*self.0) };
        let cur = header.count.get();
        debug_assert!(cur > 0);
        if cur == 1 {
            mem::drop(UniqueBuf(self.0 as *mut _));
            return;
        }
        header.count.set(cur - 1);
    }
}

impl UniqueBuf {
    fn layout(size: usize) -> Layout {
        Layout::from_size_align(
            size + mem::size_of::<BufHeader>(),
            mem::align_of::<BufHeader>(),
        )
            .unwrap()
    }
    pub fn new(size: usize) -> UniqueBuf {
        let layout = UniqueBuf::layout(size);
        unsafe {
            let alloced = alloc_zeroed(layout) as *mut BufHeader;
            assert!(!alloced.is_null());
            ptr::write(
                alloced,
                BufHeader {
                    size,
                    count: Cell::new(1),
                },
            );
            UniqueBuf(alloced)
        }
    }
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        let header: &BufHeader = unsafe { &(*self.0) };
        debug_assert_eq!(header.count.get(), 1);
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), header.size) }
    }
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        let header: &BufHeader = unsafe { &(*self.0) };
        debug_assert_eq!(header.count.get(), 1);
        unsafe { self.0.offset(1) as *mut u8 }
    }
    pub fn into_buf(self) -> Buf {
        let res = Buf(self.0);
        mem::forget(self);
        res
    }
}

impl Buf {
    pub fn into_str<'a>(self) -> Str<'a> {
        Str::from_rep(
            Boxed {
                len: self.len() as u64,
                buf: self,
            }
                .into(),
        )
    }

    pub(crate) fn len(&self) -> usize {
        unsafe { &(*self.0) }.size
    }

    pub fn as_bytes(&self) -> &[u8] {
        let size = self.len();
        unsafe { slice::from_raw_parts(self.as_ptr(), size) }
    }

    pub fn as_ptr(&self) -> *const u8 {
        unsafe { self.0.offset(1) as *const u8 }
    }

    fn refcount(&self) -> usize {
        let header: &BufHeader = unsafe { &(*self.0) };
        header.count.get()
    }

    // Unsafe because `from` and `to` must point to the start of characters.
    #[allow(clippy::suspicious_else_formatting)]
    pub fn slice_to_str<'a>(&self, from: usize, to: usize) -> Str<'a> {
        debug_assert!(from <= self.len());
        debug_assert!(to <= self.len());
        debug_assert!(from <= to, "invalid slice [{}, {})", from, to);
        let len = to.saturating_sub(from);
        if len == 0 {
            Str::default()
        } else
        /* NB: we could also have the following.
         * This creates a tradeoff: in scripts where we split several fields, performing this copy
         * has a noticeable impact on performance.
         *
         * In scripts that mainly read a small number of columns, the additional indirection layer
         * of indirection leads to a marginal performance hit when reading this data. For now, we
         * opt for the faster `slice` operation, but there's a solid case for either one, to the
         * point where we may want this to be configurable.
        if len <= MAX_INLINE_SIZE {
            unsafe {
                Str::from_rep(
                    Inline::from_raw(self.as_ptr().offset(std::cmp::max(0, from as isize)), len)
                        .into(),
                )
            }
        } else */
        if likely(from <= u32::MAX as usize && to <= u32::MAX as usize) {
            Str::from_rep(
                Shared {
                    buf: self.clone(),
                    start: from as u32,
                    end: to as u32,
                }
                    .into(),
            )
        } else {
            self.clone().into_str().slice(from, to)
        }
    }

    pub(crate) unsafe fn read_from_raw(ptr: *const u8, len: usize) -> Buf {
        let mut ubuf = UniqueBuf::new(len);
        ptr::copy_nonoverlapping(ptr, ubuf.as_mut_ptr(), len);
        ubuf.into_buf()
    }

    pub fn read_from_bytes(s: &[u8]) -> Buf {
        unsafe { Buf::read_from_raw(s.as_ptr(), s.len()) }
    }
    pub fn try_unique(self) -> Result<UniqueBuf, Buf> {
        if self.refcount() == 1 {
            let res = UniqueBuf(self.0 as *mut _);
            mem::forget(self);
            Ok(res)
        } else {
            Err(self)
        }
    }
}

/// Byte regexes may produce empty matches inside a multi-byte UTF-8 character; substituting at
/// such a position would corrupt the character. This detects those matches so they can be
/// skipped. Validity of the input is only checked (once) when such a match is actually seen, so
/// non-UTF-8 input keeps the byte-oriented behavior.
struct CharBoundary<'s> {
    s: &'s [u8],
    valid_utf8: Option<bool>,
}

impl<'s> CharBoundary<'s> {
    fn new(s: &'s [u8]) -> Self {
        CharBoundary { s, valid_utf8: None }
    }

    fn splits_char(&mut self, start: usize, end: usize) -> bool {
        // UTF-8 continuation bytes are 0b10xxxxxx.
        if start != end || start >= self.s.len() || (self.s[start] & 0xC0) != 0x80 {
            return false;
        }
        let s = self.s;
        *self
            .valid_utf8
            .get_or_insert_with(|| std::str::from_utf8(s).is_ok())
    }
}

/// Helper function for `subst_first` and `subst_all`: handles '&' syntax.
fn process_match(matched: &[u8], subst: &[u8], w: &mut impl Write) -> io::Result<()> {
    if memchr::memchr(b'&', subst).is_none() {
        w.write_all(subst).unwrap();
        return Ok(());
    }
    let mut start = 0;
    let mut escaped = false;
    for (i, b) in subst.iter().cloned().enumerate() {
        match b {
            b'&' => {
                if escaped {
                    w.write_all(&subst[start..i - 1])?;
                    w.write_all(&[b'&'])?;
                } else {
                    w.write_all(&subst[start..i])?;
                    w.write_all(matched)?;
                }
                start = i + 1;
            }
            b'\\' => {
                if !escaped {
                    escaped = true;
                    continue;
                }
                w.write_all(&subst[start..i])?;
                start = i + 1;
            }
            _ => {}
        }
        escaped = false;
    }
    w.write_all(&subst[start..])?;
    Ok(())
}

/// Helper function for `subst_gen` function; handles the syntax for &, \0, \1, etc...
fn process_match_gen(matched: Captures, subst: &[u8], w: &mut impl Write) -> io::Result<()> {
    let mut start = 0;
    let mut escaped = false;
    for (i, b) in subst.iter().cloned().enumerate() {
        match b {
            b'0'..=b'9' => {
                if escaped {
                    w.write_all(&subst[start..i - 1])?;
                    let n = b - b'0';
                    match matched.get(n as usize) {
                        Some(match_) => w.write_all(match_.as_bytes())?,
                        None => eprintln_ignore!(
                            // no match - no substitution (same as gawk); warning is nice though
                            "Couldn't substitute match {}, we have only {}",
                            n,
                            matched.len()
                        ),
                    }
                } else {
                    w.write_all(&subst[start..i])?;
                    w.write_all(&[b])?;
                }
                start = i + 1;
            }
            b'&' => {
                if escaped {
                    w.write_all(&subst[start..i - 1])?;
                    w.write_all(&[b'&'])?;
                } else {
                    w.write_all(&subst[start..i])?;
                    w.write_all(matched.get(0).unwrap().as_bytes())?;
                }
                start = i + 1;
            }
            b'\\' => {
                if !escaped {
                    escaped = true;
                    continue;
                }
                w.write_all(&subst[start..i])?;
                start = i + 1;
            }
            _ => {}
        }
        escaped = false;
    }
    w.write_all(&subst[start..])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_text_round_trip() {
        let cases: &[&[u8]] = &[
            b"",
            b"plain",
            "caf\u{e9} \u{4f60}\u{597d}".as_bytes(),
            b"caf\xe9x",
            b"\xff\xfe\x80",
            // real chars in the placeholder range, and invalid bytes around them
            "a\u{10FF80}b\u{10FFFF}".as_bytes(),
            b"\xf4\x8f\xbe\x80\xf4\x8f\xbe",
        ];
        for &bs in cases {
            let s = Str::from(bs);
            let text = s.as_text();
            assert_eq!(text.chars().filter(|&c| c == '\u{FFFD}').count(), 0);
            assert_eq!(Str::from_text(text.into_owned()).with_bytes(|out| out.to_vec()), bs);
        }
        assert!(matches!(Str::from("café").as_text(), Cow::Borrowed(_)));
    }

    #[test]
    fn non_utf8_len_and_as_str() {
        // "caf" + Latin-1 'é' (0xE9) is not valid UTF-8
        let s: Str = Buf::read_from_bytes(b"caf\xe9").into_str();
        assert_eq!(s.len(), 4);
        assert_eq!(s.as_str(), "caf\u{FFFD}");
        let ascii: Str = "hello".into();
        assert_eq!(ascii.len(), 5);
        let cjk: Str = "你好Hello".into();
        assert_eq!(cjk.len(), 7);
    }

    #[test]
    fn non_utf8_stdlib_keeps_bytes() {
        let s: Str = Buf::read_from_bytes(b" \t\xe3\x80\x80ab\xffcd \n").into_str();
        s.trim(&" ".into()).with_bytes(|bs| assert_eq!(bs, b"ab\xffcd"));
        let s: Str = Buf::read_from_bytes(b"xxab\xffxx").into_str();
        s.trim(&"x".into()).with_bytes(|bs| assert_eq!(bs, b"ab\xff"));
        let s: Str = Buf::read_from_bytes(b"ab\xffcd").into_str();
        s.truncate(3, &"...".into()).with_bytes(|bs| assert_eq!(bs, b"ab\xff"));
        s.truncate(4, &"..".into()).with_bytes(|bs| assert_eq!(bs, b"ab.."));
        s.truncate(9, &"..".into()).with_bytes(|bs| assert_eq!(bs, b"ab\xffcd"));
        let cjk: Str = "你好世界".into();
        assert_eq!(cjk.truncate(3, &"…".into()).as_str(), "你好…");
        assert_eq!(s.to_string(), "ab\u{FFFD}cd");
    }

    #[test]
    fn sub_str_counts_chars_and_keeps_bytes() {
        // 'a', 0xFF, 0xFE, 'b', 中, 文, and a truncated 中 (0xE4 0xB8): 7 chars
        let bytes: &[u8] = b"a\xff\xfeb\xe4\xb8\xad\xe6\x96\x87\xe4\xb8";
        let s: Str = Buf::read_from_bytes(bytes).into_str();
        assert_eq!(s.len(), 7);
        let chars: Vec<&[u8]> = vec![b"a", b"\xff", b"\xfe", b"b", "中".as_bytes(), "文".as_bytes(), b"\xe4\xb8"];
        let check = |l: usize, r: usize| {
            let want: Vec<u8> = chars.iter().skip(l).take(r).flat_map(|c| c.iter().copied()).collect();
            s.sub_str(l, r).with_bytes(|bs| assert_eq!(bs, &want[..], "sub_str({l}, {r})"));
        };
        // forward, backward and random access exercise the cached cursor
        for l in 0..9 {
            check(l, 1);
            check(l, 3);
        }
        for l in (0..9).rev() {
            check(l, 2);
            check(l, usize::MAX);
        }
        for &(l, r) in &[(5, 1), (2, 2), (6, 0), (0, 7), (4, 1), (1, 5)] {
            check(l, r);
        }
        let long: Str = "中文".repeat(20).into();
        assert_eq!(long.sub_str(30, 3).as_str(), "中文中");
        assert_eq!(long.sub_str(3, 2).as_str(), "文中");
        assert_eq!(long.char_at(39).as_str(), "文");
        assert_eq!(long.char_at(40).as_str(), "");
    }

    #[test]
    fn inline_basics() {
        let test_str = "hello there";
        unsafe {
            let i = Inline::from_unchecked(test_str.as_bytes());
            assert_eq!(test_str, str::from_utf8(i.bytes()).unwrap());
        }

        let s: Str = "hi there".into();
        assert_eq!(unsafe { s.rep().get_tag() }, StrTag::Inline);
        let s1 = s.slice(0, 1);
        assert_eq!(unsafe { s1.rep().get_tag() }, StrTag::Inline);
        s1.with_bytes(|bs1| assert_eq!(bs1, b"h"));
    }

    #[test]
    fn basic_behavior() {
        let base_1 = b"hi there fellow";
        let base_2 = b"how are you?";
        let base_3 = b"hi there fellowhow are you?";
        let s1 = Str::from(&base_1[..]);
        let s2 = Str::from(&base_2[..]);
        let s3 = Str::from(&base_3[..]);
        s1.with_bytes(|bs| assert_eq!(bs, base_1));
        s2.with_bytes(|bs| assert_eq!(bs, base_2, "{:?}", s2));
        s3.with_bytes(|bs| assert_eq!(bs, base_3));

        let s4 = Str::concat(s1, s2.clone());
        assert_eq!(s3, s4);
        s4.with_bytes(|bs| assert_eq!(bs, base_3));
        let s5 = Str::concat(
            Str::concat(Str::from("hi"), Str::from(" there")),
            Str::concat(
                Str::from(" "),
                Str::concat(Str::from("fel"), Str::from("low")),
            ),
        );
        s5.with_bytes(|bs| assert_eq!(bs, base_1));

        // Do this multiple times to play with the refcount.
        assert_eq!(s2.slice(1, 4), s3.slice(16, 19));
        assert_eq!(s2.slice(2, 6), s3.slice(17, 21));
    }

    fn test_str_split(pat: &Regex, base: &[u8]) {
        let s = Str::from(base);
        let want = pat
            .split(base)
            .skip_while(|x| x.is_empty())
            .collect::<Vec<_>>();
        let mut got = Vec::new();
        s.split(
            pat,
            |sub, _is_empty| {
                got.push(sub);
                1
            },
            &FieldSet::all(),
        );
        let total_got = got.len();
        let total = want.len();
        for (g, w) in got.iter().cloned().zip(want.iter().cloned()) {
            assert_eq!(g, Str::from(std::str::from_utf8(w).unwrap()));
        }
        if total_got > total {
            // We want there to be trailing empty fields in this case.
            for s in &got[total..] {
                assert_eq!(s.bytes_len(), 0);
            }
        } else {
            assert_eq!(total_got, total, "got={:?} vs want={:?}", got, want);
        }
    }

    #[test]
    fn basic_splitting() {
        let pat0 = Regex::new(",").unwrap();
        test_str_split(&pat0, b"what,is,,,up,");
        let pat = Regex::new(r#"[ \t]"#).unwrap();
        test_str_split(&pat, b"what is \t up ");
    }

    #[test]
    fn split_long_string() {
        let pat = Regex::new(r#"[ \t]"#).unwrap();
        test_str_split(
            &pat,
            crate::test_string_constants::PRIDE_PREJUDICE_CH2.as_bytes(),
        );
    }

    #[test]
    fn dynamic_string() {
        let mut d = DynamicBuf::new(0);
        writeln!(
            &mut d,
            "This is the first part of the string with formatting and everything!"
        )
            .unwrap();
        write!(&mut d, "And this is the second part").unwrap();
        let s = d.into_str();
        s.with_bytes(|bs| {
            assert_eq!(
                bs,
                br#"This is the first part of the string with formatting and everything!
And this is the second part"#
            )
        });
    }

    #[test]
    fn subst() {
        let s1: Str = "String number one".into();
        let s2: Str = "m".into();
        let re1 = Regex::new("n").unwrap();
        let (s3, n1) = s1.subst_all(&re1, &s2);
        assert_eq!(n1, 3);
        s3.with_bytes(|bs| assert_eq!(bs, b"Strimg mumber ome"));

        let re2 = Regex::new("xxyz").unwrap();
        let (s4, n2) = s3.subst_all(&re2, &s2);
        assert_eq!(n2, 0);
        assert_eq!(s3, s4);

        let empty = Str::default();
        let (s5, n3) = empty.subst_all(&re1, &s2);
        assert_eq!(n3, 0);
        assert_eq!(empty, s5);

        let s6: Str = "xxyz substituted into another xxyz".into();
        let (s7, subbed) = s6.subst_first(&re2, &s1);
        s7.with_bytes(|bs| assert_eq!(bs, b"String number one substituted into another xxyz"));
        assert!(subbed);
    }

    #[test]
    fn subst_ampersand() {
        let s1: Str = "hahbhc".into();
        let s2: Str = "ha&".into();
        let re1 = Regex::new("h.").unwrap();
        let (s3, subbed) = s1.subst_first(&re1, &s2);
        assert!(subbed);
        s3.with_bytes(|bs| assert_eq!(bs, b"hahahbhc"));
        let (s4, count) = s1.subst_all(&re1, &s2);
        s4.with_bytes(|bs| assert_eq!(bs, b"hahahahbhahc"));
        assert_eq!(count, 3);
        let s5: Str = "hz\\&".into();
        let (s6, subbed) = s1.subst_first(&re1, &s5);
        s6.with_bytes(|bs| assert_eq!(bs, b"hz&hbhc"));
        assert!(subbed);
    }

    #[test]
    fn gen_subst_basic() {
        let s1: Str = "String number one".into();
        let s2: Str = "m".into();
        let re1 = Regex::new("n").unwrap();
        let s3 = s1.gen_subst_dynamic(&re1, &s2, &"g".into());
        s3.with_bytes(|bs| assert_eq!(bs, b"Strimg mumber ome"));

        let re2 = Regex::new("xxyz").unwrap();
        let s4 = s3.gen_subst_dynamic(&re2, &s2, &"g".into());
        assert_eq!(s3, s4);

        let empty = Str::default();
        let s5 = empty.gen_subst_dynamic(&re1, &s2, &"g".into());
        assert_eq!(empty, s5);

        let s6: Str = "xxyz substituted into another xxyz".into();
        let s7 = s6.gen_subst_dynamic(&re2, &s1, &"1".into());
        s7.with_bytes(|bs| assert_eq!(bs, b"String number one substituted into another xxyz"));
    }

    #[test]
    fn gen_subst() {
        let s1: Str = "abc def".into();
        let s2: Str = "\\2 \\1 \\0".into();
        let re1 = Regex::new("(.+) (.+)").unwrap();
        let s3 = s1.gen_subst_dynamic(&re1, &s2, &"g".into());
        s3.with_bytes(|bs| assert_eq!(bs, b"def abc abc def"));
    }
}

#[cfg(all(feature = "unstable", test))]
mod bench {
    extern crate test;

    use super::*;
    use test::{black_box, Bencher};

    fn bench_max_min(b: &mut Bencher, min: i64, max: i64) {
        use rand::RngExt;
        let mut rng = rand::rng();
        let mut v = Vec::new();
        let size = 1 << 12;
        v.resize_with(size, || rng.random_range(min..=max));
        let mut i = 0;
        b.iter(|| {
            let n = unsafe { *v.get_unchecked(i) };
            i += 1;
            i &= size - 1;
            black_box(Str::from(n))
        })
    }

    #[bench]
    fn bench_itoa_small(b: &mut Bencher) {
        bench_max_min(b, -99999, 99999)
    }

    #[bench]
    fn bench_itoa_medium(b: &mut Bencher) {
        bench_max_min(b, -99999999999999, 999999999999999)
    }

    #[bench]
    fn bench_itoa_large(b: &mut Bencher) {
        bench_max_min(b, i64::min_value(), i64::max_value())
    }

    #[bench]
    fn bench_get_bytes_drop_empty(b: &mut Bencher) {
        b.iter(|| {
            let s = Str::default();
            black_box(s.get_bytes());
        });
    }

    #[bench]
    fn bench_get_bytes_drop_literal(b: &mut Bencher) {
        // Arena will align the string properly.
        use crate::arena::Arena;
        let a = Arena::default();
        let literal = a.alloc_str("this is a string that is longer than the maximum inline size");
        b.iter(|| {
            let s: Str = literal.into();
            black_box(s.get_bytes());
        });
    }

    #[bench]
    fn bench_get_bytes_drop_inline(b: &mut Bencher) {
        let literal = "AAAAAAAA";
        b.iter(|| {
            let s: Str = literal.into();
            black_box(s.get_bytes());
        });
    }

    #[bench]
    fn bench_substr_inline(b: &mut Bencher) {
        let literal = "AAAAAAAA";
        let mut i = 0;
        let len = literal.len();
        let s: Str = literal.into();
        b.iter(|| {
            i &= 7;
            black_box(s.slice(i, len));
            i += 1;
        });
    }

    #[bench]
    fn bench_substr_boxed(b: &mut Bencher) {
        // Write 4KiB of As
        let mut dbuf = DynamicBuf::new(4096);
        let bs: Vec<u8> = (0..4096).map(|_| b'A').collect();
        dbuf.write_all(&bs[..]).unwrap();
        let s = dbuf.into_str();
        let mut i = 0;
        let len = 4096;
        b.iter(|| {
            i &= 4095;
            black_box(s.slice(i, len));
            i += 1;
        });
    }
}

mod formatting {
    use super::*;
    use std::fmt::{self, Debug, Display, Formatter};

    impl<'a> Display for Str<'a> {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            self.with_bytes(|bs| write!(f, "{}", String::from_utf8_lossy(bs)))
        }
    }

    impl<'a> Debug for Str<'a> {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            unsafe {
                let rep = self.rep_mut();
                match rep.get_tag() {
                    StrTag::Inline => {
                        rep.view_as_inline(|i| write!(f, "Str(Inline({:?}))", i.bytes()))
                    }
                    StrTag::Literal => rep.view_as(|l: &Literal| write!(f, "Str({:?})", l)),
                    StrTag::Shared => rep.view_as(|s: &Shared| write!(f, "Str({:?})", s)),
                    StrTag::Concat => rep.view_as(|c: &Concat| {
                        write!(f, "Str(Concat({:?}, {:?}))", c.left(), c.right())
                    }),
                    StrTag::Boxed => rep.view_as(|b: &Boxed| write!(f, "Str({:?})", b)),
                }?
            }
            write!(f, "/[disp=<{}>]", self)
        }
    }

    impl<'a> Debug for Literal<'a> {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            write!(
                f,
                "Literal {{ len: {}, ptr: {:x}=>{:?} }}",
                self.len,
                self.ptr as usize,
                str::from_utf8(unsafe { slice::from_raw_parts(self.ptr, self.len as usize) })
                    .unwrap(),
            )
        }
    }

    impl Debug for Buf {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            let header = unsafe { &*self.0 };
            write!(
                f,
                "Buf {{ size: {}, count: {}, contents: {:?} }}",
                header.size,
                header.count.get(),
                self.as_bytes(),
            )
        }
    }
}

static CHARACTERS_AS_BYTES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `-b`/`--characters-as-bytes`, as in gawk: `length()`, `substr()`, `index()`, `match()` and
/// printf widths/precisions count bytes instead of chars.
pub fn set_characters_as_bytes(on: bool) {
    CHARACTERS_AS_BYTES.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[inline]
pub(crate) fn characters_as_bytes() -> bool {
    CHARACTERS_AS_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Where the last non-ASCII `substr` of a buffer started. Holding `_buf` keeps the buffer alive
/// and shared (so immutable), which makes `ptr`/`len` a sound identity for it.
struct SubstrCursor {
    _buf: Buf,
    ptr: *const u8,
    len: usize,
    char_ix: usize,
    byte_off: usize,
}

thread_local! {
    static SUBSTR_CURSOR: std::cell::RefCell<Option<SubstrCursor>> = const { std::cell::RefCell::new(None) };
}

/// Byte range of chars `[l, l + r)` of `bs`, clamped to its end; `None` if `l` is past the end.
/// The scan starts at char `ci`, which begins at byte `bo` (`ci <= l`). Each invalid UTF-8
/// sequence counts as one char, consistent with [`char_count`].
fn char_byte_range(bs: &[u8], ci: usize, bo: usize, l: usize, r: usize) -> Option<(usize, usize)> {
    let end = l.saturating_add(r);
    let mut idx = ci;
    let mut pos = bo;
    let mut from = None;
    loop {
        if idx == l {
            from = Some(pos);
        }
        if idx == end || pos >= bs.len() {
            return from.map(|f| (f, pos));
        }
        pos += char_width(&bs[pos..]);
        idx += 1;
    }
}

/// Byte offset of char `l`, stepping back from char `ci` at byte `bo` (`l <= ci`); `None` if that
/// crosses invalid UTF-8. Every non-continuation byte starts a char, so a valid char ending at a
/// char boundary starts at one too.
fn char_back(bs: &[u8], ci: usize, bo: usize, l: usize) -> Option<usize> {
    let mut pos = bo;
    for _ in l..ci {
        let mut start = pos.checked_sub(1)?;
        while start > 0 && pos - start < 4 && bs[start] & 0xC0 == 0x80 {
            start -= 1;
        }
        let ch = str::from_utf8(&bs[start..pos]).ok()?;
        if ch.chars().count() != 1 {
            return None;
        }
        pos = start;
    }
    Some(pos)
}

/// Byte width of the char (or invalid UTF-8 sequence, grouped as `String::from_utf8_lossy` does)
/// at the start of non-empty `bs`. Unlike `utf8_chunks`, this doesn't validate past it.
fn char_width(bs: &[u8]) -> usize {
    if bs[0] < 0x80 {
        return 1;
    }
    let window = &bs[..bs.len().min(4)];
    match str::from_utf8(window) {
        Err(e) if e.valid_up_to() == 0 => e.error_len().unwrap_or(window.len()),
        // A valid char: its width follows from the lead byte.
        _ => match bs[0] {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        },
    }
}

/// The number of chars in `bs`, counting each invalid UTF-8 sequence as one char, as `length()`
/// does. Used to turn byte offsets (of a substring or regex match) into the char positions of
/// `index()`, `match()` and `RSTART`/`RLENGTH`, which `substr()` takes.
pub(crate) fn char_count(bs: &[u8]) -> usize {
    if bs.is_ascii() || characters_as_bytes() {
        bs.len()
    } else {
        match str::from_utf8(bs) {
            Ok(text) => text.chars().count(),
            Err(_) => String::from_utf8_lossy(bs).chars().count(),
        }
    }
}
