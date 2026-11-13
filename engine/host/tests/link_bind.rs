//! A sensor's board `link` is checked at bind, through the host's own routing: the sensor asks the
//! device the link names what is wired to its input, and the answer decides its health. The two
//! drivers here are stand-ins for a camera sensor and its receiver, with the contracts they serve.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::quick;
use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig,
    MAX_FEED_NAME, Packing,
};
use lemnos_contract_camera_sensor::{
    AppliedLanding, CameraSensor, Controls, FrameValues, Landing, LinkCheck, Mode, SensorInfo,
    check_link,
};
use lemnos_core::{Config, Dev, Health, Lifecycle, Result, Status, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, entry};
use lemnos_sim::{SimBoard, SimClock};

/// The name the kernel gives the sensor, as the receiver answers it when the link is right.
const SENSOR: &[u8] = b"ov9782 10-0036";

/// A receiver whose `source` answers the board's `config.source`: a name, `nothing` for nothing
/// wired to its input, or `unknown`.
#[driver("link-provider", provides = [CameraReceiver])]
struct LinkedProvider {
    source: String,
}

impl Lifecycle for LinkedProvider {
    type Needs = LinkedProviderNeeds;

    fn start(_needs: LinkedProviderNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            source: cfg.str("source").unwrap_or("unknown").to_string(),
        })
    }
}

impl CameraReceiver for LinkedProvider {
    fn limits(&mut self) -> Result<Limits> {
        Err(Status::NOT_SUPPORTED)
    }
    fn configure(&mut self, _link: LinkConfig, _packing: Packing) -> Result<FrameLayout> {
        Err(Status::NOT_SUPPORTED)
    }
    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(None)
    }
    fn frame_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn start(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn stop(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn counters(&mut self) -> Result<Counters> {
        Err(Status::NOT_SUPPORTED)
    }
    fn feeds(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
    fn source(&mut self, into: &mut [u8]) -> Result<usize> {
        match self.source.as_str() {
            "unknown" => Err(Status::NOT_SUPPORTED),
            "nothing" => Ok(0),
            // A device that does not serve the receiver contract refuses the question as an
            // invalid call, as the host answers for a link to a device without it.
            "refuse" => Err(Status::INVALID_ARGUMENT),
            name => {
                into.get_mut(..name.len())
                    .ok_or(Status::BUFFER_TOO_SMALL)?
                    .copy_from_slice(name.as_bytes());
                Ok(name.len())
            }
        }
    }
    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(None)
    }
    /// Not counted yet: the frame-start count is not wired in this driver.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Err(Status::NOT_SUPPORTED)
    }

    fn embedded_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
}

/// A sensor whose bind asks the receiver its link, as the camera sensors do. It serves no frames:
/// every `CameraSensor` method refuses.
#[driver("link-consumer", provides = [CameraSensor])]
struct LinkedConsumer {
    #[link]
    _receiver: Option<Dev<dyn CameraReceiver>>,
    check: LinkCheck,
}

impl Lifecycle for LinkedConsumer {
    type Needs = LinkedConsumerNeeds;

    fn start(mut needs: LinkedConsumerNeeds, _cfg: &Config<'_>) -> Result<Self> {
        let check = match needs._receiver.as_mut() {
            None => LinkCheck::Unknown("the board names no receiver for this sensor"),
            Some(receiver) => {
                let mut name = [0; MAX_FEED_NAME];
                let answer = receiver.source(&mut name).map(|len| &name[..len]);
                check_link(answer, Some(SENSOR))?
            }
        };
        Ok(Self {
            _receiver: needs._receiver,
            check,
        })
    }

    fn health(&self) -> Health {
        match self.check {
            LinkCheck::Confirmed => Health::Ok,
            LinkCheck::Unknown(reason) => Health::Degraded(reason),
            LinkCheck::Mismatch(reason) => Health::Failed(reason),
        }
    }
}

