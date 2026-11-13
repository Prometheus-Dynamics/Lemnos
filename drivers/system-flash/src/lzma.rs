//! LZMA2, decoded a chunk at a time and pushed into a window that flushes to a sink.
//!
//! An LZMA2 chunk is decoded from its complete compressed bytes, so the caller buffers one chunk
//! (at most 64 KiB) and never more: a push-based decoder cannot pull from a stream it does not own.
//! The dictionary is a ring of the stream's declared size; it is flushed to the sink before it
//! wraps, so the output's size is not bounded by it. Nothing here is `unsafe`.

use lemnos_core::Status;

use crate::crc::Check;
use crate::xz::Error;

/// The longest match LZMA can copy: 273 bytes.
const MAX_MATCH: usize = 273;
/// The smallest ring: a multiple of 16, enough that a flush always has room.
pub const MIN_WINDOW: usize = 4096;
/// The most compressed bytes an LZMA2 chunk holds.
pub const MAX_PACKED: usize = 1 << 16;
/// The most decoded bytes an LZMA2 chunk holds.
pub const MAX_UNPACKED: usize = 1 << 21;
const PROB_INIT: u16 = 1024;
const MOVE_BITS: u32 = 5;
const TOP: u32 = 1 << 24;
const END_POS_MODEL_INDEX: u32 = 14;
const NUM_FULL_DISTANCES: u32 = 128;
const NUM_ALIGN_BITS: u32 = 4;
const NUM_STATES: usize = 12;

fn malformed(why: &'static str) -> Error {
    Error::Malformed(why)
}

/// The probabilities of one LZMA stream, and the coder that uses them.
struct Probs {
    is_match: [u16; NUM_STATES << 4],
    is_rep: [u16; NUM_STATES],
    is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES],
    is_rep_g2: [u16; NUM_STATES],
    is_rep0_long: [u16; NUM_STATES << 4],
    pos_slot: [[u16; 64]; 4],
    pos_special: [u16; 1 + (NUM_FULL_DISTANCES - END_POS_MODEL_INDEX) as usize],
    align: [u16; 1 << NUM_ALIGN_BITS],
    len: LenProbs,
    rep_len: LenProbs,
    literal: Vec<u16>,
}

struct LenProbs {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}

impl LenProbs {
    const fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; 16],
            mid: [[PROB_INIT; 8]; 16],
            high: [PROB_INIT; 256],
        }
    }

    fn reset(&mut self) {
        self.choice = PROB_INIT;
        self.choice2 = PROB_INIT;
        for row in self.low.iter_mut().chain(self.mid.iter_mut()) {
            row.fill(PROB_INIT);
        }
        self.high.fill(PROB_INIT);
    }
}

impl Probs {
    fn new(literal_bits: u32) -> Self {
        Self {
            is_match: [PROB_INIT; NUM_STATES << 4],
            is_rep: [PROB_INIT; NUM_STATES],
            is_rep_g0: [PROB_INIT; NUM_STATES],
            is_rep_g1: [PROB_INIT; NUM_STATES],
            is_rep_g2: [PROB_INIT; NUM_STATES],
            is_rep0_long: [PROB_INIT; NUM_STATES << 4],
            pos_slot: [[PROB_INIT; 64]; 4],
            pos_special: [PROB_INIT; 1 + (NUM_FULL_DISTANCES - END_POS_MODEL_INDEX) as usize],
            align: [PROB_INIT; 1 << NUM_ALIGN_BITS],
            len: LenProbs::new(),
            rep_len: LenProbs::new(),
            literal: vec![PROB_INIT; 0x300 << literal_bits],
        }
    }

    /// Every probability back to its start, in place: a state reset allocates nothing.
    fn reset(&mut self) {
        self.is_match.fill(PROB_INIT);
        self.is_rep.fill(PROB_INIT);
        self.is_rep_g0.fill(PROB_INIT);
        self.is_rep_g1.fill(PROB_INIT);
        self.is_rep_g2.fill(PROB_INIT);
        self.is_rep0_long.fill(PROB_INIT);
        for row in &mut self.pos_slot {
            row.fill(PROB_INIT);
        }
        self.pos_special.fill(PROB_INIT);
        self.align.fill(PROB_INIT);
        self.len.reset();
        self.rep_len.reset();
        self.literal.fill(PROB_INIT);
    }
}

