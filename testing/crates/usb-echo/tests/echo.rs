//! The tool against a running service on the simulated board, with the test as the USB host.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_hal::blocks::{ControlSetup, UsbFunctionEvent};
use lemnos_hal::mock::ControlAnswer;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "t"

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc0" }
config = { vendor = 0x1209, product = 1, interfaces = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n" }

[devices.streams.data]
buffers = 4
buffer_bytes = 8192
job_slots = 8
job_bytes = 18
"#;

fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    true
}

fn request(request_type: u8) -> ControlSetup {
    ControlSetup {
        request_type,
        request: 1,
        value: 0,
        index: 0,
    }
}

#[test]
fn the_tool_answers_requests_and_echoes_what_the_host_sends() {
    let dir: PathBuf = std::env::temp_dir().join(format!("lemnos-usb-echo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let sim = SimBoard::new();
    let usb = sim.add_usb_function("udc0");
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(BOARD).unwrap(),
        drivers: vec![entry::<UsbGadget>()],
        backend: Box::new(sim),
        options: Options::default(),
    })
    .unwrap();
    assert!(until(|| usb.descriptors().is_some()), "the gadget is up");

    let path = socket.to_str().unwrap().to_string();
    let tool = thread::spawn(move || lemnos_usb_echo::serve(&path, "").map_err(|e| e.to_string()));

    // Configured, the device echoes; before that a receive would only fail.
    usb.event(UsbFunctionEvent::Configured);
    for round in 0..3u8 {
        let sent: Vec<u8> = (0..1500u32).map(|i| (i as u8) ^ round).collect();
        usb.host_sends(0x02, &sent);
        assert!(
            until(|| usb.host_received().len() == usize::from(round) + 1),
            "round {round} came back"
        );
        assert_eq!(usb.host_received()[usize::from(round)], (0x81, sent));
    }

    // The three kinds of request.
    usb.request(request(0xc0), 4, &[]);
    assert!(until(|| usb.answers().len() == 1));
    usb.request(request(0x40), 3, b"abc");
    assert!(until(|| usb.answers().len() == 2));
    usb.request(request(0xa1), 1, &[]);
    assert!(until(|| usb.answers().len() == 3));
    assert_eq!(
        usb.answers(),
        [
            (request(0xc0), ControlAnswer::In(b"lemn".to_vec())),
            (request(0x40), ControlAnswer::Out),
            (request(0xa1), ControlAnswer::Stalled),
        ]
    );

    // The service going ends the stream, and the tool with it.
    drop(service);
    // It returns: cleanly when it saw the stream end, with the lost connection otherwise.
    let _ = tool.join().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
