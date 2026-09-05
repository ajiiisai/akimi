use super::{
    build_treemap, squarify, tile_hue, tile_light_range, worst_aspect, LayoutSize, MapTile, Rect,
    Tile, TileKind, ARCHIVE_COLOR, DIR_COLOR, MAP_MAX_TILES, MAP_MIN_NEST, MAP_REF_H, MAP_REF_W,
    TYPE_COLORS, UNKNOWN_COLOR,
};
use akimi_model::{FilesystemScan, NameArena, Node, NodeArena, NodeId, NodeKind, ScanResult};
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
fn row_rounding_uses_the_remaining_pixel_rectangle() {
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 101.0,
        h: 73.0,
    };
    let mut tiles = Vec::new();
    squarify(
        &[
            (NodeId(1), 6),
            (NodeId(2), 3),
            (NodeId(3), 2),
            (NodeId(4), 1),
        ],
        rect,
        12,
        0.0,
        &mut tiles,
    );
    let rectangles = tiles.iter().map(|tile| tile.rect).collect::<Vec<_>>();
    assert_eq!(
        rectangles,
        vec![
            Rect {
                x: 0.0,
                y: 0.0,
                w: 51.0,
                h: 73.0
            },
            Rect {
                x: 51.0,
                y: 0.0,
                w: 50.0,
                h: 37.0
            },
            Rect {
                x: 51.0,
                y: 37.0,
                w: 33.0,
                h: 36.0
            },
            Rect {
                x: 84.0,
                y: 37.0,
                w: 17.0,
                h: 36.0
            },
        ]
    );
}

fn mixed_scan() -> FilesystemScan {
    let source = synthetic_scan(&[60, 40], 1);
    let mut nodes = source.result.arena.nodes().to_vec();
    let file = nodes[3];
    nodes.splice(
        3..3,
        [45, 35].map(|size| Node {
            parent: NodeId::ROOT,
            allocated_size: size,
            logical_size: size,
            ..file
        }),
    );
    let mut names = NameArena::default();
    for (index, node) in nodes.iter_mut().enumerate() {
        node.name = names.push(format!("item{index}").as_bytes()).unwrap();
    }
    FilesystemScan {
        result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
        ..source
    }
}

#[test]
fn direct_files_form_one_rectangle_beside_subdirectories() {
    let scan = mixed_scan();
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::new(900.0, 600.0).unwrap());
    let first = map.rect_for(NodeId(3)).unwrap();
    let second = map.rect_for(NodeId(4)).unwrap();
    let width = (first.x + first.w).max(second.x + second.w) - first.x.min(second.x);
    let height = (first.y + first.h).max(second.y + second.h) - first.y.min(second.y);
    assert!(
        (width * height - first.area() - second.area()).abs() < 0.00001,
        "direct files are scattered between directories"
    );
    for id in [NodeId(3), NodeId(4)] {
        let rect = map.rect_for(id).unwrap();
        assert_eq!(
            map.hit_test(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
            Some(id)
        );
    }
    assert_eq!(
        map.rect_for(NodeId::ROOT),
        Some(Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0
        })
    );
    let painted: f32 = map
        .tiles()
        .iter()
        .filter(|tile| !tile.is_frame())
        .map(|tile| tile.rect.area())
        .sum();
    assert!((painted - 1.0).abs() < 0.00001);
}

#[test]
fn equal_allocations_sort_by_logical_size() {
    let source = synthetic_scan(&[100], 2);
    let mut nodes = source.result.arena.nodes().to_vec();
    nodes[2].logical_size = 10;
    nodes[3].logical_size = 40;
    let mut names = NameArena::default();
    for node in &mut nodes {
        node.name = names.push(b"file").unwrap();
    }
    let scan = FilesystemScan {
        result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
        ..source
    };
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
    let larger = map.rect_for(NodeId(3)).unwrap();
    let smaller = map.rect_for(NodeId(2)).unwrap();
    assert!(
        larger.x <= smaller.x && larger.y <= smaller.y,
        "logical-size tie-breaker is reversed"
    );
}

#[test]
fn equal_tiles_stay_square_on_a_square_canvas() {
    let scan = synthetic_scan(&[100; 9], 1);
    let map = build_treemap(
        &scan,
        NodeId::ROOT,
        LayoutSize::new(1000.0, 1000.0).unwrap(),
    );
    for id in 1..=9 {
        let rect = map.rect_for(NodeId(id)).unwrap();
        let width = rect.w * 1000.0;
        let height = rect.h * 1000.0;
        let aspect = (width / height).max(height / width);
        assert!(
            aspect < 1.5,
            "tile {id} is {width}x{height}, aspect {aspect}"
        );
    }
}

#[test]
fn tiny_tail_keeps_its_area_as_a_remainder() {
    let scan = synthetic_scan(&[1000, 1], 1);
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::new(100.0, 100.0).unwrap());
    let painted: f32 = map
        .tiles()
        .iter()
        .filter(|tile| !tile.is_frame())
        .map(|tile| tile.rect.area())
        .sum();
    assert!(
        (painted - 1.0).abs() < 0.00001,
        "only {painted} of the map is covered"
    );
}

#[test]
fn layouts_preserve_area_and_hit_targets_across_panel_shapes() {
    let scan = synthetic_scan(&[900_000, 220_000, 120_000, 30_000], 200);
    for (width, height) in [(1250.0, 1000.0), (1600.0, 300.0), (300.0, 1200.0)] {
        let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::new(width, height).unwrap());
        let mut area = 0.0;
        for tile in map.tiles().iter().filter(|tile| !tile.is_frame()) {
            let rect = tile.rect();
            area += rect.area();
            assert!(rect.x >= 0.0 && rect.y >= 0.0);
            assert!(rect.x + rect.w <= 1.00001 && rect.y + rect.h <= 1.00001);
            assert_eq!(
                map.hit_test(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
                Some(tile.id)
            );
            if tile.kind == TileKind::Leaf {
                assert!(rect.w * width >= 2.99 && rect.h * height >= 2.99);
            }
        }
        assert!(
            (area - 1.0).abs() < 0.0001,
            "{width}x{height}: covered {area}"
        );
    }
}

