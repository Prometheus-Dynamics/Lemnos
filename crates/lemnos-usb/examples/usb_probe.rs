#![allow(clippy::print_stdout, clippy::print_stderr)]

//! Exercises the Linux USB layer on a real device: listing, descriptors,
//! control transfers, UVC streaming over isochronous URBs, interrupt reads,
//! bulk reads, and reattaching the kernel driver after a detaching claim.
//!
//! Run it on a board, under the device lock, one command per process:
//!
//! ```text
//! usb_probe list                          # every device, with its interfaces
//! usb_probe describe LOC                  # descriptors, endpoints, driver, speed
//! usb_probe control LOC                   # GET_DESCRIPTOR, GET_STATUS, UVC GET_CUR/GET_DEF/GET_MIN/GET_MAX
//! usb_probe stream LOC FRAMES [caller]    # UVC probe/commit, alt setting, FRAMES frames over isochronous URBs
//! usb_probe interrupt LOC SECONDS         # the VideoControl status endpoint
//! usb_probe bulk LOC                      # bulk round-trip on the first bulk IN endpoint, if there is one
//! usb_probe reattach LOC                  # detach, claim, release; the kernel driver and /dev/videoN come back
//! ```
//!
//! LOC is the sysfs location (`3-1`). Nothing is written to the device except
//! the UVC probe and commit (the streaming parameters, which the driver would
//! set the same way). `stream` leaves the device at alternate setting 0, and
//! `reattach` and every command that claims an interface hand it back to the
//! kernel driver when they finish.

use lemnos_usb::{
    ConfigurationInfo, ControlKind, ControlRecipient, ControlSetup, Direction, EndpointInfo,
    TransferStatus, TransferType, UsbDevice, UsbDeviceInfo, UsbLocation,
};
use std::path::Path;
use std::time::{Duration, Instant};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

/// UVC class codes (USB Video Class 1.1).
const CLASS_VIDEO: u8 = 0x0e;
const SUBCLASS_CONTROL: u8 = 0x01;
const SUBCLASS_STREAMING: u8 = 0x02;

/// UVC requests and the probe/commit selector.
const SET_CUR: u8 = 0x01;
const GET_CUR: u8 = 0x81;
const GET_MIN: u8 = 0x82;
const GET_MAX: u8 = 0x83;
const GET_DEF: u8 = 0x87;
const VS_PROBE_CONTROL: u16 = 0x0100;
const VS_COMMIT_CONTROL: u16 = 0x0200;
/// The probe structure of UVC 1.1 (26 bytes).
const PROBE_LEN: u16 = 26;

/// Standard requests.
const GET_STATUS: u8 = 0x00;
const GET_DESCRIPTOR: u8 = 0x06;

