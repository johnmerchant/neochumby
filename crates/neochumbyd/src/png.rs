//! A PNG of a Chumby's screen: 8-bit RGB, its zlib stream made of stored
//! (uncompressed) deflate blocks. A 320x240 screen comes to about 230 KB;
//! nothing reads these but people, so compressing them isn't worth a
//! dependency.

/// The PNG of `pixels`: `width` x `height` RGB565, little-endian, row by
/// row (the framebuffer's layout). Each channel is widened by repeating
/// its top bits, so white stays white.
pub fn rgb565_le(width: u16, height: u16, pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(pixels.len(), w * h * 2, "a whole screen");
    // Each row: filter type 0 (none), then RGB.
    let mut raw = Vec::with_capacity(h * (1 + 3 * w));
    for row in pixels.chunks_exact(2 * w.max(1)).take(h) {
        raw.push(0);
        for p in row.as_chunks::<2>().0 {
            let v = u16::from_le_bytes(*p);
            let (r, g, b) = ((v >> 11) as u8 & 0x1f, (v >> 5) as u8 & 0x3f, v as u8 & 0x1f);
            raw.extend_from_slice(&[(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]);
        }
    }

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    // 8 bits, RGB, deflate, no filtering beyond per-row, not interlaced.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream (RFC 1950) of stored deflate blocks (RFC 1951 3.2.4).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut z = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 11);
    // Deflate, 32 KB window, no dictionary; 0x7801 is a multiple of 31.
    z.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = data.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(b) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let n = b.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The image data of a PNG this module wrote, checked on the way:
    /// (width, height, the rows as filtered).
    pub fn read(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut at = 8;
        let (mut w, mut h, mut idat) = (0, 0, Vec::new());
        let be = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap());
        while at < png.len() {
            let n = be(&png[at..at + 4]) as usize;
            let (kind, data) = (&png[at + 4..at + 8], &png[at + 8..at + 8 + n]);
            assert_eq!(be(&png[at + 8 + n..at + 12 + n]), crc32(&png[at + 4..at + 8 + n]));
            match kind {
                b"IHDR" => (w, h) = (be(&data[..4]), be(&data[4..8])),
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            at += 12 + n;
        }
        // Undo the stored blocks.
        assert_eq!(&idat[..2], [0x78, 0x01]);
        let (mut raw, mut i) = (Vec::new(), 2);
        loop {
            let last = idat[i] & 1 == 1;
            let n = u16::from_le_bytes([idat[i + 1], idat[i + 2]]) as usize;
            assert_eq!(!(n as u16), u16::from_le_bytes([idat[i + 3], idat[i + 4]]));
            raw.extend_from_slice(&idat[i + 5..i + 5 + n]);
            i += 5 + n;
            if last {
                break;
            }
        }
        assert_eq!(be(&idat[i..i + 4]), adler32(&raw));
        (w, h, raw)
    }

    #[test]
    fn a_screen_as_png() {
        // White, red, green, blue, black: then the same again.
        let px: Vec<u8> =
            [0xffffu16, 0xf800, 0x07e0, 0x001f, 0].repeat(320 * 240 / 5).iter().flat_map(|p| p.to_le_bytes()).collect();
        let png = rgb565_le(320, 240, &px);
        let (w, h, raw) = read(&png);
        assert_eq!((w, h), (320, 240));
        assert_eq!(raw.len(), 240 * (1 + 320 * 3));
        assert_eq!(&raw[..16], [0, 255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }
}
