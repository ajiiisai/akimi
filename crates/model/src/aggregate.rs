use std::error::Error;
use std::fmt;

use crate::{Node, NodeId, NodeKind};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Totals {
    pub self_allocated: u64,
    pub recursive_allocated: u64,
    pub recursive_logical: u64,
    pub recursive_items: u64,
    /// Number of regular files in this subtree.
    pub recursive_files: u64,
    /// Number of directories strictly below this node (excludes the node itself).
    pub recursive_subdirs: u64,
    /// Newest modification time anywhere in this subtree (Unix seconds).
    pub latest_mtime: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregateError {
    InvalidParent { node: NodeId, parent: NodeId },
    ParentCycle,
}

impl fmt::Display for AggregateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParent { node, parent } => {
                write!(formatter, "node {} has invalid parent {}", node.0, parent.0)
            }
            Self::ParentCycle => formatter.write_str("node parent relationships contain a cycle"),
        }
    }
}

impl Error for AggregateError {}

pub fn aggregate(nodes: &[Node]) -> Result<Vec<Totals>, AggregateError> {
    let mut child_counts = vec![0_u32; nodes.len()];
    let mut totals = nodes
        .iter()
        .map(|node| Totals {
            self_allocated: node.allocated_size,
            recursive_allocated: node.allocated_size,
            recursive_logical: node.logical_size,
            recursive_items: 1,
            recursive_files: (node.kind == NodeKind::File) as u64,
            recursive_subdirs: 0,
            latest_mtime: node.mtime,
        })
        .collect::<Vec<_>>();

    for (index, node) in nodes.iter().enumerate().skip(1) {
        let parent = node.parent.index();
        if parent >= nodes.len() {
            return Err(AggregateError::InvalidParent {
                node: NodeId(index as u32),
                parent: node.parent,
            });
        }
        child_counts[parent] += 1;
    }

    let mut leaves = child_counts
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect::<Vec<_>>();
    let mut processed = 0_usize;

    // Order is irrelevant here: totals are commutative sums plus a max, so a
    // LIFO stack gives better cache behavior than a FIFO queue with no change
    // to the results.
    while let Some(child) = leaves.pop() {
        processed += 1;
        let parent = nodes[child].parent.index();
        totals[parent].recursive_allocated = totals[parent]
            .recursive_allocated
            .saturating_add(totals[child].recursive_allocated);
        totals[parent].recursive_logical = totals[parent]
            .recursive_logical
            .saturating_add(totals[child].recursive_logical);
        totals[parent].recursive_items = totals[parent]
            .recursive_items
            .saturating_add(totals[child].recursive_items);
        totals[parent].recursive_files = totals[parent]
            .recursive_files
            .saturating_add(totals[child].recursive_files);
        totals[parent].recursive_subdirs = totals[parent]
            .recursive_subdirs
            .saturating_add(totals[child].recursive_subdirs)
            .saturating_add((nodes[child].kind == NodeKind::Directory) as u64);
        totals[parent].latest_mtime = totals[parent].latest_mtime.max(totals[child].latest_mtime);
        child_counts[parent] -= 1;
        if parent != NodeId::ROOT.index() && child_counts[parent] == 0 {
            leaves.push(parent);
        }
    }

    if !nodes.is_empty() && processed != nodes.len() - 1 {
        return Err(AggregateError::ParentCycle);
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use crate::{aggregate, NameRef, Node, NodeId, NodeKind};

    fn node(parent: u32, allocated: u64, logical: u64) -> Node {
        Node {
            parent: NodeId(parent),
            inode: 0,
            name: NameRef { offset: 0, len: 0 },
            kind: NodeKind::Directory,
            logical_size: logical,
            allocated_size: allocated,
            links: 1,
            mtime: 0,
        }
    }

    #[test]
    fn aggregates_nodes_without_requiring_tree_order() {
        let nodes = vec![node(0, 4, 4), node(2, 8, 10), node(0, 2, 2)];
        let totals = aggregate(&nodes).unwrap();

        assert_eq!(totals[0].recursive_allocated, 14);
        assert_eq!(totals[0].recursive_logical, 16);
        assert_eq!(totals[2].recursive_allocated, 10);
        assert_eq!(totals[0].recursive_items, 3);
    }

    #[test]
    fn rejects_parent_cycles() {
        let nodes = vec![node(0, 0, 0), node(2, 0, 0), node(1, 0, 0)];
        assert!(aggregate(&nodes).is_err());
    }
}
