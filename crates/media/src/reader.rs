//! Native random-access input through FilmCraft's public reader openers.
//! Containers retain a file handle and their index, never the complete compressed file.

use std::fs::File;
#[cfg(not(any(unix, windows)))]
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(any(unix, windows)))]
use std::sync::Mutex;

use filmcraft_media::{ByteReader, SharedReader, SharedSource};

use crate::{MediaError, Result};

/// Formats without a streaming opener must fit this per-file input budget. Decoded
/// frame storage has its own limits; this is not a total process memory budget.
const MAX_BUFFERED_BYTES: u64 = 512 << 20;
const MAX_READ_BYTES: usize = 64 << 20;

struct FileReader {
    #[cfg(any(unix, windows))]
    file: File,
    #[cfg(not(any(unix, windows)))]
    file: Mutex<File>,
    path: PathBuf,
    len: u64,
}

impl FileReader {
    fn open(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
        if !metadata.is_file() {
            return Err(MediaError::Unsupported(format!("{}: media input must be a regular file", path.display())));
        }
        let file = File::open(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
        let metadata = file.metadata().map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
        if !metadata.is_file() {
            return Err(MediaError::Unsupported(format!("{}: media input must be a regular file", path.display())));
        }
        Ok(Self {
            #[cfg(any(unix, windows))]
            file,
            #[cfg(not(any(unix, windows)))]
            file: Mutex::new(file),
            path: path.to_path_buf(),
            len: metadata.len(),
        })
    }

    fn check_length(&self) -> Result<()> {
        #[cfg(any(unix, windows))]
        let file = &self.file;
        #[cfg(not(any(unix, windows)))]
        let file = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let len = file.metadata().map_err(|e| MediaError::Io(format!("{}: {e}", self.path.display())))?.len();
        if len != self.len {
            return Err(MediaError::Io(format!("{}: file size changed while reading; retry after the recording finishes", self.path.display())));
        }
        Ok(())
    }
}

impl ByteReader for FileReader {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let bad = |kind: std::io::ErrorKind, why: &str| std::io::Error::new(kind, format!("{}: {why}", self.path.display()));
        let end = u64::try_from(buf.len())
            .ok()
            .and_then(|len| offset.checked_add(len))
            .ok_or_else(|| bad(std::io::ErrorKind::InvalidInput, "read range overflow"))?;
        if end > self.len {
            return Err(bad(std::io::ErrorKind::UnexpectedEof, "unexpected end of media file"));
        }
        if buf.len() > MAX_READ_BYTES {
            return Err(bad(std::io::ErrorKind::InvalidInput, "one media read exceeds the 64 MiB input limit"));
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_exact_at(&self.file, buf, offset).map_err(|e| bad(e.kind(), &e.to_string()))
        }
        #[cfg(windows)]
        {
            let (mut done, mut at) = (0usize, offset);
            while done < buf.len() {
                let rest = buf.get_mut(done..).ok_or_else(|| bad(std::io::ErrorKind::InvalidInput, "invalid read buffer offset"))?;
                match std::os::windows::fs::FileExt::seek_read(&self.file, rest, at) {
                    Ok(0) => return Err(bad(std::io::ErrorKind::UnexpectedEof, "unexpected end of media file")),
                    Ok(n) => {
                        done = done.checked_add(n).ok_or_else(|| bad(std::io::ErrorKind::InvalidInput, "read count overflow"))?;
                        at = at.checked_add(n as u64).ok_or_else(|| bad(std::io::ErrorKind::InvalidInput, "read position overflow"))?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(bad(e.kind(), &e.to_string())),
                }
            }
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let mut file = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            file.seek(SeekFrom::Start(offset)).map_err(|e| bad(e.kind(), &e.to_string()))?;
            file.read_exact(buf).map_err(|e| bad(e.kind(), &e.to_string()))
        }
    }
}

/// Ask streaming openers directly. A declined format returns `None`; an accepted format
/// that fails returns its error and must never fall back to a whole-file read.
fn open_reader(name: &str, reader: SharedReader) -> Result<Option<SharedSource>> {
    let mut head = [0u8; 4096];
    let len = usize::try_from(reader.len().min(head.len() as u64)).map_err(|e| MediaError::Io(format!("{name}: {e}")))?;
    let head = head.get_mut(..len).ok_or_else(|| MediaError::Io(format!("{name}: invalid media header length")))?;
    reader.read_at(0, head).map_err(|e| MediaError::Io(format!("{name}: {e}")))?;
    for opener in filmcraft_codecs::reader_openers() {
        if let Some(opened) = opener(name, head, &reader) {
            return opened.map(Some).map_err(MediaError::from);
        }
    }
    Ok(None)
}

pub(crate) fn open_container(path: &Path) -> Result<Option<SharedSource>> {
    let reader = Arc::new(FileReader::open(path)?);
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_else(|| path.to_string_lossy());
    let opened = open_reader(&name, reader.clone())?;
    reader.check_length()?;
    Ok(opened)
}

fn buffer_reader(name: &str, reader: &dyn ByteReader, limit: u64) -> Result<Vec<u8>> {
    let len = reader.len();
    if len > limit {
        return Err(MediaError::Unsupported(format!("{name}: this format needs an in-memory reader; {len} bytes exceeds its {limit}-byte input budget")));
    }
    let len = usize::try_from(len).map_err(|e| MediaError::Io(format!("{name}: input size does not fit this platform: {e}")))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|e| MediaError::Io(format!("{name}: cannot allocate media input: {e}")))?;
    bytes.resize(len, 0);
    let mut offset = 0u64;
    for chunk in bytes.chunks_mut(1 << 20) {
        reader.read_at(offset, chunk).map_err(|e| MediaError::Io(format!("{name}: {e}")))?;
        offset = offset.checked_add(chunk.len() as u64).ok_or_else(|| MediaError::Io(format!("{name}: media input offset overflow")))?;
    }
    Ok(bytes)
}

pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let reader = FileReader::open(path)?;
    let bytes = buffer_reader(&path.to_string_lossy(), &reader, MAX_BUFFERED_BYTES)?;
    reader.check_length()?;
    Ok(bytes)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(ext: &str, bytes: &[u8]) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-stream-reader");
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(format!("{}-{}.{ext}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    pub(crate) fn movie() -> Vec<u8> {
        let mut config = filmcraft_h264enc::EncoderConfig::new(16, 16, 120, 1);
        config.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
        config.aud = false;
        let mut encoder = filmcraft_h264enc::Encoder::new(config).unwrap();
        let entry = SampleEntry::avc(AvcConfig::parse(&encoder.avcc()).unwrap(), 16, 16);
        let mut writer = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
        let track = writer.add_track(TrackConfig::new(entry, 120)).unwrap();
        for index in 0..3 {
            let y = vec![40 + index * 60; 256];
            let frame = filmcraft_h264enc::YuvFrame { y: &y, u: &[100; 64], v: &[170; 64], y_stride: 16, uv_stride: 8 };
            for packet in encoder.try_encode(&frame, i64::from(index)).unwrap() {
                writer
                    .write_sample(
                        track,
                        WriteSample { data: &packet.data, duration: 1, composition_offset: (packet.pts - packet.dts) as i32, is_sync: packet.keyframe },
                    )
                    .unwrap();
            }
        }
        for packet in encoder.flush() {
            writer
                .write_sample(
                    track,
                    WriteSample { data: &packet.data, duration: 1, composition_offset: (packet.pts - packet.dts) as i32, is_sync: packet.keyframe },
                )
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn moov_start(bytes: &[u8]) -> usize {
        let mut at = 0;
        while at < bytes.len() {
            let header = &bytes[at..at + 8];
            if &header[4..8] == b"moov" {
                return at;
            }
            let short = u32::from_be_bytes(header[..4].try_into().unwrap());
            let size = if short == 1 { u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap()) } else { u64::from(short) };
            assert!(size >= 8);
            at += usize::try_from(size).unwrap();
        }
        panic!("the fixture writer must emit moov");
    }

    /// A one-terabyte logical MP4 with a free box between mdat and moov. Only its two
    /// small populated regions exist; padding reads yield virtual zeros without allocating it.
    struct SparseMovie {
        before: Vec<u8>,
        moov: Vec<u8>,
        moov_at: u64,
        bytes_read: AtomicU64,
    }

    impl ByteReader for SparseMovie {
        fn len(&self) -> u64 {
            self.moov_at + self.moov.len() as u64
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            self.bytes_read.fetch_add(buf.len() as u64, Ordering::Relaxed);
            let (region, relative) = if offset >= self.moov_at { (&self.moov, offset - self.moov_at) } else { (&self.before, offset) };
            let at = usize::try_from(relative).map_err(std::io::Error::other)?;
            // Header sniffing may span the beginning of the free padding.
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = region.get(at + i).copied().unwrap_or(0);
            }
            Ok(())
        }
    }

    #[test]
    fn a_large_movie_reads_only_its_index_and_requested_samples() {
        let bytes = movie();
        let at = moov_start(&bytes);
        let moov_at = 1 << 40;
        let mut before = bytes[..at].to_vec();
        before.extend_from_slice(&1u32.to_be_bytes());
        before.extend_from_slice(b"free");
        before.extend_from_slice(&(moov_at - at as u64).to_be_bytes());
        let reader = Arc::new(SparseMovie { before, moov: bytes[at..].to_vec(), moov_at, bytes_read: AtomicU64::new(0) });
        let streaming = open_reader("large.mp4", reader.clone()).unwrap().unwrap();
        let original = filmcraft_codecs::open_bytes("small.mp4", bytes.clone().into()).unwrap();
        let before_frames = reader.bytes_read.load(Ordering::Relaxed);
        assert!(before_frames < 32 << 10, "opening never visits the large free region");
        assert_eq!(streaming.info().duration, original.info().duration);
        assert_eq!(streaming.info().video.as_ref().unwrap().width, 16);
        for index in [0, 2, 1, 0] {
            let request = filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_120.tick_of(index));
            let stream_frame = streaming.video_frame(request).unwrap();
            let request = filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_120.tick_of(index));
            let memory_frame = original.video_frame(request).unwrap();
            let alpha = crate::convert::AlphaOp::new(effectcraft_project::AlphaMode::Ignore, [0.0; 3]);
            let stream_image = crate::convert::frame_to_image_in(&stream_frame, alpha, Vec::new()).unwrap();
            let alpha = crate::convert::AlphaOp::new(effectcraft_project::AlphaMode::Ignore, [0.0; 3]);
            let memory_image = crate::convert::frame_to_image_in(&memory_frame, alpha, Vec::new()).unwrap();
            assert_eq!(stream_image.data, memory_image.data, "streaming must preserve decoded pixels after seeking");
        }
        assert!(reader.bytes_read.load(Ordering::Relaxed) > before_frames, "frames read sample data on demand");
        assert!(reader.bytes_read.load(Ordering::Relaxed) < 128 << 10);

        let file = TempFile::new("mp4", &bytes);
        let probed = crate::probe(&file.0).unwrap();
        let old_probe = crate::probe_bytes("small.mp4", bytes.into()).unwrap();
        assert_eq!(
            (probed.width, probed.height, probed.frame_rate, probed.duration, &probed.codec),
            (old_probe.width, old_probe.height, old_probe.frame_rate, old_probe.duration, &old_probe.codec)
        );
        let pool = crate::MediaPool::new();
        let disk_image = pool.frame_at(&probed, effectcraft_time::Tick::ZERO).unwrap();
        assert_eq!(disk_image.width, 16);
        assert_eq!(pool.stats().opened, 1);
    }

    #[test]
    fn file_ranges_and_changed_files_return_checked_errors() {
        let file = TempFile::new("bin", b"01234567");
        let reader = FileReader::open(&file.0).unwrap();
        let mut bytes = [0; 3];
        reader.read_at(2, &mut bytes).unwrap();
        assert_eq!(&bytes, b"234");
        assert_eq!(reader.read_at(u64::MAX, &mut bytes).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(reader.read_at(6, &mut bytes).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        reader.read_at(8, &mut []).unwrap();
        File::options().write(true).open(&file.0).unwrap().set_len(4).unwrap();
        assert!(reader.read_at(3, &mut bytes).is_err());
        assert!(reader.check_length().unwrap_err().to_string().contains("file size changed"));
        assert!(FileReader::open(file.0.parent().unwrap()).is_err());
    }

    #[test]
    fn positional_reads_keep_independent_offsets_under_contention() {
        let bytes: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let file = TempFile::new("bin", &bytes);
        let reader = FileReader::open(&file.0).unwrap();
        std::thread::scope(|scope| {
            for lane in 0..8usize {
                let (reader, bytes) = (&reader, &bytes);
                scope.spawn(move || {
                    for round in 0..128usize {
                        let offset = (lane * 907 + round * 31) % (bytes.len() - 97);
                        let mut read = [0; 97];
                        reader.read_at(offset as u64, &mut read).unwrap();
                        assert_eq!(read, bytes[offset..offset + read.len()]);
                    }
                });
            }
        });
    }

    #[test]
    fn buffered_fallback_rejects_hostile_lengths_before_allocating() {
        struct Oversized;
        impl ByteReader for Oversized {
            fn len(&self) -> u64 {
                u64::MAX
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> std::io::Result<()> {
                panic!("oversized input must never be read")
            }
        }
        let error = buffer_reader("huge.unknown", &Oversized, MAX_BUFFERED_BYTES).unwrap_err();
        assert!(error.to_string().contains("input budget"));
        let file = TempFile::new("bin", b"123");
        let reader = FileReader::open(&file.0).unwrap();
        assert!(buffer_reader("small", &reader, 2).is_err());
        assert_eq!(buffer_reader("small", &reader, 3).unwrap(), b"123");
    }

    #[test]
    fn a_bad_accepted_container_keeps_its_error_and_can_be_repaired() {
        let file = TempFile::new("mp4", &[0, 0, 0, 24, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm']);
        assert!(open_container(&file.0).is_err());
        let pool = crate::MediaPool::new();
        let footage = effectcraft_project::Footage {
            path: file.0.to_string_lossy().into_owned(),
            kind: effectcraft_project::FootageKind::Video,
            width: 16,
            height: 16,
            has_video: true,
            duration: effectcraft_time::Tick::from_seconds_f64(1.0),
            ..Default::default()
        };
        let error = pool.frame_at(&footage, effectcraft_time::Tick::ZERO).unwrap_err().to_string();
        assert!(!error.ends_with("cannot open"), "decoder context must reach the caller");
        assert_eq!(pool.stats().frames, 0);
        std::fs::write(&file.0, movie()).unwrap();
        pool.forget(&footage.path);
        assert_eq!(pool.frame_at(&footage, effectcraft_time::Tick::ZERO).unwrap().width, 16);
    }
}
