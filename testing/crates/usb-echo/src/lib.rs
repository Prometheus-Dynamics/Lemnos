//! Serves a `usb.function/1` device through the service, for the board checks in
//! `testing/board-runs/E36.md`: it is the device a USB host talks to.
//!
//! - A vendor request whose data goes to the host (`bmRequestType` 0xc0) is answered with the
//!   bytes `lemnos`, cut to the request's length.
//! - A vendor request whose data comes from the host (0x40) is accepted, and its bytes printed.
//! - Any other request is stalled.
//! - What arrives on the first OUT endpoint is sent back on the first IN endpoint.
//!
//! The program is `lemnos-usb-echo SOCKET [DEVICE]`. Without a device name, the first device
//! with the tag is served. It runs until it is killed or the service ends the stream.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use lemnos_client::{Client, Error, FrameStream};
use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, DATA_OFFSET, EVENT_BYTES, Event, EventKind, JOB_BYTES, Job,
    JobKind, JobStatus, UsbFunction,
};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_linux_stream::LinuxStreams;

const TAG: &str = "usb.function/1";
/// The buffers this program names: one each for the event job, the receive and the send.
const EVENT_BUFFER: u32 = 0;
const RECEIVE_BUFFER: u32 = 1;
const SEND_BUFFER: u32 = 2;
/// The jobs' identities, so a completion says which it is.
const EVENT: u64 = 1;
const RECEIVE: u64 = 2;
const SEND: u64 = 3;

/// A line on standard output. A reader that went is no reason to stop serving.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = writeln!(std::io::stdout(), $($arg)*); }
    };
}

/// What stops the program: the service or the stream, or a call the device refused.
pub enum Stop {
    Client(Error),
    Call(Status),
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(error) => write!(f, "{error}"),
            Self::Call(status) => write!(f, "the device refused a call with status {}", status.0),
        }
    }
}

impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Self::Client(error)
    }
}

impl From<Status> for Stop {
    fn from(status: Status) -> Self {
        Self::Call(status)
    }
}

fn post(stream: &mut FrameStream, job: Job) -> Result<(), Error> {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).map_err(|_| Error::TooSmall)?;
    let used = writer.len();
    if stream.post_job(&bytes[..used])? {
        Ok(())
    } else {
        Err(Error::Full)
    }
}

fn job(identity: u64, kind: JobKind, endpoint: u8, buffer: u32, length: u32) -> Job {
    Job {
        identity,
        kind,
        endpoint,
        buffer,
        length,
    }
}

/// Serves the device called `name` (the first with the tag when `name` is empty) of the
/// service at `socket`, until the service ends the stream.
pub fn serve(socket: &str, name: &str) -> Result<(), Stop> {
    let client = Client::connect(socket, "usb-echo")?;
    let entry = client
        .list()?
        .into_iter()
        .find(|d| d.tags.iter().any(|t| t == TAG) && (name.is_empty() || d.name == name))
        .ok_or(Error::Disconnected)?;
    let mut device = client.open::<dyn UsbFunction>(&entry.name)?;
    let info = device.info()?;
    let mut to_host = None;
    let mut from_host = None;
    for index in 0..info.endpoints {
        let endpoint = device.endpoint(index)?;
        say!(
            "endpoint {:#04x}: attributes {:#04x}, {} bytes a packet at full speed",
            endpoint.address,
            endpoint.attributes,
            endpoint.max_packet
        );
        let slot = if endpoint.address & 0x80 != 0 {
            &mut to_host
        } else {
            &mut from_host
        };
        slot.get_or_insert(endpoint.address);
    }
    let mut stream_name = [0u8; 32];
    let len = device.data_stream(&mut stream_name)?;
    let stream_name = String::from_utf8_lossy(&stream_name[..len]).into_owned();
    let mut stream =
        client.open_stream_writable(entry.index, &stream_name, Arc::new(LinuxStreams))?;
    let room = (stream.layout().buffer_bytes as usize).saturating_sub(DATA_OFFSET);
    // A whole number of the largest packets a high-speed or faster host sends.
    let most = (room.min(info.max_transfer as usize) / 1024 * 1024) as u32;
    say!(
        "serving {} on stream {stream_name}: transfers of up to {most} bytes",
        entry.name
    );

    let receive = |stream: &mut FrameStream| -> Result<(), Error> {
        match from_host {
            Some(endpoint) if most > 0 => post(
                stream,
                job(RECEIVE, JobKind::Receive, endpoint, RECEIVE_BUFFER, most),
            ),
            _ => Ok(()),
        }
    };
    post(&mut stream, job(EVENT, JobKind::Event, 0, EVENT_BUFFER, 0))?;
    // A receive posted with no configuration fails; one is posted when the host configures.
    if device.state()?.configured {
        receive(&mut stream)?;
    }
    let mut echoed = 0u64;
    while stream.is_open() {
        let Some(frame) = stream.next_frame(Duration::from_secs(1))? else {
            continue;
        };
        let identity = frame.identity();
        let bytes = stream.bytes(&frame);
        let record = bytes
            .get(..COMPLETION_BYTES)
            .and_then(|record| Completion::decode(&mut Reader::new(record)).ok())
            .unwrap_or_default();
        let data = bytes.get(DATA_OFFSET..).unwrap_or(&[]).to_vec();
        stream.release(frame)?;
        match (identity, record.status) {
            (EVENT, JobStatus::Done) if data.len() == EVENT_BYTES => {
                let event = Event::decode(&mut Reader::new(&data)).unwrap_or_default();
                say!("event {:?}", event.kind);
                match event.kind {
                    EventKind::Configured => receive(&mut stream)?,
                    EventKind::Setup => {
                        let request = event.request;
                        let answer = match request.request_type {
                            0xc0 => device.control_in(b"lemnos").map(|sent| sent as usize),
                            0x40 => {
                                let mut taken = [0u8; 256];
                                device.control_out(&mut taken).inspect(|&came| {
                                    say!("request data {:02x?}", &taken[..came]);
                                })
                            }
                            _ => device.control_stall().map(|()| 0),
                        };
                        say!("request {request:?}: {answer:?}");
                    }
                    _ => {}
                }
                post(&mut stream, job(EVENT, JobKind::Event, 0, EVENT_BUFFER, 0))?;
            }
            (RECEIVE, JobStatus::Done) => {
                if let Some(endpoint) = to_host {
                    stream.write(SEND_BUFFER, DATA_OFFSET, &data)?;
                    let length = data.len() as u32;
                    post(
                        &mut stream,
                        job(SEND, JobKind::Send, endpoint, SEND_BUFFER, length),
                    )?;
                } else {
                    receive(&mut stream)?;
                }
            }
            (SEND, JobStatus::Done) => {
                echoed += u64::from(record.count);
                say!("echoed {} bytes, {echoed} so far", record.count);
                receive(&mut stream)?;
            }
            // A transfer that failed because the configuration went is posted again when the
            // host configures the device.
            (which, status) => say!("job {which} ended {status:?}"),
        }
    }
    say!("the service ended the stream: {:?}", device.counters());
    Ok(())
}
