// SPDX-License-Identifier: GPL-3.0-only

//! Geometric rendering of the Box Drawing (U+2500..U+257F) and Block Elements
//! (U+2580..U+259F) glyphs.
//!
//! Terminals render these characters constantly (TUIs, `tput`, ncurses, git
//! UIs, …) and users expect their lines to tile seamlessly into unbroken
//! borders. Relying on the font's glyphs does not achieve this: a font draws
//! each glyph inside its own em box, but the terminal cell is a different size
//! (especially once line height is taken into account), so the strokes don't
//! reach the cell edges and adjacent cells show hairline gaps — most visibly at
//! corners and T/cross junctions.
//!
//! Instead we draw these glyphs ourselves as axis-aligned rectangles sized to
//! the exact terminal cell. A vertical stroke spans the full cell height, a
//! horizontal stroke the full width, and a corner is the union of two such
//! strokes — so they connect by construction at any cell size, font, or line
//! height. This is the same approach taken by Alacritty, kitty and WezTerm.
//!
//! The geometry table in [`shape`] is derived from the regular grammar of the
//! Unicode character names (e.g. "BOX DRAWINGS HEAVY VERTICAL AND LIGHT RIGHT")
//! by `scripts/gen_box_drawing.py`; re-run that script to regenerate it.
//!
//! Diagonals (U+2571..U+2573) are intentionally omitted: they cannot be drawn
//! cleanly with axis-aligned rectangles, so they fall through to the font.

/// Weight of one arm of a line-drawing glyph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Weight {
    Light,
    Heavy,
    Double,
}

/// A line-drawing glyph described by the weight of each of its four arms plus
/// the dash count (0 = solid) and whether the corner is rounded.
struct Spec {
    up: Option<Weight>,
    down: Option<Weight>,
    left: Option<Weight>,
    right: Option<Weight>,
    dashes: u8,
    arc: bool,
}

/// The geometry of a single box-drawing/block codepoint, in cell-relative terms.
enum Shape {
    /// Lines, corners, junctions, dashes and arcs.
    Lines(Spec),
    /// A solid rectangle, fractions of the cell (used for blocks and eighths).
    Rect { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// A full-cell fill at the given alpha (the shade characters).
    Shade(f32),
    /// Any combination of the four cell quadrants (the quadrant characters).
    Quadrants {
        ul: bool,
        ur: bool,
        ll: bool,
        lr: bool,
    },
}

/// A terminal cell's rectangle, in absolute pixel coordinates.
#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Cell {
    fn cx(&self) -> f32 {
        self.x + self.width / 2.0
    }
    fn cy(&self) -> f32 {
        self.y + self.height / 2.0
    }
}

/// A rectangle to fill, in absolute pixel coordinates, plus an alpha multiplier.
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub alpha: f32,
}

/// Returns `true` if [`render`] can draw `c`, i.e. the font glyph should be
/// suppressed in favor of our geometry. Callers must keep buffer suppression
/// and rendering in agreement, so both gate on this single predicate.
pub(crate) fn is_box_drawing(c: char) -> bool {
    shape(c).is_some()
}

/// Light and heavy stroke thicknesses (in pixels) for the given font size.
///
/// Heavy is roughly twice light; both are at least one pixel. Pixel snapping is
/// left to the caller (so the result is correct on HiDPI).
pub(crate) fn line_thickness(font_size: f32) -> (f32, f32) {
    let light = (font_size / 10.0).round().max(1.0);
    let heavy = (light * 2.0).max(light + 1.0);
    (light, heavy)
}

