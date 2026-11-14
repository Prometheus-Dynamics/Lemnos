//! The bridge's endpoint offer against a real lemnosd and a real comm core. lemnosd's USB gadget
//! runs over a mock function whose endpoint files are socket pairs: the bridge takes the pair from
//! lemnosd and offers it to the comm core, and the test plays the host at the far end of each file.
//! The comm core runs its USB link over the offered pair, so Orion protocol frames cross in both
//! directions through it.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lemnos_client::{Error as ClientError, Session};
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_hal::blocks::UsbFunctionEvent;
use lemnos_hal::mock::MockUsbFunction;
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_linux_stream::LinuxStreams;
use lemnos_orion::config::{Config, Link};
use lemnos_orion::links;
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::Address;
use orion_hello::Hello as HelloService;
use orion_keys::{Keys, KeysConfig, SharedKeys};
use orion_link_serial::StreamCarrier;
use orion_link_usb::UsbLinks;
use orion_msg_session::{Hello, Ping, Pong};
use orion_server::{Comm as CommCore, Limits, Roles, Security};
use orion_unix::UnixCarrier;
use orion_wire::{Body, Decode, StreamDecoder, encode_body, encode_frame, kind};

const WAIT: Duration = Duration::from_secs(20);

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc0\" }\n\
    config = { vendor = 0x1209, product = 1, interfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\" }\n";

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lu-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

/// lemnosd over the simulated board, whose gadget function is `function`.
fn start_lemnosd(socket: &Path, function: &MockUsbFunction) -> Service {
    let sim = SimBoard::new();
    sim.add_usb_function_of("udc0", function.clone());
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(BOARD).expect("board"),
        drivers: vec![entry::<UsbGadget>()],
        backend: Box::new(sim),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// A comm core in a thread of its own, with a USB link for each of `links` and its board socket.
struct CommThread {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn start_comm(socket: &Path, links: &[&str]) -> CommThread {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (ready, started) = mpsc::channel();
    let (path, names): (PathBuf, Vec<String>) = (
        socket.to_path_buf(),
        links.iter().map(|name| (*name).to_owned()).collect(),
    );
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async move {
            let node = "board".parse().expect("name");
            let keys = SharedKeys::new(
                Keys::open(KeysConfig::in_memory("lab"), Security::Off).expect("keys"),
            );
            let comm = CommCore::new(
                node,
                Security::Off,
                Some(Rc::new(keys.clone())),
                Limits::default(),
            );
            comm.add_service(HelloService::new(
                "board".parse().expect("name"),
                keys.clone(),
                None,
            ));
            let usb = UsbLinks::new();
            comm.add_service(usb.service());
            for name in names {
                let carrier = StreamCarrier::new(name.parse().expect("name"), usb.link(&name));
                let serving = comm.clone();
                tokio::task::spawn_local(async move { serving.serve(carrier).await });
            }
            let unix = UnixCarrier::bind(&path, Some(0o600), &[]).expect("socket");
            let serving = comm.clone();
            tokio::task::spawn_local(async move { serving.serve(unix).await });
            ready.send(()).expect("ready");
            let _ = stopped.await;
        });
    });
    started.recv().expect("the comm core starts");
    CommThread {
        stop: Some(stop),
        thread: Some(thread),
    }
}

impl Drop for CommThread {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The bridge's config for `lemnosd` at `lemnosd_socket` and the comm core at `orion`.
fn bridge_config(lemnosd_socket: &Path, orion: &Path) -> Config {
    Config {
        lemnosd_socket: lemnosd_socket.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: None,
        run_as: None,
        files: Vec::new(),
        links: Vec::new(),
        streams: Vec::new(),
        lease: Some(lemnos_orion::config::LEASE),
        stall: lemnos_orion::files::IDLE,
    }
}

/// Runs the bridge's hold for `link` in a thread: its result comes back on the channel.
fn spawn_hold(config: Config, link: &str) -> mpsc::Receiver<Result<(), String>> {
    let link = Link {
        device: "gadget".to_owned(),
        link: link.to_owned(),
    };
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(links::hold(&config, &link));
    });
    result
}

