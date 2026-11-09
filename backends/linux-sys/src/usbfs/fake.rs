//! A stand-in for the kernel behind a [`UsbfsFile`](super::UsbfsFile): it records what it is
//! asked and answers with what the test crafted, including lies about lengths. Test support only.
//!
//! Its completions write real memory, as the kernel does, but only within what was submitted.
//! They can claim anything in the length fields, which is what a test needs.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::ring::UrbStorage;
use super::{
    BULK, BulkTransfer, CLAIMINTERFACE, CONTROL, CtrlTransfer, DIR_IN, DISCARDURB, IsoPacketDesc,
    REAPURBNDELAY, RELEASEINTERFACE, SETINTERFACE, SUBMITURB, SetInterface, UrbHeader,
};
use crate::poll::{POLLHUP, POLLOUT};

/// URBs the fake can hold without allocating.
const MAX_FAKE_URBS: usize = 16;

/// One call the fake answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// `USBDEVFS_CONTROL`.
    Control {
        /// `bmRequestType`.
        request_type: u8,
        /// `bRequest`.
        request: u8,
        /// `wValue`.
        value: u16,
        /// `wIndex`.
        index: u16,
        /// `wLength`.
        length: u16,
        /// Milliseconds.
        timeout: u32,
        /// The data pointer's address.
        data: usize,
    },
    /// `USBDEVFS_BULK`.
    Bulk {
        /// The endpoint address.
        endpoint: u32,
        /// Bytes.
        length: u32,
        /// Milliseconds.
        timeout: u32,
        /// The data pointer's address.
        data: usize,
    },
    /// `USBDEVFS_CLAIMINTERFACE`.
    Claim(u32),
    /// `USBDEVFS_RELEASEINTERFACE`.
    Release(u32),
    /// `USBDEVFS_SETINTERFACE`.
    SetInterface {
        /// The interface number.
        interface: u32,
        /// The alternate setting.
        alternate: u32,
    },
    /// `USBDEVFS_SUBMITURB`.
    Submit {
        /// The URB's address.
        urb: usize,
        /// `usbdevfs_urb.type`.
        kind: u8,
        /// The endpoint address.
        endpoint: u8,
        /// `usbdevfs_urb.flags`.
        flags: u32,
        /// The buffer's address.
        buffer: usize,
        /// `usbdevfs_urb.buffer_length`.
        buffer_length: i32,
        /// `usbdevfs_iso_packet_desc.length` of each packet.
        lengths: Vec<u32>,
    },
    /// `USBDEVFS_DISCARDURB`.
    Discard(usize),
}

/// One crafted completion: every field a real completion carries, and the header fields the
/// kernel never writes, for a test to change.
#[derive(Debug, Clone, Default)]
pub struct CraftedUrb {
    /// `usbdevfs_urb.status`.
    pub status: i32,
    /// `usbdevfs_urb.actual_length`; `None` adds up the packets.
    pub actual_length: Option<i32>,
    /// `usbdevfs_urb.error_count`.
    pub error_count: i32,
    /// `(status, actual_length)` of each packet.
    pub packets: Vec<(i32, u32)>,
    /// Bytes written to the buffer from its start, up to the submitted length.
    pub payload: Vec<u8>,
    /// Overwrites `number_of_packets`.
    pub number_of_packets: Option<i32>,
    /// Overwrites `buffer_length`.
    pub buffer_length: Option<i32>,
    /// Overwrites the `buffer` pointer.
    pub buffer: Option<usize>,
    /// The reap reports this address instead of the URB, which stays in flight.
    pub foreign: Option<usize>,
    /// The reap reports the URB's own address a second time after this one.
    pub repeat_address: bool,
}

