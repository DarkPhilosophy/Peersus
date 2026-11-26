//! The rendering and write budgets belong to one supervised WASM helper generation.
use peersus_api::canvas::{
    FrameHeader, MAX_CANVAS_BYTES_PER_SECOND, MAX_CANVAS_FPS, MAX_FRAME_HEADER_BYTES,
    PRESENT_INVALID, PRESENT_LIMITED, PRESENT_OK, PRESENT_STREAM_FAILED,
};
use peersus_plugin_runtime::CanvasStreams;
use peersus_render::{DisplayList, RenderError, Renderer};
use std::{
    collections::{
        BTreeMap, VecDeque,
        btree_map::{Entry, OccupiedEntry},
    },
    io::{self, Write},
    time::{Duration, Instant},
};

const FRAME_WRITE_LIMIT: Duration = Duration::from_millis(50);

/// Canvas handles are not ordinary data handles: the guest cannot write raw bytes to them.
pub(crate) struct Streams<S> {
    open: BTreeMap<i32, Canvas<S>>,
    renderer: Renderer,
    budget: Budget,
    sounds: crate::sound::Sounds,
}

struct Canvas<S> {
    stream: S,
    last: Option<(String, u64)>,
}

impl<S: Write> Streams<S> {
    pub(crate) fn new() -> Self {
        Self {
            open: BTreeMap::new(),
            renderer: Renderer::default(),
            budget: Budget::default(),
            sounds: Default::default(),
        }
    }
}

impl<S: Write> CanvasStreams<S> for Streams<S> {
    fn count(&self) -> usize {
        self.open.len()
    }

    fn open(&mut self, handle: i32, stream: S) {
        self.open.insert(handle, Canvas { stream, last: None });
    }

    fn close(&mut self, handle: i32) -> bool {
        self.open.remove(&handle).is_some()
    }

    fn decode_picture(
        &mut self,
        encoded: &[u8],
        max_edge: u32,
    ) -> Result<(u32, u32, Vec<u8>), i64> {
        use peersus_api::pictures::{DECODE_INVALID, DECODE_TOO_LARGE};
        use peersus_render::picture::{DecodeError, decode};
        decode(encoded, max_edge)
            .map(|picture| (picture.width, picture.height, picture.rgba))
            .map_err(|error| match error {
                DecodeError::Invalid => DECODE_INVALID,
                DecodeError::TooLarge => DECODE_TOO_LARGE,
            })
    }

    fn audio_open(&mut self, encoded: &[u8]) -> i64 {
        self.sounds.open(encoded)
    }

    fn audio_read(&mut self, handle: i64, into: &mut [u8]) -> i64 {
        self.sounds.read(handle, into)
    }

    fn audio_length(&mut self, handle: i64) -> i64 {
        self.sounds.length(handle)
    }

    fn audio_close(&mut self, handle: i64) -> i64 {
        self.sounds.close(handle)
    }

    fn present(&mut self, source: &[u8]) -> i32 {
        let list = match DisplayList::parse(source) {
            Ok(list) => list,
            Err(RenderError::Limit) => return PRESENT_LIMITED,
            Err(_) => return PRESENT_INVALID,
        };
        let handle = list.handle();
        let Entry::Occupied(canvas) = self.open.entry(handle) else {
            return PRESENT_INVALID;
        };
        let header = list.header();
        if canvas
            .get()
            .last
            .as_ref()
            .is_some_and(|(id, seq)| id != header.id || *seq >= header.seq)
        {
            return PRESENT_INVALID;
        }
        let mut encoded = [0; MAX_FRAME_HEADER_BYTES];
        let Ok(encoded) = header.encode(&mut encoded) else {
            return PRESENT_INVALID;
        };
        // Reserve before touching Skia, even if the full command stream turns out to be invalid.
        if !self
            .budget
            .admit(Instant::now(), encoded.len() + header.layout.byte_len())
        {
            return PRESENT_LIMITED;
        }
        let pixels = match self.renderer.render(list) {
            Ok(pixels) => pixels,
            Err(RenderError::Limit | RenderError::Resource) => return PRESENT_LIMITED,
            Err(_) => return PRESENT_INVALID,
        };
        deliver_frame(canvas, header, encoded, pixels)
    }
}

