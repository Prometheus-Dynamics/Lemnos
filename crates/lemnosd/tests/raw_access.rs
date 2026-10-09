//! Raw bus and line access through lemnosd, end to end over the mock
//! server: claims, arbitration against board devices, transactions, claims
//! and control writes ending with the connection, and the client options.

use lemnos_hal::raw::{Direction, EdgeDetect, LineConfig, Polarity, PwmConfig, SafeState};
use lemnos_ipc::{
    ClientError, ClientEvent, ClientOptions, DeviceClient, Event, I2cOp, LedStatus, LineTarget,
    PwmTarget, Refusal, SpiXfer, Update,
};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::time::{Duration, Instant};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
raw = ["selftest"]
config = { gyro_address = 0x68 }

[[devices]]
id = "usb-power"
driver = "gpio-output"
config = { chip = "pinctrl-rp1", line = 20 }

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/leds0"
config = { count = 4, fade_ms = 0 }

[[lines]]
name = "aux"
chip = "pinctrl-rp1"
line = 5
safe = "low"

[[pwms]]
name = "buzzer"
chip = 0
channel = 1
"#;

fn hardware() -> MockHardware {
    let hardware = MockHardware::new();
    hardware.label_chip("pinctrl-rp1", "gpiochip0");
    hardware.name_line("BUTTON", "gpiochip0", 9);
    let bus = hardware.i2c(1);
    // The IMU's chip ids, and an unowned EEPROM-like target at 0x50.
    let _ = bus
        .clone()
        .with_target(0x18, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_target(0x68, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x68, 0x00, &[0x0f])
        .with_target(0x50, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x50, 0x10, &[0xaa, 0xbb]);
    hardware
}

fn start() -> MockLemnosd {
    let root = std::env::temp_dir().join(format!(
        "lemnosd-raw-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), "").unwrap();
    MockLemnosd::start_in(root, BOARD, hardware()).unwrap()
}

fn client(service: &MockLemnosd, name: &str) -> DeviceClient {
    ClientOptions::new(service.socket(), name)
        .devices()
        .unwrap()
}

fn refused(result: Result<impl std::fmt::Debug, ClientError>) -> Refusal {
    match result {
        Err(ClientError::Refused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn lines_claim_drive_report_edges_and_go_safe_on_disconnect() {
    let service = start();
    let hw = service.hardware().clone();
    let mut helios = client(&service, "helios");

    // By board name: driven, then left low (its safe state) on disconnect.
    let aux = helios
        .claim_line(LineTarget::Name("aux".into()), LineConfig::output(true))
        .unwrap();
    assert_eq!(hw.line("gpiochip0", 5).level(), Some(true));
    aux.set(&mut helios, false).unwrap();
    assert_eq!(hw.line("gpiochip0", 5).level(), Some(false));
    aux.set(&mut helios, true).unwrap();
    assert!(aux.get(&mut helios).unwrap());

    // By kernel line name, as an input with edges.
    let button = helios
        .claim_line(
            LineTarget::Name("BUTTON".into()),
            LineConfig::input().with_edge(EdgeDetect::Both),
        )
        .unwrap();
    hw.line("gpiochip0", 9).edge(true, 1234);
    let edge = loop {
        match helios.next_event_timeout(Duration::from_secs(5)).unwrap() {
            Some(ClientEvent::Data(Update::Event(Event::Edge {
                handle,
                rising,
                timestamp_ns,
                ..
            }))) => break (handle, rising, timestamp_ns),
            Some(_) => {}
            None => panic!("no edge"),
        }
    };
    assert_eq!(edge, (button.handle(), true, 1234));
    assert!(button.get(&mut helios).unwrap());

    // By chip label and offset, with its own release state.
    let free = helios
        .claim_line_with(
            LineTarget::Chip {
                chip: "pinctrl-rp1".into(),
                offset: 6,
            },
            LineConfig::output(true),
            Some(SafeState::Keep),
        )
        .unwrap();

    // Exclusive: another client is refused; unknown handles too.
    let mut other = client(&service, "photonvision");
    assert_eq!(
        refused(other.claim_line(LineTarget::Name("aux".into()), LineConfig::input())),
        Refusal::Claimed
    );
    assert_eq!(refused(aux.set(&mut other, false)), Refusal::UnknownHandle);
    // A board device's line is never handed out.
    assert_eq!(
        refused(other.claim_line(
            LineTarget::Chip {
                chip: "gpiochip0".into(),
                offset: 20
            },
            LineConfig::input()
        )),
        Refusal::Owned
    );

    // An explicit release: the line goes high-impedance (no board entry).
    let plain = helios
        .claim_line(
            LineTarget::Chip {
                chip: "gpiochip0".into(),
                offset: 7,
            },
            LineConfig::output(true),
        )
        .unwrap();
    plain.release(&mut helios).unwrap();
    assert_eq!(
        hw.line("gpiochip0", 7).config(),
        LineConfig::high_impedance()
    );
    let _ = free;

    // The client disappears (as with kill -9): every claim ends.
    drop(helios);
    eventually(|| hw.line("gpiochip0", 5).level() == Some(false));
    assert_eq!(hw.line("gpiochip0", 9).config().direction, Direction::Input);
    assert_eq!(hw.line("gpiochip0", 6).level(), Some(true), "kept as set");
    // Free again.
    other
        .claim_line(LineTarget::Name("aux".into()), LineConfig::input())
        .unwrap();
}

#[test]
fn i2c_spi_and_pwm_with_arbitration() {
    let service = start();
    let hw = service.hardware().clone();
    let mut helios = client(&service, "helios");

    // An unowned address: register access, combined write-then-read.
    let eeprom = helios.i2c("1", 0x50);
    assert_eq!(
        eeprom.read_regs(&mut helios, 0x10, 2).unwrap(),
        [0xaa, 0xbb]
    );
    eeprom.write_reg8(&mut helios, 0x20, 0x42).unwrap();
    assert_eq!(hw.i2c(1).register(0x50, 0x20), 0x42);
    assert_eq!(
        eeprom
            .transfer(&mut helios, vec![I2cOp::Write(vec![0x20]), I2cOp::Read(1)])
            .unwrap(),
        [0x42]
    );
    // The IMU's addresses (both of them) are the board's...
    assert_eq!(
        refused(helios.i2c("i2c-1", 0x68).read_reg8(&mut helios, 0)),
        Refusal::Owned
    );
    assert_eq!(
        refused(helios.i2c("1", 0x18).read_reg8(&mut helios, 0)),
        Refusal::Owned
    );
    // ...but the device lists `selftest` for brokered access.
    let mut selftest = client(&service, "selftest");
    assert_eq!(
        selftest.i2c("1", 0x18).read_reg8(&mut selftest, 0).unwrap(),
        0x1e
    );
    assert_eq!(
        selftest.i2c("1", 0x68).read_reg8(&mut selftest, 0).unwrap(),
        0x0f
    );
    assert_eq!(
        refused(selftest.i2c("1", 0x18).lock(&mut selftest)),
        Refusal::Owned
    );
    // A bus that does not exist, and a missing target.
    hw.remove_i2c(9);
    assert!(matches!(
        refused(helios.i2c("9", 0x50).read(&mut helios, 1)),
        Refusal::Device(_)
    ));
    assert!(matches!(
        refused(helios.i2c("1", 0x51).read(&mut helios, 1)),
        Refusal::Device(lemnos_hal::ErrorKind::Nack)
    ));

    // Locks keep others off across transactions, and end with the client.
    eeprom.lock(&mut helios).unwrap();
    assert_eq!(
        refused(selftest.i2c("1", 0x50).read(&mut selftest, 1)),
        Refusal::Claimed
    );
    drop(helios);
    eventually(|| selftest.i2c("1", 0x50).read(&mut selftest, 1).is_ok());

    // SPI: full duplex, per-transfer settings.
    let spi = hw.spi(0, 1);
    spi.respond(&[0xef, 0x40, 0x18]);
    let mut xfer = SpiXfer::new(vec![0x9f, 0, 0, 0], 4);
    xfer.config.speed_hz = 1_000_000;
    let flash = selftest.spi(0, 1);
    assert_eq!(
        flash.transfer(&mut selftest, vec![xfer]).unwrap(),
        [0xef, 0x40, 0x18, 0]
    );
    let recorded = &spi.transactions()[0][0];
    assert_eq!(
        (recorded.tx.as_slice(), recorded.config.speed_hz),
        (&[0x9f, 0, 0, 0][..], 1_000_000)
    );

    // PWM by board name; disabled when the claim ends.
    let buzzer = selftest
        .claim_pwm(PwmTarget::Name("buzzer".into()))
        .unwrap();
    let config = PwmConfig {
        period_ns: 250_000,
        duty_ns: 125_000,
        polarity: Polarity::Normal,
        enabled: true,
    };
    buzzer.configure(&mut selftest, config).unwrap();
    assert_eq!(hw.pwm(0, 1).current(), config);
    assert_eq!(
        refused(buzzer.configure(
            &mut selftest,
            PwmConfig {
                duty_ns: 1,
                period_ns: 0,
                ..config
            }
        )),
        Refusal::Device(lemnos_hal::ErrorKind::InvalidInput)
    );
    drop(selftest);
    eventually(|| !hw.pwm(0, 1).current().enabled);
}

#[test]
fn control_writes_end_with_the_connection_unless_kept() {
    let service = start();
    let mut helios = client(&service, "helios");
    assert_eq!(helios.get("ring", "brightness").unwrap(), 1.0);
    helios.set("ring", "brightness", 0.25).unwrap();
    let mut watcher = client(&service, "watcher");
    assert!((watcher.get("ring", "brightness").unwrap() - 0.25).abs() < 0.01);
    // Disconnect: the value from before the first write comes back.
    drop(helios);
    eventually(|| watcher.get("ring", "brightness").unwrap() == 1.0);

    // A keeping client (lemnos-ctl) persists, until it restores by name.
    let mut ctl = ClientOptions::new(service.socket(), "lemnos-ctl")
        .keep_intents()
        .devices()
        .unwrap();
    ctl.set("ring", "brightness", 0.5).unwrap();
    drop(ctl);
    std::thread::sleep(Duration::from_millis(100));
    assert!((watcher.get("ring", "brightness").unwrap() - 0.5).abs() < 0.01);
    let mut ctl = ClientOptions::new(service.socket(), "lemnos-ctl")
        .keep_intents()
        .devices()
        .unwrap();
    ctl.restore("ring", None).unwrap();
    assert_eq!(watcher.get("ring", "brightness").unwrap(), 1.0);
    assert_eq!(refused(ctl.restore("nope", None)), Refusal::UnknownDevice);
}

#[test]
fn raw_access_follows_the_board_policy() {
    // A top-level key, so before the tables.
    let board = format!("raw_clients = [\"helios\"]\n{BOARD}").replace("{root}", "/nonexistent");
    let service = MockLemnosd::start(&board, hardware()).unwrap();
    let mut other = client(&service, "photonvision");
    assert_eq!(
        refused(other.claim_line(LineTarget::Name("aux".into()), LineConfig::input())),
        Refusal::NotAllowed
    );
    assert_eq!(
        refused(other.i2c("1", 0x50).read(&mut other, 1)),
        Refusal::NotAllowed
    );
    let mut helios = client(&service, "helios");
    assert!(helios.i2c("1", 0x50).read(&mut helios, 1).is_ok());
}

#[test]
fn clients_wait_for_the_service_and_led_clients_get_no_events_by_default() {
    let root = std::env::temp_dir().join(format!("lemnosd-raw-wait-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), "").unwrap();
    let socket = root.join("lemnosd.sock");

    // Started before the service: refused at once without `wait`...
    assert!(ClientOptions::new(&socket, "early").devices().is_err());
    // ...reported as Disconnected and retried with `reconnecting`...
    let mut lazy = ClientOptions::new(&socket, "lazy")
        .reconnecting()
        .devices()
        .unwrap();
    assert!(!lazy.is_connected());
    // ...and connected once the service is up with `wait`.
    let starter = {
        let root = root.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            MockLemnosd::start_in(root, BOARD, hardware()).unwrap()
        })
    };
    let waiting = ClientOptions::new(&socket, "waiting")
        .wait(Duration::from_secs(10))
        .devices()
        .unwrap();
    let service = starter.join().unwrap();
    assert_eq!(waiting.board(), "raze");
    match lazy.next_event().unwrap() {
        ClientEvent::Disconnected { .. } => {}
        other => panic!("{other:?}"),
    }
    match lazy.next_event().unwrap() {
        ClientEvent::Connected { .. } => {}
        other => panic!("{other:?}"),
    }

    // LED clients: no events unless asked.
    let mut quiet = ClientOptions::new(service.socket(), "quiet")
        .leds()
        .unwrap();
    let mut loud = ClientOptions::new(service.socket(), "loud")
        .events(true)
        .leds()
        .unwrap();
    let mut app = ClientOptions::new(service.socket(), "app").leds().unwrap();
    app.status(LedStatus::Warn).unwrap();
    app.sync().unwrap();
    let mut owner_event = false;
    while let Some(event) = loud.next_event(Some(Duration::from_millis(500))).unwrap() {
        if matches!(event, ClientEvent::Data(Event::LedOwner { .. })) {
            owner_event = true;
            break;
        }
    }
    assert!(owner_event);
    while let Some(event) = quiet.next_event(Some(Duration::from_millis(200))).unwrap() {
        assert!(
            matches!(event, ClientEvent::Connected { .. }),
            "unexpected {event:?}"
        );
    }
    drop(service);
}

#[test]
fn lemnos_ctl_raw_commands() {
    let service = start();
    let hw = service.hardware().clone();
    let ctl = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lemnos-ctl"))
            .arg("--socket")
            .arg(service.socket())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    assert_eq!(ctl(&["i2c", "read", "1", "0x50", "0x10", "2"]), "aa bb");
    ctl(&["i2c", "write", "1", "0x50", "0x30", "0x07"]);
    assert_eq!(hw.i2c(1).register(0x50, 0x30), 7);
    assert_eq!(ctl(&["i2c", "xfer", "1", "0x50", "w:30", "r:1"]), "07");
    hw.spi(0, 0).respond(&[0x12, 0x34]);
    assert_eq!(ctl(&["spi", "xfer", "0.0", "9f00", "--mode", "3"]), "12 34");

    // A kept claim outlives lemnos-ctl until released by handle.
    let out = ctl(&["gpio", "set", "aux", "1", "--keep"]);
    let handle = out.split_whitespace().nth(1).unwrap().to_string();
    assert_eq!(hw.line("gpiochip0", 5).level(), Some(true));
    ctl(&["gpio", "release", &handle]);
    assert_eq!(
        hw.line("gpiochip0", 5).level(),
        Some(false),
        "board safe state"
    );
    // A held claim ends with the process.
    ctl(&["gpio", "set", "pinctrl-rp1:8", "1", "--hold", "0"]);
    assert_eq!(
        hw.line("gpiochip0", 8).config(),
        LineConfig::high_impedance()
    );
    hw.line("gpiochip0", 3).drive(true);
    assert_eq!(ctl(&["gpio", "get", "gpiochip0:3"]), "1");

    // `set` persists after lemnos-ctl exits; `restore` undoes it.
    ctl(&["set", "ring", "brightness", "0.5"]);
    let mut watcher = client(&service, "watcher");
    assert!((watcher.get("ring", "brightness").unwrap() - 0.5).abs() < 0.01);
    ctl(&["restore", "ring"]);
    assert_eq!(watcher.get("ring", "brightness").unwrap(), 1.0);

    ctl(&[
        "pwm", "set", "0:2", "--period", "1000", "--duty", "500", "--hold", "0",
    ]);
    assert!(!hw.pwm(0, 2).current().enabled);
    assert_eq!(hw.pwm(0, 2).history()[0].duty_ns, 500);
}

#[test]
fn faulted_devices_say_why() {
    let hardware = MockHardware::new();
    // The accelerometer answers; the gyroscope does not.
    let _ = hardware
        .i2c(2)
        .with_target(0x18, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e]);
    let service = MockLemnosd::start(
        r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "raze"
[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-2"
backend = "userspace"
[[devices]]
id = "nowhere"
driver = "bmi088"
bus = "i2c:compatible=nothing-like-this"
"#,
        hardware,
    )
    .unwrap();
    let mut helios = client(&service, "helios");
    let list = helios.list().unwrap();
    let imu = list.iter().find(|d| d.id == "imu").unwrap();
    assert!(
        imu.reason.starts_with("init: BMI088 gyro chip id:"),
        "{}",
        imu.reason
    );
    let nowhere = list.iter().find(|d| d.id == "nowhere").unwrap();
    assert!(
        nowhere.reason.contains("no I2C adapter matches"),
        "{}",
        nowhere.reason
    );

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lemnos-ctl"))
        .arg("--socket")
        .arg(service.socket())
        .args(["read", "imu"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gyro chip id"), "{stderr}");
}
