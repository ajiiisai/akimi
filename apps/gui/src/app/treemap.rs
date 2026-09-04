use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use akimi_ext4::FilesystemScan;
use akimi_model::{NodeId, NodeKind};

mod viewport;

pub(crate) use viewport::{ScrollAmount, TreemapViewport};

const TYPE_COLORS: [u32; 11] = [
    0x3f474e, 0x59636b, 0xce62d5, 0x8f70d5, 0x32ad9f, 0xc88a38, 0x5c86cf, 0x78a64d, 0xd05c5c,
    0x7d8993, 0xc5a83e,
];
const DIR_COLOR: usize = 0;
const REST_COLOR: usize = 1;
const IMAGE_COLOR: usize = 2;
const VIDEO_COLOR: usize = 3;
const AUDIO_COLOR: usize = 4;
const ARCHIVE_COLOR: usize = 5;
const DOC_COLOR: usize = 6;
const CODE_COLOR: usize = 7;
const EXEC_COLOR: usize = 8;
const UNKNOWN_COLOR: usize = 9;
const LINK_COLOR: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

impl Rect {
    fn area(self) -> f32 {
        self.w * self.h
    }

    fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

struct Tile {
    id: NodeId,
    rect: Rect,
}

/// Squarified treemap layout (Bruls, Huizing & van Wijk, 2000). Rows grow
/// across the rectangle's short side while the worst aspect ratio improves.
///
/// `items` must be sorted largest first. `total` is the sum of *every* size the
/// rectangle stands for, including any tail the caller trimmed off as too small
/// to draw; that keeps the visible tiles' proportions truthful. Layout stops as
/// soon as the leftover rectangle is thinner than `min_side`.
fn squarify(
    items: &[(NodeId, u64)],
    mut rect: Rect,
    total: u64,
    min_side: f32,
    out: &mut Vec<Tile>,
) {
    if items.is_empty() || total == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    // Pixels per byte. Constant for the whole call: every row consumes exactly
    // the area of the sizes it holds, so the leftover rectangle always matches
    // the leftover mass.
    let scale = (rect.w as f64 * rect.h as f64) / total as f64;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }

    let mut index = 0;
    while index < items.len() {
        let side = rect.w.min(rect.h) as f64;
        if side < min_side as f64 {
            return;
        }
        let largest = items[index].1 as f64 * scale;
        if largest <= 0.0 {
            return;
        }
        let mut sum = 0.0_f64;
        let mut count = 0_usize;
        let mut best = f64::INFINITY;
        while index + count < items.len() {
            let area = items[index + count].1 as f64 * scale;
            if area <= 0.0 {
                break;
            }
            let ratio = worst_aspect(side, sum + area, largest, area);
            if count > 0 && ratio > best {
                break;
            }
            best = ratio;
            sum += area;
            count += 1;
        }
        if count == 0 {
            return;
        }

        let thickness = (sum / side) as f32;
        let mut offset = 0.0_f32;
        if rect.w >= rect.h {
            for item in &items[index..index + count] {
                let area = item.1 as f64 * scale;
                let height = ((area / sum) * side) as f32;
                out.push(Tile {
                    id: item.0,
                    rect: Rect {
                        x: rect.x,
                        y: rect.y + offset,
                        w: thickness,
                        h: height,
                    },
                });
                offset += height;
            }
            rect.x += thickness;
            rect.w -= thickness;
        } else {
            for item in &items[index..index + count] {
                let area = item.1 as f64 * scale;
                let width = ((area / sum) * side) as f32;
                out.push(Tile {
                    id: item.0,
                    rect: Rect {
                        x: rect.x + offset,
                        y: rect.y,
                        w: width,
                        h: thickness,
                    },
                });
                offset += width;
            }
            rect.y += thickness;
            rect.h -= thickness;
        }
        index += count;
    }
}