/// One reply to a control or bulk call.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    /// Bytes written to the caller's buffer, up to the length it passed (IN only).
    pub data: Vec<u8>,
    /// What the ioctl returns; `None` is the number of bytes moved.
    pub returned: Option<i32>,
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    recording: bool,
    fail_next: Option<i32>,
    replies: VecDeque<Reply>,
    /// The reply that repeats once `replies` is empty.
    steady_reply: Option<Reply>,
    /// Device, then configurations.
    descriptors: Vec<u8>,
    in_flight: VecDeque<usize>,
    ready: VecDeque<CraftedUrb>,
    /// Completes URBs like this forever, with nothing allocated.
    steady_urb: Option<CraftedUrb>,
    /// A URB completes only when `ready` says so: `DISCARDURB` does nothing.
    stuck: bool,
    /// Reaps fail with `ENODEV` once nothing is ready, and polls report a hang-up.
    gone: bool,
    duplicate: Option<usize>,
}

/// The fake kernel. Clones share state.
#[derive(Clone, Default)]
pub struct FakeUsbfs(Arc<Mutex<State>>);

impl FakeUsbfs {
    /// A fake that records every call.
    pub fn new() -> Self {
        let fake = Self::default();
        {
            let mut state = fake.lock();
            state.recording = true;
            // So that a steady stream through the fake allocates nothing either.
            state.in_flight.reserve(MAX_FAKE_URBS);
        }
        fake
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops recording calls, so a steady-state run records nothing.
    pub fn stop_recording(&self) {
        self.lock().recording = false;
    }

    /// What has been called so far.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// The next ioctl fails with `errno`.
    pub fn fail_next(&self, errno: i32) {
        self.lock().fail_next = Some(errno);
    }

    /// Queues one control or bulk reply.
    pub fn queue_reply(&self, reply: Reply) {
        self.lock().replies.push_back(reply);
    }

    /// The reply to every control or bulk call once none are queued.
    pub fn set_steady_reply(&self, reply: Reply) {
        self.lock().steady_reply = Some(reply);
    }

    /// The bytes `read_at` serves.
    pub fn set_descriptors(&self, descriptors: &[u8]) {
        self.lock().descriptors = descriptors.to_vec();
    }

    /// The next reap of a URB in flight completes it with `urb`.
    pub fn queue_urb(&self, urb: CraftedUrb) {
        self.lock().ready.push_back(urb);
    }

    /// Every URB in flight completes as `urb`, with no bookkeeping that allocates.
    pub fn set_steady_urb(&self, urb: CraftedUrb) {
        self.lock().steady_urb = Some(urb);
    }

    /// Whether `DISCARDURB` is ignored, as for a URB the kernel holds on to.
    pub fn set_stuck(&self, stuck: bool) {
        self.lock().stuck = stuck;
    }

    /// Whether the device has left.
    pub fn set_gone(&self, gone: bool) {
        self.lock().gone = gone;
    }

    /// URBs the kernel holds.
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight.len()
    }

    pub(super) fn read_at(&self, out: &mut [u8], offset: u64) -> usize {
        let state = self.lock();
        let Some(rest) = usize::try_from(offset)
            .ok()
            .and_then(|offset| state.descriptors.get(offset..))
        else {
            return 0;
        };
        let n = rest.len().min(out.len());
        out[..n].copy_from_slice(&rest[..n]);
        n
    }

    /// `poll` on the device file, for `POLLOUT`.
    pub(super) fn poll(&self, timeout: Duration) -> i16 {
        {
            let state = self.lock();
            let ready = !state.in_flight.is_empty()
                && (!state.ready.is_empty() || state.steady_urb.is_some())
                || state.duplicate.is_some();
            if ready {
                return POLLOUT;
            }
            if state.gone {
                return POLLHUP;
            }
        }
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
        0
    }

    pub(super) fn ioctl_int(
        &self,
        request: u32,
        arg: libc::c_ulong,
    ) -> std::io::Result<libc::c_int> {
        assert_eq!(request, DISCARDURB, "the only by-value request");
        let mut state = self.lock();
        let urb = arg as usize;
        if state.recording {
            state.calls.push(Call::Discard(urb));
        }
        let held = state.in_flight.contains(&urb);
        if !held {
            return Err(errno(libc::EINVAL));
        }
        if !state.stuck {
            state.ready.push_back(CraftedUrb {
                status: -libc::ENOENT,
                ..CraftedUrb::default()
            });
        }
        Ok(0)
    }

