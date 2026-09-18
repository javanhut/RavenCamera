//! Camera video as it is recorded: the camera's own JPEG frames, one after
//! another, each with its time.
//!
//! A webcam already compresses every frame to JPEG in its own hardware, so
//! the cheapest thing to do while recording — on a machine that could not
//! encode H.264 in real time — is to keep those bytes exactly as they came.
//! The H.264 encode happens when the recording is finished.
//!
//! ```text
//! header  "RVNCAM" version u16 width u32 height u32          16 bytes
//! record  pts_us u64  length u32  JPEG bytes                 12 + length
//! end     pts_us u64  0u32                                   12
//! ```
//!
//! Little-endian. A file with no end record was cut short and reads back to
//! its last whole frame.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

const MAGIC: &[u8; 6] = b"RVNCAM";
const VERSION: u16 = 1;
const HEADER: u64 = 16;
/// No camera frame is anywhere near this; a length past it is corruption.
const MAX_FRAME: u32 = 64 << 20;

#[derive(Debug)]
pub struct Writer {
    out: BufWriter<File>,
    last: Duration,
    pub frames: u64,
}

impl Writer {
    pub fn create(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&width.to_le_bytes())?;
        out.write_all(&height.to_le_bytes())?;
        Ok(Self {
            out,
            last: Duration::ZERO,
            frames: 0,
        })
    }

    /// Append a frame. Times never go backwards; one that would is written
    /// at the previous frame's time.
    pub fn push(&mut self, pts: Duration, jpeg: &[u8]) -> io::Result<()> {
        if jpeg.is_empty() {
            return Ok(());
        }
        let pts = pts.max(self.last);
        self.last = pts;
        self.out
            .write_all(&(pts.as_micros() as u64).to_le_bytes())?;
        self.out.write_all(&(jpeg.len() as u32).to_le_bytes())?;
        self.out.write_all(jpeg)?;
        self.frames += 1;
        Ok(())
    }

    pub fn finish(mut self, end: Duration) -> io::Result<u64> {
        let end = end.max(self.last);
        self.out
            .write_all(&(end.as_micros() as u64).to_le_bytes())?;
        self.out.write_all(&0u32.to_le_bytes())?;
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        Ok(self.frames)
    }
}

/// Where each frame is, found by walking the headers once. Frames are then
/// read in any order, which is what lets several threads finish one
/// recording.
#[derive(Debug, Clone)]
pub struct Index {
    pub width: u32,
    pub height: u32,
    /// `(pts, offset of the JPEG bytes, length)`.
    pub frames: Vec<(Duration, u64, u32)>,
    /// When recording stopped, if the file says.
    pub end: Option<Duration>,
}

impl Index {
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut r = BufReader::new(File::open(path)?);
        let mut head = [0u8; HEADER as usize];
        r.read_exact(&mut head)?;
        if &head[0..6] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a camera recording",
            ));
        }
        let width = u32::from_le_bytes(head[8..12].try_into().unwrap());
        let height = u32::from_le_bytes(head[12..16].try_into().unwrap());
        let file_len = r.get_ref().metadata()?.len();
        let mut frames = Vec::new();
        let mut end = None;
        let mut at = HEADER;
        loop {
            let mut rec = [0u8; 12];
            if at + 12 > file_len || r.read_exact(&mut rec).is_err() {
                break;
            }
            let pts = Duration::from_micros(u64::from_le_bytes(rec[0..8].try_into().unwrap()));
            let len = u32::from_le_bytes(rec[8..12].try_into().unwrap());
            if len == 0 {
                end = Some(pts);
                break;
            }
            if len > MAX_FRAME || at + 12 + u64::from(len) > file_len {
                break;
            }
            frames.push((pts, at + 12, len));
            r.seek(SeekFrom::Current(i64::from(len)))?;
            at += 12 + u64::from(len);
        }
        Ok(Self {
            width,
            height,
            frames,
            end,
        })
    }

    /// The end of the recording: the end record, or the last frame plus a
    /// frame's worth.
    pub fn length(&self) -> Duration {
        self.end.unwrap_or_else(|| {
            self.frames
                .last()
                .map(|f| f.0 + Duration::from_millis(33))
                .unwrap_or_default()
        })
    }

    /// The last frame shown at or before `t`: which camera picture goes with
    /// a screen frame at `t` in a picture-in-picture.
    pub fn at(&self, t: Duration) -> Option<usize> {
        match self.frames.binary_search_by(|f| f.0.cmp(&t)) {
            Ok(i) => Some(i),
            Err(0) => (!self.frames.is_empty()).then_some(0),
            Err(i) => Some(i - 1),
        }
    }
}

/// Reads frames by index.
#[derive(Debug)]
pub struct Reader {
    file: File,
}

impl Reader {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
        })
    }

    pub fn frame(&mut self, offset: u64, len: u32) -> io::Result<Vec<u8>> {
        let mut buf = vec![0; len as usize];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_frames_index_and_read_back() {
        let dir = std::env::temp_dir().join(format!("raven-camera-camfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.rcam");
        let mut w = Writer::create(&path, 640, 480).unwrap();
        w.push(Duration::from_millis(0), b"first").unwrap();
        w.push(Duration::from_millis(40), b"second!").unwrap();
        // Backwards time is held at the previous frame's.
        w.push(Duration::from_millis(30), b"third").unwrap();
        assert_eq!(w.finish(Duration::from_millis(100)).unwrap(), 3);

        let index = Index::read(&path).unwrap();
        assert_eq!((index.width, index.height), (640, 480));
        assert_eq!(index.frames.len(), 3);
        assert_eq!(index.frames[2].0, Duration::from_millis(40));
        assert_eq!(index.end, Some(Duration::from_millis(100)));
        let mut r = Reader::open(&path).unwrap();
        let (_, off, len) = index.frames[1];
        assert_eq!(r.frame(off, len).unwrap(), b"second!");
        assert_eq!(index.at(Duration::from_millis(39)), Some(0));
        // Two frames share 40 ms; either is the picture at that moment.
        assert!(matches!(index.at(Duration::from_millis(40)), Some(1 | 2)));
        assert_eq!(index.at(Duration::from_secs(9)), Some(2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cut_short_file_reads_to_its_last_whole_frame() {
        let dir = std::env::temp_dir().join(format!("raven-camera-camcut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.rcam");
        let mut w = Writer::create(&path, 2, 2).unwrap();
        w.push(Duration::from_millis(0), b"aaaa").unwrap();
        w.push(Duration::from_millis(33), b"bbbb").unwrap();
        drop(w); // no end record
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 2); // and the last frame torn
        std::fs::write(&path, bytes).unwrap();
        let index = Index::read(&path).unwrap();
        assert_eq!(index.frames.len(), 1);
        assert_eq!(index.end, None);
        assert_eq!(index.length(), Duration::from_millis(33));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