/// Worst (largest) aspect ratio in a row of area `sum` laid along `side`, whose
/// members range from `min` to `max` in area.
fn worst_aspect(side: f64, sum: f64, max: f64, min: f64) -> f64 {
    if side <= 0.0 || sum <= 0.0 || min <= 0.0 {
        return f64::INFINITY;
    }
    let side2 = side * side;
    let sum2 = sum * sum;
    ((side2 * max) / sum2).max(sum2 / (side2 * min))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileKind {
    /// A directory whose children are drawn on top of it, covering it fully.
    /// It is never visible itself; it only exists so subdivision has a tile
    /// to replace.
    Frame,
    /// A file, or a directory too small to be worth subdividing. Either way,
    /// it stands for its whole subtree.
    Leaf,
    /// Everything in a directory that is too small to draw one by one,
    /// gathered into a single block so no area goes unaccounted for.
    Rest,
}

#[derive(Clone, Copy)]
pub(crate) struct MapTile {
    /// The node this tile stands for. A `Rest` tile carries its parent's id,
    /// since that is what selecting it should reveal.
    id: NodeId,
    /// Unit-space rect, scaled to the canvas at paint time.
    rect: Rect,
    /// Index into `TYPE_COLORS`: what the tile stands for, never where it sits.
    color: usize,
    depth: u16,
    kind: TileKind,
}

impl MapTile {
    pub(crate) fn rect(&self) -> Rect {
        self.rect
    }

    pub(crate) fn is_frame(&self) -> bool {
        self.kind == TileKind::Frame
    }

    pub(crate) fn paint_colors(&self) -> (u32, u32, u32) {
        let base = tile_hue(self);
        let (shadow, light) = tile_light_range(self.kind);
        (base, shade(base, shadow), shade(base, light))
    }
}

#[derive(Default)]
pub(crate) struct Treemap {
    /// Ordered so a directory always precedes the children painted on top of
    /// it, which also makes the last hit in a reverse scan the deepest one.
    tiles: Vec<MapTile>,
    /// Tile position by node, for the selection outline.
    index: HashMap<NodeId, u32>,
}

impl Treemap {
    pub(crate) fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub(crate) fn tiles(&self) -> &[MapTile] {
        &self.tiles
    }

    pub(crate) fn rect_for(&self, id: NodeId) -> Option<Rect> {
        self.index
            .get(&id)
            .and_then(|position| self.tiles.get(*position as usize))
            .map(|tile| tile.rect)
    }

    pub(crate) fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.tiles
            .iter()
            .rev()
            .find(|tile| tile.rect.contains(x, y))
            .map(|tile| tile.id)
    }
}

/// Reference canvas the layout is computed against. Tiles are stored in unit
/// space and scaled to whatever the window is. Resizing never changes a
/// layout; selecting a new zoom root does.
const MAP_REF_W: f32 = 1600.0;
const MAP_REF_H: f32 = 900.0;
const MAP_MIN_SIDE: f32 = 1.0;
/// Below this, a child is folded into its directory's `Rest` block instead of
/// getting a rectangle of its own.
const MAP_MIN_AREA: f32 = 9.0;
/// A directory is only subdivided once it is this big; below that it stands
/// for its whole subtree.
const MAP_MIN_NEST: f32 = 12.0;
const MAP_MAX_DEPTH: u16 = 40;
/// Tile budget. Every tile is a quad repainted on each frame, so this is the
/// knob that decides whether the map stays interactive. It is spent on the
/// largest rectangles first, so running out only costs the finest detail.
const MAP_MAX_TILES: usize = 20_000;

