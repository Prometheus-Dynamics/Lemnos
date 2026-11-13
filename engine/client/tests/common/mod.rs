//! A stand-in service: real socket, real frames, and just enough behaviour.

#![allow(dead_code)]

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::scm::send_with_fds;
use lemnos_wire::ring::Layout;
use lemnos_wire::{Frame, Identity, Tail, append, decode, encode_description, health, state};

/// What the fake answers StreamOpen with: a layout, how many descriptors it claims, and the
/// descriptors it actually sends.
pub struct StreamScript {
    pub layout: Layout,
    pub claimed: u8,
    pub fds: Vec<OwnedFd>,
}

#[derive(Default)]
pub struct Control {
    /// StreamRingHandles' `plane_bytes`: empty for buffers of one plane.
    pub plane_bytes: Mutex<Vec<u8>>,
    pub connections: AtomicU32,
    /// 0 means answer with the real version.
    pub version: AtomicU16,
    pub kick: AtomicBool,
    pub stop: AtomicBool,
    pub refuse_subscribe: AtomicBool,
    pub unsubscribed: AtomicU32,
    pub power_calls: AtomicU32,
    pub values_before_reply: AtomicU32,
    pub names: Mutex<Vec<String>>,
    pub stream: Mutex<Option<StreamScript>>,
    /// The `features` of Welcome. 0 is a service from before any were added.
    pub features: AtomicU64,
    /// Answer a stream being opened with StreamRingHandles, not StreamRings.
    pub answer_handles: AtomicBool,
    /// The kind of every frame that asked for a stream, in order.
    pub opens: Mutex<Vec<u16>>,
    pub stream_closes: AtomicU32,
    /// How many times it was asked what became of its packages, and the `mode` it answers
    /// with.
    pub package_asks: AtomicU32,
    pub package_mode: AtomicU32,
    /// Every job cancel asked for.
    pub cancels: Mutex<Vec<u64>>,
    /// The layout hash Welcome carries. 0 means this build's.
    pub welcome_layout: AtomicU64,
    /// The contracts Describe answers with. `None` means one contract, `a/1`.
    pub provides: Mutex<Option<&'static [lemnos_core::ContractInfo]>>,
}

pub struct FakeHost {
    pub path: PathBuf,
    pub control: Arc<Control>,
    _dir: TempDir,
}

pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn temp_dir(label: &str) -> TempDir {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-client-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

impl FakeHost {
    pub fn start() -> Self {
        let dir = temp_dir("host");
        let path = dir.0.join("sock");
        let control = Arc::new(Control::default());
        spawn(&path, Arc::clone(&control));
        Self {
            path,
            control,
            _dir: dir,
        }
    }

    /// Ends the current connection, as a service restart would.
    pub fn restart(&self) {
        self.control.kick.store(true, Ordering::SeqCst);
        while self.control.kick.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::SeqCst);
    }
}

fn spawn(path: &Path, control: Arc<Control>) {
    let listener = UnixListener::bind(path).unwrap();
    listener.set_nonblocking(true).unwrap();
    thread::spawn(move || {
        while !control.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let control = Arc::clone(&control);
                    thread::spawn(move || serve(stream, control));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return,
            }
        }
    });
}

