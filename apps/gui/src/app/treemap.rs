use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use akimi_model::{FilesystemScan, NodeId, NodeKind};

pub(crate) mod geometry;
mod viewport;

pub(crate) use viewport::{ScrollAmount, TreemapViewport};

pub(crate) const SELECTION_COLOR: u32 = 0xffffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutSize {
    width: u32,
    height: u32,
}

impl LayoutSize {
    pub(crate) fn new(width: f32, height: f32) -> Option<Self> {
        if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
            return None;
        }
        Some(Self {
            width: width.round() as u32,
            height: height.round() as u32,
        })
    }
}

impl Default for LayoutSize {
    fn default() -> Self {
        Self {
            width: 1600,
            height: 900,
        }
    }
}

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
/// to draw. The returned rectangle holds any undrawn tail, preserving its area.
fn squarify(
    items: &[(NodeId, u64)],
    mut rect: Rect,
    total: u64,
    min_side: f32,
    out: &mut Vec<Tile>,
) -> Option<Rect> {
    if items.is_empty() || total == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    let mut index = 0;
    let mut remaining = total;
    while index < items.len() {
        // Each rounded row changes the pixels available to the remaining bytes.
        let scale = (rect.w as f64 * rect.h as f64) / remaining as f64;
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let side = rect.w.min(rect.h) as f64;
        if side < min_side as f64 {
            return Some(rect);
        }
        let largest = items[index].1 as f64 * scale;
        if largest <= 0.0 {
            return Some(rect);
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
            return Some(rect);
        }

        let thickness = ((sum / side).round() as f32).min(rect.w.max(rect.h));
        if thickness < min_side {
            return Some(rect);
        }
        let mut offset = 0.0_f64;
        if rect.w >= rect.h {
            for item in &items[index..index + count] {
                let area = item.1 as f64 * scale;
                let start = offset.round() as f32;
                offset += (area / sum) * side;
                let height = ((offset.round() as f32).min(rect.h) - start).max(0.0);
                out.push(Tile {
                    id: item.0,
                    rect: Rect {
                        x: rect.x,
                        y: rect.y + start,
                        w: thickness,
                        h: height,
                    },
                });
            }
            rect.x += thickness;
            rect.w -= thickness;
        } else {
            for item in &items[index..index + count] {
                let area = item.1 as f64 * scale;
                let start = offset.round() as f32;
                offset += (area / sum) * side;
                let width = ((offset.round() as f32).min(rect.w) - start).max(0.0);
                out.push(Tile {
                    id: item.0,
                    rect: Rect {
                        x: rect.x + start,
                        y: rect.y,
                        w: width,
                        h: thickness,
                    },
                });
            }
            rect.y += thickness;
            rect.h -= thickness;
        }
        remaining =
            remaining.saturating_sub(items[index..index + count].iter().map(|item| item.1).sum());
        index += count;
    }
    (remaining > 0 && rect.w > 0.0 && rect.h > 0.0).then_some(rect)
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
    /// A virtual group containing the directory's direct files.
    Files,
    /// The expanded file group. It must not replace the directory's outline.
    FileFrame,
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
        matches!(self.kind, TileKind::Frame | TileKind::FileFrame)
    }

    pub(crate) fn paint_colors(&self) -> (u32, u32, u32) {
        let base = tile_hue(self);
        let (shadow, light) = tile_light_range(self.kind);
        (base, shade(base, shadow), shade(base, light))
    }
}

