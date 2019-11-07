//! This file contains common type definitions and utilities used in other parts of the project.
use hashbrown::HashSet;
use std::hash::Hash;
pub(crate) type NumTy = u32;
pub(crate) type NodeIx = petgraph::graph::NodeIndex<NumTy>;
pub(crate) type Graph<V, E> = petgraph::Graph<V, E, petgraph::Directed, NumTy>;
pub(crate) type Result<T> = std::result::Result<T, CompileError>;
#[derive(Clone, Debug)]
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

macro_rules! for_either {
    ($e:expr, |$id:ident| $body:expr) => {{
        match $e {
            crate::common::Either::Left($id) => $body,
            crate::common::Either::Right($id) => $body,
        }
    }};
}

impl<L, R, T> Iterator for Either<L, R>
where
    L: Iterator<Item = T>,
    R: Iterator<Item = T>,
{
    type Item = T;
    fn next(&mut self) -> Option<T> {
        for_either!(self, |x| x.next())
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        for_either!(self, |x| x.size_hint())
    }
    fn count(self) -> usize {
        for_either!(self, |x| x.count())
    }
}

pub(crate) struct IntoIter<L, R>(pub Either<L, R>);

impl<L, R, T> IntoIterator for IntoIter<L, R>
where
    L: IntoIterator<Item = T>,
    R: IntoIterator<Item = T>,
{
    type Item = T;
    type IntoIter = Either<L::IntoIter, R::IntoIter>;
    fn into_iter(self) -> Self::IntoIter {
        match self.0 {
            Either::Left(l) => Either::Left(l.into_iter()),
            Either::Right(r) => Either::Right(r.into_iter()),
        }
    }
}
impl<L, R, T> Either<L, R>
where
    L: IntoIterator<Item = T>,
    R: IntoIterator<Item = T>,
{
    pub(crate) fn into_iter(self) -> impl Iterator<Item = T> {
        IntoIter(self).into_iter()
    }
}

#[derive(Debug)]
pub(crate) struct CompileError(pub String);

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! err {
    ($($t:expr),*) => { Err($crate::common::CompileError(format!($($t),*))) }
}

macro_rules! static_map {
    ($name:ident<$kty:ty, $vty:ty>, $([$k:expr, $v:expr]),*) => {
        $crate::lazy_static::lazy_static! {
            pub(crate) static ref $name: hashbrown::HashMap<$kty,$vty> = {
                let mut m = hashbrown::HashMap::new();
                $(
                    m.insert($k, $v);
                )*
                m
            };
        }
    }
}

pub(crate) struct WorkList<T> {
    set: HashSet<T>,
    mem: Vec<T>,
}

impl<T: Hash + Eq> Default for WorkList<T> {
    fn default() -> WorkList<T> {
        WorkList {
            set: Default::default(),
            mem: Default::default(),
        }
    }
}

impl<T: Clone + Hash + Eq> WorkList<T> {
    pub(crate) fn insert(&mut self, t: T) {
        if self.set.insert(t.clone()) {
            self.mem.push(t)
        }
    }
    pub(crate) fn extend(&mut self, ts: impl Iterator<Item = T>) {
        for t in ts {
            self.insert(t);
        }
    }
    pub(crate) fn pop(&mut self) -> Option<T> {
        let next = self.mem.pop()?;
        let _was_there = self.set.remove(&next);
        debug_assert!(_was_there);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_elems<T: Clone + Hash + Eq>(wl: &mut WorkList<T>) -> HashSet<T> {
        let mut res = HashSet::default();
        while let Some(e) = wl.pop() {
            assert!(res.insert(e));
        }
        res
    }

    #[test]
    fn worklist_elems() {
        let mut wl = WorkList::<i32>::default();
        for i in 0..10 {
            wl.insert(i);
        }
        assert_eq!(get_elems(&mut wl), (0i32..10).collect());
    }

    #[test]
    fn worklist_idempotent() {
        let mut wl = WorkList::<i32>::default();
        wl.extend(0..10);
        for i in 0..10 {
            wl.insert(i);
        }
        assert_eq!(get_elems(&mut wl), (0i32..10).collect());
    }
}