/// Device numbers change per connection, as they may when a service restarts.
fn serve(mut stream: UnixStream, control: Arc<Control>) {
    let session = control.connections.fetch_add(1, Ordering::SeqCst) + 1;
    let device = session * 10;
    stream
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    let mut buf = vec![0u8; 1 << 16];
    let mut have = 0;
    let mut out = Vec::with_capacity(1 << 16);
    let mut sub: Option<u64> = None;
    let mut tick = 0u64;
    loop {
        if control.stop.load(Ordering::SeqCst) || control.kick.swap(false, Ordering::SeqCst) {
            return;
        }
        match stream.read(&mut buf[have..]) {
            Ok(0) => return,
            Ok(n) => have += n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        let mut used = 0;
        out.clear();
        while let Ok(Some((frame, n))) = decode(&buf[used..have]) {
            used += n;
            let send = |frame: Frame<'_>, out: &mut Vec<u8>| append(&frame, out).unwrap();
            if matches!(
                frame,
                Frame::StreamOpen { .. } | Frame::StreamOpenHandles { .. }
            ) {
                control.opens.lock().unwrap().push(frame.kind());
            }
            match frame {
                Frame::Hello { version, name, .. } => {
                    control.names.lock().unwrap().push(name.to_string());
                    let wanted = control.version.load(Ordering::SeqCst);
                    let answer = if wanted == 0 { version } else { wanted };
                    send(
                        Frame::Welcome {
                            core: match control.welcome_layout.load(Ordering::SeqCst) {
                                0 => lemnos_wire::CORE_HASH,
                                other => other,
                            },
                            parts: lemnos_wire::Tail(&lemnos_wire::PART_BYTES[..]),
                            lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
                            version: answer,
                            features: control.features.load(Ordering::SeqCst),
                            host: "fake",
                        },
                        &mut out,
                    );
                }
                Frame::Packages { id } => {
                    control.package_asks.fetch_add(1, Ordering::SeqCst);
                    send(
                        Frame::Package {
                            id,
                            path: "/p/a.so",
                            driver: "a",
                            reason: "",
                        },
                        &mut out,
                    );
                    send(
                        Frame::Package {
                            id,
                            path: "/p/b.so",
                            driver: "",
                            reason: "unsigned",
                        },
                        &mut out,
                    );
                    send(
                        Frame::PackagesDone {
                            id,
                            mode: control.package_mode.load(Ordering::SeqCst) as u8,
                            note: "half read",
                        },
                        &mut out,
                    );
                }
                Frame::List { id } => {
                    send(
                        Frame::Device {
                            id,
                            device,
                            name: "one",
                            driver: "d",
                            tags: "example.counter/1,other/1",
                            state: state::RUNNING,
                            health: health::OK,
                            reason: "",
                            link: Tail("two"),
                        },
                        &mut out,
                    );
                    send(
                        Frame::Device {
                            id,
                            device: device + 1,
                            name: "two",
                            driver: "d",
                            tags: "x/1",
                            state: state::BACKOFF,
                            health: health::FAILED,
                            reason: "bad",
                            link: Tail(""),
                        },
                        &mut out,
                    );
                    send(Frame::ListDone { id }, &mut out);
                }
                Frame::Open { id, tag, .. } => {
                    if tag == "missing/1" {
                        send(
                            Frame::Refusal {
                                id,
                                status: -2,
                                text: "no such device",
                            },
                            &mut out,
                        );
                    } else {
                        // The contract's index in what Describe answers, as the service has it.
                        let provided = control.provides.lock().unwrap().unwrap_or(&[]);
                        let contract = provided
                            .iter()
                            .position(|c| c.tag.as_str() == tag)
                            .unwrap_or(0);
                        send(
                            Frame::Opened {
                                id,
                                device,
                                contract: contract as u8,
                            },
                            &mut out,
                        );
                    }
                }
                Frame::Describe { id, device: _ } => {
                    use lemnos_core::{ContractInfo, Description, Manifest, Shape, Tag};
                    const INFO: ContractInfo = ContractInfo {
                        tag: Tag::new("a/1"),
                        kind: "other",
                        methods: &[lemnos_core::MethodInfo {
                            id: 1,
                            name: "m",
                            params: &[],
                            returns: Shape::U8,
                            has_out_buffer: false,
                            job: false,
                            reading: None,
                            added: false,
                        }],
                    };
                    let provided: &'static [lemnos_core::ContractInfo] =
                        control.provides.lock().unwrap().unwrap_or(&[INFO]);
                    let manifest = Manifest {
                        name: "d",
                        needs: &[],
                        provides: provided,
                    };
                    let mut bytes = [0; 256];
                    let n = encode_description(
                        &manifest,
                        &Description::new(),
                        &Identity::default(),
                        &mut bytes,
                    )
                    .unwrap();
                    send(
                        Frame::Description {
                            id,
                            bytes: &bytes[..n],
                        },
                        &mut out,
                    );
                }
                Frame::Call {
                    id,
                    device: target,
                    args,
                    ..
                } => {
                    if target != device {
                        send(
                            Frame::Refusal {
                                id,
                                status: -19,
                                text: "stale device number",
                            },
                            &mut out,
                        );
                    } else {
                        let before = control.values_before_reply.swap(0, Ordering::SeqCst);
                        for i in 0..before {
                            send(
                                Frame::Value {
                                    subscription: sub.unwrap_or(0),
                                    stamp_ns: u64::from(i),
                                    bytes: &[i as u8],
                                },
                                &mut out,
                            );
                        }
                        send(Frame::Reply { id, bytes: args }, &mut out);
                    }
                }
                Frame::Subscribe { id, period_us, .. } => {
                    if control.refuse_subscribe.load(Ordering::SeqCst) {
                        send(
                            Frame::Refusal {
                                id,
                                status: -16,
                                text: "busy",
                            },
                            &mut out,
                        );
                    } else {
                        let server = 100 + u64::from(session);
                        sub = Some(server);
                        send(
                            Frame::Subscribed {
                                id,
                                subscription: server,
                                period_us,
                            },
                            &mut out,
                        );
                    }
                }
                Frame::Unsubscribe { id, .. } => {
                    sub = None;
                    control.unsubscribed.fetch_add(1, Ordering::SeqCst);
                    send(Frame::Reply { id, bytes: &[] }, &mut out);
                }
                Frame::StreamClose { id, .. } => {
                    control.stream_closes.fetch_add(1, Ordering::SeqCst);
                    send(Frame::Reply { id, bytes: &[] }, &mut out);
                }
                Frame::StreamOpen { id, .. } | Frame::StreamOpenHandles { id, .. } => {
                    match control.stream.lock().unwrap().take() {
                        None => send(
                            Frame::Refusal {
                                id,
                                status: -2,
                                text: "no stream",
                            },
                            &mut out,
                        ),
                        Some(script) => {
                            stream.write_all(&out).unwrap();
                            out.clear();
                            let mut ready = Vec::new();
                            let Layout {
                                buffers,
                                buffer_bytes,
                                frame_slots,
                                job_slots,
                                job_bytes,
                            } = script.layout;
                            let descriptors = script.claimed;
                            let plane_bytes = control.plane_bytes.lock().unwrap().clone();
                            let answer = if control.answer_handles.load(Ordering::SeqCst) {
                                Frame::StreamRingHandles {
                                    id,
                                    buffers,
                                    descriptors,
                                    buffer_bytes,
                                    frame_slots,
                                    job_slots,
                                    job_bytes,
                                    plane_bytes: &plane_bytes,
                                }
                            } else {
                                Frame::StreamRings {
                                    id,
                                    buffers,
                                    descriptors,
                                    buffer_bytes,
                                    frame_slots,
                                    job_slots,
                                    job_bytes,
                                }
                            };
                            append(&answer, &mut ready).unwrap();
                            let borrowed: Vec<_> = script.fds.iter().map(AsFd::as_fd).collect();
                            if borrowed.is_empty() {
                                stream.write_all(&ready).unwrap();
                            } else {
                                let n = send_with_fds(stream.as_fd(), &ready, &borrowed).unwrap();
                                assert_eq!(n, ready.len());
                            }
                        }
                    }
                }
                // A job's first argument byte says how it runs: 0 ends at once with its
                // arguments as the result, after one step of progress; 1 does the same but sends
                // it all before JobStarted, as the service may; 2 runs until cancelled.
                Frame::CallJob { id, args, .. } => {
                    let job = u64::from(session) << 32 | u64::from(id);
                    let how = args.first().copied().unwrap_or(0);
                    if how != 1 {
                        send(Frame::JobStarted { id, job }, &mut out);
                    }
                    send(
                        Frame::JobProgress {
                            job,
                            done: 1,
                            total: 2,
                        },
                        &mut out,
                    );
                    if how != 2 {
                        send(
                            Frame::JobDone {
                                job,
                                status: 0,
                                bytes: args,
                            },
                            &mut out,
                        );
                    }
                    if how == 1 {
                        send(Frame::JobStarted { id, job }, &mut out);
                    }
                }
                Frame::JobCancel { id, job } => {
                    control.cancels.lock().unwrap().push(job);
                    send(Frame::Reply { id, bytes: &[] }, &mut out);
                    send(
                        Frame::JobDone {
                            job,
                            status: -125,
                            bytes: &[],
                        },
                        &mut out,
                    );
                }
                Frame::SetPower { id, .. } => {
                    control.power_calls.fetch_add(1, Ordering::SeqCst);
                    send(Frame::Reply { id, bytes: &[] }, &mut out);
                }
                _ => {}
            }
        }
        buf.copy_within(used..have, 0);
        have -= used;
        if let Some(server) = sub {
            tick += 1;
            append(
                &Frame::Value {
                    subscription: server,
                    stamp_ns: tick,
                    bytes: &tick.to_le_bytes(),
                },
                &mut out,
            )
            .unwrap();
        }
        if stream.write_all(&out).is_err() {
            return;
        }
    }
}

// The host's end of a stream, made over the shm objects a service makes. The descriptors come
// out in the order `lemnos_wire::ring::fds` gives, so a fake service can send them as it would.

use lemnos_wire::ring::{FrameInfo, JOB_ACK_BYTES, JobCollector};
use shm_ring::{
    FieldId, Flags, ReadMap, Ring as ShmRing, SampleTime, WriteMap, read_only, sealed_memfd,
};
use shm_ring_buffers::{BufferIndex, BufferWriter, MAX_PER_FRAME, Pool};
use shm_ring_doorbell::{Bell, Doorbell};
use shm_ring_holds::{HoldTable, Limits, Marks};

pub struct StreamHost {
    layout: lemnos_wire::ring::Layout,
    planes: u32,
    ring: ShmRing,
    publisher: BufferWriter<WriteMap>,
    jobs: JobCollector,
    holds: HoldTable<ReadMap>,
    bell: Bell,
}

impl StreamHost {
    /// A stream of `layout`, with each plane's length in `planes` (empty for one plane of a whole
    /// stride). `shared` makes the pool one descriptor; otherwise each plane of each buffer is its
    /// own. Returns the host's end and the descriptors a service sends, in order.
    pub fn make(
        layout: lemnos_wire::ring::Layout,
        planes: &[u32],
        shared: bool,
    ) -> (Self, Vec<OwnedFd>) {
        let layout = layout.checked().unwrap();
        let descs = layout.pool_descs(shared, planes);
        let pool_fds: Vec<OwnedFd> = if shared {
            vec![sealed_memfd(layout.pool_bytes() as usize).unwrap()]
        } else {
            descs
                .iter()
                .map(|d| sealed_memfd(d.size as usize).unwrap())
                .collect()
        };
        let copies: Vec<OwnedFd> = pool_fds.iter().map(|fd| fd.try_clone().unwrap()).collect();
        let pool = Pool::mapped(1, copies, descs).unwrap();
        let ring = ShmRing::create(&layout.frame_spec()).unwrap();
        let jobs_ring = ShmRing::create(&layout.job_spec()).unwrap();
        let acks = sealed_memfd(JOB_ACK_BYTES).unwrap();
        let publisher = BufferWriter::new(ring.writer().unwrap(), FieldId(0), pool);
        let marks_page =
            sealed_memfd(Marks::<WriteMap>::page_bytes(layout.buffers).unwrap()).unwrap();
        let mut holds = HoldTable::new(layout.buffers);
        holds
            .add(0, read_only(marks_page.as_fd()).unwrap(), Limits::default())
            .unwrap();
        let doorbell = Doorbell::create().unwrap();
        let (bell_fd, bell_page) = doorbell.subscriber_fds().unwrap();
        let (host_eventfd, host_page) = doorbell.bell_fds().unwrap();
        let bell = Bell::attach(host_eventfd, host_page).unwrap();

        let mut out = vec![
            ring.read_fd().unwrap(),
            publisher.pool().page_fd().unwrap(),
            jobs_ring.write_fd().unwrap(),
            read_only(acks.as_fd()).unwrap(),
            bell_fd,
            bell_page,
            marks_page,
        ];
        for fd in publisher.pool().descriptors() {
            out.push(fd.try_clone_to_owned().unwrap());
        }
        let jobs = JobCollector::new(
            jobs_ring.read_fd().unwrap(),
            acks.try_clone().unwrap(),
            &layout,
        )
        .unwrap();
        let host = Self {
            layout,
            planes: planes.len().max(1) as u32,
            ring,
            publisher,
            jobs,
            holds,
            bell,
        };
        (host, out)
    }

    /// Writes `data` into pool buffer `index` (buffer `b`'s plane `p` is `b × planes + p`) at
    /// `offset`. Only a buffer that isn't published can be written: before its frame goes out, or
    /// once it has been reclaimed.
    pub fn write(&mut self, index: u32, offset: usize, data: &[u8]) -> bool {
        let end = offset.checked_add(data.len());
        self.publisher
            .buffer_mut(BufferIndex(index))
            .and_then(|bytes| bytes.get_mut(offset..end?))
            .map(|dst| dst.copy_from_slice(data))
            .is_some()
    }

    /// The bytes of pool buffer `index`, whatever its state.
    pub fn bytes(&self, index: u32) -> Vec<u8> {
        let view = self.publisher.pool().view().unwrap();
        view.bytes(BufferIndex(index))
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }

    /// Publishes buffer `buffer` (all its planes) as a frame of `bytes` bytes. `false` if a plane
    /// isn't free to publish.
    pub fn push(
        &mut self,
        buffer: u32,
        bytes: u32,
        sequence: u64,
        identity: u64,
        timestamp_ns: u64,
    ) -> bool {
        let meta = FrameInfo {
            sequence,
            identity,
            bytes,
        }
        .encode();
        let count = self.planes as usize;
        let mut names = [BufferIndex(0); MAX_PER_FRAME];
        for (plane, name) in names.iter_mut().take(count).enumerate() {
            *name = BufferIndex(buffer * self.planes + plane as u32);
        }
        self.publisher
            .publish(&names[..count], SampleTime::from_nanos(timestamp_ns), &meta)
            .is_ok()
    }

    /// Publishes a frame whose payload is `payload` as it is, for a producer that doesn't keep
    /// the frame format. `false` if the ring refused it.
    pub fn push_raw(&mut self, payload: &[u8]) -> bool {
        self.ring
            .writer()
            .unwrap()
            .publish(FieldId(0), SampleTime::ZERO, Flags::NONE, payload)
            .is_ok()
    }

    /// Takes back buffer `buffer`'s planes once no consumer holds them. `true` when all are free.
    pub fn reclaim(&mut self, buffer: u32) -> bool {
        let mut free = true;
        for plane in 0..self.planes {
            let index = BufferIndex(buffer * self.planes + plane);
            free &= self.publisher.reclaim(index, &self.holds);
        }
        free
    }

    /// Ends the stream with `state`, and rings the consumer's bell.
    pub fn end(&mut self, state: u32) {
        self.ring.set_end(state);
        self.bell.ring();
    }

    /// The next job the consumer posted, into `out`.
    pub fn pop_job(
        &mut self,
        out: &mut [u8],
    ) -> Result<Option<usize>, lemnos_wire::ring::RingError> {
        self.jobs.next(out)
    }

    pub const fn layout(&self) -> &lemnos_wire::ring::Layout {
        &self.layout
    }
}
