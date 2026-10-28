//! Box-drawing and block characters, drawn rather than taken from the font
//! (as terminals do): embedded-graphics' 5x8 has Latin-1 only, and these
//! must meet their neighbours exactly. Each is a 5x8 bitmap, a row a byte,
//! its top five bits the pixels from the left, laid out as misc-fixed's
//! own: light lines on row 3 and column 2.

use super::grid::{CELL_H, CELL_W};

#[derive(Clone, Copy, PartialEq)]
enum Line {
    No,
    Light,
    Heavy,
    Double,
}

use Line::{Double as D, Heavy as H, Light as L, No as N};

/// The lines of a box-drawing character: up, down, left, right.
fn lines(ch: char) -> Option<[Line; 4]> {
    Some(match ch {
        '─' | '┄' | '┈' | '╌' => [N, N, L, L],
        '━' | '┅' | '┉' | '╍' => [N, N, H, H],
        '│' | '┆' | '┊' | '╎' => [L, L, N, N],
        '┃' | '┇' | '┋' | '╏' => [H, H, N, N],
        '┌' | '╭' => [N, L, N, L],
        '┐' | '╮' => [N, L, L, N],
        '└' | '╰' => [L, N, N, L],
        '┘' | '╯' => [L, N, L, N],
        '┏' => [N, H, N, H],
        '┓' => [N, H, H, N],
        '┗' => [H, N, N, H],
        '┛' => [H, N, H, N],
        '├' => [L, L, N, L],
        '┤' => [L, L, L, N],
        '┬' => [N, L, L, L],
        '┴' => [L, N, L, L],
        '┼' => [L, L, L, L],
        '┣' => [H, H, N, H],
        '┫' => [H, H, H, N],
        '┳' => [N, H, H, H],
        '┻' => [H, N, H, H],
        '╋' => [H, H, H, H],
        '═' => [N, N, D, D],
        '║' => [D, D, N, N],
        '╔' => [N, D, N, D],
        '╗' => [N, D, D, N],
        '╚' => [D, N, N, D],
        '╝' => [D, N, D, N],
        '╠' => [D, D, N, D],
        '╣' => [D, D, D, N],
        '╦' => [N, D, D, D],
        '╩' => [D, N, D, D],
        '╬' => [D, D, D, D],
        '╴' => [N, N, L, N],
        '╵' => [L, N, N, N],
        '╶' => [N, N, N, L],
        '╷' => [N, L, N, N],
        '╸' => [N, N, H, N],
        '╹' => [H, N, N, N],
        '╺' => [N, N, N, H],
        '╻' => [N, H, N, N],
        _ => return None,
    })
}

/// The rows a horizontal line of this weight takes, and the columns a
/// vertical one does.
fn rows(l: Line) -> &'static [usize] {
    match l {
        N => &[],
        L => &[3],
        H => &[3, 4],
        D => &[2, 4],
    }
}

fn cols(l: Line) -> &'static [usize] {
    match l {
        N => &[],
        L => &[2],
        H => &[2, 3],
        D => &[1, 3],
    }
}

fn box_glyph([up, down, left, right]: [Line; 4]) -> [u8; CELL_H] {
    let mut g = [0u8; CELL_H];
    let mut set = |x: usize, y: usize| g[y] |= 0x80 >> x;
    // Each arm runs from the cell's edge to the far side of the lines it
    // meets, so that they join.
    let vcols: Vec<usize> = [up, down].iter().flat_map(|&l| cols(l).iter().copied()).collect();
    let hrows: Vec<usize> = [left, right].iter().flat_map(|&l| rows(l).iter().copied()).collect();
    let (x0, x1) = (vcols.iter().copied().min().unwrap_or(2), vcols.iter().copied().max().unwrap_or(2));
    let (y0, y1) = (hrows.iter().copied().min().unwrap_or(3), hrows.iter().copied().max().unwrap_or(3));
    for &r in rows(left) {
        (0..=x1).for_each(|x| set(x, r));
    }
    for &r in rows(right) {
        (x0..CELL_W).for_each(|x| set(x, r));
    }
    for &c in cols(up) {
        (0..=y1).for_each(|y| set(c, y));
    }
    for &c in cols(down) {
        (y0..CELL_H).for_each(|y| set(c, y));
    }
    g
}

/// The block elements, U+2580 to U+259F.
fn block_glyph(ch: char) -> Option<[u8; CELL_H]> {
    // Columns 0..3 are the left half, as misc-fixed's ▌; rows 0..4 the top.
    const LEFT: u8 = 0xe0;
    const RIGHT: u8 = 0x18;
    let rows = |from: usize, to: usize, bits: u8| {
        let mut g = [0u8; CELL_H];
        g[from..to].fill(bits);
        g
    };
    // Quadrants: top-left, top-right, bottom-left, bottom-right.
    let quads = |tl: bool, tr: bool, bl: bool, br: bool| {
        let top = if tl { LEFT } else { 0 } | if tr { RIGHT } else { 0 };
        let bottom = if bl { LEFT } else { 0 } | if br { RIGHT } else { 0 };
        let mut g = [top; CELL_H];
        g[CELL_H / 2..].fill(bottom);
        g
    };
    let n = ch as usize;
    Some(match ch {
        '▀' => rows(0, 4, 0xf8),
        // Lower one to seven eighths: one row each.
        '▁'..='▇' => rows(CELL_H - (n - 0x2580), CELL_H, 0xf8),
        '█' => rows(0, CELL_H, 0xf8),
        // Left seven eighths to one eighth, to the nearest column.
        '▉'..='▏' => {
            let w = ((0x2590 - n) * CELL_W + 4) / 8;
            rows(0, CELL_H, !(0xffu8 >> w) & 0xf8)
        }
        '▐' => rows(0, CELL_H, RIGHT),
        '░' => [0xa8, 0x00, 0x50, 0x00, 0xa8, 0x00, 0x50, 0x00],
        '▒' => [0x50, 0xa8, 0x50, 0xa8, 0x50, 0xa8, 0x50, 0xa8],
        '▓' => [0xf8, 0x50, 0xf8, 0xa8, 0xf8, 0x50, 0xf8, 0xa8],
        '▔' => rows(0, 1, 0xf8),
        '▕' => rows(0, CELL_H, 0x08),
        '▖' => quads(false, false, true, false),
        '▗' => quads(false, false, false, true),
        '▘' => quads(true, false, false, false),
        '▙' => quads(true, false, true, true),
        '▚' => quads(true, false, false, true),
        '▛' => quads(true, true, true, false),
        '▜' => quads(true, true, false, true),
        '▝' => quads(false, true, false, false),
        '▞' => quads(false, true, true, false),
        '▟' => quads(false, true, true, true),
        _ => return None,
    })
}

/// The bitmap for `ch` if it is one drawn here.
pub fn glyph(ch: char) -> Option<[u8; CELL_H]> {
    lines(ch).map(box_glyph).or_else(|| block_glyph(ch))
}