/// The host keeps the gadget configured: the bridge offers the pair only to a configured host, and a
/// let-go clears what the function knows, so the event is sent again for a while.
fn keep_configured(function: &MockUsbFunction) {
    let function = function.clone();
    std::thread::spawn(move || {
        for _ in 0..200 {
            function.event(UsbFunctionEvent::Configured);
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

/// The host's end of the gadget's two endpoint files: what it writes goes to the OUT file, and
/// what it reads comes from the IN file.
struct Host {
    to_device: UnixStream,
    from_device: UnixStream,
    decoder: Box<StreamDecoder<1024>>,
    seq: u16,
}

impl Host {
    fn new(to_device: UnixStream, from_device: UnixStream) -> Self {
        to_device.set_write_timeout(Some(WAIT)).expect("timeout");
        from_device.set_read_timeout(Some(WAIT)).expect("timeout");
        Self {
            to_device,
            from_device,
            decoder: Box::new(StreamDecoder::new()),
            seq: 0,
        }
    }

    fn send<B: Body>(&mut self, body: &B) {
        self.seq += 1;
        let (mut frame, mut wire) = ([0u8; 256], [0u8; 300]);
        let len = encode_body(body, self.seq, &mut frame).expect("encode");
        let len = encode_frame(&frame[..len], &mut wire).expect("cobs");
        self.to_device
            .write_all(&wire[..len])
            .expect("the device takes the bytes");
    }

    /// The kind and payload of the next frame the device sends.
    fn next(&mut self) -> (u8, Vec<u8>) {
        let mut chunk = [0u8; 256];
        loop {
            let len = self
                .from_device
                .read(&mut chunk)
                .expect("the device answers");
            assert!(len > 0, "the device closed its IN endpoint");
            let mut at = 0;
            while at < len {
                let (used, pushed) = self.decoder.push_slice(&chunk[at..len]);
                at += used;
                if let Ok(Some(frame)) = pushed {
                    return (frame.kind(), frame.payload().to_vec());
                }
            }
        }
    }
}

#[test]
fn a_link_offered_from_the_gadget_carries_frames_both_ways_until_the_gadget_goes_down() {
    let dir = dir("both-ways");
    let (out_device, out_host) = UnixStream::pair().expect("pair");
    let (in_device, in_host) = UnixStream::pair().expect("pair");
    let function =
        MockUsbFunction::with_endpoint_files(OwnedFd::from(out_device), OwnedFd::from(in_device));
    let lemnosd = dir.join("lemnosd.sock");
    let _service = start_lemnosd(&lemnosd, &function);
    keep_configured(&function);
    let orion = dir.join("orion.sock");
    let _comm = start_comm(&orion, &["usb0"]);
    let config = bridge_config(&lemnosd, &orion);
    let hold = spawn_hold(config, "usb0");

    // The host says hello on the OUT endpoint; the link answers on the IN endpoint.
    let mut host = Host::new(out_host, in_host);
    host.send(&Hello {
        name: "sensor",
        roles: Roles::DEVICE,
        max_frame: 512,
        revision: 1,
    });
    assert_eq!(host.next().0, kind::WELCOME);
    for round in 0..20u64 {
        host.send(&Ping { now_ms: round });
        let (kind_seen, payload) = host.next();
        assert_eq!(kind_seen, kind::PONG);
        assert_eq!(Pong::decode(&payload).expect("pong").now_ms, round);
    }

    // The gadget goes down: the controller lets go, and the hold ends with it.
    function.release();
    let ended = hold
        .recv_timeout(WAIT)
        .expect("the bridge gives the hold up when the gadget goes down");
    assert_eq!(ended, Ok(()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_link_the_comm_core_does_not_have_is_refused_and_the_pair_is_given_back() {
    let dir = dir("refused");
    let (out_device, _out_host) = UnixStream::pair().expect("pair");
    let (in_device, _in_host) = UnixStream::pair().expect("pair");
    let function =
        MockUsbFunction::with_endpoint_files(OwnedFd::from(out_device), OwnedFd::from(in_device));
    let lemnosd = dir.join("lemnosd.sock");
    let _service = start_lemnosd(&lemnosd, &function);
    keep_configured(&function);
    let orion = dir.join("orion.sock");
    let _comm = start_comm(&orion, &["usb0"]);
    let config = bridge_config(&lemnosd, &orion);

    let refused = spawn_hold(config, "nope")
        .recv_timeout(WAIT)
        .expect("the bridge gives up the offer");
    let error = refused.expect_err("the comm core refuses a link it does not have");
    assert!(error.contains("refused"), "{error}");

    // The refusal gave the pair back: lemnosd lends it to the next caller.
    let deadline = Instant::now() + WAIT;
    let mut session = Session::connect(&lemnosd, "after").expect("lemnosd");
    let device = session
        .find("usb.function/1")
        .expect("find")
        .expect("the gadget")
        .index;
    loop {
        match session.usb_endpoints(device, std::sync::Arc::new(LinuxStreams)) {
            Ok(_) => break,
            Err(ClientError::Refused { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("the pair was not given back: {error:?}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_link_comes_back_after_the_gadget_reconnects_and_carries_frames_again() {
    let dir = dir("reconnect");
    let (out_device, out_host) = UnixStream::pair().expect("pair");
    let (in_device, in_host) = UnixStream::pair().expect("pair");
    let function =
        MockUsbFunction::with_endpoint_files(OwnedFd::from(out_device), OwnedFd::from(in_device));
    let lemnosd = dir.join("lemnosd.sock");
    let _service = start_lemnosd(&lemnosd, &function);
    keep_configured(&function);
    let orion = dir.join("orion.sock");
    let _comm = start_comm(&orion, &["usb0"]);
    let hold = spawn_hold(bridge_config(&lemnosd, &orion), "usb0");

    let mut host = Host::new(out_host, in_host);
    host.send(&Hello {
        name: "sensor",
        roles: Roles::DEVICE,
        max_frame: 512,
        revision: 1,
    });
    assert_eq!(host.next().0, kind::WELCOME);

    // The host goes away: the gadget comes down, and the bridge gives the hold up.
    function.release();
    assert_eq!(hold.recv_timeout(WAIT).expect("the hold ends"), Ok(()));

    // The gadget reconnects: the bridge takes the pair again and offers it once more.
    let again = spawn_hold(bridge_config(&lemnosd, &orion), "usb0");
    for round in 0..5u64 {
        host.send(&Ping { now_ms: round });
        let (kind_seen, payload) = host.next();
        assert_eq!(kind_seen, kind::PONG, "frames flow after the reconnect");
        assert_eq!(Pong::decode(&payload).expect("pong").now_ms, round);
    }
    assert!(again.try_recv().is_err(), "the second hold stands");
    let _ = std::fs::remove_dir_all(&dir);
}
