//! A PNG of a Chumby's screen: 8-bit RGB.

/// The PNG of `pixels`: `width` x `height` RGB565, little-endian, row by
/// row (the framebuffer's layout). Each channel is widened by repeating
/// its top bits, so white stays white.
pub fn rgb565_le(width: u16, height: u16, pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(pixels.len(), w * h * 2, "a whole screen");
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in pixels.as_chunks::<2>().0 {
        let v = u16::from_le_bytes(*p);
        let (r, g, b) = ((v >> 11) as u8 & 0x1f, (v >> 5) as u8 & 0x3f, v as u8 & 0x1f);
        rgb.extend_from_slice(&[(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]);
    }

    let mut png = Vec::new();
    let mut e = ::png::Encoder::new(&mut png, width.into(), height.into());
    e.set_color(::png::ColorType::Rgb);
    e.set_depth(::png::BitDepth::Eight);
    // Into a Vec, of a size checked above: it can't fail.
    let mut w = e.write_header().expect("PNG header");
    w.write_image_data(&rgb).expect("PNG data");
    w.finish().expect("PNG end");
    png
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A PNG's (width, height, RGB pixels), decoded.
    pub fn read(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut r = ::png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut rgb = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut rgb).unwrap();
        assert_eq!((info.color_type, info.bit_depth), (::png::ColorType::Rgb, ::png::BitDepth::Eight));
        rgb.truncate(info.buffer_size());
        (info.width, info.height, rgb)
    }

    #[test]
    fn a_screen_as_png() {
        // White, red, green, blue, black: then the same again.
        let px: Vec<u8> =
            [0xffffu16, 0xf800, 0x07e0, 0x001f, 0].repeat(320 * 240 / 5).iter().flat_map(|p| p.to_le_bytes()).collect();
        let png = rgb565_le(320, 240, &px);
        let (w, h, rgb) = read(&png);
        assert_eq!((w, h), (320, 240));
        assert_eq!(rgb.len(), 240 * 320 * 3);
        assert_eq!(&rgb[..15], [255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
    }
}
