//! Trees with labelled arcs and optionally labelled nodes.
//!
//! Ports Unison `src/tree.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! OCaml builds trees through a persistent "unfinished tree" (`Tree.u`);
//! here that is [`Builder`], which keeps the path from the root as a stack.

use gemel_compat::umarshal::{self, Reader, Writer};

/// `Tree.t`: a tree with `A`-labelled arcs and `B`-labelled nodes, where
/// internal nodes need not be labelled.
#[derive(Debug, Clone, PartialEq)]
pub enum Tree<A, B> {
    /// Children, in order, and maybe a label.
    Node(Vec<(A, Tree<A, B>)>, Option<B>),
    /// A labelled leaf.
    Leaf(B),
}

impl<A, B> Tree<A, B> {
    /// `Tree.m`.
    pub fn marshal(
        &self,
        w: &mut Writer,
        ma: &impl Fn(&mut Writer, &A),
        mb: &impl Fn(&mut Writer, &B),
    ) {
        match self {
            Tree::Node(children, v) => {
                w.tag(0);
                w.list(children, |w, (a, t)| {
                    ma(w, a);
                    t.marshal(w, ma, mb);
                });
                w.option(v.as_ref(), |w, v| mb(w, v));
            }
            Tree::Leaf(v) => {
                w.tag(1);
                mb(w, v);
            }
        }
    }