/// The range decoder over one chunk's compressed bytes.
struct Range<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Range<'a> {
    fn new(input: &'a [u8]) -> Result<Self, Error> {
        if input.len() < 5 || input[0] != 0 {
            return Err(malformed("an LZMA chunk does not start its range coder"));
        }
        let code = u32::from_be_bytes([input[1], input[2], input[3], input[4]]);
        if code == u32::MAX {
            return Err(malformed("an LZMA chunk's range coder starts wrong"));
        }
        Ok(Self {
            input,
            pos: 5,
            range: u32::MAX,
            code,
        })
    }

    fn normalize(&mut self) -> Result<(), Error> {
        if self.range < TOP {
            self.range <<= 8;
            let byte = *self
                .input
                .get(self.pos)
                .ok_or(malformed("an LZMA chunk ends before its symbols do"))?;
            self.pos += 1;
            self.code = (self.code << 8) | u32::from(byte);
        }
        Ok(())
    }

    fn bit(&mut self, prob: &mut u16) -> Result<u32, Error> {
        let bound = (self.range >> 11) * u32::from(*prob);
        let bit = if self.code < bound {
            self.range = bound;
            *prob += ((1 << 11) - *prob) >> MOVE_BITS;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob -= *prob >> MOVE_BITS;
            1
        };
        self.normalize()?;
        Ok(bit)
    }

    fn direct(&mut self, bits: u32) -> Result<u32, Error> {
        let mut result = 0u32;
        for _ in 0..bits {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            // All ones when the subtraction went negative, zero otherwise.
            let t = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & t);
            if self.code == self.range {
                return Err(malformed("an LZMA chunk's range coder is corrupt"));
            }
            self.normalize()?;
            result = (result << 1).wrapping_add(t.wrapping_add(1));
        }
        Ok(result)
    }

    /// A bit tree of `bits` bits, most significant first, its probabilities from `probs[1..]`.
    fn tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32, Error> {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) | self.bit(&mut probs[m])? as usize;
        }
        Ok(m as u32 - (1 << bits))
    }

    /// A bit tree of `bits` bits, least significant first, its probabilities at `probs[base + m]`.
    fn reverse(&mut self, probs: &mut [u16], bits: u32) -> Result<u32, Error> {
        let mut m = 1usize;
        let mut symbol = 0u32;
        for i in 0..bits {
            let bit = self.bit(&mut probs[m])?;
            m = (m << 1) | bit as usize;
            symbol |= bit << i;
        }
        Ok(symbol)
    }

    fn finished(&self) -> bool {
        self.code == 0 && self.pos == self.input.len()
    }
}

/// The dictionary: the last `cap` decoded bytes, flushed to the sink as they are made.
pub struct Window {
    buf: Vec<u8>,
    cap: usize,
    /// Where the next byte goes.
    pos: usize,
    /// Bytes decoded since the dictionary was last reset.
    total: u64,
    /// Decoded bytes not yet given to the sink: the last `pending` of them.
    pending: usize,
    /// The block's check, over every byte handed to the sink.
    pub check: Check,
    /// Every byte handed to the sink so far.
    pub emitted: u64,
}

impl Window {
    /// A window able to hold `size` bytes of history, at least [`MIN_WINDOW`].
    pub fn new(size: usize) -> Self {
        let cap = size.max(MIN_WINDOW);
        Self {
            buf: vec![0; cap],
            cap,
            pos: 0,
            total: 0,
            pending: 0,
            check: Check::None,
            emitted: 0,
        }
    }

    /// Forgets the history: the next byte starts a new dictionary.
    fn reset(&mut self, sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>) -> Result<(), Error> {
        self.flush(sink)?;
        self.total = 0;
        Ok(())
    }

    fn put(&mut self, byte: u8) {
        self.buf[self.pos] = byte;
        self.pos += 1;
        if self.pos == self.cap {
            self.pos = 0;
        }
        self.total += 1;
        self.pending += 1;
    }

    /// The byte `distance` back (1 is the last one).
    fn back(&self, distance: usize) -> u8 {
        self.buf[(self.pos + self.cap - distance) % self.cap]
    }