    /// The fake's side of an ioctl with a pointer argument.
    ///
    /// # Safety
    ///
    /// `arg` must be a live value of the type `request` encodes, and any pointer in it must be
    /// valid for the reads and writes the request performs. A URB handed to `SUBMITURB` must stay
    /// live until the same URB comes back from a reap.
    pub(super) unsafe fn ioctl_ptr(
        &self,
        request: u32,
        arg: *mut c_void,
    ) -> std::io::Result<libc::c_int> {
        let mut state = self.lock();
        if request != REAPURBNDELAY
            && let Some(code) = state.fail_next.take()
        {
            return Err(errno(code));
        }
        match request {
            CONTROL => {
                // SAFETY: the caller promises `arg` is a live `usbdevfs_ctrltransfer`.
                let transfer = unsafe { arg.cast::<CtrlTransfer>().read() };
                let call = Call::Control {
                    request_type: transfer.request_type,
                    request: transfer.request,
                    value: transfer.value,
                    index: transfer.index,
                    length: transfer.length,
                    timeout: transfer.timeout,
                    data: transfer.data as usize,
                };
                if state.recording {
                    state.calls.push(call);
                }
                // SAFETY: the caller promises `data` holds `length` bytes, writable for IN.
                unsafe {
                    state.reply(
                        transfer.request_type & DIR_IN != 0,
                        transfer.data.cast(),
                        usize::from(transfer.length),
                    )
                }
            }
            BULK => {
                // SAFETY: the caller promises `arg` is a live `usbdevfs_bulktransfer`.
                let transfer = unsafe { arg.cast::<BulkTransfer>().read() };
                let call = Call::Bulk {
                    endpoint: transfer.endpoint,
                    length: transfer.length,
                    timeout: transfer.timeout,
                    data: transfer.data as usize,
                };
                if state.recording {
                    state.calls.push(call);
                }
                // SAFETY: the caller promises `data` holds `length` bytes, writable for IN.
                unsafe {
                    state.reply(
                        transfer.endpoint & u32::from(DIR_IN) != 0,
                        transfer.data.cast(),
                        transfer.length as usize,
                    )
                }
            }
            CLAIMINTERFACE | RELEASEINTERFACE => {
                // SAFETY: the caller promises `arg` is a live `unsigned int`.
                let number = unsafe { arg.cast::<libc::c_uint>().read() };
                let call = if request == CLAIMINTERFACE {
                    Call::Claim(number)
                } else {
                    Call::Release(number)
                };
                if state.recording {
                    state.calls.push(call);
                }
                Ok(0)
            }
            SETINTERFACE => {
                // SAFETY: the caller promises `arg` is a live `usbdevfs_setinterface`.
                let setting = unsafe { arg.cast::<SetInterface>().read() };
                if state.recording {
                    state.calls.push(Call::SetInterface {
                        interface: setting.interface,
                        alternate: setting.alternate,
                    });
                }
                Ok(0)
            }
            SUBMITURB => {
                // SAFETY: the caller promises `arg` is a live URB and stays so until reaped.
                unsafe { state.submit(arg.cast()) };
                Ok(0)
            }
            REAPURBNDELAY => {
                // SAFETY: the caller promises `arg` is a live pointer slot, and every address in
                // `in_flight` is a URB that is still live.
                unsafe { state.reap(arg.cast()) }
            }
            other => panic!("the fake has no request {other:#x}"),
        }
    }
}

impl State {
    /// # Safety
    ///
    /// `data` holds `length` bytes, writable when `is_in`.
    unsafe fn reply(
        &mut self,
        is_in: bool,
        data: *mut u8,
        length: usize,
    ) -> std::io::Result<libc::c_int> {
        let queued = self.replies.pop_front();
        let reply = queued.as_ref().or(self.steady_reply.as_ref());
        if is_in && reply.is_none() {
            return Err(errno(libc::ETIMEDOUT));
        }
        let (bytes, returned) = reply.map_or((&[][..], None), |r| (r.data.as_slice(), r.returned));
        let mut moved = length;
        if is_in {
            moved = bytes.len().min(length);
            // SAFETY: `moved` fits `data` (the caller's promise) and the reply's bytes; the
            // ranges don't overlap, since the reply is this fake's own allocation.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, moved) };
        }
        Ok(returned.unwrap_or(moved as i32))
    }

