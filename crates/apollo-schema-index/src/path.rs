//! Defines [`Scored`], a wrapper pairing a value with a BM25-style relevance score.

use std::fmt;
use std::fmt::Display;
use std::hash::Hash;

/// An item with a score
pub struct Scored<T: Eq + Hash + Display> {
    pub inner: T,
    score: f32,
}

impl<T: Eq + Hash + Display> Scored<T> {
    /// Create a new scored item
    pub fn new(inner: T, score: f32) -> Self {
        Self { inner, score }
    }

    /// Get the score associated with this item
    pub fn score(&self) -> f32 {
        self.score
    }
}

impl<T: Eq + Hash + Display> PartialEq for Scored<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner && self.score() == other.score()
    }
}

impl<T: Eq + Hash + Display> Eq for Scored<T> {}

impl<T: Eq + Hash + Display> PartialOrd for Scored<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Eq + Hash + Display> Ord for Scored<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score().total_cmp(&other.score())
    }
}

impl<T: Eq + Hash + Display> Hash for Scored<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl<T: Eq + Hash + Display> Display for Scored<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.inner, self.score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn new_exposes_inner_and_score() {
        let s = Scored::new("op", 1.5);
        assert_eq!(s.inner, "op");
        assert_eq!(s.score(), 1.5);
    }

    #[test]
    fn eq_requires_same_inner_and_score() {
        // Scored has no Debug impl, so use ==/!= directly rather than assert_eq!.
        assert!(Scored::new("a", 1.0) == Scored::new("a", 1.0));
        assert!(Scored::new("a", 1.0) != Scored::new("b", 1.0)); // different inner
        assert!(Scored::new("a", 1.0) != Scored::new("a", 2.0)); // different score
    }

    #[test]
    fn ord_and_partial_ord_rank_by_score() {
        // partial_cmp / cmp are used by comparison operators and sort.
        assert!(Scored::new("a", 0.1) < Scored::new("b", 0.2));
        let mut v = [
            Scored::new("low", 0.1),
            Scored::new("high", 0.9),
            Scored::new("mid", 0.5),
        ];
        v.sort();
        let order: Vec<_> = v.iter().map(|s| s.inner).collect();
        assert_eq!(order, ["low", "mid", "high"]);
    }

    #[test]
    fn hash_dedups_equal_items() {
        let mut set = HashSet::new();
        assert!(set.insert(Scored::new("x", 1.0)));
        // Same inner + score hashes and compares equal -> not re-inserted.
        assert!(!set.insert(Scored::new("x", 1.0)));
    }

    #[test]
    fn display_shows_inner_and_score() {
        assert_eq!(format!("{}", Scored::new(42, 1.5)), "42 (1.5)");
    }
}
