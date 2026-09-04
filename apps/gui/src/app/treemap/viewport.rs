use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use akimi_ext4::FilesystemScan;
use akimi_model::{NodeId, NodeKind};

use super::{build_treemap, Treemap};

const PIXEL_SCROLL_THRESHOLD: f32 = 48.0;
const MAX_CACHED_MAPS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ScrollAmount {
    Pixels(f32),
    Lines(f32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ZoomDirection {
    In,
    Out,
}

pub(crate) struct TreemapViewport {
    root: NodeId,
    map: Arc<Treemap>,
    cache: HashMap<NodeId, Arc<Treemap>>,
    cache_order: VecDeque<NodeId>,
    scroll_accumulator: f32,
}

impl TreemapViewport {
    pub(crate) fn new(root_map: Arc<Treemap>) -> Self {
        let mut cache = HashMap::new();
        cache.insert(NodeId::ROOT, root_map.clone());
        Self {
            root: NodeId::ROOT,
            map: root_map,
            cache,
            cache_order: VecDeque::from([NodeId::ROOT]),
            scroll_accumulator: 0.0,
        }
    }

    pub(crate) fn root(&self) -> NodeId {
        self.root
    }

    pub(crate) fn map(&self) -> Arc<Treemap> {
        self.map.clone()
    }

    pub(crate) fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.map.hit_test(x, y)
    }

    pub(crate) fn scroll(
        &mut self,
        scan: &FilesystemScan,
        x: f32,
        y: f32,
        amount: ScrollAmount,
    ) -> Option<NodeId> {
        match self.consume_scroll(amount)? {
            ZoomDirection::In => self.zoom_in_at(scan, x, y),
            ZoomDirection::Out => self.zoom_out(scan),
        }
    }

    pub(crate) fn zoom_in_at(&mut self, scan: &FilesystemScan, x: f32, y: f32) -> Option<NodeId> {
        let hit = self.hit_test(x, y)?;
        let target = next_directory_toward(scan, self.root, hit)?;
        self.zoom_to(scan, target)
    }

    pub(crate) fn zoom_out(&mut self, scan: &FilesystemScan) -> Option<NodeId> {
        if self.root == NodeId::ROOT {
            return None;
        }
        let parent = scan.result.arena.nodes().get(self.root.index())?.parent;
        self.zoom_to(scan, parent)
    }

    pub(crate) fn zoom_to(&mut self, scan: &FilesystemScan, target: NodeId) -> Option<NodeId> {
        let node = scan.result.arena.nodes().get(target.index())?;
        if node.kind != NodeKind::Directory || target == self.root {
            return None;
        }

        let map = if let Some(map) = self.cache.get(&target) {
            map.clone()
        } else {
            let map = Arc::new(build_treemap(scan, target));
            self.insert_cache(target, map.clone());
            map
        };
        self.touch_cache(target);
        self.root = target;
        self.map = map;
        self.scroll_accumulator = 0.0;
        Some(target)
    }

    pub(crate) fn reset(&mut self, scan: &FilesystemScan) -> Option<NodeId> {
        self.zoom_to(scan, NodeId::ROOT)
    }

    fn consume_scroll(&mut self, amount: ScrollAmount) -> Option<ZoomDirection> {
        let delta = match amount {
            ScrollAmount::Pixels(value) => value / PIXEL_SCROLL_THRESHOLD,
            ScrollAmount::Lines(value) => value,
        };
        if !delta.is_finite() || delta == 0.0 {
            return None;
        }
        if self.scroll_accumulator != 0.0 && self.scroll_accumulator.signum() != delta.signum() {
            self.scroll_accumulator = 0.0;
        }
        self.scroll_accumulator += delta;
        if self.scroll_accumulator.abs() < 1.0 {
            return None;
        }

        let direction = if self.scroll_accumulator > 0.0 {
            ZoomDirection::In
        } else {
            ZoomDirection::Out
        };
        self.scroll_accumulator = 0.0;
        Some(direction)
    }

    fn insert_cache(&mut self, id: NodeId, map: Arc<Treemap>) {
        while self.cache.len() >= MAX_CACHED_MAPS {
            let Some(position) = self
                .cache_order
                .iter()
                .position(|cached| *cached != NodeId::ROOT && *cached != self.root)
            else {
                break;
            };
            if let Some(evicted) = self.cache_order.remove(position) {
                self.cache.remove(&evicted);
            }
        }
        self.cache.insert(id, map);
        self.cache_order.push_back(id);
    }

    fn touch_cache(&mut self, id: NodeId) {
        if let Some(position) = self.cache_order.iter().position(|cached| *cached == id) {
            self.cache_order.remove(position);
        }
        self.cache_order.push_back(id);
    }
}

fn next_directory_toward(
    scan: &FilesystemScan,
    current_root: NodeId,
    hit: NodeId,
) -> Option<NodeId> {
    let nodes = scan.result.arena.nodes();
    let hit_node = nodes.get(hit.index())?;
    let mut candidate = if hit_node.kind == NodeKind::Directory {
        hit
    } else {
        hit_node.parent
    };
    if candidate == current_root {
        return None;
    }

    for _ in 0..nodes.len() {
        let node = nodes.get(candidate.index())?;
        if node.kind != NodeKind::Directory {
            return None;
        }
        if node.parent == current_root {
            return Some(candidate);
        }
        if candidate == NodeId::ROOT || node.parent == candidate {
            return None;
        }
        candidate = node.parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::build_treemap;
    use super::{ScrollAmount, TreemapViewport, ZoomDirection};
    use akimi_ext4::FilesystemScan;
    use akimi_model::{NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};
    use std::sync::Arc;

    fn nested_scan() -> FilesystemScan {
        let mut names = NameArena::with_capacity(32);
        let root = names.push(b"/").unwrap();
        let first = names.push(b"first").unwrap();
        let second = names.push(b"second").unwrap();
        let file = names.push(b"file.bin").unwrap();
        let directory = |parent, name| Node {
            parent,
            inode: 1,
            name,
            kind: NodeKind::Directory,
            logical_size: 0,
            allocated_size: 0,
            links: 1,
            mtime: 0,
        };
        let nodes = vec![
            directory(NodeId::ROOT, root),
            directory(NodeId::ROOT, first),
            directory(NodeId(1), second),
            Node {
                parent: NodeId(2),
                inode: 2,
                name: file,
                kind: NodeKind::File,
                logical_size: 1_000,
                allocated_size: 1_000,
                links: 1,
                mtime: 0,
            },
        ];
        FilesystemScan {
            result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
            stats: Default::default(),
            timings: Default::default(),
            warnings: Default::default(),
            workers: 1,
        }
    }

    fn center_of(viewport: &TreemapViewport, id: NodeId) -> (f32, f32) {
        let map = viewport.map();
        let tile = map.rect_for(id).unwrap();
        (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0)
    }

    #[test]
    fn zooms_one_directory_level_toward_the_item_under_the_pointer() {
        let scan = nested_scan();
        let map = Arc::new(build_treemap(&scan, NodeId::ROOT));
        let mut viewport = TreemapViewport::new(map);
        let (x, y) = center_of(&viewport, NodeId(3));

        assert_eq!(viewport.zoom_in_at(&scan, x, y), Some(NodeId(1)));

        let (x, y) = center_of(&viewport, NodeId(3));
        assert_eq!(viewport.zoom_in_at(&scan, x, y), Some(NodeId(2)));
    }

    #[test]
    fn zooming_out_moves_to_one_parent_at_a_time() {
        let scan = nested_scan();
        let map = Arc::new(build_treemap(&scan, NodeId::ROOT));
        let mut viewport = TreemapViewport::new(map);
        assert_eq!(viewport.zoom_to(&scan, NodeId(2)), Some(NodeId(2)));

        assert_eq!(viewport.zoom_out(&scan), Some(NodeId(1)));
        assert_eq!(viewport.zoom_out(&scan), Some(NodeId::ROOT));
        assert_eq!(viewport.zoom_out(&scan), None);
    }

    #[test]
    fn precise_scroll_accumulates_before_zooming() {
        let scan = nested_scan();
        let map = Arc::new(build_treemap(&scan, NodeId::ROOT));
        let mut viewport = TreemapViewport::new(map);
        let (x, y) = center_of(&viewport, NodeId(3));

        assert_eq!(
            viewport.scroll(&scan, x, y, ScrollAmount::Pixels(20.0)),
            None
        );
        assert_eq!(
            viewport.scroll(&scan, x, y, ScrollAmount::Pixels(28.0)),
            Some(NodeId(1))
        );
    }

    #[test]
    fn reversing_scroll_discards_the_old_partial_gesture() {
        let scan = nested_scan();
        let map = Arc::new(build_treemap(&scan, NodeId::ROOT));
        let mut viewport = TreemapViewport::new(map);
        viewport.scroll_accumulator = 0.75;

        assert_eq!(viewport.consume_scroll(ScrollAmount::Pixels(-24.0)), None);
        assert_eq!(viewport.scroll_accumulator, -0.5);
        assert_eq!(
            viewport.consume_scroll(ScrollAmount::Lines(-1.0)),
            Some(ZoomDirection::Out)
        );
    }
}