pub(crate) struct Treemap {
    size: LayoutSize,
    root: NodeId,
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

/// Initial size before the GUI measures its canvas.
#[cfg(test)]
const MAP_REF_W: f32 = 1600.0;
#[cfg(test)]
const MAP_REF_H: f32 = 900.0;
const MAP_MIN_SIDE: f32 = 3.0;
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
pub(crate) fn build_treemap(scan: &FilesystemScan, root: NodeId, size: LayoutSize) -> Treemap {
    let width = size.width as f32;
    let height = size.height as f32;
    let mut tiles: Vec<MapTile> = Vec::new();
    if root.index() < scan.result.arena.nodes().len() {
        tiles.push(MapTile {
            id: root,
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: height,
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
            let share = tile.rect.area() / (width * height);
            let allowance = ((share * MAP_MAX_TILES as f32) as usize).clamp(2, remaining);
            if !subdivide(scan, &tile, allowance, &mut pieces, &mut children) {
                continue;
            }
            tiles[candidate.position].kind = if tile.kind == TileKind::Files {
                TileKind::FileFrame
            } else {
                TileKind::Frame
            };
            for child in children.drain(..) {
                let position = tiles.len();
                if matches!(child.kind, TileKind::Leaf | TileKind::Files) {
                    queue.push(Candidate::new(&child, position));
                }
                tiles.push(child);
            }
        }
    }
    for tile in &mut tiles {
        tile.rect.x /= width;
        tile.rect.w /= width;
        tile.rect.y /= height;
        tile.rect.h /= height;
    }
    // `Rest` tiles borrow their parent's id, so they must not displace the
    // parent's own tile in the lookup.
    let index = tiles
        .iter()
        .enumerate()
        .filter(|(_, tile)| matches!(tile.kind, TileKind::Leaf | TileKind::Frame))
        .map(|(position, tile)| (tile.id, position as u32))
        .collect();
    Treemap {
        tiles,
        index,
        size,
        root,
    }
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
    let Some((items, total)) = partition(
        scan,
        tile.id,
        tile.rect,
        allowance,
        tile.kind == TileKind::Files,
    ) else {
        return false;
    };

    pieces.clear();
    let remainder = squarify(&items, tile.rect, total, MAP_MIN_SIDE, pieces);
    for piece in pieces.iter() {
        let kind = if piece.rect.w < MAP_MIN_SIDE || piece.rect.h < MAP_MIN_SIDE {
            TileKind::Rest
        } else if piece.id == tile.id {
            TileKind::Files
        } else {
            TileKind::Leaf
        };
        children.push(MapTile {
            id: if kind == TileKind::Rest {
                tile.id
            } else {
                piece.id
            },
            rect: piece.rect,
            color: if matches!(kind, TileKind::Rest | TileKind::Files) {
                REST_COLOR
            } else {
                type_color_index(scan, piece.id)
            },
            depth: tile.depth + 1,
            kind,
        });
    }
    if let Some(rect) = remainder {
        children.push(MapTile {
            id: tile.id,
            rect,
            color: REST_COLOR,
            depth: tile.depth + 1,
            kind: TileKind::Rest,
        });
    }
    !children.is_empty()
}

/// Select visible children, largest first, reserving one tile for the tail.
/// `total` includes omitted children so the layout can preserve their area.
///
/// Returns `None` when nothing inside is worth drawing separately, which leaves
/// the directory as a single leaf rather than a block of undifferentiated
/// remainder.
fn partition(
    scan: &FilesystemScan,
    parent: NodeId,
    rect: Rect,
    allowance: usize,
    files_only: bool,
) -> Option<(Vec<(NodeId, u64)>, u64)> {
    let totals = &scan.result.totals;
    let arena = &scan.result.arena;
    let range = arena.child_range(parent);
    let group_files = !files_only
        && range.clone().any(|index| {
            arena.nodes()[index].kind == NodeKind::Directory
                && totals[index].recursive_allocated > 0
        });
    let mut files_size = 0_u64;
    let mut files_logical = 0_u64;
    let mut items = Vec::new();
    for index in range {
        let size = totals[index].recursive_allocated;
        let directory = arena.nodes()[index].kind == NodeKind::Directory;
        if size == 0 || (files_only && directory) {
            continue;
        }
        if group_files && !directory {
            files_size = files_size.saturating_add(size);
            files_logical = files_logical.saturating_add(totals[index].recursive_logical);
        } else {
            items.push((NodeId(index as u32), size));
        }
    }
    if files_size > 0 {
        items.push((parent, files_size));
    }
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
    let logical_size = |id: NodeId| {
        if id == parent && group_files {
            files_logical
        } else {
            totals[id.index()].recursive_logical
        }
    };
    let compare = |left: &(NodeId, u64), right: &(NodeId, u64)| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| logical_size(right.0).cmp(&logical_size(left.0)))
            .then_with(|| left.0.cmp(&right.0))
    };
    if items.len() > capacity {
        items.select_nth_unstable_by(capacity, compare);
        items.truncate(capacity);
    }
    items.sort_unstable_by(compare);

    let scale = rect.area() as f64 / total as f64;
    let keep = items
        .iter()
        .position(|(_, size)| (*size as f64) * scale < MAP_MIN_AREA as f64)
        .unwrap_or(items.len());
    if keep == 0 {
        return None;
    }
    items.truncate(keep);

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
        let value = (((color >> shift) & 0xff_u32) as f32 * factor).clamp(0.0, 255.0) as u32;
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
        TileKind::Frame | TileKind::FileFrame => (0.90, 1.0),
        TileKind::Rest | TileKind::Files => (0.86, 1.04),
        TileKind::Leaf => (0.84, 1.08),
    }
}

#[cfg(test)]
mod tests;
