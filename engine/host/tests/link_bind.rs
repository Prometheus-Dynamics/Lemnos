//! A sensor's board `link` is checked at bind, through the host's own routing: the sensor asks the
//! device the link names what is wired to its input, and the answer decides its health. The two
//! drivers here are stand-ins for a camera sensor and its receiver, with the contracts they serve.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::quick;
use lemnos_contract_camera_isp::{
    CameraIsp, ConfigLayout, Counters as IspCounters, IspInfo, IspInput, IspOutput, OutputLayout,
};
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
    /// The ISP the receiver feeds: its `link`, checked at bind.
    #[link]
    _isp: Option<Dev<dyn CameraIsp>>,
}

impl Lifecycle for LinkedProvider {
    type Needs = LinkedProviderNeeds;

    fn start(needs: LinkedProviderNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            source: cfg.str("source").unwrap_or("unknown").to_string(),
            _isp: needs._isp,
        })
    }
}

/// An ISP, as a front end or back end: what a receiver or another ISP links to. Its `link`, when it
/// has one, is the next ISP. It answers nothing: every method refuses.
#[driver("link-stage", provides = [CameraIsp])]
struct LinkedStage {
    #[link]
    _next: Option<Dev<dyn CameraIsp>>,
}

impl Lifecycle for LinkedStage {
    type Needs = LinkedStageNeeds;

    fn start(needs: LinkedStageNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self { _next: needs._next })
    }
}

impl CameraIsp for LinkedStage {
    fn info(&mut self) -> Result<IspInfo> {
        Err(Status::NOT_SUPPORTED)
    }
    fn config_layout(&mut self, _index: u8) -> Result<ConfigLayout> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_input(&mut self, _input: IspInput) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_output(&mut self, _output: u8, _image: IspOutput) -> Result<OutputLayout> {
        Err(Status::NOT_SUPPORTED)
    }
    fn clear_output(&mut self, _output: u8) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn config_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn statistics_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn output_stream(&mut self, _output: u8, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn start(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn stop(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    fn counters(&mut self) -> Result<IspCounters> {
        Err(Status::NOT_SUPPORTED)
    }
    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Ok(None)
    }
    fn output_layout(&mut self, _output: u8) -> Result<Option<OutputLayout>> {
        Ok(None)
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
    drivers.push(entry::<LinkedStage>());
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
fn a_link_to_a_device_that_does_not_serve_the_contract_says_what_it_needs() {
    // The sensor's link wants a camera.receiver/1 device. `cam0-peer` serves a sensor only: the
    // link cannot be bound, and the reason says which contract is wanted and which the device serves.
    let (host, clock) = start(
        "[[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\nlink = \"cam0-peer\"\n\
         [[devices]]\nid = \"cam0-peer\"\ndriver = \"link-consumer\"\n",
    );
    settle(&host, &clock);
    let sensor = host.device("cam0-sensor").unwrap();
    let DeviceState::Backoff { reason, .. } = &sensor.state else {
        panic!("{sensor:?}");
    };
    assert_eq!(
        reason,
        "the link to `cam0-peer` needs a camera.receiver/1 device, but `cam0-peer` serves \
         camera.sensor/1: a sensor links to a camera.receiver/1 device, and a receiver and each ISP \
         to a camera.isp/1 device"
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

/// The sensor's link, the receiver's link and each ISP's link, each resolved at bind: a valid
/// hop binds, and a hop to a device of the wrong contract is refused with the reason.
#[test]
fn a_chain_of_links_binds_every_hop() {
    let (host, clock) = start(
        "[[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\nlink = \"cam0-receiver\"\n\
         [[devices]]\nid = \"cam0-receiver\"\ndriver = \"link-provider\"\nlink = \"cam0-fe\"\n\
         config = { source = \"ov9782 10-0036\" }\n\
         [[devices]]\nid = \"cam0-fe\"\ndriver = \"link-stage\"\nlink = \"cam0-be\"\n\
         [[devices]]\nid = \"cam0-be\"\ndriver = \"link-stage\"\n",
    );
    settle(&host, &clock);
    for id in ["cam0-sensor", "cam0-receiver", "cam0-fe", "cam0-be"] {
        assert_eq!(host.device(id).unwrap().state, DeviceState::Running, "{id}");
    }
    assert_eq!(host.device("cam0-sensor").unwrap().health, Health::Ok);
}

#[test]
fn a_receiver_linked_to_a_sensor_is_refused_with_the_reason() {
    let (host, clock) = start(
        "[[devices]]\nid = \"cam0-receiver\"\ndriver = \"link-provider\"\nlink = \"cam0-sensor\"\n\
         [[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\n",
    );
    settle(&host, &clock);
    let DeviceState::Backoff { reason, .. } = &host.device("cam0-receiver").unwrap().state else {
        panic!("receiver should be refused");
    };
    assert_eq!(
        reason,
        "the link to `cam0-sensor` needs a camera.isp/1 device, but `cam0-sensor` serves \
         camera.sensor/1: a sensor links to a camera.receiver/1 device, and a receiver and each ISP \
         to a camera.isp/1 device"
    );
}

#[test]
fn a_sensor_linked_straight_to_an_isp_is_refused_with_the_reason() {
    let (host, clock) = start(
        "[[devices]]\nid = \"cam0-sensor\"\ndriver = \"link-consumer\"\nlink = \"cam0-fe\"\n\
         [[devices]]\nid = \"cam0-fe\"\ndriver = \"link-stage\"\n",
    );
    settle(&host, &clock);
    let DeviceState::Backoff { reason, .. } = &host.device("cam0-sensor").unwrap().state else {
        panic!("sensor should be refused");
    };
    assert_eq!(
        reason,
        "the link to `cam0-fe` needs a camera.receiver/1 device, but `cam0-fe` serves \
         camera.isp/1: a sensor links to a camera.receiver/1 device, and a receiver and each ISP to \
         a camera.isp/1 device"
    );
}

#[test]
fn an_isp_linked_to_a_receiver_is_refused_with_the_reason() {
    let (host, clock) = start(
        "[[devices]]\nid = \"cam0-fe\"\ndriver = \"link-stage\"\nlink = \"cam0-receiver\"\n\
         [[devices]]\nid = \"cam0-receiver\"\ndriver = \"link-provider\"\n",
    );
    settle(&host, &clock);
    let DeviceState::Backoff { reason, .. } = &host.device("cam0-fe").unwrap().state else {
        panic!("front end should be refused");
    };
    assert_eq!(
        reason,
        "the link to `cam0-receiver` needs a camera.isp/1 device, but `cam0-receiver` serves \
         camera.receiver/1: a sensor links to a camera.receiver/1 device, and a receiver and each ISP \
         to a camera.isp/1 device"
    );
}
