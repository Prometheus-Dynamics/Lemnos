extern crate std;

use lemnos_hal::mock::MockI2c;
use lemnos_hal::register::AddressWidth;
use std::boxed::Box;

use lemnos_hal::embedded_hal::i2c::I2c as _;

use crate::*;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

const INFO: ContractInfo = ContractInfo {
    tag: Tag::new("example.thing/1"),
    methods: &[MethodInfo {
        id: 1,
        name: "read",
        params: &[],
        returns: Shape::U8,
        has_out_buffer: false,
        job: false,
    }],
};

struct Fake {
    bus: I2c,
    offset: u8,
}

impl Driver for Fake {
    const MANIFEST: Manifest = Manifest {
        name: "fake",
        needs: &[Need {
            name: "bus",
            kind: NeedKind::Bus(BusKind::I2c),
            optional: false,
        }],
        provides: &[INFO],
    };

    fn bind(res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            bus: res.i2c("bus")?,
            offset: cfg.int("offset").unwrap_or(0) as u8,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(
            Item::value("level", READ)
                .unit(Unit::Volt, -3)
                .range(0, 5000),
        );
    }

    fn dispatch(&mut self, method: MethodId, _args: &[u8], reply: &mut Reply<'_>) -> Status {
        if method != READ {
            return Status::NOT_SUPPORTED;
        }
        let mut byte = [0];
        if let Err(kind) = self.bus.write_read(0x10, &[0], &mut byte) {
            return status_of(kind);
        }
        match (byte[0] + self.offset).encode(reply) {
            Ok(()) => Status::OK,
            Err(status) => status,
        }
    }
}

struct Board(Option<MockI2c>);

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        need == "bus"
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        if need != "bus" {
            return Err(Status::NOT_FOUND);
        }
        Ok(I2c(crate::Held::boxed(Box::new(
            self.0.take().ok_or(Status::BUSY)?,
        ))))
    }
}

#[test]
fn a_handwritten_driver_binds_describes_and_runs() {
    let bus = MockI2c::new()
        .with_target(0x10, AddressWidth::Bits8)
        .with_registers(0x10, 0, &[41]);
    let mut board = Board(Some(bus));
    let mut res = Resources::new(&mut board, Some("csi0"));
    assert_eq!(res.link(), Some("csi0"));
    let entries = [("offset", ConfigValue::Int(1))];
    let mut driver = Fake::bind(&mut res, &Config::new(&entries)).unwrap();

    let mut description = Description::new();
    driver.describe(&mut description);
    let item = description.find("level").unwrap();
    assert_eq!((item.unit.symbol(), item.exponent), ("V", -3));
    assert_eq!(item.method, READ);

    let mut buf = [0; 8];
    let mut reply = Reply::new(&mut buf);
    assert_eq!(driver.dispatch(READ, &[], &mut reply), Status::OK);
    assert_eq!(reply.len(), 1);
    assert_eq!(buf[0], 42);

    let other = MethodId {
        contract: 0,
        method: 9,
    };
    let mut reply = Reply::new(&mut buf);
    assert_eq!(
        driver.dispatch(other, &[], &mut reply),
        Status::NOT_SUPPORTED
    );
}

#[test]
fn tags_split_into_name_and_major() {
    const TAG: Tag = Tag::new("example.thing/12");
    assert_eq!((TAG.name(), TAG.major()), ("example.thing", 12));
}

#[test]
#[should_panic(expected = "name/major")]
fn a_tag_needs_a_major() {
    let _ = Tag::new("nothing");
}

