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