/// Emit the rectangles that draw `c` within `cell`, using the given
/// `light`/`heavy` stroke thicknesses. Coordinates are absolute pixels; `c`
/// must satisfy [`is_box_drawing`].
pub(crate) fn render(c: char, cell: Cell, light: f32, heavy: f32, mut emit: impl FnMut(Rect)) {
    let Some(shape) = shape(c) else {
        return;
    };
    let (x, y, w, h) = (cell.x, cell.y, cell.width, cell.height);

    let mut push = |x: f32, y: f32, width: f32, height: f32, alpha: f32| {
        if width > 0.0 && height > 0.0 {
            emit(Rect {
                x,
                y,
                width,
                height,
                alpha,
            });
        }
    };

    match shape {
        Shape::Rect { x0, y0, x1, y1 } => {
            push(x + x0 * w, y + y0 * h, (x1 - x0) * w, (y1 - y0) * h, 1.0);
        }
        Shape::Shade(alpha) => push(x, y, w, h, alpha),
        Shape::Quadrants { ul, ur, ll, lr } => {
            let (hw, hh) = (w / 2.0, h / 2.0);
            if ul {
                push(x, y, hw, hh, 1.0);
            }
            if ur {
                push(x + hw, y, hw, hh, 1.0);
            }
            if ll {
                push(x, y + hh, hw, hh, 1.0);
            }
            if lr {
                push(x + hw, y + hh, hw, hh, 1.0);
            }
        }
        Shape::Lines(spec) => render_lines(&spec, cell, light, heavy, &mut push),
    }
}

fn render_lines(
    spec: &Spec,
    cell: Cell,
    light: f32,
    heavy: f32,
    push: &mut impl FnMut(f32, f32, f32, f32, f32),
) {
    let (x, y, w, h) = (cell.x, cell.y, cell.width, cell.height);
    let (cx, cy) = (cell.cx(), cell.cy());
    let thick = |weight: Weight| match weight {
        Weight::Light | Weight::Double => light,
        Weight::Heavy => heavy,
    };

    if spec.arc {
        render_arc(spec, cell, light, push);
        return;
    }

    if spec.dashes > 0 {
        render_dashed(spec, cell, thick(weight_of(spec)), push);
        return;
    }

    let is_double = [spec.up, spec.down, spec.left, spec.right].contains(&Some(Weight::Double));
    if is_double {
        render_double(spec, cell, light, push);
        return;
    }

    // Single-rail regime: each arm is a half-bar from the cell edge to the
    // center with its own thickness, plus a central junction rectangle sized to
    // the thickest arms on each axis so corners fill without a notch.
    if let Some(weight) = spec.left {
        let t = thick(weight);
        push(x, cy - t / 2.0, cx - x, t, 1.0);
    }
    if let Some(weight) = spec.right {
        let t = thick(weight);
        push(cx, cy - t / 2.0, x + w - cx, t, 1.0);
    }
    if let Some(weight) = spec.up {
        let t = thick(weight);
        push(cx - t / 2.0, y, t, cy - y, 1.0);
    }
    if let Some(weight) = spec.down {
        let t = thick(weight);
        push(cx - t / 2.0, cy, t, y + h - cy, 1.0);
    }
    let vmax = max_thick(spec.up, spec.down, &thick);
    let hmax = max_thick(spec.left, spec.right, &thick);
    if vmax > 0.0 && hmax > 0.0 {
        push(cx - vmax / 2.0, cy - hmax / 2.0, vmax, hmax, 1.0);
    }
}

