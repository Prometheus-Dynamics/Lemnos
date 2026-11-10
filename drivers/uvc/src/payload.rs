//! UVC payload headers and frame assembly. Every isochronous packet comes from the device and is
//! untrusted.
//!
//! The rules are `uvcvideo`'s: a frame ends at EOF or when FID toggles; the stream is joined at
//! the first frame boundary seen, so a partial first frame is never delivered; after EOF the next
//! frame must toggle FID. A failed packet, a packet whose header can't be read or the ERR bit
//! damages the frame. A frame that doesn't fit is dropped whole: nothing is written past the
//! buffer.
//!
//! Assembling writes only into the one buffer this holds, which is sized once when it is made.

const FID: u8 = 0x01;
const EOF: u8 = 0x02;
const PTS: u8 = 0x04;
const SCR: u8 = 0x08;
const ERR: u8 = 0x40;

const PTS_BYTES: usize = 4;
const SCR_BYTES: usize = 6;

/// A payload header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// `bHeaderLength`: the data starts here.
    pub len: usize,
    pub fid: bool,
    pub eof: bool,
    pub error: bool,
    pub pts: Option<u32>,
    /// The device clock of the source clock reference.
    pub stc: Option<u32>,
}

impl Header {
    /// Reads the header at the start of `payload`, or `None` if it can't be right: too short, a
    /// length past the end of the packet, or too short for the fields its flags announce.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let len = usize::from(*payload.first()?);
        let flags = *payload.get(1)?;
        if len < 2 || len > payload.len() {
            return None;
        }
        let mut at = 2;
        let pts = if flags & PTS != 0 {
            let value = field(payload, at, len)?;
            at += PTS_BYTES;
            Some(value)
        } else {
            None
        };
        let stc = if flags & SCR != 0 {
            // The rest of the reference is the bus's frame counter, and it fits past the clock.
            if len < at + SCR_BYTES {
                return None;
            }
            Some(field(payload, at, len)?)
        } else {
            None
        };
        Some(Self {
            len,
            fid: flags & FID != 0,
            eof: flags & EOF != 0,
            error: flags & ERR != 0,
            pts,
            stc,
        })
    }
}

/// A little-endian `u32` at `at`, or `None` if it runs past the header's `len`.
fn field(payload: &[u8], at: usize, len: usize) -> Option<u32> {
    let end = at.checked_add(PTS_BYTES).filter(|end| *end <= len)?;
    let bytes = payload.get(at..end)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// A finished frame, good to deliver.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub bytes: &'a [u8],
    pub pts: Option<u32>,
    pub stc: Option<u32>,
}

/// What the assembler has thrown away since it was last reset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames that ended damaged, or the wrong size for an uncompressed format.
    pub damaged: u64,
    /// Frames larger than the buffer.
    pub overruns: u64,
    /// Packets whose header could not be read.
    pub bad_packets: u64,
    /// Packets the bus reported as failed.
    pub failed_packets: u64,
    /// Packets carrying data but belonging to no frame being assembled.
    pub out_of_sync: u64,
}

/// Puts frames together from payloads.
pub struct Assembler {
    buffer: Vec<u8>,
    /// Bytes a complete frame has, for an uncompressed format.
    expected: Option<usize>,
    len: usize,
    active: bool,
    fid: bool,
    damaged: bool,
    overrun: bool,
    pts: Option<u32>,
    stc: Option<u32>,
    /// FID of the last packet with a readable header.
    previous_fid: Option<bool>,
    /// FID of the last frame that ended: the next one must differ.
    done_fid: Option<bool>,
    synced: bool,
    /// A packet was lost between frames: it may have been the start of the next one.
    pending_error: bool,
    stats: Stats,
}

impl Assembler {
    /// An assembler for frames of at most `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0; capacity],
            expected: None,
            len: 0,
            active: false,
            fid: false,
            damaged: false,
            overrun: false,
            pts: None,
            stc: None,
            previous_fid: None,
            done_fid: None,
            synced: false,
            pending_error: false,
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Forgets any frame in progress and rejoins at the next frame boundary.
    pub fn reset(&mut self, expected: Option<usize>) {
        self.expected = expected;
        self.len = 0;
        self.active = false;
        self.damaged = false;
        self.overrun = false;
        self.pts = None;
        self.stc = None;
        self.previous_fid = None;
        self.done_fid = None;
        self.synced = false;
        self.pending_error = false;
    }

    /// Clears the counts as well, for a new `start`.
    pub fn clear_stats(&mut self) {
        self.stats = Stats::default();
    }

    /// Counts a packet that claimed more than its capacity. Its contents can't be trusted.
    pub fn bad_packet(&mut self) {
        self.stats.bad_packets += 1;
        self.lost_packet();
    }

    /// Feeds one packet. `sink` gets each good frame, of which one packet can finish two.
    pub fn push(&mut self, payload: &[u8], ok: bool, sink: &mut impl FnMut(Frame<'_>)) {
        if !ok {
            self.stats.failed_packets += 1;
            self.lost_packet();
            return;
        }
        if payload.is_empty() {
            return;
        }
        let Some(header) = Header::parse(payload) else {
            self.bad_packet();
            return;
        };
        let body = payload.get(header.len..).unwrap_or(&[]);
        if self.active && self.fid != header.fid {
            self.finish(sink);
        }
        if !self.active && !self.begin(&header, body) {
            return;
        }
        self.previous_fid = Some(header.fid);
        self.pts = self.pts.or(header.pts);
        self.stc = self.stc.or(header.stc);
        self.damaged |= header.error;
        self.append(body);
        if header.eof {
            self.finish(sink);
        }
    }

    /// Decides whether this packet starts a frame, and starts it if so.
    fn begin(&mut self, header: &Header, body: &[u8]) -> bool {
        let starts = if self.synced {
            self.done_fid != Some(header.fid)
        } else if self.previous_fid.is_some_and(|fid| fid != header.fid) {
            self.synced = true;
            true
        } else {
            if header.eof {
                self.synced = true;
                self.done_fid = Some(header.fid);
            }
            false
        };
        if !starts {
            if !body.is_empty() || !self.synced {
                self.stats.out_of_sync += 1;
            }
            self.previous_fid = Some(header.fid);
            return false;
        }
        self.active = true;
        self.fid = header.fid;
        self.len = 0;
        self.overrun = false;
        self.pts = None;
        self.stc = None;
        self.damaged = core::mem::take(&mut self.pending_error);
        true
    }

    fn append(&mut self, body: &[u8]) {
        let end = self
            .len
            .checked_add(body.len())
            .filter(|end| *end <= self.buffer.len());
        match end {
            Some(end) => {
                self.buffer[self.len..end].copy_from_slice(body);
                self.len = end;
            }
            None => self.overrun = true,
        }
    }

    fn lost_packet(&mut self) {
        if self.active {
            self.damaged = true;
        } else {
            self.pending_error = self.synced;
        }
    }

    fn finish(&mut self, sink: &mut impl FnMut(Frame<'_>)) {
        self.active = false;
        self.done_fid = Some(self.fid);
        if self.overrun {
            self.stats.overruns += 1;
            return;
        }
        if !self.damaged && self.len == 0 {
            // Only header-only packets: there was no frame.
            return;
        }
        if self.damaged || self.expected.is_some_and(|expected| expected != self.len) {
            self.stats.damaged += 1;
            return;
        }
        sink(Frame {
            bytes: &self.buffer[..self.len],
            pts: self.pts,
            stc: self.stc,
        });
    }
}
