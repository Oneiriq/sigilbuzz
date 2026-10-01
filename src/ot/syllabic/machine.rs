//! A longest-match scanner over category bytes, which does what the
//! Ragel scanners (`|* ... *|`) of HarfBuzz's syllable machines do: at
//! each position it takes the longest run of characters one of the
//! rules matches, the earliest rule winning a tie, and a character no
//! rule starts with becomes a syllable of its own.
//!
//! The rules are regular expressions over categories ([`Pat`]),
//! compiled to a Thompson NFA and simulated one character at a time,
//! so a scan costs time linear in the characters it reads. A scan can
//! read past the syllable it returns while some rule might still
//! match, which on a long run of one category (joiners waiting for a
//! sign that never comes) would make every syllable in the run rescan
//! it. When a step over a character leaves the set of live states as
//! it was and the next character has the same category, the rest of
//! that run leaves it unchanged too, so the scan jumps to the run's
//! end. That keeps the whole scan linear.

use alloc::boxed::Box;
use alloc::vec::Vec;

/// A pattern over category bytes. Categories must be below 64.
#[derive(Debug, Clone)]
pub(crate) enum Pat {
    /// One character whose category bit is set.
    Set(u64),
    /// The patterns one after another.
    Seq(Vec<Pat>),
    /// Any one of the patterns.
    Alt(Vec<Pat>),
    /// The pattern or nothing.
    Opt(Box<Pat>),
    /// The pattern repeated zero or more times.
    Star(Box<Pat>),
}

/// One character of any of `cats`.
pub(crate) fn one(cats: &[u8]) -> Pat {
    Pat::Set(cats.iter().fold(0u64, |set, &c| set | 1u64 << (c & 63)))
}

/// The patterns in sequence.
pub(crate) fn seq<const N: usize>(items: [Pat; N]) -> Pat {
    Pat::Seq(items.into())
}

/// Any one of the patterns.
pub(crate) fn alt<const N: usize>(items: [Pat; N]) -> Pat {
    Pat::Alt(items.into())
}

/// The pattern or nothing.
pub(crate) fn opt(p: Pat) -> Pat {
    Pat::Opt(Box::new(p))
}

/// The pattern zero or more times.
pub(crate) fn star(p: Pat) -> Pat {
    Pat::Star(Box::new(p))
}

/// One NFA node.
#[derive(Debug, Clone, Copy)]
enum Node {
    /// Consumes a character whose category bit is in `set`.
    Char { set: u64, next: u32 },
    /// Continues at both.
    Split(u32, u32),
    /// Rule `n` matched.
    Accept(u8),
    /// Placeholder while a loop is compiled.
    Hole,
}

/// One syllable of a scan: the characters `start..end`, of type `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Syllable {
    /// First character.
    pub(crate) start: usize,
    /// One past the last character.
    pub(crate) end: usize,
    /// The syllable type: the kind of the rule that matched it, or the
    /// scanner's `other` kind.
    pub(crate) kind: u8,
}

/// A compiled set of rules.
#[derive(Debug)]
pub(crate) struct Machine {
    nodes: Vec<Node>,
    start: u32,
    kinds: Vec<u8>,
    other: u8,
}

impl Machine {
    /// Compiles `rules`, each a pattern and the syllable type it
    /// yields, in priority order. `other` is the type of a character
    /// no rule matches.
    pub(crate) fn new(rules: Vec<(Pat, u8)>, other: u8) -> Self {
        let mut m = Self {
            nodes: Vec::new(),
            start: 0,
            kinds: Vec::with_capacity(rules.len()),
            other,
        };
        let mut entries = Vec::with_capacity(rules.len());
        for (index, (pat, kind)) in rules.iter().enumerate() {
            m.kinds.push(*kind);
            let accept = m.push(Node::Accept(index as u8));
            entries.push(m.compile(pat, accept));
        }
        let mut start = entries.pop().unwrap_or(0);
        while let Some(e) = entries.pop() {
            start = m.push(Node::Split(e, start));
        }
        m.start = start;
        m
    }

