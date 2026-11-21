//! Reading a frame's pixels whatever its layout: RGB as it is, YUV (YUYV,
//! NV12) turned into RGB on the way. Frames stay in the layout the camera
//! gave until something needs RGB on the CPU (a photo, a merge, a
//! histogram), so the viewfinder and the recorder take YUV as it is.

use crate::backend::{PixelLayout, YuvColor};

/// YCbCr to RGB in 16.16 fixed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Coeffs {
    /// Y scale, Cr to R, Cb and Cr to G, Cb to B.
    y_scale: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
    /// Black level of Y: 16 in limited range, 0 in full range.
    y_offset: i32,
}

impl Coeffs {
    fn new(c: YuvColor) -> Coeffs {
        let (kr, kb) = if c.bt709 {
            (0.2126, 0.0722)
        } else {
            (0.299, 0.114)
        };
        let kg = 1.0 - kr - kb;
        let (ys, cs) = if c.limited {
            (255.0 / 219.0, 255.0 / 224.0)
        } else {
            (1.0, 1.0)
        };
        let fixed = |v: f64| (v * 65536.0).round() as i32;
        Coeffs {
            y_scale: fixed(ys),
            rv: fixed(2.0 * (1.0 - kr) * cs),
            gu: fixed(2.0 * (1.0 - kb) * kb / kg * cs),
            gv: fixed(2.0 * (1.0 - kr) * kr / kg * cs),
            bu: fixed(2.0 * (1.0 - kb) * cs),
            y_offset: if c.limited { 16 } else { 0 },
        }
    }

    /// B, G, R, X.
    #[inline]
    fn bgrx(&self, y: u8, cb: u8, cr: u8) -> [u8; 4] {
        const HALF: i32 = 1 << 15;
        let clamp = |v: i32| (v >> 16).clamp(0, 255) as u8;
        let l = (y as i32 - self.y_offset) * self.y_scale + HALF;
        let (u, v) = (cb as i32 - 128, cr as i32 - 128);
        [
            clamp(l + self.bu * u),
            clamp(l - self.gu * u - self.gv * v),
            clamp(l + self.rv * v),
            255,
        ]
    }

    #[inline]
    fn luma(&self, y: u8) -> u8 {
        (((y as i32 - self.y_offset) * self.y_scale + (1 << 15)) >> 16).clamp(0, 255) as u8
    }
}

/// A frame's pixels with their layout, checked to be long enough.
#[derive(Debug, Clone, Copy)]
pub struct Pixels<'a> {
    data: &'a [u8],
    pub width: u32,
    pub height: u32,
    stride: u32,
    layout: PixelLayout,
    coeffs: Option<Coeffs>,
}

