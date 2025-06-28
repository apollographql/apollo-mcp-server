use apollo_compiler::ast::NamedType;
use std::borrow::Cow;
use std::fmt;
use std::fmt::Display;
use std::hash::Hash;

/// A path to a type in a schema, starting from a root type
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RootPath<'a> {
    pub types: Vec<Cow<'a, NamedType>>,
}

impl<'a> RootPath<'a> {
    /// Create a new root path from the given types
    pub fn new(types: Vec<&'a NamedType>) -> Self {
        Self {
            types: types.into_iter().map(Cow::Borrowed).collect(),
        }
    }

    /// Extend this path by adding a new type to the end
    pub fn extend(&self, next_type: &'a NamedType) -> Self {
        let mut types = self.types.clone();
        types.push(Cow::Borrowed(next_type));
        Self { types }
    }

    /// Determine if this path contains a cycle
    pub fn has_cycle(&self) -> bool {
        if let Some(last_type) = self.types.last() {
            self.types
                .get(0..self.types.len() - 1)
                .map(|slice| slice.contains(last_type))
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Get the type immediately referencing the leaf type
    pub fn referencing_type(&self) -> Option<&NamedType> {
        if self.types.len() > 1 {
            self.types.get(self.types.len() - 2).map(|t| t.as_ref())
        } else {
            None
        }
    }
}

impl<'a> Display for RootPath<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.types
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        )
    }
}

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