    fn push(&mut self, node: Node) -> u32 {
        self.nodes.push(node);
        (self.nodes.len() - 1) as u32
    }

    /// Compiles `pat` to continue at `next` and returns its entry node.
    fn compile(&mut self, pat: &Pat, next: u32) -> u32 {
        match pat {
            Pat::Set(set) => self.push(Node::Char { set: *set, next }),
            Pat::Seq(items) => items
                .iter()
                .rev()
                .fold(next, |next, item| self.compile(item, next)),
            Pat::Alt(items) => {
                let mut entries: Vec<u32> = items.iter().map(|i| self.compile(i, next)).collect();
                let mut entry = entries.pop().unwrap_or(next);
                while let Some(e) = entries.pop() {
                    entry = self.push(Node::Split(e, entry));
                }
                entry
            }
            Pat::Opt(inner) => {
                let e = self.compile(inner, next);
                self.push(Node::Split(e, next))
            }
            Pat::Star(inner) => {
                let hole = self.push(Node::Hole);
                let e = self.compile(inner, hole);
                if let Some(slot) = self.nodes.get_mut(hole as usize) {
                    *slot = Node::Split(e, next);
                }
                hole
            }
        }
    }

    /// Splits `cats` into syllables.
    pub(crate) fn scan(&self, cats: &[u8]) -> Vec<Syllable> {
        let n = cats.len();
        // run_end[i]: one past the last character of the run of equal
        // categories that holds `i`.
        let mut run_end = alloc::vec![0usize; n];
        for i in (0..n).rev() {
            run_end[i] = if i + 1 < n && cats[i + 1] == cats[i] {
                run_end[i + 1]
            } else {
                i + 1
            };
        }
        let mut sim = Sim::new(self.nodes.len());
        let mut out = Vec::new();
        let mut p = 0;
        while p < n {
            let (end, kind) = match self.longest(cats, p, &run_end, &mut sim) {
                Some((end, rule)) if end > p => (
                    end,
                    self.kinds.get(rule as usize).copied().unwrap_or(self.other),
                ),
                _ => (p + 1, self.other),
            };
            out.push(Syllable {
                start: p,
                end,
                kind,
            });
            p = end;
        }
        out
    }

    /// The longest match at `p`: its end and rule.
    fn longest(
        &self,
        cats: &[u8],
        p: usize,
        run_end: &[usize],
        sim: &mut Sim,
    ) -> Option<(usize, u8)> {
        sim.cur.clear();
        sim.gen += 1;
        let gen = sim.gen;
        self.close(self.start, &mut sim.cur, &mut sim.mark, gen, &mut sim.stack);
        sim.cur.sort_unstable();
        let mut best = None;
        let mut q = p;
        while q < cats.len() && !sim.cur.is_empty() {
            let c = u32::from(cats[q] & 63);
            sim.gen += 1;
            let gen = sim.gen;
            sim.next.clear();
            for &s in &sim.cur {
                if let Some(&Node::Char { set, next }) = self.nodes.get(s as usize) {
                    if set >> c & 1 != 0 {
                        self.close(next, &mut sim.next, &mut sim.mark, gen, &mut sim.stack);
                    }
                }
            }
            sim.next.sort_unstable();
            q += 1;
            // Same states and the same category ahead: the rest of the
            // run changes nothing, so skip to its end.
            if sim.next == sim.cur && q < cats.len() && cats[q] == cats[q - 1] {
                q = run_end[q];
            }
            if let Some(rule) = self.accepting(&sim.next) {
                best = Some((q, rule));
            }
            core::mem::swap(&mut sim.cur, &mut sim.next);
        }
        best
    }