    /// The last byte decoded in this dictionary, or 0 when there is none.
    fn last(&self) -> u8 {
        if self.total == 0 { 0 } else { self.back(1) }
    }

    /// Hands every pending byte to the sink, in order.
    pub fn flush(
        &mut self,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
    ) -> Result<(), Error> {
        if self.pending == 0 {
            return Ok(());
        }
        let start = (self.pos + self.cap - self.pending) % self.cap;
        let first = self.pending.min(self.cap - start);
        self.emit(start, first, sink)?;
        if first < self.pending {
            self.emit(0, self.pending - first, sink)?;
        }
        self.pending = 0;
        Ok(())
    }

    /// Hands `len` bytes at `start` to the check and the sink.
    fn emit(
        &mut self,
        start: usize,
        len: usize,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
    ) -> Result<(), Error> {
        let bytes = &self.buf[start..start + len];
        self.check.update(bytes);
        self.emitted += len as u64;
        sink(bytes).map_err(Error::Sink)
    }

    /// How many bytes the ring holds.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// The most history a match may reach back to.
    fn reach(&self) -> u64 {
        self.total.min(self.cap as u64)
    }
}

/// The LZMA state the chunks carry from one to the next, until a reset.
pub struct Lzma {
    lc: u32,
    lp: u32,
    pb: u32,
    probs: Probs,
    state: usize,
    reps: [usize; 4],
}

/// The state transitions after each kind of symbol.
fn after_literal(state: usize) -> usize {
    if state < 4 {
        0
    } else if state < 10 {
        state - 3
    } else {
        state - 6
    }
}

impl Lzma {
    /// A fresh LZMA state with the given properties.
    pub fn new(lc: u32, lp: u32, pb: u32) -> Self {
        Self {
            lc,
            lp,
            pb,
            probs: Probs::new(lc + lp),
            state: 0,
            reps: [1; 4],
        }
    }

    /// Resets the probabilities and the state, keeping the properties.
    pub fn reset_state(&mut self) {
        self.probs.reset();
        self.state = 0;
        self.reps = [1; 4];
    }

    /// Decodes one chunk of `unpacked` bytes from exactly `packed` (the chunk's compressed bytes),
    /// into `win`.
    fn chunk(
        &mut self,
        win: &mut Window,
        packed: &[u8],
        unpacked: usize,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
    ) -> Result<(), Error> {
        let mut rc = Range::new(packed)?;
        let pb_mask = (1usize << self.pb) - 1;
        let lp_mask = (1u64 << self.lp) - 1;
        let mut remaining = unpacked;
        while remaining > 0 {
            if win.pending + MAX_MATCH > win.cap {
                win.flush(sink)?;
            }
            let pos_state = (win.total as usize) & pb_mask;
            let st = self.state;
            if rc.bit(&mut self.probs.is_match[(st << 4) + pos_state])? == 0 {
                self.literal(&mut rc, win, lp_mask)?;
                self.state = after_literal(st);
                remaining -= 1;
                continue;
            }
            // Each branch below assigns `len` or continues; the late assignment reads best as it is.
            #[allow(clippy::needless_late_init)]
            let len;
            if rc.bit(&mut self.probs.is_rep[st])? == 0 {
                // A match with a new distance.
                let length = len_value(&mut rc, &mut self.probs.len, pos_state)?;
                self.state = if st < 7 { 7 } else { 10 };
                let distance = self.distance(&mut rc, length)?;
                self.reps = [distance, self.reps[0], self.reps[1], self.reps[2]];
                len = length as usize + 2;
            } else if rc.bit(&mut self.probs.is_rep_g0[st])? == 0 {
                if rc.bit(&mut self.probs.is_rep0_long[(st << 4) + pos_state])? == 0 {
                    // A single byte from the last distance.
                    self.state = if st < 7 { 9 } else { 11 };
                    check_distance(win, self.reps[0])?;
                    let byte = win.back(self.reps[0]);
                    win.put(byte);
                    remaining -= 1;
                    continue;
                }
                len = rep_len(&mut rc, &mut self.probs, pos_state, st, &mut self.state)?;
            } else {
                // The distance moves to the front of the last four; no new distance is decoded.
                if rc.bit(&mut self.probs.is_rep_g1[st])? == 0 {
                    let d = self.reps[1];
                    self.reps = [d, self.reps[0], self.reps[2], self.reps[3]];
                } else if rc.bit(&mut self.probs.is_rep_g2[st])? == 0 {
                    let d = self.reps[2];
                    self.reps = [d, self.reps[0], self.reps[1], self.reps[3]];
                } else {
                    let d = self.reps[3];
                    self.reps = [d, self.reps[0], self.reps[1], self.reps[2]];
                }
                len = rep_len(&mut rc, &mut self.probs, pos_state, st, &mut self.state)?;
            }
            if len > remaining {
                return Err(malformed("an LZMA match runs past its chunk"));
            }
            check_distance(win, self.reps[0])?;
            for _ in 0..len {
                let byte = win.back(self.reps[0]);
                win.put(byte);
            }
            remaining -= len;
        }
        if !rc.finished() {
            return Err(malformed("an LZMA chunk's size does not match its symbols"));
        }
        Ok(())
    }

