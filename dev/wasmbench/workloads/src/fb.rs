//! The framebuffer-style memory kernel: 320x240 RGB565 buffers, drawn as an
//! app would, in four parts that the `fb` binary times one by one:
//!
//!   fill   every pixel written by a loop (a gradient that changes per frame)
//!   copy   the whole back buffer copied to the front (`copy_from_slice`,
//!          which is memcpy natively and memory.copy under wasm with bulk
//!          memory)
//!   blit   a 64x64 sprite copied row by row to 48 places on the frame
//!   rect   solid rectangles: 32 of 80x60, one u16 at a time
//!
//! The buffers are the caller's slices, `W * H` pixels each, row by row.

/// The frame's width in pixels.
pub const W: usize = 320;
/// The frame's height in pixels.
pub const H: usize = 240;
/// The sprite's side in pixels.
pub const S: usize = 64;

/// The buffer's checksum: each pixel in turn, times 31.
pub fn sum(b: &[u16]) -> u32 {
    b.iter().fold(0u32, |s, &p| s.wrapping_mul(31).wrapping_add(u32::from(p)))
}

/// Draws `frames` frames of a moving gradient into `back`, every pixel.
pub fn fill(back: &mut [u16], frames: i32) {
    for f in 0..frames {
        for (y, row) in back.as_chunks_mut::<W>().0.iter_mut().enumerate() {
            let y = y as i32;
            for (x, p) in row.iter_mut().enumerate() {
                let x = x as i32;
                *p = (((x + f) >> 3) << 11 | ((y + f) >> 2) << 5 | (x ^ y) >> 4) as u16;
            }
        }
    }
}

/// Copies `back` to `front` once a frame, after marking pixel `f` of it.
pub fn copy(back: &mut [u16], front: &mut [u16], frames: i32) {
    for f in 0..frames {
        back[f as usize] = f as u16;
        front.copy_from_slice(back);
    }
}

/// The sprite: pixel `i` is `i * 7`.
pub fn sprite(sprite: &mut [u16]) {
    for (i, p) in sprite.iter_mut().enumerate() {
        *p = (i * 7) as u16;
    }
}

/// Copies `sprite` to 48 places on `front` a frame, a row at a time.
pub fn blit(front: &mut [u16], sprite: &[u16], frames: i32) {
    let (w, h) = (W as i32, H as i32);
    for f in 0..frames {
        for n in 0..48 {
            let ox = ((n * 37 + f) % (w - S as i32)) as usize;
            let oy = ((n * 23 + f) % (h - S as i32)) as usize;
            for (y, src) in sprite.as_chunks::<S>().0.iter().enumerate() {
                let at = (oy + y) * W + ox;
                front[at..at + S].copy_from_slice(src);
            }
        }
    }
}

/// Fills 32 rectangles of 80x60 on `front` a frame, a pixel at a time.
pub fn rect(front: &mut [u16], frames: i32) {
    let (w, h) = (W as i32, H as i32);
    for f in 0..frames {
        for n in 0..32 {
            let ox = ((n * 53 + f) % (w - 80)) as usize;
            let oy = ((n * 29 + f) % (h - 60)) as usize;
            let c = (n * 2048 + f) as u16;
            for row in front[oy * W + ox..].chunks_mut(W).take(60) {
                for p in &mut row[..80] {
                    *p = c;
                }
            }
        }
    }
}
