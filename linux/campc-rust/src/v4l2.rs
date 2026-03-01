use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;

// ── V4L2 constants ────────────────────────────────────────────────────────────

// With exclusive_caps=1, v4l2loopback only advertises CAPTURE capability.
// The producer must use CAPTURE buf_type for VIDIOC_S_FMT to avoid EINVAL,
// then write raw frames directly via write().
const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_FIELD_NONE: u32 = 1;
// YU12 = planar YUV 4:2:0, same as FFmpeg yuv420p
const V4L2_PIX_FMT_YUV420: u32 = fourcc(b'Y', b'U', b'1', b'2');

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

// VIDIOC_S_FMT = _IOWR('V', 5, struct v4l2_format)
// sizeof(v4l2_format) = 208 on x86_64 Linux (4 type + 4 pad + 200 union)
const fn iowr(nr_type: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((3u32 << 30) | (size << 16) | (nr_type << 8) | nr) as libc::c_ulong
}
const VIDIOC_S_FMT: libc::c_ulong = iowr(b'V' as u32, 5, 208);

// ── v4l2_format layout (x86_64 Linux) ────────────────────────────────────────
// offset  0: __u32 type        (4 bytes)
// offset  4: __u32 _pad        (4 bytes — alignment padding for pointer in union)
// offset  8: union fmt         (200 bytes, raw_data[200])
// total:  208 bytes
//
// v4l2_pix_format at start of union (offset 8 in struct):
//   [0]  width        u32
//   [4]  height       u32
//   [8]  pixelformat  u32
//   [12] field        u32
//   [16] bytesperline u32
//   [20] sizeimage    u32
//   [24] colorspace   u32 (0 = default)

#[repr(C)]
struct V4l2Format {
    buf_type: u32,
    _pad: u32,
    fmt: [u8; 200],
}

fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_ne_bytes());
}

// ── Public writer ─────────────────────────────────────────────────────────────

pub struct V4l2Writer {
    file: File,
}

impl V4l2Writer {
    /// Open `device` and configure it for yuv420p output at `width`×`height`.
    /// Returns None if the device can't be opened or VIDIOC_S_FMT fails.
    pub fn new(device: &str, width: u32, height: u32) -> Option<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open(device)
            .map_err(|e| eprintln!("[v4l2] open {device}: {e}"))
            .ok()?;

        let bytesperline = width;
        let sizeimage = width * height * 3 / 2;

        let mut fmt = V4l2Format {
            buf_type: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            _pad: 0,
            fmt: [0u8; 200],
        };
        write_u32(&mut fmt.fmt, 0, width);
        write_u32(&mut fmt.fmt, 4, height);
        write_u32(&mut fmt.fmt, 8, V4L2_PIX_FMT_YUV420);
        write_u32(&mut fmt.fmt, 12, V4L2_FIELD_NONE);
        write_u32(&mut fmt.fmt, 16, bytesperline);
        write_u32(&mut fmt.fmt, 20, sizeimage);

        let ret = unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_S_FMT, &mut fmt as *mut V4l2Format) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            eprintln!("[v4l2] VIDIOC_S_FMT failed: {err}");
            return None;
        }

        eprintln!("[v4l2] device configured: {width}x{height} YUV420");
        Some(V4l2Writer { file })
    }

    /// Write one yuv420p frame to the device. Returns false on error.
    pub fn write_frame(&mut self, data: &[u8]) -> bool {
        self.file.write_all(data).is_ok()
    }
}
