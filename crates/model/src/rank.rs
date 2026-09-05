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
    let limit = limit.min(nodes.len());
    if limit == 0 {
        return Vec::new();
    }

    let mut heap = BinaryHeap::with_capacity(limit);
    for (index, node) in nodes.iter().enumerate() {
        if !filter.includes(node) {
            continue;
        }
        let allocated = if node.kind == NodeKind::Directory {
            totals[index].recursive_allocated
        } else {
            node.allocated_size
        };
        // The root is the worst retained entry, including the tie-breaker.
        let candidate = Reverse((allocated, Reverse(index as u32)));
        if heap.len() < limit {
            heap.push(candidate);
        } else if let Some(mut worst) = heap.peek_mut() {
            if candidate < *worst {
                *worst = candidate;
            }
        }
    }

    let mut ranked = heap
        .into_iter()
        .map(|Reverse((allocated_size, Reverse(index)))| {
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
    fn ties_at_the_cutoff_follow_output_order() {
        let nodes = [Node {
            parent: NodeId::ROOT,
            inode: 0,
            name: NameRef { offset: 0, len: 0 },
            kind: NodeKind::File,
            logical_size: 10,
            allocated_size: 10,
            links: 1,
            mtime: 0,
        }; 3];
        let ranked = rank_largest(&nodes, &[], RankFilter::Files, 2);
        assert_eq!(
            ranked.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![NodeId(0), NodeId(1)]
        );
    }

    #[test]
    fn accepts_limits_larger_than_the_scan() {
        assert!(rank_largest(&[], &[], RankFilter::Files, usize::MAX).is_empty());
    }

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
        assert_eq!(
            rank_largest(&nodes, &totals, RankFilter::Files, usize::MAX).len(),
            nodes.len()
        );
    }

    #[test]
    fn bounded_ranking_matches_a_full_sort() {
        let nodes = (0..96)
            .map(|index| Node {
                parent: NodeId::ROOT,
                inode: index,
                name: NameRef { offset: 0, len: 0 },
                kind: match index % 3 {
                    0 => NodeKind::Directory,
                    1 => NodeKind::File,
                    _ => NodeKind::Symlink,
                },
                logical_size: index * 10,
                allocated_size: index * 17 % 11,
                links: 1,
                mtime: 0,
            })
            .collect::<Vec<_>>();
        let totals = (0..nodes.len())
            .map(|index| Totals {
                recursive_allocated: (index * 7 % 13) as u64,
                recursive_logical: (index * 100) as u64,
                ..Totals::default()
            })
            .collect::<Vec<_>>();
        for filter in [
            RankFilter::Files,
            RankFilter::Directories,
            RankFilter::DirectoriesAndFiles,
        ] {
            let mut expected = nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| filter.includes(node))
                .map(|(index, node)| super::RankedNode {
                    id: NodeId(index as u32),
                    allocated_size: if node.kind == NodeKind::Directory {
                        totals[index].recursive_allocated
                    } else {
                        node.allocated_size
                    },
                    logical_size: if node.kind == NodeKind::Directory {
                        totals[index].recursive_logical
                    } else {
                        node.logical_size
                    },
                })
                .collect::<Vec<_>>();
            expected.sort_by_key(|node| (std::cmp::Reverse(node.allocated_size), node.id));
            for limit in 0..=nodes.len() + 1 {
                assert_eq!(
                    rank_largest(&nodes, &totals, filter, limit),
                    expected[..limit.min(expected.len())]
                );
            }
        }
    }
}
