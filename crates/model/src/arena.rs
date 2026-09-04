use std::error::Error;
use std::fmt;
use std::ops::Range;

use crate::{NameRef, Node, NodeId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArenaError {
    NameTooLong(usize),
    NamesTooLarge,
    TooManyNodes,
}

impl fmt::Display for ArenaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameTooLong(len) => write!(formatter, "name is {len} bytes; maximum is 65535"),
            Self::NamesTooLarge => formatter.write_str("name arena exceeds 4 GiB"),
            Self::TooManyNodes => formatter.write_str("node arena exceeds u32::MAX entries"),
        }
    }
}

impl Error for ArenaError {}

#[derive(Debug, Default)]
pub struct NameArena {
    bytes: Vec<u8>,
}

impl NameArena {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.bytes.reserve(additional);
    }

    pub fn push(&mut self, name: &[u8]) -> Result<NameRef, ArenaError> {
        let len = u16::try_from(name.len()).map_err(|_| ArenaError::NameTooLong(name.len()))?;
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ArenaError::NamesTooLarge)?;
        let end = self
            .bytes
            .len()
            .checked_add(name.len())
            .and_then(|value| value.checked_add(1))
            .ok_or(ArenaError::NamesTooLarge)?;
        if end > u32::MAX as usize {
            return Err(ArenaError::NamesTooLarge);
        }
        self.bytes.extend_from_slice(name);
        self.bytes.push(0);
        Ok(NameRef { offset, len })
    }

    pub fn append(&mut self, other: Self) -> Result<u32, ArenaError> {
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ArenaError::NamesTooLarge)?;
        let end = self
            .bytes
            .len()
            .checked_add(other.bytes.len())
            .ok_or(ArenaError::NamesTooLarge)?;
        if end > u32::MAX as usize {
            return Err(ArenaError::NamesTooLarge);
        }
        self.bytes.extend(other.bytes);
        Ok(offset)
    }

    pub fn get(&self, name: NameRef) -> &[u8] {
        let start = name.offset as usize;
        let end = start + name.len as usize;
        &self.bytes[start..end]
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct NodeArena {
    nodes: Vec<Node>,
    names: NameArena,
}

impl NodeArena {
    pub fn from_parts(nodes: Vec<Node>, names: NameArena) -> Self {
        debug_assert!(
            nodes
                .get(1..)
                .unwrap_or_default()
                .windows(2)
                .all(|pair| pair[0].parent <= pair[1].parent),
            "nodes must group children in ascending parent order"
        );
        Self { nodes, names }
    }

    pub fn push_node(&mut self, node: Node) -> Result<NodeId, ArenaError> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| ArenaError::TooManyNodes)?;
        self.nodes.push(node);
        Ok(NodeId(id))
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn names(&self) -> &NameArena {
        &self.names
    }

    pub fn name(&self, id: NodeId) -> &[u8] {
        self.names.get(self.nodes[id.index()].name)
    }

    pub fn child_range(&self, parent: NodeId) -> Range<usize> {
        let descendants = self.nodes.get(1..).unwrap_or_default();
        let start = descendants.partition_point(|node| node.parent < parent) + 1;
        let end = descendants.partition_point(|node| node.parent <= parent) + 1;
        start..end
    }

    pub fn path_bytes(&self, id: NodeId) -> Vec<u8> {
        if id == NodeId::ROOT {
            return vec![b'/'];
        }

        let mut components = Vec::new();
        let mut current = id;
        for _ in 0..self.nodes.len() {
            if current == NodeId::ROOT {
                break;
            }
            let node = &self.nodes[current.index()];
            components.push(node.name);
            current = node.parent;
        }

        let capacity = 1 + components
            .iter()
            .map(|name| name.len as usize + 1)
            .sum::<usize>();
        let mut path = Vec::with_capacity(capacity);
        path.push(b'/');
        for (index, name) in components.into_iter().rev().enumerate() {
            if index > 0 {
                path.push(b'/');
            }
            path.extend_from_slice(self.names.get(name));
        }
        path
    }

    pub fn display_path(&self, id: NodeId) -> String {
        String::from_utf8_lossy(&self.path_bytes(id)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use crate::{NameArena, NameRef, Node, NodeArena, NodeId, NodeKind};

    #[test]
    fn stores_names_in_one_byte_arena() {
        let mut names = NameArena::default();
        let first = names.push(b"alpha").unwrap();
        let second = names.push(b"beta").unwrap();

        assert_eq!(names.get(first), b"alpha");
        assert_eq!(names.get(second), b"beta");
        assert_eq!(names.len(), 11);
    }

    #[test]
    fn appends_name_arenas_and_returns_the_offset() {
        let mut first = NameArena::default();
        let alpha = first.push(b"alpha").unwrap();
        let mut second = NameArena::default();
        let beta = second.push(b"beta").unwrap();

        let offset = first.append(second).unwrap();
        let shifted_beta = NameRef {
            offset: beta.offset + offset,
            len: beta.len,
        };

        assert_eq!(first.get(alpha), b"alpha");
        assert_eq!(first.get(shifted_beta), b"beta");
    }

    #[test]
    fn reconstructs_paths_from_parent_ids() {
        let mut names = NameArena::default();
        let root = names.push(b"/").unwrap();
        let foo = names.push(b"foo").unwrap();
        let file = names.push(b"file.bin").unwrap();
        let nodes = vec![
            Node {
                parent: NodeId::ROOT,
                inode: 2,
                name: root,
                kind: NodeKind::Directory,
                logical_size: 0,
                allocated_size: 0,
                links: 1,
                mtime: 0,
            },
            Node {
                parent: NodeId::ROOT,
                inode: 12,
                name: foo,
                kind: NodeKind::Directory,
                logical_size: 0,
                allocated_size: 0,
                links: 1,
                mtime: 0,
            },
            Node {
                parent: NodeId(1),
                inode: 13,
                name: file,
                kind: NodeKind::File,
                logical_size: 10,
                allocated_size: 10,
                links: 1,
                mtime: 0,
            },
        ];
        let arena = NodeArena::from_parts(nodes, names);

        assert_eq!(arena.path_bytes(NodeId::ROOT), b"/");
        assert_eq!(arena.path_bytes(NodeId(2)), b"/foo/file.bin");
    }

    #[test]
    fn finds_contiguous_children_without_an_index() {
        let mut names = NameArena::default();
        let root = names.push(b"/").unwrap();
        let child = names.push(b"child").unwrap();
        let leaf = names.push(b"leaf").unwrap();
        let nodes = vec![
            Node {
                parent: NodeId::ROOT,
                inode: 2,
                name: root,
                kind: NodeKind::Directory,
                logical_size: 0,
                allocated_size: 0,
                links: 1,
                mtime: 0,
            },
            Node {
                parent: NodeId::ROOT,
                inode: 12,
                name: child,
                kind: NodeKind::Directory,
                logical_size: 0,
                allocated_size: 0,
                links: 1,
                mtime: 0,
            },
            Node {
                parent: NodeId(1),
                inode: 13,
                name: leaf,
                kind: NodeKind::File,
                logical_size: 10,
                allocated_size: 10,
                links: 1,
                mtime: 0,
            },
        ];
        let arena = NodeArena::from_parts(nodes, names);

        assert_eq!(arena.child_range(NodeId::ROOT), 1..2);
        assert_eq!(arena.child_range(NodeId(1)), 2..3);
        assert_eq!(arena.child_range(NodeId(2)), 3..3);
    }
}
