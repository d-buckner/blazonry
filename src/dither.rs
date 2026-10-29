//! Greyscale -> 4-level dithering. E-ink panels take 4 grey levels; antialiased
//! text and charts must be dithered or they band badly. Floyd–Steinberg on the
//! 4-level lattice.

/// Map a greyscale 0..=255 buffer to 2-bit indices (0=black..3=white) using
/// error-diffusion dithering. Output: one byte per pixel.
pub fn floyd_steinberg_4(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut buf: Vec<i32> = src.iter().map(|&v| v as i32).collect();
    let mut out = vec![0u8; width * height];
    let levels = [0i32, 85, 170, 255];

    let nearest = |v: i32| -> (u8, i32) {
        let mut best = (0u8, i32::MAX);
        for (i, &l) in levels.iter().enumerate() {
            let d = (v - l).abs();
            if d < best.1 {
                best = (i as u8, d);
            }
        }
        best
    };

    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let old = buf[i].clamp(0, 255);
            let (idx, _) = nearest(old);
            out[i] = idx;
            let err = old - levels[idx as usize];
            // standard FS kernel, clipped to bounds
            if x + 1 < width {
                buf[i + 1] += err * 7 / 16;
            }
            if y + 1 < height {
                if x > 0 {
                    buf[i + width - 1] += err * 3 / 16;
                }
                buf[i + width] += err * 5 / 16;
                if x + 1 < width {
                    buf[i + width + 1] += err * 1 / 16;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_greys_map_to_nearest_level() {
        let src = vec![0u8, 40, 128, 210, 255];
        let out = floyd_steinberg_4(&src, 5, 1);
        assert_eq!(out, vec![0, 0, 2, 2, 3]); // 128 is nearer 170 than 85
    }

    #[test]
    fn mid_grey_dithers_between_levels() {
        // 128 sits between 85 and 170; a field of it must produce both levels
        let src = vec![128u8; 64 * 64];
        let out = floyd_steinberg_4(&src, 64, 64);
        let ones = out.iter().filter(|&&v| v == 1).count();
        let twos = out.iter().filter(|&&v| v == 2).count();
        assert!(ones > 100 && twos > 100, "expected dither mix, got {ones}/{twos}");
        assert!(out.iter().all(|&v| v <= 3));
    }
}
