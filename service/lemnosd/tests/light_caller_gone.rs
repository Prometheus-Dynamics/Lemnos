//! A status light's look ends with the connection that showed it. A client process shows a look,
//! and is SIGKILLed: the light leaves that look within [`BOUND`], and shows the next live look by
//! precedence. The other client's look is untouched.
//!
//! The bound is what lemnosd takes to learn the connection is gone (its poll wakes on the
//! socket's end at once, then the connection is released) plus the driver's next frame, which is
//! drawn on the lane in the same call that tells it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_light::Light;
use lemnos_contract_pixels::Pixels;
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, Status, driver};
use lemnos_driver_status_light::{Envelope, MAX_LEN, Rgbw, Spec, StatusLight};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// The bound on a client's look leaving the light after its connection is SIGKILLed.
const BOUND: Duration = Duration::from_secs(1);
/// How long the test waits before it calls a look still shown a failure.
const PATIENCE: Duration = Duration::from_secs(5);
const LEDS: usize = 4;

/// The frame the light last sent to the pixels (4 bytes per LED), and the frame being built.
static PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static SHOWN: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// A `pixels/1` stand-in that keeps what the light shows, so the test can read the ring.
#[driver("pixel-stand-in", provides = [Pixels])]
pub struct PixelStandIn {
    _leds: usize,
}

impl Lifecycle for PixelStandIn {
    type Needs = PixelStandInNeeds;

    fn start(_: PixelStandInNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { _leds: LEDS })
    }
}

impl Pixels for PixelStandIn {
    fn pixel_count(&mut self) -> Result<u16> {
        Ok(LEDS as u16)
    }

    fn set(&mut self, offset: u16, pixels: &[u8]) -> Result<()> {
        let mut pending = PENDING.lock().unwrap();
        if pending.is_empty() {
            pending.resize(LEDS * 4, 0);
        }
        let at = usize::from(offset) * 4;
        let end = at + pixels.len();
        if end > pending.len() {
            return Err(Status::INVALID_ARGUMENT);
        }
        pending[at..end].copy_from_slice(pixels);
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        let pending = PENDING.lock().unwrap();
        *SHOWN.lock().unwrap() = pending.clone();
        Ok(())
    }
}

/// The ring when every LED is `colour` (`[r, g, b, w]`).
fn ring(colour: [u8; 4]) -> Vec<u8> {
    colour.iter().copied().cycle().take(LEDS * 4).collect()
}

const RED: [u8; 4] = [255, 0, 0, 0];
const BLUE: [u8; 4] = [0, 0, 255, 0];
/// The built-in status.ok at the board's settings: the board's green at full look brightness.
const DEFAULT_GREEN: [u8; 4] = [0, 178, 0, 0];

/// The look bytes of a full-brightness solid fill of `colour`.
fn fill(colour: u32) -> Vec<u8> {
    let mut out = [0u8; MAX_LEN];
    let n = Spec::Fill {
        color: Rgbw::rgb(colour),
        brightness: 255,
        envelope: Envelope::Solid,
    }
    .encode(&mut out);
    out[..n].to_vec()
}

/// A scratch directory, removed when the guard drops, pass or panic.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child process that is always killed and reaped: a test that fails first takes it down too.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn start(dir: &Path) -> Service {
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"strip\"\ndriver = \"pixel-stand-in\"\n\
                [[devices]]\nid = \"light\"\ndriver = \"status-light\"\n\
                uses = { pixels = \"strip\" }\n\
                config = { count = 4, fade_ms = 0, look_brightness = 1.0 }\n";
    Service::start(Config {
        socket,
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<StatusLight>(), entry::<PixelStandIn>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap()
}

/// Waits until the ring shows `want`, or panics after [`PATIENCE`] with what it shows.
fn wait_for_ring(want: Vec<u8>, what: &str) -> Instant {
    let start = Instant::now();
    loop {
        let shown = SHOWN.lock().unwrap().clone();
        if shown == want {
            return Instant::now();
        }
        assert!(
            start.elapsed() < PATIENCE,
            "{what}: the ring still shows {shown:?}, not {want:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The client process: shows a red look at priority 200 on layer 0, says so, and waits to be
/// killed. It runs only when the test starts it with `LEMNOS_LIGHT_CLIENT` set.
#[test]
fn light_client_role() {
    let Ok(socket) = std::env::var("LEMNOS_LIGHT_CLIENT") else {
        return;
    };
    let client = Client::connect(PathBuf::from(socket), "intent").unwrap();
    let mut light = client.open::<dyn Light>("light").unwrap();
    light.show(0, 200, &fill(0xff0000)).unwrap();
    println!("LIGHT-CLIENT shown");
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn a_killed_clients_look_leaves_the_light_within_the_bound() {
    let dir = Dir(std::env::temp_dir().join(format!("light-caller-gone-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let service = start(&dir.0);
    let socket = dir.0.join("sock");

    // Another client's look, on layer 1 at priority 50: it must survive the kill.
    let other = Client::connect(&socket, "other").unwrap();
    let mut other_light = other.open::<dyn Light>("light").unwrap();
    other_light.show(1, 50, &fill(0x0000ff)).unwrap();
    wait_for_ring(ring(BLUE), "the other client's look shows");

    // The client that will die: its red look wins while it lives.
    let mut child = Reaped(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "light_client_role",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LEMNOS_LIGHT_CLIENT", &socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut out = BufReader::new(child.0.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        let read = out.read_line(&mut line).unwrap();
        assert!(
            read > 0,
            "the client process ended before it showed its look"
        );
        if line.contains("LIGHT-CLIENT shown") {
            break;
        }
    }
    wait_for_ring(ring(RED), "the client's look shows while it lives");

    // SIGKILL: no goodbye, the kernel closes its socket.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let killed = Instant::now();
    let left = wait_for_ring(ring(BLUE), "the killed client's look is still shown");
    let measured = left.duration_since(killed);
    println!(
        "LIGHT-CALLER-GONE measured {} ms from SIGKILL to the next live look (bound {} ms)",
        measured.as_millis(),
        BOUND.as_millis()
    );
    assert!(
        measured <= BOUND,
        "the look took {} ms to leave, over the {} ms bound",
        measured.as_millis(),
        BOUND.as_millis()
    );
    // Only the killed client's look ended: the other client's look still holds the ring, and once
    // it clears its own, the built-in status shows.
    assert_eq!(*SHOWN.lock().unwrap(), ring(BLUE));
    other_light.clear(1).unwrap();
    wait_for_ring(
        ring(DEFAULT_GREEN),
        "the built-in status shows with no look left",
    );
    drop(service);
}
