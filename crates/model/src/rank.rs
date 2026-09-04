use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::{Node, NodeId, NodeKind, Totals};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RankFilter {
    Directories,
    Files,
    DirectoriesAndFiles,
}

impl RankFilter {
    fn includes(self, node: &Node) -> bool {
        match self {
            Self::Directories => node.kind == NodeKind::Directory,
            Self::Files => node.kind == NodeKind::File,
            Self::DirectoriesAndFiles => {
                matches!(node.kind, NodeKind::Directory | NodeKind::File)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankedNode {
    pub id: NodeId,
    pub allocated_size: u64,
    pub logical_size: u64,
}

pub fn rank_largest(
    nodes: &[Node],
    totals: &[Totals],
    filter: RankFilter,
    limit: usize,
) -> Vec<RankedNode> {
    if limit == 0 {
        return Vec::new();
    }

    let mut heap = BinaryHeap::with_capacity(limit + 1);
    for (index, node) in nodes.iter().enumerate() {
        if !filter.includes(node) {
            continue;
        }
        let allocated = if node.kind == NodeKind::Directory {
            totals[index].recursive_allocated
        } else {
            node.allocated_size
        };
        heap.push(Reverse((allocated, index as u32)));
        if heap.len() > limit {
            heap.pop();
        }
    }

    let mut ranked = heap
        .into_iter()
        .map(|Reverse((allocated_size, index))| {
            let node = &nodes[index as usize];
            RankedNode {
                id: NodeId(index),
                allocated_size,
                logical_size: if node.kind == NodeKind::Directory {
                    totals[index as usize].recursive_logical
                } else {
                    node.logical_size
                },
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|left, right| {
        right
            .allocated_size
            .cmp(&left.allocated_size)
            .then_with(|| left.id.cmp(&right.id))
    });
    ranked
}

#[cfg(test)]
mod tests {
    use crate::{rank_largest, NameRef, Node, NodeId, NodeKind, RankFilter, Totals};

    #[test]
    fn retains_only_the_largest_nodes() {
        let nodes = [5, 20, 10].map(|size| Node {
            parent: NodeId::ROOT,
            inode: 0,
            name: NameRef { offset: 0, len: 0 },
            kind: NodeKind::File,
            logical_size: size,
            allocated_size: size,
            links: 1,
            mtime: 0,
        });
        let totals = [Totals::default(); 3];
        let ranked = rank_largest(&nodes, &totals, RankFilter::Files, 2);

        assert_eq!(ranked[0].allocated_size, 20);
        assert_eq!(ranked[1].allocated_size, 10);
    }
}
