use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use akimi_model::{FilesystemScan, NodeId, NodeKind};

const MAX_VISIBLE_ROWS: usize = 50_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub(crate) id: NodeId,
    pub(crate) depth: usize,
    pub(crate) expanded: bool,
    pub(crate) expandable: bool,
}

pub(crate) struct TreeModel {
    expanded: HashSet<NodeId>,
    rows: Vec<TreeRow>,
    row_index: HashMap<NodeId, usize>,
    sorted_children: HashMap<NodeId, Arc<[NodeId]>>,
}

impl TreeModel {
    pub(crate) fn new(scan: &FilesystemScan) -> Self {
        let mut model = Self {
            expanded: HashSet::from([NodeId::ROOT]),
            rows: Vec::new(),
            row_index: HashMap::new(),
            sorted_children: HashMap::new(),
        };
        model.rebuild(scan);
        model
    }

    pub(crate) fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    pub(crate) fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub(crate) fn toggle(&mut self, scan: &FilesystemScan, id: NodeId) -> bool {
        let arena = &scan.result.arena;
        if arena.nodes()[id.index()].kind != NodeKind::Directory || arena.child_range(id).is_empty()
        {
            return false;
        }

        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild(scan);
        true
    }

    pub(crate) fn reveal(&mut self, scan: &FilesystemScan, id: NodeId) -> Option<usize> {
        let mut changed = false;
        for ancestor in ancestor_chain(scan, id) {
            if ancestor != id {
                changed |= self.expanded.insert(ancestor);
            }
        }
        if changed {
            self.rebuild(scan);
        }
        self.row_index.get(&id).copied()
    }

    pub(crate) fn collapse_all(&mut self, scan: &FilesystemScan) {
        self.expanded.clear();
        self.expanded.insert(NodeId::ROOT);
        self.rebuild(scan);
    }

    fn rebuild(&mut self, scan: &FilesystemScan) {
        self.rows.clear();
        let arena = &scan.result.arena;
        // Each frame holds the next sibling to visit. Memory grows with depth,
        // and the row limit stops the walk even in very wide directories.
        let mut stack: Vec<(Arc<[NodeId]>, usize)> = Vec::new();
        if !arena.nodes().is_empty() {
            stack.push((Arc::from([NodeId::ROOT]), 0));
        }
        while self.rows.len() < MAX_VISIBLE_ROWS {
            let depth = stack.len().saturating_sub(1);
            let Some((siblings, position)) = stack.last_mut() else {
                break;
            };
            let Some(&id) = siblings.get(*position) else {
                stack.pop();
                continue;
            };
            *position += 1;
            let expandable = arena.nodes()[id.index()].kind == NodeKind::Directory
                && !arena.child_range(id).is_empty();
            let expanded = expandable && self.expanded.contains(&id);
            self.rows.push(TreeRow {
                id,
                depth,
                expanded,
                expandable,
            });
            if expanded && self.rows.len() < MAX_VISIBLE_ROWS {
                stack.push((sorted_children(scan, &mut self.sorted_children, id), 0));
            }
        }
        self.row_index.clear();
        self.row_index.extend(
            self.rows
                .iter()
                .enumerate()
                .map(|(position, row)| (row.id, position)),
        );
    }
}

pub(crate) fn ancestor_chain(scan: &FilesystemScan, mut id: NodeId) -> Vec<NodeId> {
    let nodes = scan.result.arena.nodes();
    let mut chain = vec![id];

    // Bound the walk so damaged parent links cannot hang the UI.
    for _ in 0..nodes.len().saturating_add(1) {
        if id == NodeId::ROOT {
            break;
        }
        let parent = nodes
            .get(id.index())
            .map(|node| node.parent)
            .unwrap_or(NodeId::ROOT);
        if parent == id {
            break;
        }
        id = parent;
        chain.push(id);
    }

    chain.reverse();
    if chain.first() != Some(&NodeId::ROOT) {
        chain.insert(0, NodeId::ROOT);
    }
    chain
}