#[test]
#[ignore = "writes an SVG fixture to AKIMI_TREEMAP_PREVIEW"]
fn export_layout_preview() {
    use std::fmt::Write;
    let source = synthetic_scan(&[900_000, 280_000, 180_000, 90_000, 40_000, 20_000], 64);
    let mut nodes = source.result.arena.nodes().to_vec();
    let mut names = NameArena::default();
    let extensions = [
        "file.zip", "file.exe", "file.png", "file.mp4", "file.rs", "file.pdf",
    ];
    for (index, node) in nodes.iter_mut().enumerate() {
        node.name = names
            .push(if node.kind == NodeKind::Directory {
                b"directory"
            } else {
                extensions[(node.parent.index() - 1) % extensions.len()].as_bytes()
            })
            .unwrap();
        node.allocated_size *= 1 + (index as u64 * 17 % 100);
    }
    let scan = FilesystemScan {
        result: ScanResult::new(NodeArena::from_parts(nodes, names)).unwrap(),
        ..source
    };
    let size = LayoutSize::new(1250.0, 1000.0).unwrap();
    let map = build_treemap(&scan, NodeId::ROOT, size);
    let canvas = Rect {
        x: 0.0,
        y: 0.0,
        w: 1250.0,
        h: 1000.0,
    };
    let mut svg = String::from("<svg xmlns='http://www.w3.org/2000/svg' width='1250' height='1000' viewBox='0 0 1250 1000'><rect width='1250' height='1000' fill='#202428'/>");
    for (index, tile) in map
        .tiles()
        .iter()
        .enumerate()
        .filter(|(_, tile)| !tile.is_frame())
    {
        let (_, shadow, light) = tile.paint_colors();
        let rect = super::geometry::place(tile.rect(), canvas, 1.0);
        writeln!(svg, "<defs><linearGradient id='g{index}' x1='0' y1='0' x2='1' y2='1'><stop stop-color='#{shadow:06x}'/><stop offset='1' stop-color='#{light:06x}'/></linearGradient></defs><rect x='{}' y='{}' width='{}' height='{}' fill='url(#g{index})'/>", rect.x, rect.y, rect.w, rect.h).unwrap();
    }
    let selected = map
        .tiles()
        .iter()
        .find(|tile| tile.color == super::EXEC_COLOR && !tile.is_frame())
        .unwrap()
        .rect();
    append_selection_svg(&mut svg, super::geometry::place(selected, canvas, 1.0), 1.0);
    svg.push_str("</svg>");
    std::fs::write(
        std::env::var_os("AKIMI_TREEMAP_PREVIEW")
            .expect("set AKIMI_TREEMAP_PREVIEW to an SVG path"),
        svg,
    )
    .unwrap();
}

fn append_selection_svg(svg: &mut String, selected: Rect, scale: f32) {
    use std::fmt::Write;
    let (rect, width, color) = super::geometry::selection(selected, scale);
    // Filled edges also represent a one-pixel tile, where an SVG stroke
    // would have a zero-width center rectangle and disappear.
    let horizontal = width.min(rect.w);
    let vertical = width.min(rect.h);
    for edge in [
        Rect {
            h: vertical,
            ..rect
        },
        Rect {
            y: rect.y + rect.h - vertical,
            h: vertical,
            ..rect
        },
        Rect {
            w: horizontal,
            ..rect
        },
        Rect {
            x: rect.x + rect.w - horizontal,
            w: horizontal,
            ..rect
        },
    ] {
        writeln!(
            svg,
            "<rect x='{}' y='{}' width='{}' height='{}' fill='#{color:06x}'/>",
            edge.x, edge.y, edge.w, edge.h
        )
        .unwrap();
    }
}

#[test]
#[ignore = "writes a strip-selection SVG to AKIMI_TREEMAP_PREVIEW"]
fn export_selection_preview() {
    use std::fmt::Write;
    let canvas = Rect {
        x: 0.0,
        y: 0.0,
        w: 600.0,
        h: 240.0,
    };
    let mut svg = String::from("<svg xmlns='http://www.w3.org/2000/svg' width='600' height='240'><rect width='600' height='240' fill='#c88a38'/><rect x='300' width='300' height='240' fill='#7d8993'/>");
    for (index, width) in [1.0, 2.0, 3.0, 6.0, 12.0].iter().enumerate() {
        let normalized = Rect {
            x: (60.0 + index as f32 * 110.0) / canvas.w,
            y: 0.15,
            w: width / canvas.w,
            h: 0.7,
        };
        let rect = super::geometry::place(normalized, canvas, 1.0);
        writeln!(
            svg,
            "<rect x='{}' y='{}' width='{}' height='{}' fill='#d05c5c'/>",
            rect.x, rect.y, rect.w, rect.h
        )
        .unwrap();
        append_selection_svg(&mut svg, rect, 1.0);
    }
    svg.push_str("</svg>");
    std::fs::write(
        std::env::var_os("AKIMI_TREEMAP_PREVIEW")
            .expect("set AKIMI_TREEMAP_PREVIEW to an SVG path"),
        svg,
    )
    .unwrap();
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());

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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
    let map = build_treemap(&scan, NodeId::ROOT, LayoutSize::default());
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