/// Render a glyph that has at least one double-weight arm.
///
/// Double lines are drawn as two parallel light rails offset from each axis. At
/// a junction, a rail is interrupted across the perpendicular span when a
/// single perpendicular arm passes through it (e.g. the inner line of a corner,
/// or the broken rail of a tee), which produces the characteristic open center.
fn render_double(spec: &Spec, cell: Cell, t: f32, push: &mut impl FnMut(f32, f32, f32, f32, f32)) {
    let (x, y, w, h) = (cell.x, cell.y, cell.width, cell.height);
    let (cx, cy) = (cell.cx(), cell.cy());
    let off = t.max(1.0);
    let (up, down, left, right) = (
        spec.up.is_some(),
        spec.down.is_some(),
        spec.left.is_some(),
        spec.right.is_some(),
    );
    let h_double = spec.left == Some(Weight::Double) || spec.right == Some(Weight::Double);
    let v_double = spec.up == Some(Weight::Double) || spec.down == Some(Weight::Double);

    let hy: &[f32] = if h_double {
        &[cy - off, cy + off]
    } else {
        &[cy]
    };
    let vx: &[f32] = if v_double {
        &[cx - off, cx + off]
    } else {
        &[cx]
    };
    let (vx_min, vx_max) = (vx[0], vx[vx.len() - 1]);
    let (hy_min, hy_max) = (hy[0], hy[hy.len() - 1]);

    // Horizontal rails exist only if there is a horizontal arm; likewise the
    // vertical rails. (Otherwise a straight ═ would sprout a vertical stub.)
    if left || right {
        for (i, &ry) in hy.iter().enumerate() {
            let top_rail = h_double && i == 0;
            let bottom_rail = h_double && i == 1;
            let start = if left {
                x
            } else if up || down {
                vx_min - t / 2.0
            } else {
                cx
            };
            let end = if right {
                x + w
            } else if up || down {
                vx_max + t / 2.0
            } else {
                cx
            };
            let gap = (top_rail && up && !down) || (bottom_rail && down && !up);
            if gap {
                let (g0, g1) = (vx_min - t / 2.0, vx_max + t / 2.0);
                push(start, ry - t / 2.0, g0 - start, t, 1.0);
                push(g1, ry - t / 2.0, end - g1, t, 1.0);
            } else {
                push(start, ry - t / 2.0, end - start, t, 1.0);
            }
        }
    }

    // Vertical rails.
    if up || down {
        for (i, &rx) in vx.iter().enumerate() {
            let left_rail = v_double && i == 0;
            let right_rail = v_double && i == 1;
            let start = if up {
                y
            } else if left || right {
                hy_min - t / 2.0
            } else {
                cy
            };
            let end = if down {
                y + h
            } else if left || right {
                hy_max + t / 2.0
            } else {
                cy
            };
            let gap = (left_rail && left && !right) || (right_rail && right && !left);
            if gap {
                let (g0, g1) = (hy_min - t / 2.0, hy_max + t / 2.0);
                push(rx - t / 2.0, start, t, g0 - start, 1.0);
                push(rx - t / 2.0, g1, t, end - g1, 1.0);
            } else {
                push(rx - t / 2.0, start, t, end - start, 1.0);
            }
        }
    }
}

/// Render a dashed straight line (the only glyphs with `dashes > 0`).
fn render_dashed(spec: &Spec, cell: Cell, t: f32, push: &mut impl FnMut(f32, f32, f32, f32, f32)) {
    let (x, y, w, h) = (cell.x, cell.y, cell.width, cell.height);
    let (cx, cy) = (cell.cx(), cell.cy());
    let n = spec.dashes as f32;
    let horizontal = spec.left.is_some() || spec.right.is_some();
    let length = if horizontal { w } else { h };
    let seg = length / n;
    let dash = seg * 0.6;
    let gap = (seg - dash) / 2.0;
    for i in 0..spec.dashes {
        let offset = i as f32 * seg + gap;
        if horizontal {
            push(x + offset, cy - t / 2.0, dash, t, 1.0);
        } else {
            push(cx - t / 2.0, y + offset, t, dash, 1.0);
        }
    }
}

/// Render a rounded corner (the arc characters ╭ ╮ ╯ ╰).
fn render_arc(spec: &Spec, cell: Cell, t: f32, push: &mut impl FnMut(f32, f32, f32, f32, f32)) {
    let (x, y, w, h) = (cell.x, cell.y, cell.width, cell.height);
    let (cx, cy) = (cell.cx(), cell.cy());
    let sign_h = if spec.right.is_some() { 1.0 } else { -1.0 };
    let sign_v = if spec.down.is_some() { 1.0 } else { -1.0 };
    let r = (w / 2.0).min(h / 2.0);
    let (ox, oy) = (cx + sign_h * r, cy + sign_v * r);

    // Straight remainder of each arm beyond the arc's tangent points.
    if spec.right.is_some() {
        push(cx + r, cy - t / 2.0, x + w - (cx + r), t, 1.0);
    } else {
        push(x, cy - t / 2.0, cx - r - x, t, 1.0);
    }
    if spec.down.is_some() {
        push(cx - t / 2.0, cy + r, t, y + h - (cy + r), 1.0);
    } else {
        push(cx - t / 2.0, y, t, cy - r - y, 1.0);
    }

    // Quarter arc as a sequence of small squares, sweeping the 90° between the
    // two tangent directions from the center O: (0, -sign_v) and (-sign_h, 0).
    let a0 = (-sign_v).atan2(0.0);
    let delta = if sign_h * sign_v > 0.0 {
        -std::f32::consts::FRAC_PI_2
    } else {
        std::f32::consts::FRAC_PI_2
    };
    let steps = (r * 2.0).ceil().max(1.0) as usize;
    for i in 0..=steps {
        let a = a0 + delta * (i as f32 / steps as f32);
        let px = ox + r * a.cos();
        let py = oy + r * a.sin();
        push(px - t / 2.0, py - t / 2.0, t, t, 1.0);
    }
}