    /// `Tree.m`.
    pub fn unmarshal(
        r: &mut Reader<'_>,
        ma: &impl Fn(&mut Reader<'_>) -> umarshal::Result<A>,
        mb: &impl Fn(&mut Reader<'_>) -> umarshal::Result<B>,
    ) -> umarshal::Result<Tree<A, B>> {
        Ok(match r.tag(2)? {
            0 => Tree::Node(
                r.list(|r| Ok((ma(r)?, Tree::unmarshal(r, ma, mb)?)))?,
                r.option(mb)?,
            ),
            _ => Tree::Leaf(mb(r)?),
        })
    }

    /// `Tree.is_empty`.
    pub fn is_empty(&self) -> bool {
        matches!(self, Tree::Node(children, None) if children.is_empty())
    }

    /// `Tree.map`: renames arcs and nodes.
    pub fn map<C, D>(&self, f: &impl Fn(&A) -> C, g: &impl Fn(&B) -> D) -> Tree<C, D> {
        match self {
            Tree::Node(children, v) => Tree::Node(
                children.iter().map(|(n, t)| (f(n), t.map(f, g))).collect(),
                v.as_ref().map(g),
            ),
            Tree::Leaf(v) => Tree::Leaf(g(v)),
        }
    }

    /// `Tree.iteri`: calls `f` on each labelled node, in depth-first order
    /// (a node before its children), with the path to it built by `pcons`.
    pub fn iteri<C>(&self, path: C, pcons: &impl Fn(&C, &A) -> C, f: &mut impl FnMut(&C, &B)) {
        match self {
            Tree::Node(children, v) => {
                if let Some(v) = v {
                    f(&path, v);
                }
                for (n, t) in children {
                    t.iteri(pcons(&path, n), pcons, f);
                }
            }
            Tree::Leaf(v) => f(&path, v),
        }
    }

    /// `Tree.size`: the number of labelled nodes.
    pub fn size(&self) -> usize {
        match self {
            Tree::Node(children, v) => {
                usize::from(v.is_some()) + children.iter().map(|(_, t)| t.size()).sum::<usize>()
            }
            Tree::Leaf(_) => 1,
        }
    }

    /// `Tree.flatten`: the labelled nodes with their paths, in the order of
    /// [`Tree::iteri`].
    pub fn flatten<C>(&self, path: C, pcons: &impl Fn(&C, &A) -> C) -> Vec<(C, &B)> {
        let mut result = Vec::new();
        self.flatten_into(path, pcons, &mut result);
        result
    }

    fn flatten_into<'a, C>(
        &'a self,
        path: C,
        pcons: &impl Fn(&C, &A) -> C,
        result: &mut Vec<(C, &'a B)>,
    ) {
        match self {
            Tree::Leaf(v) => result.push((path, v)),
            Tree::Node(children, v) => {
                let children_paths: Vec<C> =
                    children.iter().map(|(n, _)| pcons(&path, n)).collect();
                if let Some(v) = v {
                    result.push((path, v));
                }
                for ((_, t), p) in children.iter().zip(children_paths) {
                    t.flatten_into(p, pcons, result);
                }
            }
        }
    }
}

struct Frame<A, B> {
    arc: Option<A>,
    node: Option<B>,
    children: Vec<(A, Tree<A, B>)>,
}

impl<A, B> Frame<A, B> {
    fn new(arc: Option<A>) -> Self {
        Frame {
            arc,
            node: None,
            children: Vec::new(),
        }
    }

    /// The finished subtree, or nothing if it is empty.
    fn into_tree(self) -> Option<Tree<A, B>> {
        match (self.node, self.children.is_empty()) {
            (None, true) => None,
            (Some(v), true) => Some(Tree::Leaf(v)),
            (v, false) => Some(Tree::Node(self.children, v)),
        }
    }
}

/// `Tree.u`: a tree being built, positioned at a node.
pub struct Builder<A, B> {
    /// From the root (which has no arc) to the current node.
    frames: Vec<Frame<A, B>>,
}

impl<A, B> Default for Builder<A, B> {
    fn default() -> Self {
        Builder::start()
    }
}

impl<A, B> Builder<A, B> {
    /// `Tree.start`: an empty tree, at the root.
    pub fn start() -> Self {
        Builder {
            frames: vec![Frame::new(None)],
        }
    }

    fn top(&mut self) -> &mut Frame<A, B> {
        self.frames.last_mut().expect("the root frame")
    }

    /// `Tree.add`: labels the current node.
    pub fn add(&mut self, v: B) {
        self.top().node = Some(v);
    }

    /// `Tree.enter`: goes down a new arc labelled `n`.
    pub fn enter(&mut self, n: A) {
        self.frames.push(Frame::new(Some(n)));
    }

    /// `Tree.leave`: goes back up, keeping the subtree unless it is empty.
    ///
    /// # Panics
    ///
    /// At the root (OCaml's `Invalid_argument "Tree.leave"`).
    pub fn leave(&mut self) {
        assert!(self.frames.len() > 1, "Tree.leave");
        let mut frame = self.frames.pop().expect("checked");
        let arc = frame.arc.take().expect("below the root");
        if let Some(t) = frame.into_tree() {
            self.top().children.push((arc, t));
        }
    }

    /// `Tree.finish`: the tree.
    ///
    /// # Panics
    ///
    /// Below the root (OCaml's `Invalid_argument "Tree.finish"`).
    pub fn finish(mut self) -> Tree<A, B> {
        assert!(self.frames.len() == 1, "Tree.finish");
        let root = self.frames.pop().expect("checked");
        root.into_tree()
            .unwrap_or_else(|| Tree::Node(Vec::new(), None))
    }

    /// `Tree.slice`: the tree so far, leaving an empty tree positioned at
    /// the same place.
    pub fn slice(&mut self) -> Tree<A, B>
    where
        A: Clone,
    {
        let arcs: Vec<A> = self.frames[1..]
            .iter()
            .map(|f| f.arc.clone().expect("below the root"))
            .collect();
        let mut done = std::mem::take(self);
        while done.frames.len() > 1 {
            done.leave();
        }
        for a in arcs {
            self.enter(a);
        }
        done.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &String, n: &&str) -> String {
        format!("{p}/{n}")
    }

    #[test]
    fn building_and_traversing() {
        let mut b = Builder::start();
        b.enter("a");
        b.add(1);
        b.enter("b");
        b.add(2);
        b.leave();
        b.enter("empty");
        b.leave();
        b.leave();
        b.enter("c");
        b.add(3);
        b.leave();
        let t = b.finish();
        assert_eq!(
            t,
            Tree::Node(
                vec![
                    ("a", Tree::Node(vec![("b", Tree::Leaf(2))], Some(1))),
                    ("c", Tree::Leaf(3)),
                ],
                None
            )
        );
        assert_eq!(t.size(), 3);
        let flat: Vec<(String, i32)> = t
            .flatten(String::new(), &path)
            .into_iter()
            .map(|(p, v)| (p, *v))
            .collect();
        assert_eq!(
            flat,
            [("/a".into(), 1), ("/a/b".into(), 2), ("/c".into(), 3)]
        );
        let mut seen = Vec::new();
        t.iteri(String::new(), &path, &mut |p, v| seen.push((p.clone(), *v)));
        assert_eq!(seen, flat);
        let doubled = t.map(&|n: &&str| n.to_uppercase(), &|v| v * 2);
        assert_eq!(doubled.size(), 3);
        let bytes = umarshal::to_bytes(|w| {
            t.marshal(w, &|w, n: &&str| w.string(n.as_bytes()), &|w, v: &i32| {
                w.int(i64::from(*v));
            });
        });
        let back = umarshal::from_bytes(&bytes, |r| {
            Tree::unmarshal(r, &|r| Ok(String::from_utf8(r.string()?).unwrap()), &|r| {
                r.int()
            })
        })
        .unwrap();
        assert_eq!(back, t.map(&|n: &&str| n.to_string(), &|v| i64::from(*v)));
        assert!(Builder::<&str, i32>::start().finish().is_empty());
    }

    #[test]
    fn slicing_keeps_the_position() {
        let mut b = Builder::start();
        b.enter("a");
        b.enter("b");
        b.add(1);
        let first = b.slice();
        assert_eq!(
            first,
            Tree::Node(
                vec![("a", Tree::Node(vec![("b", Tree::Leaf(1))], None))],
                None
            )
        );
        b.add(2);
        b.leave();
        b.leave();
        assert_eq!(
            b.finish(),
            Tree::Node(
                vec![("a", Tree::Node(vec![("b", Tree::Leaf(2))], None))],
                None
            )
        );
    }
}
