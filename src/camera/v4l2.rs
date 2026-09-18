//! The V4L2 ioctls a webcam needs, and nothing else.
//!
//! Every UVC camera — the one in the lid and the one on a USB cable alike —
//! is a `/dev/videoN` node that answers the same dozen ioctls, and those
//! structs have been frozen in `<linux/videodev2.h>` for over a decade. So
//! they are declared here, laid out for the 64-bit kernel ABI, rather than
//! taken from a crate that wraps them: this module is the whole surface, and
//! the tests pin every struct's size to the header's.
//!
//! Streaming uses mmap'd driver buffers, the one I/O method every capture
//! driver implements.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const MEMORY_MMAP: u32 = 1;
pub const FIELD_NONE: u32 = 1;

pub const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
pub const CAP_STREAMING: u32 = 0x0400_0000;
pub const CAP_DEVICE_CAPS: u32 = 0x8000_0000;

pub const FRMSIZE_TYPE_DISCRETE: u32 = 1;
pub const FRMIVAL_TYPE_DISCRETE: u32 = 1;

pub const CTRL_TYPE_INTEGER: u32 = 1;
pub const CTRL_TYPE_BOOLEAN: u32 = 2;
pub const CTRL_TYPE_MENU: u32 = 3;
pub const CTRL_FLAG_DISABLED: u32 = 0x0001;
pub const CTRL_FLAG_INACTIVE: u32 = 0x0010;
pub const CTRL_FLAG_NEXT_CTRL: u32 = 0x8000_0000;

/// `v4l2_fourcc(a, b, c, d)`.
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | (code[1] as u32) << 8 | (code[2] as u32) << 16 | (code[3] as u32) << 24
}

pub const PIX_MJPEG: u32 = fourcc(b"MJPG");
pub const PIX_JPEG: u32 = fourcc(b"JPEG");
pub const PIX_YUYV: u32 = fourcc(b"YUYV");
pub const PIX_NV12: u32 = fourcc(b"NV12");