fn main() {
    if let Err(message) = run() {
        eprintln!("usb_probe: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("list");
    if command == "list" {
        return list();
    }
    let location: UsbLocation = args
        .get(1)
        .ok_or("a location is needed (for example 3-1)")?
        .parse()
        .map_err(|error| format!("{error}"))?;
    match command {
        "describe" => describe(&location),
        "control" => control(&location),
        "stream" => {
            let frames: u32 = args
                .get(2)
                .ok_or("stream needs a frame count")?
                .parse()
                .map_err(|_| "the frame count is a number")?;
            let caller = args.get(3).is_some_and(|mode| mode == "caller");
            stream(&location, frames, caller)
        }
        "interrupt" => {
            let seconds: u64 = args
                .get(2)
                .ok_or("interrupt needs a number of seconds")?
                .parse()
                .map_err(|_| "the seconds are a number")?;
            interrupt(&location, Duration::from_secs(seconds))
        }
        "bulk" => bulk(&location),
        "reattach" => reattach(&location),
        other => Err(format!("unknown command {other}")),
    }
}

fn open(location: &UsbLocation) -> Result<UsbDevice, String> {
    UsbDevice::open_location(location).map_err(|error| format!("opening {location}: {error}"))
}

fn list() -> Result<(), String> {
    for info in lemnos_usb::list_devices().map_err(|error| error.to_string())? {
        println!(
            "{} {:04x}:{:04x} class {:02x}/{:02x} speed {:?} driver {:?} product {:?}",
            info.location(),
            info.vendor_id(),
            info.product_id(),
            info.class(),
            info.subclass(),
            info.speed(),
            info.driver(),
            info.product(),
        );
        for interface in info.interfaces() {
            println!(
                "    interface {} class {:02x}/{:02x}/{:02x}",
                interface.number, interface.class, interface.subclass, interface.protocol
            );
        }
    }
    Ok(())
}

/// The listing entry for `location`, and the open device.
fn open_with_info(location: &UsbLocation) -> Result<(UsbDeviceInfo, UsbDevice), String> {
    let info = lemnos_usb::find_device(location)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no device at {location}"))?;
    let device = UsbDevice::open(&info).map_err(|error| format!("opening {location}: {error}"))?;
    Ok((info, device))
}

/// The UVC interface numbers of the device: (control, streaming), if any.
fn uvc_interfaces(config: &ConfigurationInfo) -> (Option<u8>, Option<u8>) {
    let mut control = None;
    let mut streaming = None;
    for interface in &config.interfaces {
        let Some(first) = interface.alternates.first() else {
            continue;
        };
        if first.class != CLASS_VIDEO {
            continue;
        }
        match first.subclass {
            SUBCLASS_CONTROL => control = Some(interface.number),
            SUBCLASS_STREAMING => streaming = Some(interface.number),
            _ => {}
        }
    }
    (control, streaming)
}

fn describe(location: &UsbLocation) -> Result<(), String> {
    let (info, device) = open_with_info(location)?;
    let descriptor = device.device_descriptor();
    println!(
        "device {} {:04x}:{:04x} usb {:x}.{:02x} device {:x}.{:02x} class {:02x}/{:02x}/{:02x} configs {}",
        location,
        descriptor.vendor_id,
        descriptor.product_id,
        descriptor.usb_version >> 8,
        descriptor.usb_version & 0xff,
        descriptor.device_version >> 8,
        descriptor.device_version & 0xff,
        descriptor.class,
        descriptor.subclass,
        descriptor.protocol,
        descriptor.num_configurations,
    );
    println!(
        "listing: speed {:?} usb_version {:04x} device_version {:04x} manufacturer {:?} product {:?} serial {:?}",
        info.speed(),
        info.usb_version(),
        info.device_version(),
        info.manufacturer(),
        info.product(),
        info.serial_number(),
    );
    let speed = device.speed().map_err(|error| error.to_string())?;
    println!("open device speed (USBDEVFS_GET_SPEED): {speed:?}");
    let active = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    println!("active configuration {}", active.value);
    for interface in &active.interfaces {
        let driver = device
            .driver(interface.number)
            .map_err(|error| error.to_string())?;
        println!("interface {} driver {:?}", interface.number, driver);
        for alternate in &interface.alternates {
            println!(
                "  alt {} class {:02x}/{:02x}/{:02x} endpoints {}",
                alternate.alternate,
                alternate.class,
                alternate.subclass,
                alternate.protocol,
                alternate.endpoints.len()
            );
            for endpoint in &alternate.endpoints {
                println!(
                    "    ep {:#04x} {:?} {:?} max {} x{} interval {}",
                    endpoint.address,
                    endpoint.direction,
                    endpoint.transfer_type,
                    endpoint.max_packet_size,
                    endpoint.packets_per_microframe,
                    endpoint.interval
                );
            }
        }
    }
    Ok(())
}

fn control(location: &UsbLocation) -> Result<(), String> {
    let device = open(location)?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;

    // Standard requests to the device: no claim needed.
    let device_descriptor = standard_in(&device, GET_DESCRIPTOR, 0x0100, 0, 18)?;
    println!(
        "GET_DESCRIPTOR(device) {} bytes: {:02x?}",
        device_descriptor.len(),
        device_descriptor
    );
    let header = standard_in(&device, GET_DESCRIPTOR, 0x0200, 0, 9)?;
    let total = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let full = standard_in(&device, GET_DESCRIPTOR, 0x0200, 0, total as u16)?;
    println!(
        "GET_DESCRIPTOR(configuration) header {} bytes, full {} of {} bytes (wTotalLength {})",
        header.len(),
        full.len(),
        total,
        total
    );
    let status = standard_in(&device, GET_STATUS, 0, 0, 2)?;
    println!("GET_STATUS(device) {status:02x?}");

    let (control_interface, streaming_interface) = uvc_interfaces(&config);
    let Some(vs) = streaming_interface else {
        println!("no UVC streaming interface: the UVC requests are skipped");
        return Ok(());
    };
    // usbfs refuses an interface-recipient request (EBUSY) unless this handle
    // claims the interface, so the class requests claim it, detaching uvcvideo.
    let interface = device
        .detach_and_claim_interface(vs)
        .map_err(|error| format!("claiming interface {vs}: {error}"))?;
    for (name, request) in [
        ("GET_MIN", GET_MIN),
        ("GET_MAX", GET_MAX),
        ("GET_DEF", GET_DEF),
        ("GET_CUR", GET_CUR),
    ] {
        let setup = uvc_setup(Direction::In, request, VS_PROBE_CONTROL, vs);
        let bytes = device
            .control_in(&setup, PROBE_LEN, CONTROL_TIMEOUT)
            .map_err(|error| format!("{name}: {error}"))?;
        println!(
            "{name} VS_PROBE on interface {vs}: {}",
            describe_probe(&bytes)
        );
    }
    let len_setup = uvc_setup(Direction::In, 0x85, VS_PROBE_CONTROL, vs);
    let len = device
        .control_in(&len_setup, 2, CONTROL_TIMEOUT)
        .map_err(|error| format!("GET_LEN: {error}"))?;
    println!("GET_LEN VS_PROBE: {len:02x?}");
    drop(interface);
    if let Some(vc) = control_interface {
        let interface = device
            .detach_and_claim_interface(vc)
            .map_err(|error| format!("claiming interface {vc}: {error}"))?;
        // VideoControl's first request is GET_INFO on the processing unit's
        // control 0; a failure is printed, not fatal: the answer depends on the device.
        let setup = uvc_setup(Direction::In, 0x86, 0x0100, vc);
        match device.control_in(&setup, 1, CONTROL_TIMEOUT) {
            Ok(bytes) => println!("GET_INFO VC selector 1 on interface {vc}: {bytes:02x?}"),
            Err(error) => println!("GET_INFO VC selector 1 on interface {vc}: {error}"),
        }
        drop(interface);
    }
    Ok(())
}

/// A standard device request (IN) with the given value and index.
fn standard_in(
    device: &UsbDevice,
    request: u8,
    value: u16,
    index: u16,
    len: u16,
) -> Result<Vec<u8>, String> {
    let setup = ControlSetup {
        direction: Direction::In,
        kind: ControlKind::Standard,
        recipient: ControlRecipient::Device,
        request,
        value,
        index,
    };
    device
        .control_in(&setup, len, CONTROL_TIMEOUT)
        .map_err(|error| format!("standard request {request:#04x}: {error}"))
}

/// A UVC class request to an interface.
fn uvc_setup(direction: Direction, request: u8, value: u16, interface: u8) -> ControlSetup {
    ControlSetup {
        direction,
        kind: ControlKind::Class,
        recipient: ControlRecipient::Interface,
        request,
        value,
        index: u16::from(interface),
    }
}

/// The fields of a UVC probe structure, for printing.
fn describe_probe(bytes: &[u8]) -> String {
    if bytes.len() < 26 {
        return format!("{} bytes (short): {bytes:02x?}", bytes.len());
    }
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    format!(
        "hint {:#06x} format {} frame {} interval {} max_frame {} max_payload {}",
        le16(0),
        bytes[2],
        bytes[3],
        le32(4),
        le32(18),
        le32(22)
    )
}

/// The decoded probe fields the stream needs.
#[derive(Clone, Copy)]
struct Probe {
    raw: [u8; 26],
}

impl Probe {
    fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let raw: [u8; 26] = bytes
            .get(..26)
            .and_then(|slice| slice.try_into().ok())
            .ok_or("the probe is shorter than 26 bytes")?;
        Ok(Self { raw })
    }

    fn frame_interval(&self) -> u32 {
        u32::from_le_bytes([self.raw[4], self.raw[5], self.raw[6], self.raw[7]])
    }

    fn max_payload(&self) -> u32 {
        u32::from_le_bytes([self.raw[22], self.raw[23], self.raw[24], self.raw[25]])
    }
}

/// The isochronous IN endpoint of `alternate` (the streaming interface's
/// alternate settings), with its packet size in bytes per microframe
/// (`wMaxPacketSize` times the high-bandwidth multiplier).
fn iso_endpoints(config: &ConfigurationInfo, interface: u8) -> Vec<(u8, EndpointInfo)> {
    let Some(interface) = config.interface(interface) else {
        return Vec::new();
    };
    interface
        .alternates
        .iter()
        .filter_map(|setting| {
            setting
                .endpoints
                .iter()
                .find(|endpoint| {
                    endpoint.direction == Direction::In
                        && endpoint.transfer_type == TransferType::Isochronous
                })
                .map(|endpoint| (setting.alternate, *endpoint))
        })
        .collect()
}

fn packet_bytes(endpoint: &EndpointInfo) -> usize {
    usize::from(endpoint.max_packet_size) * usize::from(endpoint.packets_per_microframe.max(1))
}

/// Counts from the UVC payload headers and the packet results of a stream.
#[derive(Default)]
struct StreamStats {
    packets: u64,
    zero_length: u64,
    bytes: u64,
    status_errors: u64,
    uvc_errors: u64,
    frames: u64,
    header_bad: u64,
    timeouts: u64,
    blocks: u64,
    last_fid: Option<u8>,
}

impl StreamStats {
    /// Reads the packets of one completed transfer: each UVC payload starts
    /// with a header (length, then flags: FID, EOF, ERR).
    fn record(&mut self, block: &lemnos_usb::IsoBlock<'_>) {
        self.blocks += 1;
        for (packet, data) in block.packets() {
            self.packets += 1;
            if packet.status != TransferStatus::Ok {
                self.status_errors += 1;
            }
            if packet.actual_length == 0 {
                self.zero_length += 1;
                continue;
            }
            self.bytes += data.len() as u64;
            let header = usize::from(data[0]);
            if header < 2 || header > data.len() {
                self.header_bad += 1;
                continue;
            }
            let flags = data[1];
            if flags & 0x40 != 0 {
                self.uvc_errors += 1;
            }
            let fid = flags & 0x01;
            if self.last_fid.is_some_and(|last| last != fid) {
                self.frames += 1;
            }
            self.last_fid = Some(fid);
        }
    }

    fn error_packets(&self) -> u64 {
        self.status_errors + self.uvc_errors
    }
}

fn stream(location: &UsbLocation, frames: u32, caller: bool) -> Result<(), String> {
    let (info, device) = open_with_info(location)?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    let (control_interface, streaming_interface) = uvc_interfaces(&config);
    let vs = streaming_interface.ok_or("no UVC streaming interface")?;
    let vc = control_interface;
    println!(
        "{} {:04x}:{:04x} streaming interface {vs} (control {vc:?}), speed {:?}, driver before {:?}",
        location,
        info.vendor_id(),
        info.product_id(),
        device.speed().map_err(|error| error.to_string())?,
        device.driver(vs).map_err(|error| error.to_string())?,
    );

    let buffer = device.allocate_buffer(4096);
    println!(
        "usbdevfs mapping: allocate_buffer is_mapped={}",
        buffer.is_mapped()
    );
    drop(buffer);

    let interface = device
        .detach_and_claim_interface(vs)
        .map_err(|error| format!("claiming interface {vs}: {error}"))?;
    println!(
        "claimed interface {vs} with detach; driver while claimed {:?}",
        device.driver(vs).map_err(|error| error.to_string())?
    );

    // Probe and commit with the device's default settings, as uvcvideo does.
    let probe_setup = uvc_setup(Direction::In, GET_DEF, VS_PROBE_CONTROL, vs);
    let default = device
        .control_in(&probe_setup, PROBE_LEN, CONTROL_TIMEOUT)
        .map_err(|error| format!("GET_DEF probe: {error}"))?;
    let mut probe = Probe::from_bytes(&default)?;
    let out_setup = uvc_setup(Direction::Out, SET_CUR, VS_PROBE_CONTROL, vs);
    device
        .control_out(&out_setup, &probe.raw, CONTROL_TIMEOUT)
        .map_err(|error| format!("SET_CUR probe: {error}"))?;
    let negotiated = device
        .control_in(
            &uvc_setup(Direction::In, GET_CUR, VS_PROBE_CONTROL, vs),
            PROBE_LEN,
            CONTROL_TIMEOUT,
        )
        .map_err(|error| format!("GET_CUR probe: {error}"))?;
    probe = Probe::from_bytes(&negotiated)?;
    println!(
        "probe negotiated: {} (frame interval {} = {:.2} fps)",
        describe_probe(&negotiated),
        probe.frame_interval(),
        1.0e7 / f64::from(probe.frame_interval().max(1))
    );
    device
        .control_out(
            &uvc_setup(Direction::Out, SET_CUR, VS_COMMIT_CONTROL, vs),
            &probe.raw,
            CONTROL_TIMEOUT,
        )
        .map_err(|error| format!("SET_CUR commit: {error}"))?;
    println!("committed");

    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    let endpoints = iso_endpoints(&config, vs);
    let wanted = probe.max_payload() as usize;
    // The smallest alternate setting whose packets carry the negotiated
    // payload; the largest one when none does.
    let (alternate, endpoint) = endpoints
        .iter()
        .filter(|(_, endpoint)| packet_bytes(endpoint) >= wanted)
        .min_by_key(|(_, endpoint)| packet_bytes(endpoint))
        .or_else(|| {
            endpoints
                .iter()
                .max_by_key(|(_, endpoint)| packet_bytes(endpoint))
        })
        .copied()
        .ok_or("no isochronous IN endpoint in the streaming interface")?;
    interface
        .set_alternate(alternate)
        .map_err(|error| format!("alternate {alternate}: {error}"))?;
    let packet_len = packet_bytes(&endpoint);
    let packets = 32;
    println!(
        "alt {alternate}: endpoint {:#04x} packet {packet_len} bytes ({} x {}), {packets} packets per transfer, payload {wanted}",
        endpoint.address, endpoint.max_packet_size, endpoint.packets_per_microframe
    );

    if caller {
        device
            .set_caller_driven_reaping(true)
            .map_err(|error| error.to_string())?;
    }
    let mut reader = device
        .isochronous_reader(endpoint.address, packet_len, packets, 4)
        .map_err(|error| error.to_string())?;
    println!(
        "reaping: {}; transfer {} bytes",
        if caller {
            "caller-driven (reap_until)"
        } else {
            "reaper thread"
        },
        reader.transfer_len()
    );

    let mut stats = StreamStats::default();
    let start = Instant::now();
    let limit = Duration::from_secs(60);
    while stats.frames < u64::from(frames) && start.elapsed() < limit {
        match reader.next_block_until(Some(Instant::now() + Duration::from_secs(2))) {
            Ok(Some(block)) => stats.record(&block),
            Ok(None) => stats.timeouts += 1,
            Err(error) => {
                drop(reader);
                let _ = interface.set_alternate(0);
                return Err(format!("reading the stream: {error}"));
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    drop(reader);
    let _ = interface.set_alternate(0);
    drop(interface);

    let error_rate = if stats.packets == 0 {
        0.0
    } else {
        100.0 * stats.error_packets() as f64 / stats.packets as f64
    };
    println!(
        "stream: {} frames in {:.3} s = {:.2} fps (target {:.2}); blocks {}, timeouts {}",
        stats.frames,
        elapsed,
        stats.frames as f64 / elapsed.max(1e-9),
        1.0e7 / f64::from(probe.frame_interval().max(1)),
        stats.blocks,
        stats.timeouts,
    );
    println!(
        "packets {} (zero-length {}), bytes {} ({:.2} MB/s), packet errors {} (status {}, UVC ERR {}), error rate {:.4}%, bad headers {}",
        stats.packets,
        stats.zero_length,
        stats.bytes,
        stats.bytes as f64 / elapsed.max(1e-9) / 1e6,
        stats.error_packets(),
        stats.status_errors,
        stats.uvc_errors,
        error_rate,
        stats.header_bad,
    );
    println!(
        "driver after the claim was released: {:?}",
        device.driver(vs).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn interrupt(location: &UsbLocation, duration: Duration) -> Result<(), String> {
    let device = open(location)?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    let (vc, _) = uvc_interfaces(&config);
    let vc = vc.ok_or("no UVC control interface")?;
    let endpoint = config
        .find_endpoint(vc, None, Direction::In, TransferType::Interrupt)
        .ok_or("the control interface has no interrupt IN endpoint")?;
    let interface = device
        .detach_and_claim_interface(vc)
        .map_err(|error| format!("claiming interface {vc}: {error}"))?;
    println!(
        "claimed control interface {vc}; interrupt IN {:#04x} max {}",
        endpoint.address, endpoint.max_packet_size
    );
    let start = Instant::now();
    let mut events = 0u64;
    let mut timeouts = 0u64;
    let mut buffer = vec![0u8; usize::from(endpoint.max_packet_size.max(16))];
    while start.elapsed() < duration {
        match device.interrupt_read(endpoint.address, &mut buffer, Duration::from_millis(500)) {
            Ok(len) => {
                events += 1;
                println!("event {len} bytes: {:02x?}", &buffer[..len]);
            }
            Err(error) if error.kind() == lemnos_usb::ErrorKind::Timeout => timeouts += 1,
            Err(error) => {
                drop(interface);
                return Err(format!("interrupt read: {error}"));
            }
        }
    }
    println!(
        "interrupt: {events} events, {timeouts} timeouts in {:.1} s",
        start.elapsed().as_secs_f64()
    );
    drop(interface);
    Ok(())
}

fn bulk(location: &UsbLocation) -> Result<(), String> {
    let device = open(location)?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    let endpoint = config
        .interfaces
        .iter()
        .flat_map(|interface| interface.alternates.iter())
        .flat_map(|setting| setting.endpoints.iter())
        .find(|endpoint| {
            endpoint.transfer_type == TransferType::Bulk && endpoint.direction == Direction::In
        })
        .copied();
    let Some(endpoint) = endpoint else {
        println!("bulk: SKIP, no bulk IN endpoint in the active configuration");
        return Ok(());
    };
    println!(
        "bulk IN endpoint {:#04x} max {}",
        endpoint.address, endpoint.max_packet_size
    );
    let mut buffer = vec![0u8; usize::from(endpoint.max_packet_size.max(64))];
    let started = Instant::now();
    match device.bulk_read(endpoint.address, &mut buffer, Duration::from_millis(500)) {
        Ok(len) => println!("bulk read {len} bytes in {:?}", started.elapsed()),
        Err(error) => println!("bulk read: {error}"),
    }
    Ok(())
}

/// The video device nodes of an interface, from sysfs (`/sys/bus/usb/devices/<name>/video4linux`).
fn video_nodes(name: &str) -> Vec<String> {
    let dir = Path::new("/sys/bus/usb/devices")
        .join(name)
        .join("video4linux");
    let mut nodes: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("video"))
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes
}

/// The sysfs driver name of an interface, from its `driver` link.
fn sysfs_driver(name: &str) -> Option<String> {
    let link = Path::new("/sys/bus/usb/devices").join(name).join("driver");
    std::fs::read_link(link).ok().and_then(|target| {
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

fn reattach(location: &UsbLocation) -> Result<(), String> {
    let (_, device) = open_with_info(location)?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;
    let (vc, vs) = uvc_interfaces(&config);
    let vs = vs.ok_or("no UVC streaming interface")?;
    let vc = vc.ok_or("no UVC control interface")?;
    // uvcvideo registers the video nodes on the control interface (and claims
    // the streaming interface from there), so both names are watched.
    let vc_name = format!("{location}:{}.{vc}", config.value);
    let vs_name = format!("{location}:{}.{vs}", config.value);
    let nodes_before = video_nodes(&vc_name);
    println!(
        "before: streaming driver {:?} (sysfs {:?}), video nodes {:?}",
        device.driver(vs).map_err(|error| error.to_string())?,
        sysfs_driver(&vs_name),
        nodes_before
    );

    let interface = device
        .detach_and_claim_interface(vs)
        .map_err(|error| format!("detaching and claiming {vs}: {error}"))?;
    println!(
        "claimed: streaming driver {:?} (sysfs {:?}), video nodes {:?}",
        device.driver(vs).map_err(|error| error.to_string())?,
        sysfs_driver(&vs_name),
        video_nodes(&vc_name)
    );
    drop(interface);

    // First: does the release alone bring the driver back? Wait up to 5 s.
    let driver_back = wait_for(Duration::from_secs(5), || sysfs_driver(&vs_name).is_some());
    let release_nodes = video_nodes(&vc_name);
    println!(
        "released: driver back {driver_back} (streaming driver {:?}, sysfs {:?}), video nodes {:?}",
        device.driver(vs).map_err(|error| error.to_string())?,
        sysfs_driver(&vs_name),
        release_nodes
    );
    if driver_back && release_nodes == nodes_before {
        println!("reattach: PASS, the release alone rebinds the driver and the video nodes");
        return Ok(());
    }

    // Second: the UVC recovery, which the kernel's own bind/unbind does. The
    // control interface's unbind releases the stream, and its bind re-registers
    // the video nodes and claims the streaming interface again.
    println!("reattach: the release alone did not rebind; rebinding uvcvideo to {vc_name}");
    let _ = std::fs::write("/sys/bus/usb/drivers/uvcvideo/unbind", &vc_name);
    std::thread::sleep(Duration::from_millis(500));
    let bind = std::fs::write("/sys/bus/usb/drivers/uvcvideo/bind", &vc_name);
    println!("bind {vc_name}: {bind:?}");
    let back = wait_for(Duration::from_secs(10), || sysfs_driver(&vs_name).is_some());
    let after_nodes = video_nodes(&vc_name);
    println!(
        "after the rebind: streaming driver {:?}, video nodes {:?}",
        sysfs_driver(&vs_name),
        after_nodes
    );
    println!(
        "reattach: {}",
        if back && after_nodes == nodes_before {
            "PASS after the UVC rebind (the release alone did not reattach)"
        } else {
            "FAIL: the driver or the video nodes did not come back"
        }
    );
    Ok(())
}

/// Polls `check` until it holds or `limit` passes; returns whether it held.
fn wait_for(limit: Duration, check: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !check() {
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}
