//! Defines a path from a root type in a GraphQL schema (Query, Mutation, or Subscription) to
//! another type.

use apollo_compiler::Name;
use apollo_compiler::ast::NamedType;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Display;
use std::hash::Hash;

/// Iterator over references to PathNode elements
pub struct PathNodeIter<'a> {
    current: Option<&'a PathNode>,
}

impl<'a> Iterator for PathNodeIter<'a> {
    type Item = &'a PathNode;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.current?;
        self.current = current.child.as_deref();
        Some(current)
    }
}

/// Iterator over mutable references to PathNode elements
pub struct PathNodeIterMut<'a> {
    current: Option<&'a mut PathNode>,
}

impl<'a> Iterator for PathNodeIterMut<'a> {
    type Item = &'a mut PathNode;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.current.take()?;
        let child_ptr = current
            .child
            .as_deref_mut()
            .map(|child| child as *mut PathNode);
        self.current = child_ptr.map(|ptr| unsafe { &mut *ptr });
        Some(current)
    }
}

/// Iterator over owned PathNode elements
pub struct PathNodeIntoIter {
    current: Option<PathNode>,
}

impl Iterator for PathNodeIntoIter {
    type Item = PathNode;

    fn next(&mut self) -> Option<Self::Item> {
        let mut current = self.current.take()?;
        self.current = current.child.map(|boxed| *boxed);
        current.child = None; // Remove child to avoid double ownership
        Some(current)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PathNode {
    /// The schema type of this node
    pub node_type: NamedType,

    /// The name of the field referencing the child type, if the child is a field type
    pub field_name: Option<Name>,

    /// The arguments of the field referencing the child type, if the child is a field type
    pub field_args: Vec<NamedType>,

    /// The child type
    child: Option<Box<PathNode>>,
}

impl PathNode {
    /// Create a new path containing just one type
    pub fn new(node_type: NamedType) -> Self {
        Self {
            node_type,
            field_name: None,
            field_args: Vec::default(),
            child: None,
        }
    }

    /// Add a child to the end of a path. Allows building up a path from the root down.
    pub fn add_child(
        self,
        field_name: Option<Name>,
        field_args: Vec<NamedType>,
        child_type: NamedType,
    ) -> Self {
        if let Some(child) = self.child {
            Self {
                node_type: self.node_type,
                field_name: self.field_name,
                field_args: self.field_args,
                child: Some(Box::new(
                    child.add_child(field_name, field_args, child_type),
                )),
            }
        } else {
            Self {
                node_type: self.node_type,
                field_name,
                field_args,
                child: Some(Box::new(PathNode::new(child_type))),
            }
        }
    }

    /// Add a parent to the beginning of a path. Allows building up a path from the bottom up.
    pub fn add_parent(
        self,
        field_name: Option<Name>,
        field_args: Vec<NamedType>,
        parent_type: NamedType,
    ) -> Self {
        Self {
            node_type: parent_type,
            field_name,
            field_args,
            child: Some(Box::new(self)),
        }
    }

    /// Gets the penultimate node in a path
    pub fn referencing_type(&self) -> Option<(&NamedType, Option<&Name>, Vec<&NamedType>)> {
        if let Some(child) = &self.child {
            child.referencing_type_inner(self)
        } else {
            None
        }
    }

    fn referencing_type_inner<'a>(
        &'a self,
        referencing_node: &'a PathNode,
    ) -> Option<(&'a NamedType, Option<&'a Name>, Vec<&'a NamedType>)> {
        if let Some(child) = &self.child {
            child.referencing_type_inner(self)
        } else {
            Some((
                &referencing_node.node_type,
                referencing_node.field_name.as_ref(),
                referencing_node.field_args.iter().collect(),
            ))
        }
    }

    /// Determines if a path contains a cycle
    pub(crate) fn has_cycle(&self) -> bool {
        self.has_cycle_inner(HashSet::new())
    }

    fn has_cycle_inner(&self, mut visited: HashSet<NamedType>) -> bool {
        if visited.contains(&self.node_type) {
            return true;
        }

        visited.insert(self.node_type.clone());

        if let Some(child) = &self.child {
            child.has_cycle_inner(visited)
        } else {
            false
        }
    }

    /// Gets the length of the path
    pub fn len(&self) -> usize {
        if let Some(child) = &self.child {
            child.len() + 1
        } else {
            1
        }
    }

    /// Get an iterator over references to all nodes in this path
    pub fn iter(&self) -> PathNodeIter<'_> {
        PathNodeIter {
            current: Some(self),
        }
    }

    /// Get an iterator over mutable references to all nodes in this path
    pub fn iter_mut(&mut self) -> PathNodeIterMut<'_> {
        PathNodeIterMut {
            current: Some(self),
        }
    }
}

