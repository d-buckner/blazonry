//! 2-bit BMP writer — TRMNL devices want an 800x480 2-bit (4 greyscale
//! levels) bottom-up BMP, base64'd into the /api/display JSON response.

pub const LEVELS: [u8; 4] = [0, 85, 170, 255]; // index 0 = black .. 3 = white

/// `pixels` is row-major top-down, one 2-bit *index* per byte (0..=3,
/// 0=black per LEVELS). Encodes a bottom-up 2-bit BMP with a greyscale palette.
pub fn encode_2bit_bmp(width: usize, height: usize, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height);
    let row_bytes = (width + 3) / 4; // 2-bit rows padded to 4-byte boundary
    let image_size = row_bytes * height;
    let palette_size = 4 * 4; // 4 entries BGRA
    let file_size = 14 + 40 + palette_size + image_size;

    let mut out = Vec::with_capacity(file_size);
    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    let data_offset = (14 + 40 + palette_size) as u32;
    out.extend_from_slice(&data_offset.to_le_bytes());

    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&2u16.to_le_bytes()); // bpp
    out.extend_from_slice(&[0; 24]); // compression, size, res, counts

    // Palette: index 0 = black, 3 = white (bottom-up rows, but palette order
    // is index order regardless).
    for &v in &LEVELS {
        out.extend_from_slice(&[v, v, v, 0]);
    }

    // Pixel data, bottom-up, 4 pixels per byte, high bits first.
    for y in (0..height).rev() {
        let row = &pixels[y * width..(y + 1) * width];
        for chunk in row.chunks(4) {
            let mut byte = 0u8;
            for (i, &p) in chunk.iter().enumerate() {
                byte |= (p & 3) << (6 - 2 * i);
            }
            out.push(byte);
        }
        // pad row to 4 bytes
        let pad = row_bytes - (width + 3) / 4; // always 0 here; row_bytes already padded
        let _ = pad;
        let written = (width + 3) / 4;
        for _ in written..row_bytes {
            out.push(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_size() {
        // width 6 -> row_bytes 2 (6px = 12 bits -> 2 bytes)
        let px = vec![0u8; 6 * 2];
        let bmp = encode_2bit_bmp(6, 2, &px);
        assert_eq!(&bmp[0..2], b"BM");
        let file_size = u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize;
        assert_eq!(file_size, bmp.len());
        let bpp = u16::from_le_bytes(bmp[28..30].try_into().unwrap());
        assert_eq!(bpp, 2);
        // first data byte = bottom row, first 4 px all black
        assert_eq!(bmp[70], 0);
    }

    #[test]
    fn bit_packing_high_bits_first() {
        let px = vec![3u8, 1, 2, 0, 0, 0, 0, 0]; // top row: 3,1,2,0
        let bmp = encode_2bit_bmp(8, 1, &px);
        // bottom-up: single row is also bottom row
        assert_eq!(bmp[70], 0b11_01_10_00);
        assert_eq!(bmp[71], 0);
    }
}
