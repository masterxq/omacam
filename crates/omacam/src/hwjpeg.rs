//! Photos through a hardware JPEG encoder: a V4L2 memory-to-memory device
//! that takes NV12 and gives a baseline JPEG, such as the MediaTek JPEG
//! encoder. The lens table names the device ([`TableSpec::jpeg_encoder`]);
//! the photo's crop is copied into the encoder's buffer, so the CPU neither
//! converts nor compresses.
//!
//! [`TableSpec::jpeg_encoder`]: omacam_core::lens::TableSpec::jpeg_encoder

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use omacam_core::backend::{PixelLayout, YuvColor};

use crate::photo::{self, Raw};

const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
const BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
const MEMORY_MMAP: u32 = 1;
const FIELD_NONE: u32 = 1;
const BUF_FLAG_ERROR: u32 = 0x40;
const SEL_TGT_CROP: u32 = 0;
const CID_JPEG_COMPRESSION_QUALITY: u32 = 0x009d_0903;
const PIX_FMT_NV12: u32 = u32::from_le_bytes(*b"NV12");
const PIX_FMT_JPEG: u32 = u32::from_le_bytes(*b"JPEG");
/// How long one photo may take the encoder.
const TIMEOUT_MS: i32 = 3000;

// The V4L2 structures the encoder needs, laid out as in
// include/uapi/linux/videodev2.h.

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PlanePixFormat {
    sizeimage: u32,
    bytesperline: u32,
    reserved: [u16; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PixFormatMplane {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    colorspace: u32,
    plane_fmt: [PlanePixFormat; 8],
    num_planes: u8,
    flags: u8,
    ycbcr_enc: u8,
    quantization: u8,
    xfer_func: u8,
    reserved: [u8; 7],
}

#[repr(C)]
union FormatUnion {
    pix_mp: PixFormatMplane,
    raw: [u8; 200],
    // The kernel's union holds pointers (struct v4l2_window).
    _align: [usize; 200 / std::mem::size_of::<usize>()],
}

#[repr(C)]
struct Format {
    type_: u32,
    fmt: FormatUnion,
}

#[repr(C)]
#[derive(Default)]
struct RequestBuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Plane {
    bytesused: u32,
    length: u32,
    /// mem_offset for MMAP buffers (the union with userptr and fd).
    m: libc::c_ulong,
    data_offset: u32,
    reserved: [u32; 11],
}

#[repr(C)]
#[derive(Default)]
struct Timecode {
    type_: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

#[repr(C)]
struct Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: [libc::c_long; 2],
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    planes: *mut Plane,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Default)]
struct Selection {
    type_: u32,
    target: u32,
    flags: u32,
    r: Rect,
    reserved: [u32; 9],
}

#[repr(C)]
struct Control {
    id: u32,
    value: i32,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as libc::c_ulong
}
const WRITE: u32 = 1;
const READ_WRITE: u32 = 3;
const VIDIOC_S_FMT: libc::c_ulong = ioc(READ_WRITE, 5, std::mem::size_of::<Format>());
const VIDIOC_REQBUFS: libc::c_ulong = ioc(READ_WRITE, 8, std::mem::size_of::<RequestBuffers>());
const VIDIOC_QUERYBUF: libc::c_ulong = ioc(READ_WRITE, 9, std::mem::size_of::<Buffer>());
const VIDIOC_QBUF: libc::c_ulong = ioc(READ_WRITE, 15, std::mem::size_of::<Buffer>());
const VIDIOC_DQBUF: libc::c_ulong = ioc(READ_WRITE, 17, std::mem::size_of::<Buffer>());
const VIDIOC_STREAMON: libc::c_ulong = ioc(WRITE, 18, std::mem::size_of::<i32>());
const VIDIOC_STREAMOFF: libc::c_ulong = ioc(WRITE, 19, std::mem::size_of::<i32>());
const VIDIOC_S_CTRL: libc::c_ulong = ioc(READ_WRITE, 28, std::mem::size_of::<Control>());
const VIDIOC_S_SELECTION: libc::c_ulong = ioc(READ_WRITE, 95, std::mem::size_of::<Selection>());

/// One ioctl on `fd` with `arg`, retried when a signal interrupts it.
fn ioctl<T>(fd: RawFd, request: libc::c_ulong, arg: &mut T, what: &str) -> Result<()> {
    loop {
        // SAFETY: `arg` is the structure `request` names, laid out as the
        // kernel's, and lives across the call.
        let r = unsafe { libc::ioctl(fd, request as _, arg as *mut T as *mut c_void) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e).with_context(|| format!("JPEG encoder: {what}"));
        }
    }
}