    fn literal(&mut self, rc: &mut Range<'_>, win: &mut Window, lp_mask: u64) -> Result<(), Error> {
        let prev = win.last() as usize;
        let lit_state = (((win.total & lp_mask) as usize) << self.lc) + (prev >> (8 - self.lc));
        let base = 0x300 * lit_state;
        let probs = &mut self.probs.literal[base..base + 0x300];
        let mut symbol = 1usize;
        if self.state >= 7 {
            // After a match the byte at the last distance says what this one is likely to be.
            let mut match_byte = usize::from(win.back(self.reps[0]));
            loop {
                let match_bit = (match_byte >> 7) & 1;
                match_byte <<= 1;
                let bit = rc.bit(&mut probs[((1 + match_bit) << 8) + symbol])? as usize;
                symbol = (symbol << 1) | bit;
                if match_bit != bit || symbol >= 0x100 {
                    break;
                }
            }
        }
        while symbol < 0x100 {
            symbol = (symbol << 1) | rc.bit(&mut probs[symbol])? as usize;
        }
        win.put((symbol - 0x100) as u8);
        Ok(())
    }

    /// The distance of a new match of length `length` (0 based), as the distance it copies
    /// from. Its value as stored is one more than the decoded distance.
    fn distance(&mut self, rc: &mut Range<'_>, length: u32) -> Result<usize, Error> {
        let len_state = length.min(3) as usize;
        let slot = rc.tree(&mut self.probs.pos_slot[len_state], 6)?;
        let distance: u64 = if slot < 4 {
            u64::from(slot)
        } else {
            let direct = (slot >> 1) - 1;
            let base = u64::from(2 | (slot & 1)) << direct;
            if slot < END_POS_MODEL_INDEX {
                let at = (base - u64::from(slot)) as usize;
                base + u64::from(rc.reverse(&mut self.probs.pos_special[at..], direct)?)
            } else {
                let high = u64::from(rc.direct(direct - NUM_ALIGN_BITS)?) << NUM_ALIGN_BITS;
                let low = u64::from(rc.reverse(&mut self.probs.align, NUM_ALIGN_BITS)?);
                base + high + low
            }
        };
        if distance == u64::from(u32::MAX) {
            return Err(malformed(
                "an LZMA chunk ends its stream with an end marker",
            ));
        }
        Ok(distance as usize + 1)
    }
}

/// A length's value (0 to 271, the match is that plus two).
fn len_value(rc: &mut Range<'_>, probs: &mut LenProbs, pos_state: usize) -> Result<u32, Error> {
    if rc.bit(&mut probs.choice)? == 0 {
        return rc.tree(&mut probs.low[pos_state], 3);
    }
    if rc.bit(&mut probs.choice2)? == 0 {
        return Ok(8 + rc.tree(&mut probs.mid[pos_state], 3)?);
    }
    Ok(16 + rc.tree(&mut probs.high, 8)?)
}

/// The length of a match that repeats a distance, and the state after it.
fn rep_len(
    rc: &mut Range<'_>,
    probs: &mut Probs,
    pos_state: usize,
    st: usize,
    state: &mut usize,
) -> Result<usize, Error> {
    let length = len_value(rc, &mut probs.rep_len, pos_state)?;
    *state = if st < 7 { 8 } else { 11 };
    Ok(length as usize + 2)
}