fn deliver_frame<S: Write>(
    mut entry: OccupiedEntry<'_, i32, Canvas<S>>,
    header: FrameHeader<'_>,
    encoded: &[u8],
    pixels: &[u8],
) -> i32 {
    let canvas = entry.get_mut();
    if write_frame(&mut canvas.stream, encoded, pixels).is_err() {
        // A partial frame leaves framing irrecoverable. The guest must get a fresh claim.
        entry.remove();
        return PRESENT_STREAM_FAILED;
    }
    if let Some((_, seq)) = &mut canvas.last {
        *seq = header.seq;
    } else {
        canvas.last = Some((header.id.to_owned(), header.seq));
    }
    PRESENT_OK
}

fn write_frame(stream: &mut impl Write, header: &[u8], pixels: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + FRAME_WRITE_LIMIT;
    for mut part in [header, pixels] {
        while !part.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match stream.write(part) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => part = part.get(n..).ok_or(io::ErrorKind::InvalidData)?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Budget {
    frames: VecDeque<(Instant, usize)>,
    bytes: usize,
}

impl Budget {
    fn admit(&mut self, now: Instant, bytes: usize) -> bool {
        while let Some(&(at, size)) = self.frames.front() {
            if now.duration_since(at) < Duration::from_secs(1) {
                break;
            }
            self.frames.pop_front();
            self.bytes -= size;
        }
        if self.frames.len() >= MAX_CANVAS_FPS || bytes > MAX_CANVAS_BYTES_PER_SECOND - self.bytes {
            return false;
        }
        self.frames.push_back((now, bytes));
        self.bytes += bytes;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_refuses_the_next_frame_in_the_same_rolling_second() {
        let mut budget = Budget::default();
        let now = Instant::now();
        for _ in 0..MAX_CANVAS_FPS {
            assert!(budget.admit(now, 100));
        }
        assert!(!budget.admit(now + Duration::from_millis(999), 1));
        assert!(budget.admit(now + Duration::from_secs(1), 100));
    }

    #[test]
    fn byte_limit_counts_all_frames_in_the_generation_together() {
        let mut budget = Budget::default();
        let now = Instant::now();
        let payload = MAX_CANVAS_BYTES_PER_SECOND / 8;
        for _ in 0..8 {
            assert!(budget.admit(now, payload));
        }
        assert!(!budget.admit(now + Duration::from_millis(999), 1));
        assert!(budget.admit(now + Duration::from_secs(1), 1));
    }

    #[test]
    fn partial_frame_closes_the_handle_so_later_frames_cannot_desynchronize_it() {
        use peersus_api::canvas::display as dl;
        let mut list = b"PSD1\x01\x03map".to_vec();
        list.extend(1i32.to_le_bytes());
        list.extend(1u64.to_le_bytes());
        list.extend(0u64.to_le_bytes());
        for n in [1u32, 1, 1000] {
            list.extend(n.to_le_bytes());
        }
        list.extend([0; dl::PALETTE_BYTES]);
        list.push(dl::END);
        let mut streams = Streams::new();
        // An output slice that has room for part of the header and then writes zero.
        let mut wire = [0; 10];
        streams.open(1, wire.as_mut_slice());
        // Test delivery independently of the unrelated raster deadline. The same
        // production sender writes the encoded header and closes a broken handle.
        let header = DisplayList::parse(&list).unwrap().header();
        let mut encoded = [0; MAX_FRAME_HEADER_BYTES];
        let encoded = header.encode(&mut encoded).unwrap();
        let Entry::Occupied(entry) = streams.open.entry(1) else {
            panic!("fixture has no canvas handle");
        };
        assert_eq!(
            deliver_frame(entry, header, encoded, &[0; 4]),
            PRESENT_STREAM_FAILED
        );
        assert_eq!(streams.present(&list), PRESENT_INVALID);
        assert!(streams.open.is_empty());
        drop(streams);
        assert_eq!(wire, encoded[..wire.len()]);
    }
}