/// A buffer of the encoder, mapped into memory.
struct Mapped {
    ptr: *mut c_void,
    len: usize,
}

impl Mapped {
    fn new(fd: RawFd, type_: u32) -> Result<Mapped> {
        let mut plane = Plane::default();
        let mut buf = buffer(type_, &mut plane);
        ioctl(fd, VIDIOC_QUERYBUF, &mut buf, "query buffer")?;
        let len = plane.length as usize;
        // SAFETY: maps the buffer the driver just described; no Rust
        // memory is involved until the mapping is checked.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                plane.m as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("JPEG encoder: map buffer");
        }
        Ok(Mapped { ptr, len })
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is `len` bytes, readable and writable, and
        // stays mapped as long as `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `new`.
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

/// Buffer 0 of the queue `type_`, one plane, memory-mapped.
fn buffer(type_: u32, plane: &mut Plane) -> Buffer {
    Buffer {
        index: 0,
        type_,
        bytesused: 0,
        flags: 0,
        field: FIELD_NONE,
        timestamp: [0; 2],
        timecode: Timecode::default(),
        sequence: 0,
        memory: MEMORY_MMAP,
        planes: plane,
        length: 1,
        reserved2: 0,
        request_fd: 0,
    }
}

fn set_format(fd: RawFd, type_: u32, pix: PixFormatMplane) -> Result<PixFormatMplane> {
    let mut f = Format {
        type_,
        fmt: FormatUnion { raw: [0; 200] },
    };
    f.fmt.pix_mp = pix;
    ioctl(fd, VIDIOC_S_FMT, &mut f, "set format")?;
    // SAFETY: the driver filled in the multi-planar format it was given.
    Ok(unsafe { f.fmt.pix_mp })
}

/// The V4L2 node named `name` in sysfs.
fn find(name: &str) -> Result<PathBuf> {
    let dir = std::fs::read_dir("/sys/class/video4linux").context("no V4L2 devices")?;
    for e in dir.flatten() {
        let named =
            std::fs::read_to_string(e.path().join("name")).is_ok_and(|n| n.trim_end() == name);
        if named {
            return Ok(PathBuf::from("/dev").join(e.file_name()));
        }
    }
    bail!("no V4L2 device named {name}")
}

/// A photo the encoder made.
pub struct Encoded {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Encode `rect` (x, y, w, h, even) of an NV12 frame, halved when `half`,
/// on the V4L2 encoder named `name`.
pub fn encode(
    name: &str,
    raw: &Raw<'_>,
    rect: (u32, u32, u32, u32),
    half: bool,
    quality: i32,
) -> Result<Encoded> {
    // JPEG's own color: BT.601, full range. Anything else would need a
    // conversion on the CPU, which the software encoder does anyway.
    if raw.layout != PixelLayout::Nv12(YuvColor::default()) {
        bail!("the JPEG encoder takes full-range BT.601 NV12 frames only");
    }
    let path = find(name)?;
    let dev = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let fd = dev.as_raw_fd();
    let (w, h) = if half {
        ((rect.2 / 2) & !1, (rect.3 / 2) & !1)
    } else {
        (rect.2 & !1, rect.3 & !1)
    };
    // The encoder reads whole 16x16 blocks, and the chroma of a one-plane
    // NV12 buffer after the block rows of luma.
    let align = |v: u32| v.next_multiple_of(16);
    let mut pix = PixFormatMplane {
        width: align(w),
        height: align(h),
        pixelformat: PIX_FMT_NV12,
        field: FIELD_NONE,
        num_planes: 1,
        ..Default::default()
    };
    let src = set_format(fd, BUF_TYPE_VIDEO_OUTPUT_MPLANE, pix)?;
    let stride = src.plane_fmt[0].bytesperline as usize;
    let rows = src.height as usize;
    if src.pixelformat != PIX_FMT_NV12 || stride < w as usize || rows < h as usize {
        bail!("the JPEG encoder doesn't take {w}x{h} NV12");
    }
    pix = PixFormatMplane {
        width: w,
        height: h,
        pixelformat: PIX_FMT_JPEG,
        field: FIELD_NONE,
        num_planes: 1,
        ..Default::default()
    };
    // Room for a photo at a byte a pixel, several times what it takes.
    pix.plane_fmt[0].sizeimage = (w * h).max(1 << 20);
    let dst = set_format(fd, BUF_TYPE_VIDEO_CAPTURE_MPLANE, pix)?;
    if dst.pixelformat != PIX_FMT_JPEG {
        bail!("the JPEG encoder doesn't give JPEG");
    }
    let mut sel = Selection {
        type_: BUF_TYPE_VIDEO_OUTPUT,
        target: SEL_TGT_CROP,
        r: Rect {
            width: w,
            height: h,
            ..Default::default()
        },
        ..Default::default()
    };
    ioctl(fd, VIDIOC_S_SELECTION, &mut sel, "set the crop")?;
    if (sel.r.width, sel.r.height) != (w, h) {
        bail!("the JPEG encoder won't crop to {w}x{h}");
    }
    let mut q = Control {
        id: CID_JPEG_COMPRESSION_QUALITY,
        value: quality.clamp(1, 100),
    };
    ioctl(fd, VIDIOC_S_CTRL, &mut q, "set the quality")?;
    for type_ in [BUF_TYPE_VIDEO_OUTPUT_MPLANE, BUF_TYPE_VIDEO_CAPTURE_MPLANE] {
        let mut req = RequestBuffers {
            count: 1,
            type_,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        ioctl(fd, VIDIOC_REQBUFS, &mut req, "allocate buffers")?;
        if req.count < 1 {
            bail!("the JPEG encoder gave no buffer");
        }
    }
    let mut input = Mapped::new(fd, BUF_TYPE_VIDEO_OUTPUT_MPLANE)?;
    let mut output = Mapped::new(fd, BUF_TYPE_VIDEO_CAPTURE_MPLANE)?;
    photo::copy_nv12(raw, rect, half, input.bytes(), stride, rows)?;

    let mut plane = Plane {
        bytesused: input.len as u32,
        ..Default::default()
    };
    let mut buf = buffer(BUF_TYPE_VIDEO_OUTPUT_MPLANE, &mut plane);
    ioctl(fd, VIDIOC_QBUF, &mut buf, "queue the picture")?;
    let mut plane = Plane::default();
    let mut buf = buffer(BUF_TYPE_VIDEO_CAPTURE_MPLANE, &mut plane);
    ioctl(fd, VIDIOC_QBUF, &mut buf, "queue the output")?;
    let mut streaming = Vec::new();
    let result = (|| {
        for type_ in [BUF_TYPE_VIDEO_OUTPUT_MPLANE, BUF_TYPE_VIDEO_CAPTURE_MPLANE] {
            let mut t = type_ as i32;
            ioctl(fd, VIDIOC_STREAMON, &mut t, "start")?;
            streaming.push(type_);
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, alive across the call.
        let n = unsafe { libc::poll(&mut pfd, 1, TIMEOUT_MS) };
        if n <= 0 {
            bail!("the JPEG encoder took longer than {TIMEOUT_MS} ms");
        }
        let mut plane = Plane::default();
        let mut buf = buffer(BUF_TYPE_VIDEO_CAPTURE_MPLANE, &mut plane);
        ioctl(fd, VIDIOC_DQBUF, &mut buf, "take the JPEG")?;
        let used = plane.bytesused as usize;
        if buf.flags & BUF_FLAG_ERROR != 0 || used < 4 || used > output.len {
            bail!("the JPEG encoder failed ({used} bytes)");
        }
        let jpeg = output.bytes()[..used].to_vec();
        if jpeg[..2] != [0xff, 0xd8] {
            bail!("the JPEG encoder's output isn't a JPEG");
        }
        Ok(jpeg)
    })();
    for type_ in streaming {
        let mut t = type_ as i32;
        let _ = ioctl(fd, VIDIOC_STREAMOFF, &mut t, "stop");
    }
    // The buffers go with the mappings and the file.
    drop((input, output));
    Ok(Encoded {
        jpeg: result?,
        width: w,
        height: h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_are_laid_out_as_the_kernel_has_them() {
        assert_eq!(std::mem::size_of::<PixFormatMplane>(), 192);
        assert_eq!(std::mem::size_of::<Selection>(), 64);
        assert_eq!(std::mem::size_of::<RequestBuffers>(), 20);
        if std::mem::size_of::<usize>() == 8 {
            assert_eq!(std::mem::size_of::<Format>(), 208);
            assert_eq!(std::mem::size_of::<Plane>(), 64);
            assert_eq!(std::mem::size_of::<Buffer>(), 88);
            // VIDIOC_S_FMT and VIDIOC_QBUF as videodev2.h has them.
            assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
            assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        }
    }

    #[test]
    fn only_full_range_bt601_nv12() {
        let d = [0u8; 96];
        let raw = Raw {
            data: &d,
            width: 8,
            height: 8,
            stride: 8,
            layout: PixelLayout::Nv12(YuvColor {
                bt709: true,
                limited: true,
            }),
        };
        assert!(encode("none", &raw, (0, 0, 8, 8), false, 90).is_err());
    }
}