impl<'a> IntoIterator for &'a PathNode {
    type Item = &'a PathNode;
    type IntoIter = PathNodeIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut PathNode {
    type Item = &'a mut PathNode;
    type IntoIter = PathNodeIterMut<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl IntoIterator for PathNode {
    type Item = PathNode;
    type IntoIter = PathNodeIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        PathNodeIntoIter {
            current: Some(self),
        }
    }
}

impl Display for PathNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(child) = &self.child {
            if let Some(field_name) = &self.field_name {
                if !self.field_args.is_empty() {
                    write!(
                        f,
                        "{} -> {}({}) -> {}",
                        self.node_type.as_str(),
                        field_name.as_str(),
                        self.field_args
                            .iter()
                            .map(|arg| arg.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                        child
                    )
                } else {
                    write!(
                        f,
                        "{} -> {} -> {}",
                        self.node_type.as_str(),
                        field_name.as_str(),
                        child
                    )
                }
            } else {
                write!(f, "{} -> {}", self.node_type.as_str(), child)
            }
        } else {
            write!(f, "{}", self.node_type.as_str())
        }
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

#[cfg(test)]
mod test {
    use super::*;
    use apollo_compiler::name;

    #[test]
    fn test_path() {
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        assert_eq!(path.to_string(), "Root -> child -> Child");
    }

    #[test]
    fn test_has_cycle() {
        // Test path without cycle
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        assert!(!path.has_cycle());

        // Test path with cycle (Root -> Child -> Root)
        let root_type = NamedType::new("Root").unwrap();
        let path = PathNode::new(root_type.clone());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let path = path.add_child(Some(name!("back_to_root")), vec![], root_type);
        assert!(path.has_cycle());
    }

    #[test]
    fn test_referencing_type() {
        // Test single level path
        let path = PathNode::new(NamedType::new("Root").unwrap());
        assert_eq!(path.referencing_type(), None);

        // Test two level path: Root -> child -> Child
        let root_type = NamedType::new("Root").unwrap();
        let child_type = NamedType::new("Child").unwrap();
        let path = PathNode::new(root_type.clone());
        let path = path.add_child(Some(name!("child")), vec![], child_type.clone());
        assert_eq!(
            path.referencing_type(),
            Some((&root_type, Some(&name!("child")), vec![])),
        );

        // Test three level path: Root -> child -> Child -> grandchild -> GrandChild
        let path = path.add_child(
            Some(name!("grandchild")),
            vec![],
            NamedType::new("GrandChild").unwrap(),
        );
        assert_eq!(
            path.referencing_type(),
            Some((&child_type, Some(&name!("grandchild")), vec![]))
        );
    }

    #[test]
    fn test_iteration() {
        // Test single node
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let nodes: Vec<_> = path.iter().collect();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type.as_str(), "Root");

        // Test two level path: Root -> child -> Child
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let nodes: Vec<_> = path.iter().collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node_type.as_str(), "Root");
        assert_eq!(nodes[1].node_type.as_str(), "Child");
        assert_eq!(nodes[0].field_name.as_ref().unwrap().as_str(), "child");
        assert_eq!(nodes[1].field_name, None);

        // Test three level path: Root -> child -> Child -> grandchild -> GrandChild
        let path = path.add_child(
            Some(name!("grandchild")),
            vec![],
            NamedType::new("GrandChild").unwrap(),
        );
        let nodes: Vec<_> = path.iter().collect();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].node_type.as_str(), "Root");
        assert_eq!(nodes[1].node_type.as_str(), "Child");
        assert_eq!(nodes[2].node_type.as_str(), "GrandChild");
        assert_eq!(nodes[0].field_name.as_ref().unwrap().as_str(), "child");
        assert_eq!(nodes[1].field_name.as_ref().unwrap().as_str(), "grandchild");
        assert_eq!(nodes[2].field_name, None);
    }

    #[test]
    fn test_iteration_mut() {
        // Test mutable iteration
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let path = path.add_child(
            Some(name!("grandchild")),
            vec![],
            NamedType::new("GrandChild").unwrap(),
        );

        let mut path = path;
        let nodes: Vec<_> = path.iter_mut().collect();
        assert_eq!(nodes.len(), 3);

        // Verify we can access the nodes mutably
        for node in nodes {
            assert!(!node.node_type.as_str().is_empty());
        }
    }

    #[test]
    fn test_into_iter() {
        // Test owned iteration
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let path = path.add_child(
            Some(name!("grandchild")),
            vec![],
            NamedType::new("GrandChild").unwrap(),
        );

        let nodes: Vec<_> = path.into_iter().collect();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].node_type.as_str(), "Root");
        assert_eq!(nodes[1].node_type.as_str(), "Child");
        assert_eq!(nodes[2].node_type.as_str(), "GrandChild");
    }

    #[test]
    fn test_iteration_with_into_iter() {
        // Test using IntoIterator trait
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let path = path.add_child(
            Some(name!("grandchild")),
            vec![],
            NamedType::new("GrandChild").unwrap(),
        );

        // Test reference iteration
        let nodes: Vec<_> = (&path).into_iter().collect();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].node_type.as_str(), "Root");
        assert_eq!(nodes[1].node_type.as_str(), "Child");
        assert_eq!(nodes[2].node_type.as_str(), "GrandChild");

        // Test mutable reference iteration
        let mut path = path;
        let nodes: Vec<_> = (&mut path).into_iter().collect();
        assert_eq!(nodes.len(), 3);

        // Test owned iteration
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let path = path.add_child(
            Some(name!("child")),
            vec![],
            NamedType::new("Child").unwrap(),
        );
        let nodes: Vec<_> = path.into_iter().collect();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn test_iteration_empty_path() {
        // Test iteration on a path with no children
        let path = PathNode::new(NamedType::new("Root").unwrap());
        let nodes: Vec<_> = path.iter().collect();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type.as_str(), "Root");
    }
}