    /// The highest-priority rule accepting in `states`.
    fn accepting(&self, states: &[u32]) -> Option<u8> {
        states
            .iter()
            .filter_map(|&s| match self.nodes.get(s as usize) {
                Some(&Node::Accept(rule)) => Some(rule),
                _ => None,
            })
            .min()
    }

    /// Adds the states `s` reaches without reading a character to
    /// `out`: its character-reading and accepting nodes.
    fn close(&self, s: u32, out: &mut Vec<u32>, mark: &mut [u32], gen: u32, stack: &mut Vec<u32>) {
        stack.clear();
        stack.push(s);
        while let Some(s) = stack.pop() {
            let Some(m) = mark.get_mut(s as usize) else {
                continue;
            };
            if *m == gen {
                continue;
            }
            *m = gen;
            match self.nodes.get(s as usize) {
                Some(Node::Split(a, b)) => {
                    stack.push(*b);
                    stack.push(*a);
                }
                Some(Node::Char { .. } | Node::Accept(_)) => out.push(s),
                Some(Node::Hole) | None => {}
            }
        }
    }
}

/// Scratch space for a scan.
struct Sim {
    cur: Vec<u32>,
    next: Vec<u32>,
    mark: Vec<u32>,
    stack: Vec<u32>,
    gen: u32,
}

impl Sim {
    fn new(nodes: usize) -> Self {
        Self {
            cur: Vec::new(),
            next: Vec::new(),
            mark: alloc::vec![0; nodes],
            stack: Vec::new(),
            gen: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u8 = 1;
    const B: u8 = 2;
    const J: u8 = 3;
    const X: u8 = 4;

    fn kinds(m: &Machine, cats: &[u8]) -> Vec<(usize, usize, u8)> {
        m.scan(cats)
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    }

    #[test]
    fn longest_match_wins_and_ties_go_to_the_first_rule() {
        // Rule 0 is A B?, rule 1 is A B B, and rule 2 is A B.
        let m = Machine::new(
            alloc::vec![
                (seq([one(&[A]), opt(one(&[B]))]), 10),
                (seq([one(&[A]), one(&[B]), one(&[B])]), 11),
                (seq([one(&[A]), one(&[B])]), 12),
            ],
            99,
        );
        assert_eq!(kinds(&m, &[A, B, B, A, B]), [(0, 3, 11), (3, 5, 10)]);
        assert_eq!(kinds(&m, &[B, A]), [(0, 1, 99), (1, 2, 10)]);
    }

    #[test]
    fn empty_matches_do_not_count() {
        let m = Machine::new(alloc::vec![(star(one(&[A])), 1)], 9);
        assert_eq!(kinds(&m, &[X, A, A]), [(0, 1, 9), (1, 3, 1)]);
    }

    #[test]
    fn a_long_joiner_run_scans_in_linear_time() {
        // A (J* X)* and J* X: every J starts an `other` syllable, and
        // each of those scans would read to the end of the run without
        // the skip.
        let joined_x = || seq([star(one(&[J])), one(&[X])]);
        let m = Machine::new(
            alloc::vec![(seq([one(&[A]), star(joined_x())]), 1), (joined_x(), 2),],
            9,
        );
        let mut cats = alloc::vec![A];
        cats.extend(core::iter::repeat(J).take(200_000));
        cats.push(B);
        let out = m.scan(&cats);
        assert_eq!(out.len(), 200_002);
        assert_eq!(
            out[0],
            Syllable {
                start: 0,
                end: 1,
                kind: 1
            }
        );
        assert!(out[1..].iter().all(|s| s.kind == 9 && s.end == s.start + 1));
        // A joiner run that does end in X joins the syllable.
        assert_eq!(kinds(&m, &[A, J, J, J, X, A]), [(0, 5, 1), (5, 6, 1)]);
        assert_eq!(kinds(&m, &[J, J, X, B]), [(0, 3, 2), (3, 4, 9)]);
    }
}