fn weight_of(spec: &Spec) -> Weight {
    spec.up
        .or(spec.down)
        .or(spec.left)
        .or(spec.right)
        .unwrap_or(Weight::Light)
}

fn max_thick(a: Option<Weight>, b: Option<Weight>, thick: &impl Fn(Weight) -> f32) -> f32 {
    let f = |w: Option<Weight>| w.map(thick).unwrap_or(0.0);
    f(a).max(f(b))
}

#[rustfmt::skip]
fn shape(c: char) -> Option<Shape> {
    Some(match c {
        '─' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '━' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '│' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: None, dashes: 0, arc: false }),
        '┃' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: None, dashes: 0, arc: false }),
        '┄' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 3, arc: false }),
        '┅' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 3, arc: false }),
        '┆' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: None, dashes: 3, arc: false }),
        '┇' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: None, dashes: 3, arc: false }),
        '┈' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 4, arc: false }),
        '┉' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 4, arc: false }),
        '┊' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: None, dashes: 4, arc: false }),
        '┋' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: None, dashes: 4, arc: false }),
        '┌' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┍' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┎' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┏' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┐' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┑' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┒' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┓' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '└' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┕' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┖' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┗' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┘' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┙' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┚' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┛' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '├' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┝' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┞' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┟' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┠' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '┡' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┢' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┣' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┤' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┥' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┦' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┧' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┨' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '┩' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┪' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┫' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '┬' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┭' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┮' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┯' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┰' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┱' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┲' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┳' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┴' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┵' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┶' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┷' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┸' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┹' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┺' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┻' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┼' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┽' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '┾' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '┿' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╀' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╁' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╂' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╃' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╄' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╅' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╆' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╇' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╈' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╉' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╊' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╋' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╌' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 2, arc: false }),
        '╍' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: Some(Weight::Heavy), dashes: 2, arc: false }),
        '╎' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: None, dashes: 2, arc: false }),
        '╏' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Heavy), left: None, right: None, dashes: 2, arc: false }),
        '═' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '║' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: None, right: None, dashes: 0, arc: false }),
        '╒' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╓' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '╔' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╕' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╖' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '╗' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╘' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╙' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '╚' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╛' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╜' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '╝' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╞' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╟' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '╠' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: None, right: Some(Weight::Double), dashes: 0, arc: false }),
        '╡' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╢' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '╣' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: Some(Weight::Double), right: None, dashes: 0, arc: false }),
        '╤' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╥' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╦' => Shape::Lines(Spec { up: None, down: Some(Weight::Double), left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╧' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╨' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╩' => Shape::Lines(Spec { up: Some(Weight::Double), down: None, left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╪' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Light), left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╫' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: Some(Weight::Light), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╬' => Shape::Lines(Spec { up: Some(Weight::Double), down: Some(Weight::Double), left: Some(Weight::Double), right: Some(Weight::Double), dashes: 0, arc: false }),
        '╭' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: None, right: Some(Weight::Light), dashes: 0, arc: true }),
        '╮' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: Some(Weight::Light), right: None, dashes: 0, arc: true }),
        '╯' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: Some(Weight::Light), right: None, dashes: 0, arc: true }),
        '╰' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: None, right: Some(Weight::Light), dashes: 0, arc: true }),
        '╴' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: None, dashes: 0, arc: false }),
        '╵' => Shape::Lines(Spec { up: Some(Weight::Light), down: None, left: None, right: None, dashes: 0, arc: false }),
        '╶' => Shape::Lines(Spec { up: None, down: None, left: None, right: Some(Weight::Light), dashes: 0, arc: false }),
        '╷' => Shape::Lines(Spec { up: None, down: Some(Weight::Light), left: None, right: None, dashes: 0, arc: false }),
        '╸' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: None, dashes: 0, arc: false }),
        '╹' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: None, left: None, right: None, dashes: 0, arc: false }),
        '╺' => Shape::Lines(Spec { up: None, down: None, left: None, right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╻' => Shape::Lines(Spec { up: None, down: Some(Weight::Heavy), left: None, right: None, dashes: 0, arc: false }),
        '╼' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Light), right: Some(Weight::Heavy), dashes: 0, arc: false }),
        '╽' => Shape::Lines(Spec { up: Some(Weight::Light), down: Some(Weight::Heavy), left: None, right: None, dashes: 0, arc: false }),
        '╾' => Shape::Lines(Spec { up: None, down: None, left: Some(Weight::Heavy), right: Some(Weight::Light), dashes: 0, arc: false }),
        '╿' => Shape::Lines(Spec { up: Some(Weight::Heavy), down: Some(Weight::Light), left: None, right: None, dashes: 0, arc: false }),
        '▀' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 0.5 },
        '▁' => Shape::Rect { x0: 0.0, y0: 0.875, x1: 1.0, y1: 1.0 },
        '▂' => Shape::Rect { x0: 0.0, y0: 0.75, x1: 1.0, y1: 1.0 },
        '▃' => Shape::Rect { x0: 0.0, y0: 0.625, x1: 1.0, y1: 1.0 },
        '▄' => Shape::Rect { x0: 0.0, y0: 0.5, x1: 1.0, y1: 1.0 },
        '▅' => Shape::Rect { x0: 0.0, y0: 0.375, x1: 1.0, y1: 1.0 },
        '▆' => Shape::Rect { x0: 0.0, y0: 0.25, x1: 1.0, y1: 1.0 },
        '▇' => Shape::Rect { x0: 0.0, y0: 0.125, x1: 1.0, y1: 1.0 },
        '█' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
        '▉' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.875, y1: 1.0 },
        '▊' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.75, y1: 1.0 },
        '▋' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.625, y1: 1.0 },
        '▌' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 1.0 },
        '▍' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.375, y1: 1.0 },
        '▎' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.25, y1: 1.0 },
        '▏' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 0.125, y1: 1.0 },
        '▐' => Shape::Rect { x0: 0.5, y0: 0.0, x1: 1.0, y1: 1.0 },
        '░' => Shape::Shade(0.25),
        '▒' => Shape::Shade(0.5),
        '▓' => Shape::Shade(0.75),
        '▔' => Shape::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 0.125 },
        '▕' => Shape::Rect { x0: 0.875, y0: 0.0, x1: 1.0, y1: 1.0 },
        '▖' => Shape::Quadrants { ul: false, ur: false, ll: true, lr: false },
        '▗' => Shape::Quadrants { ul: false, ur: false, ll: false, lr: true },
        '▘' => Shape::Quadrants { ul: true, ur: false, ll: false, lr: false },
        '▙' => Shape::Quadrants { ul: true, ur: false, ll: true, lr: true },
        '▚' => Shape::Quadrants { ul: true, ur: false, ll: false, lr: true },
        '▛' => Shape::Quadrants { ul: true, ur: true, ll: true, lr: false },
        '▜' => Shape::Quadrants { ul: true, ur: true, ll: false, lr: true },
        '▝' => Shape::Quadrants { ul: false, ur: true, ll: false, lr: false },
        '▞' => Shape::Quadrants { ul: false, ur: true, ll: true, lr: false },
        '▟' => Shape::Quadrants { ul: false, ur: true, ll: true, lr: true },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_matches_table() {
        // Everything in the Box Drawing + Block Elements range that we draw must
        // report as box drawing, and diagonals must not.
        assert!(is_box_drawing('┼'));
        assert!(is_box_drawing('╬'));
        assert!(is_box_drawing('█'));
        assert!(is_box_drawing('░'));
        assert!(!is_box_drawing('╱')); // diagonal: left to the font
        assert!(!is_box_drawing('A'));
    }

    #[test]
    fn cross_covers_center() {
        // A light cross should produce strokes that reach all four edges.
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        let cell = Cell {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 20.0,
        };
        render('┼', cell, 1.0, 2.0, |r| {
            min_x = min_x.min(r.x);
            max_x = max_x.max(r.x + r.width);
            min_y = min_y.min(r.y);
            max_y = max_y.max(r.y + r.height);
        });
        assert_eq!(min_x, 0.0);
        assert_eq!(max_x, 10.0);
        assert_eq!(min_y, 0.0);
        assert_eq!(max_y, 20.0);
    }

    /// Rasterizes a sheet of box-drawing samples to `/tmp/box_sample.ppm` for
    /// visual inspection. Ignored by default; run with
    /// `cargo test --bin cosmic-term -- --ignored render_sample_sheet`.
    #[test]
    #[ignore]
    fn render_sample_sheet() {
        let rows = [
            "┌───┬───┐  ┏━━━┳━━━┓  ╔═══╦═══╗  ╭───┬───╮",
            "│   │   │  ┃   ┃   ┃  ║   ║   ║  │   │   │",
            "├───┼───┤  ┣━━━╋━━━┫  ╠═══╬═══╣  ├───┼───┤",
            "│   │   │  ┃   ┃   ┃  ║   ║   ║  │   │   │",
            "└───┴───┘  ┗━━━┻━━━┛  ╚═══╩═══╝  ╰───┴───╯",
            "                                          ",
            "█▉▊▋▌▍▎▏  ░▒▓  ▁▂▃▄▅▆▇█  ▖▗▘▙▚▛▜▝▞▟  ┄┅┆┇",
            "                                          ",
            "│ │      ─╮│      │╭─      │ │             ",
            "├─╮      ─┤│      │├─      ╰─┤             ",
            "│ │      ─╯│      │╰─      │ │             ",
        ];
        let (cw, ch) = (16usize, 28usize);
        let cols = rows.iter().map(|r| r.chars().count()).max().unwrap();
        let (iw, ih) = (cols * cw, rows.len() * ch);
        let mut buf = vec![0u8; iw * ih * 3];
        let (light, heavy) = line_thickness(20.0);

        for (ry, row) in rows.iter().enumerate() {
            for (cxi, c) in row.chars().enumerate() {
                if !is_box_drawing(c) {
                    continue;
                }
                let cell = Cell {
                    x: (cxi * cw) as f32,
                    y: (ry * ch) as f32,
                    width: cw as f32,
                    height: ch as f32,
                };
                render(c, cell, light, heavy, |r| {
                    // Snap to pixels and blend white at the rect's alpha.
                    let x0 = r.x.round().max(0.0) as usize;
                    let y0 = r.y.round().max(0.0) as usize;
                    let x1 = ((r.x + r.width).round() as usize).min(iw);
                    let y1 = ((r.y + r.height).round() as usize).min(ih);
                    let v = (255.0 * r.alpha) as u8;
                    for py in y0..y1 {
                        for px in x0..x1 {
                            let i = (py * iw + px) * 3;
                            buf[i] = buf[i].max(v);
                            buf[i + 1] = buf[i + 1].max(v);
                            buf[i + 2] = buf[i + 2].max(v);
                        }
                    }
                });
            }
        }

        let mut out = format!("P6\n{iw} {ih}\n255\n").into_bytes();
        out.extend_from_slice(&buf);
        std::fs::write("/tmp/box_sample.ppm", out).unwrap();
    }

    #[test]
    fn vertical_spans_full_height() {
        // The key property for seamless tiling: a vertical bar fills the cell
        // top to bottom so it meets its neighbors above and below.
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        let cell = Cell {
            x: 5.0,
            y: 7.0,
            width: 10.0,
            height: 20.0,
        };
        render('│', cell, 1.0, 2.0, |r| {
            min_y = min_y.min(r.y);
            max_y = max_y.max(r.y + r.height);
        });
        assert_eq!(min_y, 7.0);
        assert_eq!(max_y, 27.0);
    }
}
