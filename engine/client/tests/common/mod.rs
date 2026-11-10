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
use lemnos_wire::{Frame, append, decode, encode_description, health, state};

/// What the fake answers StreamOpen with: a layout, how many descriptors it claims, and the
/// descriptors it actually sends.
pub struct StreamScript {
    pub layout: Layout,
    pub claimed: u8,
    pub fds: Vec<OwnedFd>,
}

#[derive(Default)]
pub struct Control {
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
    /// Answer a stream being opened with StreamHandles, not StreamReady.
    pub answer_handles: AtomicBool,
    /// The kind of every frame that asked for a stream, in order.
    pub opens: Mutex<Vec<u16>>,
    pub stream_closes: AtomicU32,
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
                            version: answer,
                            features: control.features.load(Ordering::SeqCst),
                            host: "fake",
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
                        send(
                            Frame::Opened {
                                id,
                                device,
                                contract: 0,
                            },
                            &mut out,
                        );
                    }
                }
                Frame::Describe { id, device: _ } => {
                    use lemnos_core::{ContractInfo, Description, Manifest, Shape, Tag};
                    const INFO: ContractInfo = ContractInfo {
                        tag: Tag::new("a/1"),
                        methods: &[lemnos_core::MethodInfo {
                            id: 1,
                            name: "m",
                            params: &[],
                            returns: Shape::U8,
                            has_out_buffer: false,
                        }],
                    };
                    let manifest = Manifest {
                        name: "d",
                        needs: &[],
                        provides: &[INFO],
                    };
                    let mut bytes = [0; 256];
                    let n = encode_description(&manifest, &Description::new(), &mut bytes).unwrap();
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
                            let answer = if control.answer_handles.load(Ordering::SeqCst) {
                                Frame::StreamHandles {
                                    id,
                                    buffers,
                                    descriptors,
                                    buffer_bytes,
                                    frame_slots,
                                    job_slots,
                                    job_bytes,
                                }
                            } else {
                                Frame::StreamReady {
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