impl CameraSensor for LinkedConsumer {
    fn post_values_at(&mut self, _values: FrameValues) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
    fn applied_landing(&mut self) -> Result<Option<AppliedLanding>> {
        Err(Status::NOT_SUPPORTED)
    }
    fn info(&mut self) -> Result<SensorInfo> {
        Err(Status::NOT_SUPPORTED)
    }
    fn mode(&mut self, _index: u16) -> Result<Mode> {
        Err(Status::NOT_SUPPORTED)
    }
    fn select_mode(&mut self, _index: u16) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn selected(&mut self) -> Result<Option<u16>> {
        Ok(None)
    }
    fn start(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn stop(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_controls(&mut self, _controls: Controls) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn post_values(&mut self, _values: FrameValues) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn applied(&mut self) -> Result<Option<FrameValues>> {
        Ok(None)
    }
    fn set_flips(&mut self, _horizontal: bool, _vertical: bool) -> Result<BayerOrder> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_test_pattern(&mut self, _code: u16) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn identity(&mut self, into: &mut [u8]) -> Result<usize> {
        into.get_mut(..SENSOR.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(SENSOR);
        Ok(SENSOR.len())
    }
}

/// The board: a sensor linked to a receiver that answers `source`.
fn camera(source: &str) -> (Host, SimClock) {
    start(&format!(
        "[[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\nlink = \"cam0-receiver\"\n\
         [[devices]]\nid = \"cam0-receiver\"\ndriver = \"link-provider\"\n\
         config = {{ source = \"{source}\" }}\n"
    ))
}

fn start(devices: &str) -> (Host, SimClock) {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    let board = parse(&text).unwrap();
    let sim = SimBoard::new();
    let clock = sim.clock();
    let mut drivers = common::drivers();
    drivers.push(entry::<LinkedConsumer>());
    drivers.push(entry::<LinkedProvider>());
    let host = Host::start(
        &board,
        &drivers,
        Box::new(sim),
        Arc::new(clock.clone()),
        quick(),
    )
    .unwrap();
    (host, clock)
}

/// Lets the retries of the failure rule run, as a daemon's ticks would.
fn settle(host: &Host, clock: &SimClock) {
    for _ in 0..4 {
        clock.advance(Duration::from_secs(2));
        host.tick();
    }
}

#[test]
fn a_receiver_that_is_fed_by_this_sensor_confirms_the_link() {
    let (host, clock) = camera("ov9782 10-0036");
    settle(&host, &clock);
    let sensor = host.device("cam0-sensor").unwrap();
    assert_eq!(sensor.state, DeviceState::Running);
    assert_eq!(sensor.health, Health::Ok);
}

#[test]
fn a_receiver_with_nothing_wired_to_its_input_fails_the_sensor() {
    let (host, clock) = camera("nothing");
    settle(&host, &clock);
    let sensor = host.device("cam0-sensor").unwrap();
    assert_eq!(sensor.state, DeviceState::Running);
    assert_eq!(
        sensor.health,
        Health::Failed("the linked receiver has nothing wired to its input")
    );
}

#[test]
fn a_receiver_fed_by_another_sensor_fails_the_sensor() {
    let (host, clock) = camera("imx219 4-0010");
    settle(&host, &clock);
    assert_eq!(
        host.device("cam0-sensor").unwrap().health,
        Health::Failed("the linked receiver is fed by another sensor")
    );
}

#[test]
fn a_receiver_that_cannot_say_leaves_the_link_unconfirmed() {
    let (host, clock) = camera("unknown");
    settle(&host, &clock);
    assert_eq!(
        host.device("cam0-sensor").unwrap().health,
        Health::Degraded("the linked receiver cannot say what is wired to its input")
    );
}

#[test]
fn a_linked_device_that_does_not_answer_as_a_receiver_leaves_the_link_unknown() {
    // A front end linked as the sensor's receiver refuses `source` as an invalid call. That must
    // not fail the bind: the sensor runs, its link unknown with the reason.
    let (host, clock) = camera("refuse");
    settle(&host, &clock);
    let sensor = host.device("cam0-sensor").unwrap();
    assert_eq!(sensor.state, DeviceState::Running);
    assert_eq!(
        sensor.health,
        Health::Degraded("the linked device does not answer as a receiver")
    );
}

#[test]
fn a_link_to_a_device_that_is_not_running_keeps_the_sensor_retrying() {
    let (host, clock) =
        start("[[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\nlink = \"absent\"\n");
    settle(&host, &clock);
    let sensor = host.device("cam0-sensor").unwrap();
    assert!(
        matches!(sensor.state, DeviceState::Backoff { .. }),
        "{sensor:?}"
    );
}
