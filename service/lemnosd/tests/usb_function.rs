//! `usb.function/1` the way a program uses it: through the service's socket with the client,
//! the data stream's memory shared with the service, and the gadget driver the service runs.
//! The simulated board's function plays the USB host.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::start_board;
use lemnos_client::{Client, FrameStream};
use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, DATA_OFFSET, EVENT_BYTES, Event, EventKind, JOB_BYTES, Job,
    JobKind, JobStatus, UsbFunction,
};
use lemnos_core::{Decode, Encode, Reader, Writer};
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_hal::blocks::{ControlSetup, UsbFunctionEvent};
use lemnos_hal::mock::ControlAnswer;
use lemnos_host::entry;
use lemnos_linux_stream::LinuxStreams;
use lemnos_sim::SimBoard;

const IN: u8 = 0x81;
const OUT: u8 = 0x02;
const WAIT: Duration = Duration::from_secs(5);

const DEVICES: &str = r#"
[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc0" }
config = { vendor = 0x1209, product = 1, interfaces = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n" }

[devices.streams.data]
buffers = 8
buffer_bytes = 4096
job_slots = 16
job_bytes = 22
"#;

fn post(stream: &mut FrameStream, job: Job) {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    assert!(stream.post_job(&bytes[..used]).unwrap());
}

/// The next completion, which must be `identity`'s and done: the bytes after its record. Its
/// buffer goes back to the stream.
fn done(stream: &mut FrameStream, identity: u64) -> Vec<u8> {
    let frame = stream.next_frame(WAIT).unwrap().expect("a completion");
    assert_eq!(frame.identity(), identity);
    let bytes = stream.bytes(&frame);
    let record = Completion::decode(&mut Reader::new(&bytes[..COMPLETION_BYTES])).unwrap();
    assert_eq!(record.status, JobStatus::Done);
    let data = bytes[DATA_OFFSET..].to_vec();
    stream.release(frame).unwrap();
    data
}

fn event(stream: &mut FrameStream, identity: u64) -> Event {
    let data = done(stream, identity);
    assert_eq!(data.len(), EVENT_BYTES);
    Event::decode(&mut Reader::new(&data)).unwrap()
}

#[test]
fn a_program_serves_the_gadget_through_the_service() {
    let sim = SimBoard::new();
    let usb = sim.add_usb_function("udc0");
    let running = start_board(DEVICES, vec![entry::<UsbGadget>()], sim, None);
    let client = Client::connect(&running.socket, "function").unwrap();
    let found = client.find("usb.function/1").unwrap().expect("the gadget");
    let mut device = client.open::<dyn UsbFunction>("gadget").unwrap();
    let mut name = [0u8; 32];
    let len = device.data_stream(&mut name).unwrap();
    let name = std::str::from_utf8(&name[..len]).unwrap();
    let mut stream = client
        .open_stream_writable(found.index, name, Arc::new(LinuxStreams))
        .unwrap();

    // The USB host configures the device and asks it something only the program can answer.
    post(
        &mut stream,
        Job {
            identity: 1,
            kind: JobKind::Event,
            endpoint: 0,
            buffer: 0,
            length: 0,
        },
    );
    usb.event(UsbFunctionEvent::Configured);
    assert_eq!(event(&mut stream, 1).kind, EventKind::Configured);
    let asked = ControlSetup {
        request_type: 0xc0,
        request: 0x51,
        value: 0,
        index: 0,
    };
    post(
        &mut stream,
        Job {
            identity: 2,
            kind: JobKind::Event,
            endpoint: 0,
            buffer: 0,
            length: 0,
        },
    );
    usb.request(asked, 8, &[]);
    let request = event(&mut stream, 2);
    assert_eq!(
        (
            request.kind,
            request.request.request,
            request.request.length
        ),
        (EventKind::Setup, 0x51, 8)
    );
    assert_eq!(device.control_in(b"answer"), Ok(6));
    assert_eq!(
        usb.answers(),
        [(asked, ControlAnswer::In(b"answer".to_vec()))]
    );

    // What the USB host sends comes to the program, which sends it back.
    let sent: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
    post(
        &mut stream,
        Job {
            identity: 3,
            kind: JobKind::Receive,
            endpoint: OUT,
            buffer: 1,
            length: 4032,
        },
    );
    usb.host_sends(OUT, &sent);
    let came = done(&mut stream, 3);
    assert_eq!(came, sent);
    // The program writes the bytes into a buffer of the stream, which the service shares.
    stream.write(2, DATA_OFFSET, &came).unwrap();
    post(
        &mut stream,
        Job {
            identity: 4,
            kind: JobKind::Send,
            endpoint: IN,
            buffer: 2,
            length: came.len() as u32,
        },
    );
    assert_eq!(done(&mut stream, 4), b"");
    assert_eq!(usb.host_received(), [(IN, sent)]);
    let counters = device.counters().unwrap();
    assert_eq!((counters.jobs, counters.errors), (4, 0));
}
