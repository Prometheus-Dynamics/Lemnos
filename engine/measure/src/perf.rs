//! Hardware counters for the calling thread, read into an array the caller
//! owns. Without the `perf` feature [`PerfGroup`] has no size and costs nothing.

use std::io;

/// A hardware event to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Cycles,
    Instructions,
    CacheReferences,
    CacheMisses,
    BranchMisses,
}

impl Event {
    /// The kernel's `PERF_COUNT_HW_*` number.
    #[cfg(feature = "perf")]
    const fn config(self) -> u64 {
        match self {
            Self::Cycles => 0,
            Self::Instructions => 1,
            Self::CacheReferences => 2,
            Self::CacheMisses => 3,
            Self::BranchMisses => 5,
        }
    }
}

/// `N` counters that start and read together, on the thread that opened them.
#[derive(Debug)]
pub struct PerfGroup<const N: usize> {
    #[cfg(feature = "perf")]
    fds: [libc::c_int; N],
}

#[cfg(not(feature = "perf"))]
impl<const N: usize> PerfGroup<N> {
    pub fn open(_events: &[Event; N]) -> io::Result<Self> {
        Ok(Self {})
    }

    pub fn read(&mut self, out: &mut [u64; N]) -> io::Result<()> {
        *out = [0; N];
        Ok(())
    }
}

#[cfg(feature = "perf")]
mod enabled {
    use super::*;

    const PERF_TYPE_HARDWARE: u32 = 0;
    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;
    const PERF_FORMAT_GROUP: u64 = 1 << 3;
    const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
    const FLAG_EXCLUDE_HV: u64 = 1 << 6;

    /// The fields of `struct perf_event_attr` this uses. The kernel treats
    /// the zeroed tail as defaults.
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        kind: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        tail: [u64; 9],
    }

    fn open_one(event: Event, group: libc::c_int) -> io::Result<libc::c_int> {
        let attr = Attr {
            kind: PERF_TYPE_HARDWARE,
            size: size_of::<Attr>() as u32,
            config: event.config(),
            read_format: PERF_FORMAT_GROUP,
            flags: FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV,
            ..Attr::default()
        };
        // SAFETY: `attr` outlives the call and begins with its own size; pid 0 and
        // cpu -1 mean this thread on any CPU; the kernel doesn't keep the pointer.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &raw const attr,
                0 as libc::pid_t,
                -1 as libc::c_int,
                group,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd as libc::c_int)
    }

    /// Copies a group read (`[count, value...]`) into `out`.
    pub(super) fn unpack<const N: usize>(raw: &[u64], out: &mut [u64; N]) -> io::Result<()> {
        if raw.first().copied() != Some(N as u64) || raw.len() < N + 1 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        out.copy_from_slice(&raw[1..=N]);
        Ok(())
    }

    impl<const N: usize> PerfGroup<N> {
        pub fn open(events: &[Event; N]) -> io::Result<Self> {
            let mut group = Self { fds: [-1; N] };
            for (at, event) in events.iter().enumerate() {
                let leader = if at == 0 { -1 } else { group.fds[0] };
                group.fds[at] = open_one(*event, leader)?;
            }
            Ok(group)
        }

        pub fn read(&mut self, out: &mut [u64; N]) -> io::Result<()> {
            let mut raw = [0u64; 33];
            let count = (N + 1).min(raw.len());
            // SAFETY: `raw` is valid for writes of `count` u64s for the whole call.
            let read = unsafe {
                libc::read(
                    self.fds[0],
                    raw.as_mut_ptr().cast(),
                    count * size_of::<u64>(),
                )
            };
            if read < 0 {
                return Err(io::Error::last_os_error());
            }
            unpack(&raw[..read as usize / size_of::<u64>()], out)
        }
    }

    impl<const N: usize> Drop for PerfGroup<N> {
        fn drop(&mut self) {
            for fd in self.fds {
                if fd >= 0 {
                    // SAFETY: this group opened `fd` and nothing else closes it.
                    unsafe { libc::close(fd) };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "perf"))]
    #[test]
    fn off_it_has_no_size_and_reads_zeros() {
        assert_eq!(size_of::<PerfGroup<3>>(), 0);
        let mut group = PerfGroup::open(&[Event::Cycles; 3]).unwrap();
        let mut out = [7; 3];
        group.read(&mut out).unwrap();
        assert_eq!(out, [0; 3]);
    }

    #[cfg(feature = "perf")]
    #[test]
    fn a_group_read_is_unpacked_and_checked() {
        let mut out = [0; 2];
        enabled::unpack(&[2, 10, 20], &mut out).unwrap();
        assert_eq!(out, [10, 20]);
        assert!(enabled::unpack(&[1, 10], &mut out).is_err());
        assert!(enabled::unpack(&[], &mut out).is_err());
    }

    // Many machines forbid perf events, so a refusal isn't a failure.
    #[cfg(feature = "perf")]
    #[test]
    fn cycles_go_up_where_perf_is_allowed() {
        let Ok(mut group) = PerfGroup::open(&[Event::Cycles, Event::Instructions]) else {
            return;
        };
        let (mut before, mut after) = ([0; 2], [0; 2]);
        group.read(&mut before).unwrap();
        let mut sum = 0u64;
        for i in 0..100_000u64 {
            sum = std::hint::black_box(sum + i);
        }
        group.read(&mut after).unwrap();
        assert!(after[0] >= before[0], "{before:?} {after:?}");
        assert!(sum > 0);
    }
}
