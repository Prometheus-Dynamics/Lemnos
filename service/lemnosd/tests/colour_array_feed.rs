//! A light's `current`, subscribed through lemnosd: the change feed. One client subscribes at 1 ms
//! while another sends a burst of 200 frames. The daemon grants the colour-array cap (20 ms), the
//! values come no faster than that, each differs from the one before, and the last is the burst's
//! final frame: the burst is coalesced to its latest.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_light::Light;
use lemnos_contract_pixels::Pixels;
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, Status, driver};
use lemnos_driver_status_light::StatusLight;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const LEDS: usize = 4;
/// `current`'s method number in `light/1`.
const CURRENT: u16 = 7;
/// The daemon's cap for a colour array.
const FRAME_PERIOD_US: u64 = 20_000;
/// Where an LED's colour starts in `current`: the generation (8) and the count (2), then 14 bytes
/// per LED.
const fn colour_at(led: usize) -> usize {
    10 + 14 * led
}

static PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// A `pixels/1` stand-in: the light needs a strip to draw on.
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
        Ok(())
    }
}

/// A scratch directory, removed when the guard drops, pass or panic.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start(dir: &Path) -> Service {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"strip\"\ndriver = \"pixel-stand-in\"\n\
                [[devices]]\nid = \"light\"\ndriver = \"status-light\"\n\
                uses = { pixels = \"strip\" }\n\
                config = { count = 4, fade_ms = 0, look_brightness = 1.0 }\n";
    Service::start(Config {
        socket: dir.join("sock"),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<StatusLight>(), entry::<PixelStandIn>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap()
}

/// A frame that sets LED 3 to `[n, 1, 2, 3]`.
fn frame(n: u8) -> Vec<u8> {
    let mut out = 3u16.to_le_bytes().to_vec();
    out.extend_from_slice(&[n, 1, 2, 3]);
    out
}

#[test]
fn a_burst_of_frames_reaches_a_subscriber_coalesced_to_the_latest_at_the_cap() {
    let dir = Dir(std::env::temp_dir().join(format!("lemnos-feed-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let _service = start(&dir.0);
    let socket = dir.0.join("sock");

    let reader = Client::connect(socket.clone(), "reader").unwrap();
    let mut handle = reader.open_tag("light", "light/1").unwrap();
    let subscription = reader.subscribe(&mut handle, CURRENT, 1_000, true).unwrap();
    assert_eq!(
        subscription.period_us, FRAME_PERIOD_US,
        "a colour array asked at 1 ms is granted the cap"
    );

    let burst = std::thread::spawn(move || {
        let writer = Client::connect(socket, "writer").unwrap();
        let mut light = writer.open::<dyn Light>("light").unwrap();
        for n in 0..200u8 {
            light.show_frame(3, 240, &frame(n)).unwrap();
        }
        // The writer stays connected: its frame is the ring's until the test ends.
        writer
    });

    let mut values: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut into = [0u8; 256];
    let mut burst = Some(burst);
    let mut writer = None;
    let mut quiet_since = Instant::now();
    loop {
        if let Some(info) = reader
            .next_value(&mut into, Duration::from_millis(10))
            .unwrap()
        {
            values.push((info.stamp_ns, into[..info.len].to_vec()));
            quiet_since = Instant::now();
        }
        if burst
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            writer = burst.take().map(|b| b.join().unwrap());
            quiet_since = Instant::now();
        }
        // Done once the burst is over and no value has come for ten periods.
        if writer.is_some() && quiet_since.elapsed() > Duration::from_millis(200) {
            break;
        }
    }

    assert!(!values.is_empty(), "no value came for the subscription");
    let last = &values.last().unwrap().1;
    let at = colour_at(3);
    assert_eq!(
        &last[at..at + 4],
        &[199, 1, 2, 3],
        "the last value is the burst's final frame ({} values)",
        values.len()
    );
    for pair in values.windows(2) {
        assert_ne!(pair[0].1, pair[1].1, "a value repeated the one before it");
    }
    // Reads fall on the scheduler's grid of deadlines (`deadline + k * period`), one per slot, and a
    // read ends before the next slot it uses (schedule/rules.rs `advance`). A value's stamp is its
    // read's end, so n values span more than n - 2 periods: at most span / period + 2 of them.
    let span_us = (values.last().unwrap().0 - values[0].0) / 1_000;
    let most = span_us / FRAME_PERIOD_US + 2;
    assert!(
        values.len() as u64 <= most,
        "{} values over {span_us} us: more than one per {FRAME_PERIOD_US} us ({most})",
        values.len()
    );
    assert!(
        values.len() < 200,
        "200 frames gave {} values: not coalesced",
        values.len()
    );
    drop(writer);
}