fn sorted_children(
    scan: &FilesystemScan,
    cache: &mut HashMap<NodeId, Arc<[NodeId]>>,
    parent: NodeId,
) -> Arc<[NodeId]> {
    if let Some(children) = cache.get(&parent) {
        return children.clone();
    }

    let mut children = scan
        .result
        .arena
        .child_range(parent)
        .map(|index| NodeId(index as u32))
        .collect::<Vec<_>>();
    children.sort_unstable_by(|left, right| {
        scan.result.totals[right.index()]
            .recursive_allocated
            .cmp(&scan.result.totals[left.index()].recursive_allocated)
            .then_with(|| {
                scan.result
                    .arena
                    .name(*left)
                    .cmp(scan.result.arena.name(*right))
            })
    });
    let children: Arc<[NodeId]> = Arc::from(children);
    cache.insert(parent, children.clone());
    children
}

#[cfg(test)]
mod tests {
    use super::{TreeModel, MAX_VISIBLE_ROWS};
    use akimi_model::{FilesystemScan, NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};

    #[test]
    fn walks_deep_trees_without_recursion() {
        let mut names = NameArena::default();
        let name = names.push(b"dir").unwrap();
        let nodes = (0..20_000_u32)
            .map(|id| node(NodeId(id.saturating_sub(1)), name, NodeKind::Directory, 1))
            .collect();
        let scan = scan(nodes, names);
        let mut model = TreeModel::new(&scan);
        assert_eq!(model.reveal(&scan, NodeId(19_999)), Some(19_999));
        assert_eq!(model.rows().len(), 20_000);
        assert_eq!(model.rows().last().unwrap().depth, 19_999);
        model.collapse_all(&scan);
        assert_eq!(model.rows().len(), 2);
    }

    #[test]
    fn limits_wide_trees_and_handles_empty_scans() {
        let empty = scan(Vec::new(), NameArena::default());
        assert!(TreeModel::new(&empty).rows().is_empty());

        let mut names = NameArena::default();
        let name = names.push(b"entry").unwrap();
        let mut nodes = vec![node(NodeId::ROOT, name, NodeKind::Directory, 0)];
        nodes.extend(
            (0..MAX_VISIBLE_ROWS + 10)
                .map(|size| node(NodeId::ROOT, name, NodeKind::File, size as u64)),
        );
        let scan = scan(nodes, names);
        let model = TreeModel::new(&scan);
        assert_eq!(model.rows().len(), MAX_VISIBLE_ROWS);
        assert_eq!(model.rows()[1].id.index(), MAX_VISIBLE_ROWS + 10);
    }

    #[test]
    fn expanded_rows_are_nested_and_sorted_by_allocated_size() {
        let scan = scan_with_nested_directory();
        let mut model = TreeModel::new(&scan);
        assert!(model.toggle(&scan, NodeId(2)));
        let rows = model.rows();
        let visible = rows
            .iter()
            .map(|row| (row.id, row.depth))
            .collect::<Vec<_>>();

        assert_eq!(
            visible,
            vec![
                (NodeId::ROOT, 0),
                (NodeId(2), 1),
                (NodeId(3), 2),
                (NodeId(1), 1),
            ]
        );
        assert!(rows[0].expanded);
        assert!(rows[1].expanded);
        assert!(!rows[2].expandable);
    }

    fn scan_with_nested_directory() -> FilesystemScan {
        let mut names = NameArena::with_capacity(32);
        let root = names.push(b"/").unwrap();
        let small = names.push(b"small.bin").unwrap();
        let directory = names.push(b"large").unwrap();
        let large = names.push(b"large.bin").unwrap();
        let nodes = vec![
            node(NodeId::ROOT, root, NodeKind::Directory, 0),
            node(NodeId::ROOT, small, NodeKind::File, 10),
            node(NodeId::ROOT, directory, NodeKind::Directory, 0),
            node(NodeId(2), large, NodeKind::File, 100),
        ];

        scan(nodes, names)
    }

    fn scan(nodes: Vec<Node>, names: NameArena) -> FilesystemScan {
        FilesystemScan {
            result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
            stats: Default::default(),
            timings: Default::default(),
            warnings: Default::default(),
            workers: 1,
        }
    }

    fn node(parent: NodeId, name: akimi_model::NameRef, kind: NodeKind, size: u64) -> Node {
        Node {
            parent,
            inode: 1,
            name,
            kind,
            logical_size: size,
            allocated_size: size,
            links: 1,
            mtime: 0,
        }
    }
}