fn check_distance(win: &Window, distance: usize) -> Result<(), Error> {
    if distance == 0 || distance as u64 > win.reach() {
        return Err(malformed(
            "an LZMA match reaches before the start of its dictionary",
        ));
    }
    Ok(())
}

/// The LZMA2 properties byte: `(pb * 5 + lp) * 9 + lc`.
pub fn props(byte: u8) -> Result<(u32, u32, u32), Error> {
    if byte >= 9 * 5 * 5 {
        return Err(malformed("an LZMA2 chunk's properties are out of range"));
    }
    let lc = u32::from(byte % 9);
    let rest = byte / 9;
    let lp = u32::from(rest % 5);
    let pb = u32::from(rest / 5);
    if lc + lp > 4 {
        return Err(malformed("an LZMA2 chunk's lc + lp is over 4"));
    }
    Ok((lc, lp, pb))
}

/// The dictionary size an LZMA2 property byte declares, in bytes.
pub fn dict_size(byte: u8) -> Result<u64, Error> {
    if byte > 40 {
        return Err(malformed("an LZMA2 dictionary property is out of range"));
    }
    if byte == 40 {
        return Ok(u64::from(u32::MAX));
    }
    Ok((2 | u64::from(byte & 1)) << (byte / 2 + 11))
}

/// What the LZMA2 chunk parser needs next.
enum Want {
    /// A control byte.
    Control,
    /// The rest of a chunk's header, of `n` bytes.
    Header(usize),
    /// A chunk's body: `n` bytes.
    Body(usize),
}

/// LZMA2 chunks, pushed a piece at a time.
pub struct Lzma2 {
    want: Want,
    scratch: Vec<u8>,
    control: u8,
    /// Decoded size and compressed size of the LZMA chunk being read, when it is one.
    unpacked: usize,
    packed: usize,
    /// The LZMA state, when one has been set up.
    lzma: Option<Lzma>,
    need_dict_reset: bool,
    need_props: bool,
}

/// What [`Lzma2::push`] left.
pub enum Pushed {
    /// The stream is not over.
    More,
    /// The end marker was read; the stream is over.
    End,
}

impl Default for Lzma2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Lzma2 {
    /// A stream that starts with a dictionary reset, as an xz block's does.
    pub fn new() -> Self {
        Self {
            want: Want::Control,
            scratch: Vec::with_capacity(MAX_PACKED + 8),
            control: 0,
            unpacked: 0,
            packed: 0,
            lzma: None,
            need_dict_reset: true,
            need_props: true,
        }
    }

    /// Takes bytes from the front of `data` (which it shrinks), decoding whole chunks into `win`.
    /// Stops at the end marker or when `data` runs out.
    pub fn push(
        &mut self,
        data: &mut &[u8],
        win: &mut Window,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
    ) -> Result<Pushed, Error> {
        loop {
            let need = match self.want {
                Want::Control => 1,
                Want::Header(n) | Want::Body(n) => n,
            };
            if !self.take(data, need) {
                return Ok(Pushed::More);
            }
            let bytes = std::mem::take(&mut self.scratch);
            let outcome = self.step(&bytes, win, sink);
            self.scratch = bytes;
            self.scratch.clear();
            if let Some(end) = outcome? {
                return Ok(end);
            }
        }
    }

    /// Adds bytes from `data` to the scratch until it holds `need`. True when it does.
    fn take(&mut self, data: &mut &[u8], need: usize) -> bool {
        let have = self.scratch.len();
        let add = (need - have).min(data.len());
        self.scratch.extend_from_slice(&data[..add]);
        *data = &data[add..];
        self.scratch.len() == need
    }