/// The fourcc as text, for logs and the probe.
pub fn fourcc_name(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FmtDesc {
    pub index: u32,
    pub kind: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrmSizeEnum {
    pub index: u32,
    pub pixel_format: u32,
    pub kind: u32,
    /// `discrete {width, height}` or `stepwise {min_width, max_width,
    /// step_width, min_height, max_height, step_height}`.
    pub size: [u32; 6],
    pub reserved: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fract {
    pub numerator: u32,
    pub denominator: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrmIvalEnum {
    pub index: u32,
    pub pixel_format: u32,
    pub width: u32,
    pub height: u32,
    pub kind: u32,
    /// `discrete` is the first fraction; `stepwise {min, max, step}` all three.
    pub interval: [Fract; 3],
    pub reserved: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PixFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub private: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

/// `struct v4l2_format`. The union is 200 bytes and, because some of its
/// members hold pointers, 8-aligned — hence the padding after `kind`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Format {
    pub kind: u32,
    _pad: u32,
    pub pix: PixFormat,
    _rest: [u8; 200 - std::mem::size_of::<PixFormat>()],
}

impl Format {
    pub fn capture(pix: PixFormat) -> Self {
        Self {
            kind: BUF_TYPE_VIDEO_CAPTURE,
            _pad: 0,
            pix,
            _rest: [0; 200 - std::mem::size_of::<PixFormat>()],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureParm {
    pub capability: u32,
    pub capturemode: u32,
    pub timeperframe: Fract,
    pub extendedmode: u32,
    pub readbuffers: u32,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct StreamParm {
    pub kind: u32,
    pub capture: CaptureParm,
    _rest: [u8; 200 - std::mem::size_of::<CaptureParm>()],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RequestBuffers {
    pub count: u32,
    pub kind: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timecode {
    pub kind: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

/// `struct v4l2_buffer`, 64-bit layout. `m` is a union of a u32 offset, an
/// unsigned long, a pointer and an int; for mmap buffers only the offset in
/// its low four bytes matters.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Buffer {
    pub index: u32,
    pub kind: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: libc::timeval,
    pub timecode: Timecode,
    pub sequence: u32,
    pub memory: u32,
    pub m: u64,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

impl Buffer {
    pub fn mmap(index: u32) -> Self {
        Self {
            index,
            kind: BUF_TYPE_VIDEO_CAPTURE,
            bytesused: 0,
            flags: 0,
            field: 0,
            timestamp: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            timecode: Timecode::default(),
            sequence: 0,
            memory: MEMORY_MMAP,
            m: 0,
            length: 0,
            reserved2: 0,
            request_fd: 0,
        }
    }

    /// The buffer's mmap offset.
    pub fn offset(&self) -> u32 {
        self.m as u32
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct QueryCtrl {
    pub id: u32,
    pub kind: u32,
    pub name: [u8; 32],
    pub minimum: i32,
    pub maximum: i32,
    pub step: i32,
    pub default_value: i32,
    pub flags: u32,
    pub reserved: [u32; 2],
}

/// `struct v4l2_querymenu`, which the header declares packed: the 64-bit
/// union after `index` starts at byte 8 with no padding either way.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct QueryMenu {
    pub id: u32,
    pub index: u32,
    pub name: [u8; 32],
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Control {
    pub id: u32,
    pub value: i32,
}

const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as libc::c_ulong
}
const fn ior<T>(nr: u32) -> libc::c_ulong {
    ioc(2, nr, std::mem::size_of::<T>())
}
const fn iow<T>(nr: u32) -> libc::c_ulong {
    ioc(1, nr, std::mem::size_of::<T>())
}
const fn iowr<T>(nr: u32) -> libc::c_ulong {
    ioc(3, nr, std::mem::size_of::<T>())
}

pub const VIDIOC_QUERYCAP: libc::c_ulong = ior::<Capability>(0);
pub const VIDIOC_ENUM_FMT: libc::c_ulong = iowr::<FmtDesc>(2);
pub const VIDIOC_S_FMT: libc::c_ulong = iowr::<Format>(5);
pub const VIDIOC_REQBUFS: libc::c_ulong = iowr::<RequestBuffers>(8);
pub const VIDIOC_QUERYBUF: libc::c_ulong = iowr::<Buffer>(9);
pub const VIDIOC_QBUF: libc::c_ulong = iowr::<Buffer>(15);
pub const VIDIOC_DQBUF: libc::c_ulong = iowr::<Buffer>(17);
pub const VIDIOC_STREAMON: libc::c_ulong = iow::<libc::c_int>(18);
pub const VIDIOC_STREAMOFF: libc::c_ulong = iow::<libc::c_int>(19);
pub const VIDIOC_G_PARM: libc::c_ulong = iowr::<StreamParm>(21);
pub const VIDIOC_S_PARM: libc::c_ulong = iowr::<StreamParm>(22);
pub const VIDIOC_G_CTRL: libc::c_ulong = iowr::<Control>(27);
pub const VIDIOC_S_CTRL: libc::c_ulong = iowr::<Control>(28);
pub const VIDIOC_QUERYCTRL: libc::c_ulong = iowr::<QueryCtrl>(36);
pub const VIDIOC_QUERYMENU: libc::c_ulong = iowr::<QueryMenu>(37);
pub const VIDIOC_ENUM_FRAMESIZES: libc::c_ulong = iowr::<FrmSizeEnum>(74);
pub const VIDIOC_ENUM_FRAMEINTERVALS: libc::c_ulong = iowr::<FrmIvalEnum>(75);

/// A zeroed `T`. Every struct in this module is plain old data for which
/// all-zero bytes are a valid value, which is how the kernel's own
/// documentation says to prepare them.
pub fn zeroed<T: Copy>() -> T {
    // SAFETY: only instantiated with the repr(C) integer-and-array structs
    // above (and libc integers), for which the all-zero bit pattern is valid.
    unsafe { std::mem::zeroed() }
}

/// `ioctl(fd, request, arg)`, retried on EINTR.
///
/// # Safety
/// `request` must be one of the constants above, and `T` the struct that
/// request's size field was computed from.
pub unsafe fn ioctl<T>(fd: RawFd, request: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: the caller guarantees `arg` is the struct the kernel will
        // read and write for `request`, so its size matches what the kernel
        // copies, and it is a live exclusive reference for the call.
        let r = unsafe { libc::ioctl(fd, request as _, arg as *mut T) };
        if r != -1 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// An open video node.
#[derive(Debug)]
pub struct Node {
    file: File,
}

impl Node {
    /// Open `path` non-blocking: capture waits on `poll`, so a stop request
    /// never sits behind a camera that stopped sending frames.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self { file })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub fn query_cap(&self) -> io::Result<Capability> {
        let mut cap: Capability = zeroed();
        // SAFETY: VIDIOC_QUERYCAP takes a v4l2_capability.
        unsafe { ioctl(self.fd(), VIDIOC_QUERYCAP, &mut cap)? };
        Ok(cap)
    }

    /// Every pixel format the node offers for capture.
    pub fn formats(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for index in 0.. {
            let mut desc: FmtDesc = zeroed();
            desc.index = index;
            desc.kind = BUF_TYPE_VIDEO_CAPTURE;
            // SAFETY: VIDIOC_ENUM_FMT takes a v4l2_fmtdesc.
            if unsafe { ioctl(self.fd(), VIDIOC_ENUM_FMT, &mut desc) }.is_err() {
                break;
            }
            out.push((desc.pixelformat, cstr(&desc.description)));
        }
        out
    }

    /// The discrete frame sizes for `format`. A stepwise camera — rare
    /// outside of capture cards — is offered its common sizes that fit.
    pub fn frame_sizes(&self, format: u32) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for index in 0.. {
            let mut e: FrmSizeEnum = zeroed();
            e.index = index;
            e.pixel_format = format;
            // SAFETY: VIDIOC_ENUM_FRAMESIZES takes a v4l2_frmsizeenum.
            if unsafe { ioctl(self.fd(), VIDIOC_ENUM_FRAMESIZES, &mut e) }.is_err() {
                break;
            }
            if e.kind == FRMSIZE_TYPE_DISCRETE {
                out.push((e.size[0], e.size[1]));
            } else {
                let [min_w, max_w, _, min_h, max_h, _] = e.size;
                for (w, h) in [
                    (640, 480),
                    (1280, 720),
                    (1920, 1080),
                    (2560, 1440),
                    (3840, 2160),
                ] {
                    if (min_w..=max_w).contains(&w) && (min_h..=max_h).contains(&h) {
                        out.push((w, h));
                    }
                }
                break;
            }
        }
        out
    }

    /// The frame rates `format` at `width`×`height` offers, as frames per
    /// second, highest first.
    pub fn frame_rates(&self, format: u32, width: u32, height: u32) -> Vec<f64> {
        let mut out = Vec::new();
        for index in 0.. {
            let mut e: FrmIvalEnum = zeroed();
            e.index = index;
            e.pixel_format = format;
            e.width = width;
            e.height = height;
            // SAFETY: VIDIOC_ENUM_FRAMEINTERVALS takes a v4l2_frmivalenum.
            if unsafe { ioctl(self.fd(), VIDIOC_ENUM_FRAMEINTERVALS, &mut e) }.is_err() {
                break;
            }
            let fastest = e.interval[0];
            if fastest.numerator > 0 {
                out.push(f64::from(fastest.denominator) / f64::from(fastest.numerator));
            }
            if e.kind != FRMIVAL_TYPE_DISCRETE {
                break;
            }
        }
        out.sort_by(|a, b| b.total_cmp(a));
        out.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        out
    }

    /// Ask for a format; the driver answers with what it will actually do.
    pub fn set_format(&self, width: u32, height: u32, pixelformat: u32) -> io::Result<PixFormat> {
        let mut fmt = Format::capture(PixFormat {
            width,
            height,
            pixelformat,
            field: FIELD_NONE,
            ..PixFormat::default()
        });
        // SAFETY: VIDIOC_S_FMT takes a v4l2_format.
        unsafe { ioctl(self.fd(), VIDIOC_S_FMT, &mut fmt)? };
        Ok(fmt.pix)
    }

    /// Ask for a frame rate. Many cameras ignore this or round it; the
    /// capture loop measures what actually arrives.
    pub fn set_frame_rate(&self, fps: u32) -> io::Result<()> {
        let mut parm: StreamParm = zeroed();
        parm.kind = BUF_TYPE_VIDEO_CAPTURE;
        // SAFETY: VIDIOC_G_PARM takes a v4l2_streamparm.
        unsafe { ioctl(self.fd(), VIDIOC_G_PARM, &mut parm)? };
        parm.capture.timeperframe = Fract {
            numerator: 1,
            denominator: fps.max(1),
        };
        // SAFETY: VIDIOC_S_PARM takes a v4l2_streamparm.
        unsafe { ioctl(self.fd(), VIDIOC_S_PARM, &mut parm) }
    }

    /// The camera's adjustable controls: brightness, contrast, white balance,
    /// exposure, focus and the rest, whatever this camera has.
    pub fn controls(&self) -> Vec<(QueryCtrl, Vec<(u32, String)>)> {
        let mut out = Vec::new();
        let mut id = CTRL_FLAG_NEXT_CTRL;
        loop {
            let mut q: QueryCtrl = zeroed();
            q.id = id;
            // SAFETY: VIDIOC_QUERYCTRL takes a v4l2_queryctrl.
            if unsafe { ioctl(self.fd(), VIDIOC_QUERYCTRL, &mut q) }.is_err() {
                break;
            }
            id = q.id | CTRL_FLAG_NEXT_CTRL;
            if q.flags & CTRL_FLAG_DISABLED != 0 {
                continue;
            }
            let menu = if q.kind == CTRL_TYPE_MENU {
                (q.minimum.max(0) as u32..=q.maximum.max(0) as u32)
                    .filter_map(|index| {
                        let mut m: QueryMenu = zeroed();
                        m.id = q.id;
                        m.index = index;
                        // SAFETY: VIDIOC_QUERYMENU takes a v4l2_querymenu.
                        unsafe { ioctl(self.fd(), VIDIOC_QUERYMENU, &mut m) }.ok()?;
                        let name = m.name;
                        Some((index, cstr(&name)))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            out.push((q, menu));
        }
        out
    }

    pub fn control(&self, id: u32) -> io::Result<i32> {
        let mut c = Control { id, value: 0 };
        // SAFETY: VIDIOC_G_CTRL takes a v4l2_control.
        unsafe { ioctl(self.fd(), VIDIOC_G_CTRL, &mut c)? };
        Ok(c.value)
    }

    pub fn set_control(&self, id: u32, value: i32) -> io::Result<()> {
        let mut c = Control { id, value };
        // SAFETY: VIDIOC_S_CTRL takes a v4l2_control.
        unsafe { ioctl(self.fd(), VIDIOC_S_CTRL, &mut c) }
    }
}

/// A NUL-terminated byte array as text.
pub fn cstr(bytes: &[u8]) -> String {
    CStr::from_bytes_until_nul(bytes)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// The sizes `<linux/videodev2.h>` gives on x86_64 and aarch64. If one of
    /// these is wrong every ioctl using it fails with ENOTTY, because the size
    /// is part of the request number.
    #[test]
    fn structs_match_the_kernel_abi() {
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<FmtDesc>(), 64);
        assert_eq!(size_of::<FrmSizeEnum>(), 44);
        assert_eq!(size_of::<FrmIvalEnum>(), 52);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<StreamParm>(), 204);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(size_of::<Buffer>(), 88);
        assert_eq!(size_of::<QueryCtrl>(), 68);
        assert_eq!(size_of::<QueryMenu>(), 44);
        assert_eq!(size_of::<Control>(), 8);
    }

    #[test]
    fn request_numbers_match_the_header() {
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
    }

    #[test]
    fn fourcc_round_trips() {
        assert_eq!(fourcc_name(PIX_MJPEG), "MJPG");
        assert_eq!(fourcc_name(PIX_YUYV), "YUYV");
    }
}