/// Nested treemap of one filesystem subtree, in the style of WizTree and
/// QDirStat. A directory's rectangle is partitioned among its children, which
/// are drawn inside it recursively.
///
/// Two properties do the heavy lifting:
///
/// * **Every rectangle is fully covered.** Children too small to draw are not
///   dropped. They are gathered into a single `Rest` block. Dropping them left
///   the parent's background showing as dead space, which read as "nothing
///   here" when the truth was "thousands of small files".
/// * **Directories are subdivided largest-first, not depth-first.** A
///   depth-first walk spent the whole budget inside the first big directory and
///   left every later sibling, including `/nix`, undrawn. Popping by area also
///   means running out of budget costs only the finest detail, anywhere on the
///   map, rather than a whole branch.
///
/// A directory is committed to being a frame only once there is budget for all
/// of its children, so a frame is never left half covered.
pub(crate) fn build_treemap(scan: &FilesystemScan, root: NodeId) -> Treemap {
    let mut tiles: Vec<MapTile> = Vec::new();
    if root.index() < scan.result.arena.nodes().len() {
        tiles.push(MapTile {
            id: root,
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: MAP_REF_W,
                h: MAP_REF_H,
            },
            color: DIR_COLOR,
            depth: 0,
            kind: TileKind::Leaf,
        });
        let mut queue = BinaryHeap::new();
        queue.push(Candidate::new(&tiles[0], 0));
        let mut pieces = Vec::new();
        let mut children = Vec::new();

        while let Some(candidate) = queue.pop() {
            let remaining = MAP_MAX_TILES - tiles.len();
            if remaining < 2 {
                break;
            }
            let tile = tiles[candidate.position];
            // A rectangle may claim tiles in proportion to how much of the map
            // it covers, so detail is shared out by size instead of by who got
            // there first. Anything over the allowance is folded into the
            // directory's remainder block, which is why this can never leave a
            // frame half covered.
            let share = tile.rect.area() / (MAP_REF_W * MAP_REF_H);
            let allowance = ((share * MAP_MAX_TILES as f32) as usize).clamp(2, remaining);
            if !subdivide(scan, &tile, allowance, &mut pieces, &mut children) {
                continue;
            }
            tiles[candidate.position].kind = TileKind::Frame;
            for child in children.drain(..) {
                let position = tiles.len();
                if child.kind == TileKind::Leaf {
                    queue.push(Candidate::new(&child, position));
                }
                tiles.push(child);
            }
        }
    }
    for tile in &mut tiles {
        tile.rect.x /= MAP_REF_W;
        tile.rect.w /= MAP_REF_W;
        tile.rect.y /= MAP_REF_H;
        tile.rect.h /= MAP_REF_H;
    }
    // `Rest` tiles borrow their parent's id, so they must not displace the
    // parent's own tile in the lookup.
    let index = tiles
        .iter()
        .enumerate()
        .filter(|(_, tile)| tile.kind != TileKind::Rest)
        .map(|(position, tile)| (tile.id, position as u32))
        .collect();
    Treemap { tiles, index }
}

/// A tile queued for subdivision, ordered by area so the biggest rectangles are
/// broken up first. Ties fall back to insertion order.
struct Candidate {
    area: u64,
    position: usize,
}