    /// # Safety
    ///
    /// `urb` is a live URB followed by its packet descriptors.
    unsafe fn submit(&mut self, urb: *mut UrbHeader) {
        if self.recording {
            // SAFETY: the caller promises `urb` is live.
            let header = unsafe { urb.read() };
            let base = urb.cast::<u8>();
            let lengths = (0..header.number_of_packets as usize)
                .map(|index| {
                    // SAFETY: the table follows the header, one descriptor per packet.
                    let desc = unsafe {
                        base.add(size_of::<UrbHeader>() + index * size_of::<IsoPacketDesc>())
                    };
                    // SAFETY: `desc` is that descriptor; `length` is its first field.
                    unsafe { desc.cast::<u32>().read() }
                })
                .collect();
            self.calls.push(Call::Submit {
                urb: urb as usize,
                kind: header.kind,
                endpoint: header.endpoint,
                flags: header.flags,
                buffer: header.buffer as usize,
                buffer_length: header.buffer_length,
                lengths,
            });
        }
        self.in_flight.push_back(urb as usize);
    }

    /// # Safety
    ///
    /// `out` is a live pointer slot, and every in-flight address is a live URB.
    unsafe fn reap(&mut self, out: *mut *mut c_void) -> std::io::Result<libc::c_int> {
        if let Some(code) = self.fail_next.take() {
            return Err(errno(code));
        }
        if let Some(urb) = self.duplicate.take() {
            // SAFETY: `out` is a live pointer slot.
            unsafe { out.write(urb as *mut c_void) };
            return Ok(0);
        }
        let Some(&address) = self.in_flight.front() else {
            return Err(errno(if self.gone {
                libc::ENODEV
            } else {
                libc::EAGAIN
            }));
        };
        let queued = self.ready.pop_front();
        let Some(crafted) = queued.as_ref().or(self.steady_urb.as_ref()) else {
            return Err(errno(if self.gone {
                libc::ENODEV
            } else {
                libc::EAGAIN
            }));
        };
        if let Some(foreign) = crafted.foreign {
            // SAFETY: `out` is a live pointer slot.
            unsafe { out.write(foreign as *mut c_void) };
            return Ok(0);
        }
        // SAFETY: `address` is a URB still in flight, live by the caller's promise, and nothing
        // else touches it until it is handed back through `out`.
        unsafe { complete(address as *mut UrbStorage, crafted) };
        if crafted.repeat_address {
            self.duplicate = Some(address);
        }
        self.in_flight.pop_front();
        // SAFETY: `out` is a live pointer slot.
        unsafe { out.write(address as *mut c_void) };
        Ok(0)
    }
}

/// Writes a crafted completion over the URB the way the kernel would.
///
/// # Safety
///
/// `urb` is a live, submitted URB nothing else touches.
unsafe fn complete(urb: *mut UrbStorage, crafted: &CraftedUrb) {
    // SAFETY: the caller's promise.
    let storage = unsafe { &mut *urb };
    let submitted = usize::try_from(storage.header.buffer_length).unwrap_or(0);
    let bytes = crafted.payload.len().min(submitted);
    // SAFETY: the buffer holds `submitted` bytes, `bytes` fits both, and the ranges don't overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(
            crafted.payload.as_ptr(),
            storage.header.buffer.cast::<u8>(),
            bytes,
        );
    }
    let mut total = 0i32;
    for (slot, &(status, actual)) in storage.packets.iter_mut().zip(&crafted.packets) {
        slot.status = status as u32;
        slot.actual_length = actual;
        total = total.wrapping_add(actual as i32);
    }
    let header = &mut storage.header;
    header.status = crafted.status;
    header.actual_length = crafted.actual_length.unwrap_or(total);
    header.error_count = crafted.error_count;
    if let Some(count) = crafted.number_of_packets {
        header.number_of_packets = count;
    }
    if let Some(length) = crafted.buffer_length {
        header.buffer_length = length;
    }
    if let Some(buffer) = crafted.buffer {
        header.buffer = buffer as *mut c_void;
    }
}

fn errno(code: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code)
}
