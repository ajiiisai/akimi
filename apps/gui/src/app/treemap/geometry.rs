use super::{Rect, SELECTION_COLOR};

pub(crate) fn place(rect: Rect, canvas: Rect, scale: f32) -> Rect {
    let snap = |value: f32| (value * scale).round() / scale;
    let x = snap(canvas.x + canvas.w * rect.x);
    let y = snap(canvas.y + canvas.h * rect.y);
    let right = snap(canvas.x + canvas.w * (rect.x + rect.w));
    let bottom = snap(canvas.y + canvas.h * (rect.y + rect.h));
    Rect {
        x,
        y,
        w: (right - x).max(0.0),
        h: (bottom - y).max(0.0),
    }
}

/// GPUI paints inward from these exact bounds, shared with the tile fill.
pub(crate) fn selection(rect: Rect, scale: f32) -> (Rect, f32, u32) {
    (rect, 1.0 / scale, SELECTION_COLOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_starts_at_the_tile_edge_and_is_one_pixel_thick() {
        let canvas = Rect {
            x: 0.3,
            y: 0.7,
            w: 100.0,
            h: 200.0,
        };
        let tile = Rect {
            x: 0.31,
            y: 0.1,
            w: 0.03,
            h: 0.5,
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let placed = place(tile, canvas, scale);
            let fill = placed;
            let (white, width, _) = selection(placed, scale);
            assert_eq!(
                white, fill,
                "selection must start exactly at the tile boundary"
            );
            assert!(
                (width * scale - 1.0).abs() < 0.001,
                "selection must be one device pixel thick"
            );
        }
    }

    #[test]
    fn adjacent_tiles_share_a_device_pixel_edge_without_overpainting() {
        let canvas = Rect {
            x: 0.3,
            y: 0.7,
            w: 101.0,
            h: 73.0,
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let left = place(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.333,
                    h: 1.0,
                },
                canvas,
                scale,
            );
            let right = place(
                Rect {
                    x: 0.333,
                    y: 0.0,
                    w: 0.667,
                    h: 1.0,
                },
                canvas,
                scale,
            );
            assert!(
                (left.x + left.w - right.x).abs() < 0.001,
                "adjacent fills overlap"
            );
            assert!((right.x * scale - (right.x * scale).round()).abs() < 0.001);
        }
    }
}