impl Candidate {
    fn new(tile: &MapTile, position: usize) -> Self {
        Self {
            // Fixed point: `BinaryHeap` needs a total order, `f32` has none.
            area: (tile.rect.area() * 256.0) as u64,
            position,
        }
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.area
            .cmp(&other.area)
            .then_with(|| other.position.cmp(&self.position))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate {}

/// Fill `children` with the tiles that partition `tile`, or return false to
/// leave it as a leaf. At most `allowance` pieces are produced, the last of
/// which is the remainder block. `pieces` is scratch space reused across calls.
fn subdivide(
    scan: &FilesystemScan,
    tile: &MapTile,
    allowance: usize,
    pieces: &mut Vec<Tile>,
    children: &mut Vec<MapTile>,
) -> bool {
    children.clear();
    if tile.depth >= MAP_MAX_DEPTH
        || scan.result.arena.nodes()[tile.id.index()].kind != NodeKind::Directory
        || tile.rect.w < MAP_MIN_NEST
        || tile.rect.h < MAP_MIN_NEST
    {
        return false;
    }
    let Some((items, total)) = partition(scan, tile.id, tile.rect, allowance) else {
        return false;
    };

    pieces.clear();
    squarify(&items, tile.rect, total, MAP_MIN_SIDE, pieces);
    for piece in pieces.iter() {
        if piece.rect.w < MAP_MIN_SIDE || piece.rect.h < MAP_MIN_SIDE {
            continue;
        }
        let kind = if piece.id == tile.id {
            TileKind::Rest
        } else {
            TileKind::Leaf
        };
        children.push(MapTile {
            id: piece.id,
            rect: piece.rect,
            color: if kind == TileKind::Rest {
                REST_COLOR
            } else {
                type_color_index(scan, piece.id)
            },
            depth: tile.depth + 1,
            kind,
        });
    }
    !children.is_empty()
}

/// How a directory's rectangle is divided up: at most `allowance` pieces,
/// largest first, the last of which stands for everything left over. The sizes
/// sum to `total`, so the pieces cover the rectangle exactly.
///
/// Returns `None` when nothing inside is worth drawing separately, which leaves
/// the directory as a single leaf rather than a block of undifferentiated
/// remainder.
fn partition(
    scan: &FilesystemScan,
    parent: NodeId,
    rect: Rect,
    allowance: usize,
) -> Option<(Vec<(NodeId, u64)>, u64)> {
    let totals = &scan.result.totals;
    let mut items: Vec<(NodeId, u64)> = scan
        .result
        .arena
        .child_range(parent)
        .map(|index| NodeId(index as u32))
        .filter_map(|id| {
            let size = totals[id.index()].recursive_allocated;
            (size > 0).then_some((id, size))
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let total = items
        .iter()
        .map(|(_, size)| *size as u128)
        .sum::<u128>()
        .min(u64::MAX as u128) as u64;
    if total == 0 {
        return None;
    }

    // Leave room for the remainder block. Beyond the allowance, a rectangle
    // can never show more than `area / MAP_MIN_AREA` tiles anyway, and taking
    // the largest few is far cheaper than sorting all 200k entries of
    // something like /nix/store. Whatever is cut here still counts towards
    // `total`, so it lands in the remainder rather than vanishing.
    let capacity = ((rect.area() / MAP_MIN_AREA).ceil() as usize)
        .min(allowance.saturating_sub(1))
        .max(1);
    if items.len() > capacity {
        items.select_nth_unstable_by(capacity, |left, right| {
            right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0))
        });
        items.truncate(capacity);
    }
    items.sort_unstable_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let scale = rect.area() as f64 / total as f64;
    let keep = items
        .iter()
        .position(|(_, size)| (*size as f64) * scale < MAP_MIN_AREA as f64)
        .unwrap_or(items.len());
    if keep == 0 {
        return None;
    }
    items.truncate(keep);

    let drawn: u64 = items.iter().map(|(_, size)| *size).sum();
    if let Some(rest) = total.checked_sub(drawn).filter(|rest| *rest > 0) {
        // The remainder can outweigh individual children, so it has to take
        // its place in the ordering squarify relies on.
        let at = items.partition_point(|(_, size)| *size > rest);
        items.insert(at, (parent, rest));
    }
    Some((items, total))
}

fn type_color_index(scan: &FilesystemScan, id: NodeId) -> usize {
    let nodes = scan.result.arena.nodes();
    let Some(node) = nodes.get(id.index()) else {
        return UNKNOWN_COLOR;
    };
    match node.kind {
        NodeKind::Directory | NodeKind::Other => DIR_COLOR,
        NodeKind::Symlink => LINK_COLOR,
        NodeKind::File => file_type_index(scan.result.arena.name(id)),
    }
}

fn file_type_index(name: &[u8]) -> usize {
    let ext = match name.iter().rposition(|byte| *byte == b'.') {
        Some(position) if position + 1 < name.len() => &name[position + 1..],
        _ => return UNKNOWN_COLOR,
    };
    if ext.len() > 8 {
        return UNKNOWN_COLOR;
    }
    let mut lower = [0u8; 8];
    for (index, byte) in ext.iter().enumerate() {
        lower[index] = byte.to_ascii_lowercase();
    }
    match &lower[..ext.len()] {
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"bmp" | b"svg" | b"webp" | b"tif" | b"tiff"
        | b"ico" | b"avif" | b"heic" => IMAGE_COLOR,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"webm" | b"m4v" | b"mpg" | b"mpeg" | b"wmv" => {
            VIDEO_COLOR
        }
        b"mp3" | b"ogg" | b"wav" | b"flac" | b"m4a" | b"opus" | b"aac" => AUDIO_COLOR,
        b"zip" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"7z" | b"rar" | b"pak" | b"vpk"
        | b"cab" | b"iso" => ARCHIVE_COLOR,
        b"txt" | b"md" | b"pdf" | b"doc" | b"docx" | b"html" | b"htm" | b"json" | b"xml"
        | b"yml" | b"yaml" | b"toml" | b"ini" | b"cfg" | b"log" | b"csv" | b"vdf" => DOC_COLOR,
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"js" | b"ts" | b"py" | b"java" | b"go"
        | b"rb" | b"lua" | b"cs" | b"css" | b"nix" => CODE_COLOR,
        b"exe" | b"msi" | b"bat" | b"cmd" | b"sh" | b"bin" | b"dll" | b"so" | b"appimage" => {
            EXEC_COLOR
        }
        _ => UNKNOWN_COLOR,
    }
}

fn shade(color: u32, factor: f32) -> u32 {
    let channel = |shift: u32| {
        let value = (((color >> shift) & 0xff) as f32 * factor).clamp(0.0, 255.0) as u32;
        value << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Equal files share one hue exactly. Position and identity never shift the
/// colour, so a user can learn the palette and trust it across scans.
fn tile_hue(tile: &MapTile) -> u32 {
    TYPE_COLORS[tile.color % TYPE_COLORS.len()]
}

/// Each tile restarts the same diagonal light sweep. Where equal-colour tiles
/// meet, the light end of one touches the shadow end of the next. The edge is
/// readable without a stroke or gap.
fn tile_light_range(kind: TileKind) -> (f32, f32) {
    match kind {
        TileKind::Frame => (0.90, 1.0),
        TileKind::Rest => (0.86, 1.04),
        TileKind::Leaf => (0.84, 1.08),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_treemap, squarify, tile_hue, tile_light_range, worst_aspect, MapTile, Rect, Tile,
        TileKind, ARCHIVE_COLOR, DIR_COLOR, MAP_MAX_TILES, MAP_MIN_NEST, MAP_REF_H, MAP_REF_W,
        TYPE_COLORS, UNKNOWN_COLOR,
    };
    use akimi_ext4::FilesystemScan;
    use akimi_model::{NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};
    use std::collections::HashMap;

    const CANVAS: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };

    fn lay_out(items: &[(NodeId, u64)]) -> Vec<Tile> {
        let total = items.iter().map(|(_, size)| size).sum::<u64>();
        let mut tiles = Vec::new();
        squarify(items, CANVAS, total, 0.0, &mut tiles);
        tiles
    }

    #[test]
    fn files_are_coloured_by_extension_case_insensitively() {
        use super::{
            file_type_index, ARCHIVE_COLOR, AUDIO_COLOR, CODE_COLOR, DOC_COLOR, EXEC_COLOR,
            IMAGE_COLOR, UNKNOWN_COLOR, VIDEO_COLOR,
        };
        assert_eq!(file_type_index(b"shot.png"), IMAGE_COLOR);
        assert_eq!(file_type_index(b"movie.MKV"), VIDEO_COLOR);
        assert_eq!(file_type_index(b"sound.Ogg"), AUDIO_COLOR);
        assert_eq!(file_type_index(b"bundle.PAK"), ARCHIVE_COLOR);
        assert_eq!(file_type_index(b"notes.md"), DOC_COLOR);
        assert_eq!(file_type_index(b"main.rs"), CODE_COLOR);
        assert_eq!(file_type_index(b"game.exe"), EXEC_COLOR);
        assert_eq!(file_type_index(b"README"), UNKNOWN_COLOR);
        assert_eq!(file_type_index(b"archive.tar.gz"), ARCHIVE_COLOR);
        assert_eq!(file_type_index(b".bashrc"), UNKNOWN_COLOR);
        assert_eq!(file_type_index(b"trailing."), UNKNOWN_COLOR);
    }

    #[test]
    fn squarify_preserves_area_and_items() {
        let tiles = lay_out(&[(NodeId(1), 60), (NodeId(2), 30), (NodeId(3), 10)]);
        assert_eq!(tiles.len(), 3);
        let area = tiles.iter().map(|tile| tile.rect.area()).sum::<f32>();
        assert!(
            (area - CANVAS.area()).abs() < 1.0,
            "laid out {area} of {}",
            CANVAS.area()
        );
    }

    #[test]
    fn squarify_areas_match_sizes() {
        let items = [(NodeId(1), 60_u64), (NodeId(2), 30), (NodeId(3), 10)];
        let tiles = lay_out(&items);
        for (tile, (_, size)) in tiles.iter().zip(items.iter()) {
            let expected = CANVAS.area() * (*size as f32 / 100.0);
            assert!(
                (tile.rect.area() - expected).abs() < 1.0,
                "tile area {} != {expected}",
                tile.rect.area()
            );
        }
    }

    #[test]
    fn squarify_tiles_do_not_overlap() {
        let tiles = lay_out(&[
            (NodeId(1), 50),
            (NodeId(2), 20),
            (NodeId(3), 15),
            (NodeId(4), 10),
            (NodeId(5), 5),
        ]);
        for (index, left) in tiles.iter().enumerate() {
            for right in tiles.iter().skip(index + 1) {
                let overlap_w = (left.rect.x + left.rect.w).min(right.rect.x + right.rect.w)
                    - left.rect.x.max(right.rect.x);
                let overlap_h = (left.rect.y + left.rect.h).min(right.rect.y + right.rect.h)
                    - left.rect.y.max(right.rect.y);
                assert!(
                    overlap_w <= 0.0001 || overlap_h <= 0.0001,
                    "tiles {index} overlap by {overlap_w}x{overlap_h}"
                );
            }
        }
    }
    #[test]
    fn squarify_keeps_tiles_near_square() {
        let items: Vec<(NodeId, u64)> = (1..=20)
            .map(|n| (NodeId(n), (21 - n as u64) * 10))
            .collect();
        let tiles = lay_out(&items);
        for tile in &tiles {
            let ratio = (tile.rect.w / tile.rect.h).max(tile.rect.h / tile.rect.w);
            assert!(ratio < 4.0, "aspect ratio {ratio} is a sliver");
        }
    }

    #[test]
    fn squarify_stops_when_the_strip_gets_thin() {
        let mut items = vec![(NodeId(1), 1_000_000_u64)];
        items.extend((2..500).map(|n| (NodeId(n), 1_u64)));
        let total = items.iter().map(|(_, size)| size).sum::<u64>();
        let mut tiles = Vec::new();
        squarify(&items, CANVAS, total, 3.0, &mut tiles);
        assert!(tiles.len() < items.len());
        assert_eq!(tiles[0].id, NodeId(1));
    }

    #[test]
    fn worst_aspect_is_one_for_a_square() {
        assert!((worst_aspect(10.0, 100.0, 100.0, 100.0) - 1.0).abs() < 1e-9);
    }
    fn synthetic_scan(sizes: &[u64], leaves: u64) -> FilesystemScan {
        let mut names = NameArena::with_capacity(64);
        let root_name = names.push(b"/").unwrap();
        let dir_name = names.push(b"dir").unwrap();
        let file_name = names.push(b"file").unwrap();

        let dir = |parent: NodeId, name| Node {
            parent,
            inode: 0,
            name,
            kind: NodeKind::Directory,
            logical_size: 0,
            allocated_size: 0,
            links: 1,
            mtime: 0,
        };

        // Nodes must be grouped in ascending parent order: root, then the
        // top-level directories, then every directory's files.
        let mut nodes = vec![dir(NodeId::ROOT, root_name)];
        for _ in sizes {
            nodes.push(dir(NodeId::ROOT, dir_name));
        }
        for (index, total) in sizes.iter().enumerate() {
            let parent = NodeId(index as u32 + 1);
            for _ in 0..leaves {
                nodes.push(Node {
                    parent,
                    inode: 0,
                    name: file_name,
                    kind: NodeKind::File,
                    logical_size: total / leaves,
                    allocated_size: total / leaves,
                    links: 1,
                    mtime: 0,
                });
            }
        }

        FilesystemScan {
            result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
            stats: Default::default(),
            timings: Default::default(),
            warnings: Default::default(),
            workers: 1,
        }
    }
    #[test]
    fn every_top_level_folder_gets_tiles() {
        let sizes = [651_000, 114_000, 31_000, 1_900, 1_100, 700];
        let scan = synthetic_scan(&sizes, 100);
        let map = build_treemap(&scan, NodeId::ROOT);
        for index in 0..sizes.len() {
            let id = NodeId(index as u32 + 1);
            assert!(
                map.index.contains_key(&id),
                "top-level folder {index} is missing from the map"
            );
        }
    }
    #[test]
    fn large_folders_are_drawn_nested() {
        let scan = synthetic_scan(&[651_000, 114_000], 400);
        let map = build_treemap(&scan, NodeId::ROOT);
        for id in [NodeId(1), NodeId(2)] {
            let tile = map.tiles[map.index[&id] as usize];
            assert_eq!(
                tile.kind,
                TileKind::Frame,
                "folder {id:?} was drawn as a flat leaf"
            );
        }
        let leaves = map.tiles.iter().filter(|tile| tile.depth == 2).count();
        assert!(leaves > 200, "only {leaves} files drawn");
    }
    #[test]
    fn frames_are_fully_covered_by_their_children() {
        let scan = synthetic_scan(&[900_000, 250_000, 40_000], 20_000);
        let map = build_treemap(&scan, NodeId::ROOT);
        let nodes = scan.result.arena.nodes();

        let mut covered: HashMap<NodeId, f32> = HashMap::new();
        for tile in &map.tiles {
            if tile.id == NodeId::ROOT && tile.kind != TileKind::Rest {
                continue;
            }
            let parent = if tile.kind == TileKind::Rest {
                tile.id
            } else {
                nodes[tile.id.index()].parent
            };
            *covered.entry(parent).or_default() += tile.rect.area();
        }

        let mut frames = 0;
        for tile in &map.tiles {
            if tile.kind != TileKind::Frame {
                continue;
            }
            frames += 1;
            // Children partition the whole tile edge to edge, like QDirStat:
            // no frame may show through.
            let inner = tile.rect.w * tile.rect.h;
            let area = covered.get(&tile.id).copied().unwrap_or(0.0);
            assert!(
                area >= inner * 0.99,
                "{:?} covers {area} of {inner}, leaving dead space",
                tile.id
            );
        }
        assert!(frames >= 3, "only {frames} folders were subdivided");
        assert!(
            map.tiles.iter().any(|tile| tile.kind == TileKind::Rest),
            "the undrawable tail produced no remainder block"
        );
    }
    #[test]
    fn nested_tiles_stay_inside_their_parent() {
        let scan = synthetic_scan(&[651_000, 114_000, 31_000], 200);
        let map = build_treemap(&scan, NodeId::ROOT);
        let nodes = scan.result.arena.nodes();
        for tile in &map.tiles {
            if tile.id == NodeId::ROOT {
                continue;
            }
            let parent = if tile.kind == TileKind::Rest {
                tile.id
            } else {
                nodes[tile.id.index()].parent
            };
            let outer = map.tiles[map.index[&parent] as usize].rect;
            assert!(
                tile.rect.x >= outer.x - 1e-4
                    && tile.rect.y >= outer.y - 1e-4
                    && tile.rect.x + tile.rect.w <= outer.x + outer.w + 1e-4
                    && tile.rect.y + tile.rect.h <= outer.y + outer.h + 1e-4,
                "{:?} escapes its parent",
                tile.rect
            );
        }
    }
    #[test]
    fn collapsed_directories_are_darker_than_unknown_files() {
        let brightness = |colour: u32| {
            (0..3)
                .map(|byte| (colour >> (byte * 8)) & 0xff)
                .sum::<u32>()
        };
        assert!(brightness(TYPE_COLORS[DIR_COLOR]) < brightness(TYPE_COLORS[UNKNOWN_COLOR]));
    }
    #[test]
    fn same_type_tiles_share_one_colour() {
        let tile = |id: NodeId| MapTile {
            id,
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 6.0,
                h: 6.0,
            },
            color: ARCHIVE_COLOR,
            depth: 2,
            kind: TileKind::Leaf,
        };
        assert_eq!(tile_hue(&tile(NodeId(7))), tile_hue(&tile(NodeId(19))));
    }

    #[test]
    fn tile_light_sweep_crosses_the_base_colour() {
        let (shadow, light) = tile_light_range(TileKind::Leaf);
        assert!(shadow < 1.0);
        assert!(light > 1.0);

        let (rest_shadow, rest_light) = tile_light_range(TileKind::Rest);
        assert!(rest_light - rest_shadow < light - shadow);
    }
    #[test]
    fn small_folders_stop_nesting() {
        let mut sizes = vec![10_000_000_u64];
        sizes.extend(std::iter::repeat_n(500, 1_500));
        let scan = synthetic_scan(&sizes, 4);
        let map = build_treemap(&scan, NodeId::ROOT);

        let small = map.tiles[map.index[&NodeId(1_000)] as usize];
        let width = small.rect.w * MAP_REF_W;
        let height = small.rect.h * MAP_REF_H;
        assert!(
            width < MAP_MIN_NEST && height < MAP_MIN_NEST,
            "{width}x{height} is big enough to nest into"
        );
        assert_eq!(small.kind, TileKind::Leaf);

        assert_eq!(
            map.tiles[map.index[&NodeId(1)] as usize].kind,
            TileKind::Frame
        );
    }
    #[test]
    fn tile_areas_track_folder_sizes() {
        let sizes = [651_000_u64, 114_000, 31_000];
        let scan = synthetic_scan(&sizes, 100);
        let map = build_treemap(&scan, NodeId::ROOT);
        let total: u64 = sizes.iter().sum();
        for (index, size) in sizes.iter().enumerate() {
            let share = map.tiles[map.index[&NodeId(index as u32 + 1)] as usize]
                .rect
                .area();
            let expected = *size as f32 / total as f32;
            assert!(
                (share - expected).abs() < 0.01,
                "folder {index} covers {share} of the map, expected {expected}"
            );
        }
    }
    #[test]
    fn tile_budget_is_respected() {
        let scan = synthetic_scan(&[651_000, 114_000, 31_000], 40_000);
        let map = build_treemap(&scan, NodeId::ROOT);
        assert!(map.tiles.len() <= MAP_MAX_TILES);
        assert!(
            map.tiles.len() > MAP_MAX_TILES / 2,
            "budget went unused: only {} tiles",
            map.tiles.len()
        );
    }
    #[test]
    fn parents_are_stored_before_their_children() {
        let scan = synthetic_scan(&[651_000, 114_000, 31_000], 200);
        let map = build_treemap(&scan, NodeId::ROOT);
        let nodes = scan.result.arena.nodes();
        for (position, tile) in map.tiles.iter().enumerate() {
            if tile.id == NodeId::ROOT {
                continue;
            }
            let parent = if tile.kind == TileKind::Rest {
                tile.id
            } else {
                nodes[tile.id.index()].parent
            };
            assert!((map.index[&parent] as usize) < position);
        }
    }
    #[test]
    fn remainder_blocks_do_not_shadow_their_parent() {
        let scan = synthetic_scan(&[900_000, 250_000], 20_000);
        let map = build_treemap(&scan, NodeId::ROOT);
        assert!(
            map.tiles.iter().any(|tile| tile.kind == TileKind::Rest),
            "expected a remainder block for the undrawable tail"
        );
        for (id, position) in &map.index {
            let tile = map.tiles[*position as usize];
            assert_ne!(tile.kind, TileKind::Rest);
            assert_eq!(tile.id, *id);
        }
    }
}