    /// Acts on a complete piece: a control byte, a header or a body. Returns the outcome when the
    /// stream ends, `None` to go on.
    fn step(
        &mut self,
        bytes: &[u8],
        win: &mut Window,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
    ) -> Result<Option<Pushed>, Error> {
        match self.want {
            Want::Control => {
                self.control = bytes[0];
                match self.control {
                    0x00 => return Ok(Some(Pushed::End)),
                    0x01 | 0x02 => {
                        if self.control == 0x01 {
                            win.reset(sink)?;
                            self.need_dict_reset = false;
                        } else if self.need_dict_reset {
                            return Err(malformed(
                                "an LZMA2 stream starts without a dictionary reset",
                            ));
                        }
                        self.want = Want::Header(2);
                    }
                    0x80..=0xFF => {
                        let reset = (self.control >> 5) & 3;
                        if self.need_dict_reset && reset != 3 {
                            return Err(malformed(
                                "an LZMA2 stream starts without a dictionary reset",
                            ));
                        }
                        if self.need_props && reset < 2 {
                            return Err(malformed(
                                "an LZMA2 stream's first LZMA chunk sets no properties",
                            ));
                        }
                        let header = if reset >= 2 { 5 } else { 4 };
                        self.want = Want::Header(header);
                    }
                    _ => {
                        return Err(malformed(
                            "an LZMA2 chunk has a control byte that is not one",
                        ));
                    }
                }
            }
            Want::Header(_) if self.control < 0x80 => {
                // An uncompressed chunk: its size, minus one, big-endian.
                let size = usize::from(u16::from_be_bytes([bytes[0], bytes[1]])) + 1;
                self.unpacked = size;
                self.want = Want::Body(size);
            }
            Want::Header(_) => {
                let unpacked = ((usize::from(self.control) & 0x1F) << 16)
                    + usize::from(u16::from_be_bytes([bytes[0], bytes[1]]))
                    + 1;
                let packed = usize::from(u16::from_be_bytes([bytes[2], bytes[3]])) + 1;
                let reset = (self.control >> 5) & 3;
                if reset >= 2 {
                    let (lc, lp, pb) = props(bytes[4])?;
                    match &mut self.lzma {
                        Some(lzma) if lzma.lc == lc && lzma.lp == lp && lzma.pb == pb => {}
                        _ => self.lzma = Some(Lzma::new(lc, lp, pb)),
                    }
                    self.need_props = false;
                }
                if reset == 3 {
                    win.reset(sink)?;
                    self.need_dict_reset = false;
                }
                let Some(lzma) = self.lzma.as_mut() else {
                    return Err(malformed("an LZMA2 chunk has no properties"));
                };
                if reset >= 1 {
                    lzma.reset_state();
                }
                if unpacked > MAX_UNPACKED || packed > MAX_PACKED {
                    return Err(malformed("an LZMA2 chunk is larger than the format allows"));
                }
                self.unpacked = unpacked;
                self.packed = packed;
                self.want = Want::Body(packed);
            }
            Want::Body(_) if self.control < 0x80 => {
                sink_bytes(win, bytes, sink)?;
                self.want = Want::Control;
            }
            Want::Body(_) => {
                let lzma = self
                    .lzma
                    .as_mut()
                    .ok_or(malformed("an LZMA2 chunk has no properties"))?;
                lzma.chunk(win, bytes, self.unpacked, sink)?;
                self.want = Want::Control;
            }
        }
        Ok(None)
    }
}

/// Copies an uncompressed chunk's bytes into the window and on to the sink.
fn sink_bytes(
    win: &mut Window,
    bytes: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> Result<(), Status>,
) -> Result<(), Error> {
    for &byte in bytes {
        if win.pending + 1 > win.cap {
            win.flush(sink)?;
        }
        win.put(byte);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_sizes_follow_the_format() {
        // (2 | p & 1) << (p / 2 + 11): xz -6 declares 8 MiB (22), xz -7 16 MiB (24).
        assert_eq!(dict_size(0).unwrap(), 4096);
        assert_eq!(dict_size(1).unwrap(), 6144);
        assert_eq!(dict_size(16).unwrap(), 1 << 20);
        assert_eq!(dict_size(22).unwrap(), 8 << 20);
        assert_eq!(dict_size(24).unwrap(), 16 << 20);
        assert!(dict_size(41).is_err());
    }

    #[test]
    fn properties_split_into_lc_lp_pb() {
        // The default of xz: lc 3, lp 0, pb 2 is 93.
        assert_eq!(props(93).unwrap(), (3, 0, 2));
        assert!(props(225).is_err());
    }
}