impl<'a> Pixels<'a> {
    /// None when `data` is shorter than the frame, or a line shorter than
    /// its pixels.
    pub fn new(
        data: &'a [u8],
        width: u32,
        height: u32,
        stride: u32,
        layout: PixelLayout,
    ) -> Option<Pixels<'a>> {
        if width == 0 || height == 0 {
            return None;
        }
        let line = match layout {
            // Whole Cb/Cr pairs.
            PixelLayout::Yuyv(_) => width.div_ceil(2) * 4,
            PixelLayout::Nv12(_) => width.div_ceil(2) * 2,
            _ => width * layout.bytes_per_pixel(),
        };
        if stride < line {
            return None;
        }
        let need = match layout {
            PixelLayout::Nv12(_) => {
                stride as usize * (height as usize + height.div_ceil(2) as usize - 1)
                    + line as usize
            }
            _ => stride as usize * (height as usize - 1) + line as usize,
        };
        if data.len() < need {
            return None;
        }
        Some(Pixels {
            data,
            width,
            height,
            stride,
            layout,
            coeffs: layout.yuv().map(Coeffs::new),
        })
    }

    pub fn layout(&self) -> PixelLayout {
        self.layout
    }

    /// The pixel at (x, y) as B, G, R, X. Panics outside the frame.
    #[inline]
    pub fn bgrx(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(x < self.width && y < self.height, "pixel outside the frame");
        let (x, y, s) = (x as usize, y as usize, self.stride as usize);
        let d = self.data;
        match (self.layout, &self.coeffs) {
            (PixelLayout::Xrgb8888, _) => {
                let i = y * s + x * 4;
                [d[i], d[i + 1], d[i + 2], 255]
            }
            (PixelLayout::Rgb888, _) => {
                let i = y * s + x * 3;
                [d[i], d[i + 1], d[i + 2], 255]
            }
            (PixelLayout::Xbgr8888, _) => {
                let i = y * s + x * 4;
                [d[i + 2], d[i + 1], d[i], 255]
            }
            (PixelLayout::Bgr888, _) => {
                let i = y * s + x * 3;
                [d[i + 2], d[i + 1], d[i], 255]
            }
            (PixelLayout::Yuyv(_), Some(c)) => {
                let pair = y * s + (x & !1) * 2;
                c.bgrx(d[pair + (x & 1) * 2], d[pair + 1], d[pair + 3])
            }
            (PixelLayout::Nv12(_), Some(c)) => {
                let chroma = s * self.height as usize + (y / 2) * s + (x & !1);
                c.bgrx(d[y * s + x], d[chroma], d[chroma + 1])
            }
            (_, None) => unreachable!("YUV layouts have coefficients"),
        }
    }

    /// The brightness (0-255) at (x, y): Y for YUV frames (stretched to
    /// full range), else the BT.601 weighting of R, G and B.
    #[inline]
    pub fn luma(&self, x: u32, y: u32) -> f64 {
        let (xs, ys, s) = (x as usize, y as usize, self.stride as usize);
        match (self.layout, &self.coeffs) {
            (PixelLayout::Yuyv(_), Some(c)) => c.luma(self.data[ys * s + xs * 2]) as f64,
            (PixelLayout::Nv12(_), Some(c)) => c.luma(self.data[ys * s + xs]) as f64,
            _ => {
                let [b, g, r, _] = self.bgrx(x, y);
                0.114 * b as f64 + 0.587 * g as f64 + 0.299 * r as f64
            }
        }
    }

    /// The whole frame as packed XRGB8888 (B, G, R, X), `width * 4` bytes
    /// a line, converted on up to four threads.
    pub fn to_xrgb(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let line = w * 4;
        let mut out = vec![0u8; line * h];
        let threads = std::thread::available_parallelism()
            .map_or(1, |t| t.get())
            .clamp(1, 4);
        let rows = h.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            for (i, chunk) in out.chunks_mut(rows * line).enumerate() {
                scope.spawn(move || {
                    for (r, dst) in chunk.chunks_exact_mut(line).enumerate() {
                        let y = (i * rows + r) as u32;
                        for (x, px) in dst.chunks_exact_mut(4).enumerate() {
                            px.copy_from_slice(&self.bgrx(x as u32, y));
                        }
                    }
                });
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: YuvColor = YuvColor {
        bt709: false,
        limited: false,
    };

    #[test]
    fn nv12_gray_stays_gray_and_colors_land_in_their_channel() {
        // 4x2 frame, stride 8: Y lines, then one CbCr line.
        let mut frame = vec![0u8; 8 * 3];
        frame[..4].copy_from_slice(&[0, 128, 255, 128]);
        frame[8..12].copy_from_slice(&[128, 128, 128, 128]);
        // Pixels 0-1 gray (Cb = Cr = 128), pixels 2-3 red (Cr high).
        frame[16..20].copy_from_slice(&[128, 128, 128, 255]);
        let p = Pixels::new(&frame, 4, 2, 8, PixelLayout::Nv12(FULL)).expect("long enough");
        let out = p.to_xrgb();
        assert_eq!(out.len(), 4 * 4 * 2);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[128, 128, 128, 255]);
        // Y 255 with Cr 255: red saturates, blue stays at Y.
        assert_eq!(out[8 + 2], 255);
        assert_eq!(out[8], 255);
        // Y 128 with Cr 255: more red than green and blue.
        assert!(out[12 + 2] > 200 && out[12 + 1] < 128 && out[12] == 128);
    }

    #[test]
    fn yuyv_shares_chroma_between_pixel_pairs() {
        // 4x1: Y0 Cb Y1 Cr, gray pair then a blue pair.
        let frame = [50u8, 128, 100, 128, 128, 255, 128, 128];
        let p = Pixels::new(&frame, 4, 1, 8, PixelLayout::Yuyv(FULL)).expect("long enough");
        assert_eq!(p.bgrx(0, 0), [50, 50, 50, 255]);
        assert_eq!(p.bgrx(1, 0), [100, 100, 100, 255]);
        let blue = p.bgrx(3, 0);
        assert!(blue[0] > 200 && blue[2] <= 128, "{blue:?}");
        assert_eq!(p.luma(1, 0), 100.0);
    }

    #[test]
    fn short_frames_are_refused() {
        let y = vec![200u8; 8 * 4 + 8 * 2];
        assert!(Pixels::new(&y, 6, 4, 8, PixelLayout::Nv12(FULL)).is_some());
        assert!(Pixels::new(&y[..40], 6, 4, 8, PixelLayout::Nv12(FULL)).is_none());
        assert!(Pixels::new(&y, 6, 4, 4, PixelLayout::Nv12(FULL)).is_none());
        assert!(Pixels::new(&y, 8, 4, 16, PixelLayout::Yuyv(FULL)).is_none());
        assert!(Pixels::new(&y, 2, 2, 8, PixelLayout::Xrgb8888).is_some());
    }

    #[test]
    fn limited_range_stretches_to_full() {
        let c = YuvColor {
            bt709: true,
            limited: true,
        };
        let frame = [16u8, 235, 16, 235, 128, 128];
        let p = Pixels::new(&frame, 2, 2, 2, PixelLayout::Nv12(c)).expect("long enough");
        assert_eq!(p.bgrx(0, 0), [0, 0, 0, 255]);
        assert_eq!(p.bgrx(1, 0), [255, 255, 255, 255]);
        assert_eq!(p.luma(1, 0), 255.0);
    }

    #[test]
    fn rgb_layouts_read_as_before() {
        let frame = [1u8, 2, 3, 0, 4, 5, 6, 0];
        let p = Pixels::new(&frame, 2, 1, 8, PixelLayout::Xrgb8888).unwrap();
        assert_eq!(p.bgrx(1, 0), [4, 5, 6, 255]);
        let p = Pixels::new(&frame, 2, 1, 8, PixelLayout::Xbgr8888).unwrap();
        assert_eq!(p.bgrx(0, 0), [3, 2, 1, 255]);
    }
}