#[test]
fn values_survive_the_codec() {
    let mut buf = [0; 64];
    let mut out = Writer::new(&mut buf);
    7u16.encode(&mut out).unwrap();
    (-1.5f32).encode(&mut out).unwrap();
    true.encode(&mut out).unwrap();
    "hi".encode(&mut out).unwrap();
    let bytes: &[u8] = &[1, 2];
    bytes.encode(&mut out).unwrap();
    Some(3u8).encode(&mut out).unwrap();
    None::<u8>.encode(&mut out).unwrap();
    [4u8, 5].encode(&mut out).unwrap();
    let n = out.len();

    let mut from = Reader::new(&buf[..n]);
    assert_eq!(u16::decode(&mut from), Ok(7));
    assert_eq!(f32::decode(&mut from), Ok(-1.5));
    assert_eq!(bool::decode(&mut from), Ok(true));
    assert_eq!(<&str>::decode(&mut from), Ok("hi"));
    assert_eq!(<&[u8]>::decode(&mut from), Ok(&[1u8, 2][..]));
    assert_eq!(Option::<u8>::decode(&mut from), Ok(Some(3)));
    assert_eq!(Option::<u8>::decode(&mut from), Ok(None));
    assert_eq!(<[u8; 2]>::decode(&mut from), Ok([4, 5]));
    assert_eq!(from.remaining(), 0);
    assert_eq!(u8::decode(&mut from), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn shapes_nest() {
    assert_eq!(<Option<u8>>::SHAPE, Shape::Option(&Shape::U8));
    assert_eq!(<[f32; 3]>::SHAPE, Shape::Array(&Shape::F32, 3));
}

#[test]
fn writing_past_the_end_is_refused() {
    let mut buf = [0; 2];
    let mut out = Writer::new(&mut buf);
    assert_eq!(7u32.encode(&mut out), Err(Status::BUFFER_TOO_SMALL));
    assert_eq!(out.len(), 0);
    assert_eq!(out.advance(3), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn a_full_description_counts_what_it_dropped() {
    let mut description = Description::new();
    for _ in 0..MAX_ITEMS + 2 {
        description.add(Item::value("x", READ));
    }
    assert_eq!(description.items().count(), MAX_ITEMS);
    assert_eq!(description.dropped(), 2);
}

#[test]
fn config_reads_by_type() {
    let entries = [
        ("i", ConfigValue::Int(2)),
        ("f", ConfigValue::Float(0.5)),
        ("b", ConfigValue::Bool(true)),
        ("s", ConfigValue::Str("x")),
    ];
    let cfg = Config::new(&entries);
    assert_eq!(cfg.int("i"), Some(2));
    assert_eq!(cfg.float("i"), Some(2.0));
    assert_eq!(cfg.float("f"), Some(0.5));
    assert_eq!(cfg.bool("b"), Some(true));
    assert_eq!(cfg.str("s"), Some("x"));
    assert_eq!(cfg.int("s"), None);
    assert_eq!(cfg.int("missing"), None);
}

mod package {
    use crate::package::abi::{HEALTH_GONE, PackageHealth, SETTING_BOOL, SETTING_TEXT, Slice, Str};
    use crate::package::*;
    use crate::*;

    const RECORD: Shape = Shape::Record(&[
        Field {
            name: "when",
            shape: Shape::U64,
        },
        Field {
            name: "values",
            shape: Shape::Array(&Shape::Option(&Shape::F32), 3),
        },
    ]);

    const MANIFEST: Manifest = Manifest {
        name: "example",
        needs: &[
            Need {
                name: "bus",
                kind: NeedKind::Bus(BusKind::Stream),
                optional: false,
            },
            Need {
                name: "other",
                kind: NeedKind::Device(Tag::new("example.thing/1")),
                optional: true,
            },
        ],
        provides: &[
            super::INFO,
            ContractInfo {
                tag: Tag::new("example.more/12"),
                methods: &[MethodInfo {
                    id: 7,
                    name: "fill",
                    params: &[
                        Param {
                            name: "label",
                            shape: Shape::Str,
                        },
                        Param {
                            name: "entry",
                            shape: RECORD,
                        },
                    ],
                    returns: Shape::Bytes,
                    has_out_buffer: true,
                    job: false,
                }],
            },
        ],
    };

    #[test]
    fn a_manifest_crosses_the_boundary_whole() {
        let crossed = manifest_out(&MANIFEST);
        assert_eq!(crossed.name.as_str(), "example");
        assert_eq!(crossed.needs.as_slice()[0].kind, bus_code(BusKind::Stream));
        assert_eq!(crossed.needs.as_slice()[1].tag.as_str(), "example.thing/1");
        assert_eq!(manifest_in(&crossed), Ok(MANIFEST));
    }

    #[test]
    fn a_manifest_the_host_cannot_use_is_refused_with_a_reason() {
        let good = manifest_out(&MANIFEST);
        let mut nameless = good;
        nameless.name = Str::new("");
        assert_eq!(manifest_in(&nameless), Err("the driver has no name"));

        let mut needs = good.needs.as_slice().to_vec();
        needs[1].tag = Str::new("no-major");
        let mut bad_tag = good;
        bad_tag.needs = Slice::new(std::vec::Vec::leak(needs.clone()));
        assert_eq!(
            manifest_in(&bad_tag),
            Err("a contract tag is not name/major")
        );

        needs[0].kind = 99;
        needs[1].tag = Str::new("example.thing/1");
        let mut bad_bus = good;
        bad_bus.needs = Slice::new(std::vec::Vec::leak(needs));
        assert_eq!(
            manifest_in(&bad_bus),
            Err("a need of a kind this host does not know")
        );

        let mut contracts = good.provides.as_slice().to_vec();
        let mut methods = contracts[0].methods.as_slice().to_vec();
        methods[0].returns.kind = 200;
        contracts[0].methods = Slice::new(std::vec::Vec::leak(methods));
        let mut bad_shape = good;
        bad_shape.provides = Slice::new(std::vec::Vec::leak(contracts));
        assert_eq!(
            manifest_in(&bad_shape),
            Err("a shape of a kind this host does not know")
        );
    }

    #[test]
    fn every_bus_and_bus_error_has_its_own_code() {
        for bus in [
            BusKind::I2c,
            BusKind::Spi,
            BusKind::Gpio,
            BusKind::Pwm,
            BusKind::Uart,
            BusKind::Usb,
            BusKind::UsbFunction,
            BusKind::Registers,
            BusKind::Interrupt,
            BusKind::Dma,
            BusKind::Clocks,
            BusKind::Reset,
            BusKind::Stream,
            BusKind::Pio,
            BusKind::Watchdog,
        ] {
            assert_ne!(bus_code(bus), abi::NEED_DEVICE);
            assert_eq!(bus_of(bus_code(bus)), Some(bus));
        }
        assert_eq!(bus_of(0), None);
        assert_eq!(bus_of(16), None);
        let mut seen = std::vec::Vec::new();
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::Unavailable,
            ErrorKind::Busy,
            ErrorKind::PermissionDenied,
            ErrorKind::Timeout,
            ErrorKind::InvalidInput,
            ErrorKind::Unsupported,
            ErrorKind::Configuration,
            ErrorKind::Nack,
            ErrorKind::Overrun,
            ErrorKind::Failed,
        ] {
            assert_eq!(bus_error_of(status_of_bus(kind)), kind);
            assert!(!status_of_bus(kind).is_ok());
            seen.push(status_of_bus(kind));
        }
        seen.sort_by_key(|status| status.0);
        seen.dedup();
        assert_eq!(seen.len(), 11, "each kind of error has its own status");
        // A handle the host no longer knows is the resource being gone.
        assert_eq!(bus_error_of(Status::STALE_HANDLE), ErrorKind::Unavailable);
        assert_eq!(bus_error_of(Status::PANIC), ErrorKind::Failed);
    }

    #[test]
    fn what_a_resource_carries_crosses_as_it_is() {
        use core::time::Duration;
        use lemnos_hal::blocks::{
            ControlSetup, DmaBuffer, InterruptEvent, StreamFrame, UsbFunctionEvent,
        };
        use lemnos_hal::raw::{
            Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, Polarity, PwmConfig,
        };

        for direction in [Direction::Input, Direction::Output] {
            for bias in [Bias::AsIs, Bias::PullUp, Bias::PullDown, Bias::Disabled] {
                for drive in [Drive::PushPull, Drive::OpenDrain, Drive::OpenSource] {
                    for edge in [
                        EdgeDetect::None,
                        EdgeDetect::Rising,
                        EdgeDetect::Falling,
                        EdgeDetect::Both,
                    ] {
                        let config = LineConfig {
                            direction,
                            initial: true,
                            active_low: true,
                            bias,
                            drive,
                            edge,
                            debounce_us: 77,
                        };
                        assert_eq!(line_config_in(line_config_out(&config)), Some(config));
                    }
                }
            }
        }
        // A value this build does not know is refused, never guessed.
        let mut unknown = line_config_out(&LineConfig::input());
        unknown.bias = 4;
        assert_eq!(line_config_in(unknown), None);
        let mut unknown = line_config_out(&LineConfig::input());
        unknown.direction = 2;
        assert_eq!(line_config_in(unknown), None);

        let edge = Edge {
            rising: true,
            timestamp_ns: u64::MAX - 1,
            seq: 9,
        };
        assert_eq!(edge_in(edge_out(Some(edge))), Some(edge));
        assert_eq!(edge_in(edge_out(None)), None);

        for polarity in [Polarity::Normal, Polarity::Inversed] {
            let config = PwmConfig {
                period_ns: 20_000_000,
                duty_ns: 1_500_000,
                polarity,
                enabled: true,
            };
            assert_eq!(pwm_config_in(pwm_config_out(&config)), Some(config));
        }
        let mut unknown = pwm_config_out(&PwmConfig::default());
        unknown.polarity = 2;
        assert_eq!(pwm_config_in(unknown), None);

        let event = InterruptEvent {
            count: 3,
            timestamp_ns: 8,
        };
        assert_eq!(
            interrupt_event_in(interrupt_event_out(Some(event))),
            Some(event)
        );
        assert_eq!(interrupt_event_in(interrupt_event_out(None)), None);

        let buffer = DmaBuffer {
            id: 4,
            len: 1 << 33,
            device_address: 0xfee0_0000_0000,
        };
        assert_eq!(dma_buffer_in(dma_buffer_out(buffer)), buffer);

        let frame = StreamFrame {
            buffer: 2,
            bytes: 640 * 480,
            sequence: 1 << 40,
            identity: u64::MAX,
            timestamp_ns: 5,
        };
        assert_eq!(frame_in(frame_out(frame)), frame);

        let setup = ControlSetup {
            request_type: 0xc1,
            request: 0x81,
            value: 0x0200,
            index: 3,
        };
        assert_eq!(setup_in(setup_out(setup)), setup);
        for event in [
            None,
            Some(UsbFunctionEvent::Configured),
            Some(UsbFunctionEvent::Deconfigured),
            Some(UsbFunctionEvent::Suspended),
            Some(UsbFunctionEvent::Resumed),
            Some(UsbFunctionEvent::Setup {
                setup,
                length: u16::MAX,
            }),
        ] {
            assert_eq!(usb_event_in(usb_event_out(event)), Ok(event));
        }
        let mut unknown = usb_event_out(None);
        unknown.kind = 6;
        assert_eq!(usb_event_in(unknown), Err(ErrorKind::Failed));
        let mut long = usb_event_out(Some(UsbFunctionEvent::Setup { setup, length: 1 }));
        long.length = 1 << 16;
        assert_eq!(usb_event_in(long), Err(ErrorKind::Failed));

        // A timeout crosses in nanoseconds and is never taken for "without limit".
        assert_eq!(
            timeout_in(timeout_out(Duration::from_millis(5))),
            Duration::from_millis(5)
        );
        assert_eq!(timeout_out(Duration::MAX), abi::NO_TIMEOUT - 1);
        assert_eq!(timeout_out(Duration::ZERO), 0);
    }

    #[test]
    fn health_items_and_settings_cross_as_they_are() {
        for health in [
            Health::Ok,
            Health::Degraded("warm"),
            Health::Failed("cold"),
            Health::Gone("unplugged"),
        ] {
            assert_eq!(health_in(health_out(health)), health);
        }
        assert_eq!(health_out(Health::Gone("x")).kind, HEALTH_GONE);
        assert!(matches!(
            health_in(PackageHealth::new(77, "")),
            Health::Failed(_)
        ));

        let config = Config::new(&[
            ("rate", ConfigValue::Int(-4)),
            ("gain", ConfigValue::Float(1.5)),
            ("on", ConfigValue::Bool(true)),
            ("name", ConfigValue::Str("left")),
        ]);
        let settings = settings_out(&config);
        assert_eq!(settings.len(), 4);
        assert_eq!((settings[0].key.as_str(), settings[0].int), ("rate", -4));
        assert_eq!(settings[1].float, 1.5);
        assert_eq!((settings[2].kind, settings[2].int), (SETTING_BOOL, 1));
        assert_eq!(
            (settings[3].kind, settings[3].text.as_str()),
            (SETTING_TEXT, "left")
        );
    }
}

mod held {
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::{Held, Home};

    /// Where a value lives between holders.
    struct Place(Mutex<Option<&'static mut [u8; 4]>>, AtomicU32);

    impl Home<[u8; 4]> for Place {
        fn take_back(&self, resource: &'static mut [u8; 4]) {
            self.1.fetch_add(1, Ordering::Relaxed);
            *self.0.lock().unwrap() = Some(resource);
        }
    }

    #[test]
    fn what_was_lent_goes_home_when_its_holder_is_dropped_and_can_be_lent_again() {
        static PLACE: Place = Place(Mutex::new(None), AtomicU32::new(0));
        let value: &'static mut [u8; 4] = Box::leak(Box::new([1, 2, 3, 4]));
        let mut held = Held::lent(value, &PLACE);
        assert_eq!(*held, [1, 2, 3, 4]);
        held[0] = 9;
        assert!(PLACE.0.lock().unwrap().is_none(), "out while it is held");
        drop(held);
        assert_eq!(PLACE.1.load(Ordering::Relaxed), 1);
        // The same memory, as its last holder left it, to lend to the next.
        let back = PLACE.0.lock().unwrap().take().expect("home again");
        assert_eq!(*back, [9, 2, 3, 4]);
        let again = Held::lent(back, &PLACE);
        drop(again);
        assert_eq!(
            PLACE.1.load(Ordering::Relaxed),
            2,
            "once for each time it was lent"
        );
    }

    #[test]
    fn a_boxed_resource_is_owned_outright_and_as_usable() {
        let mut held: Held<[u8]> = Held::boxed(Box::new([5u8, 6]));
        held[1] = 7;
        assert_eq!(&*held, [5, 7]);
        let from: Held<[u8]> = Box::<[u8]>::from([1u8]).into();
        assert_eq!(from.len(), 1);
    }
}
