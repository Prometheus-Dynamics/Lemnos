//! Descriptors laid out the way a USB 2 webcam lays them out, with made-up values: an interface
//! association, a video control interface with a camera terminal, a processing unit, an output
//! terminal and a status endpoint, then a streaming interface with an uncompressed and an MJPEG
//! format, a colour descriptor, and three isochronous alternate settings.

pub const ENDPOINT: u8 = 0x81;
pub const CONTROL_INTERFACE: u8 = 0;
pub const STREAM_INTERFACE: u8 = 1;

pub const MODE_VGA_30: u16 = 0;
pub const MODE_VGA_15: u16 = 1;
pub const MODE_QVGA_MIN: u16 = 2;
pub const MODE_MJPEG: u16 = 5;

/// The bytes, and where things are in them for a test to break.
pub struct Webcam {
    pub bytes: Vec<u8>,
    pub vc_header: usize,
    pub vs_header: usize,
    pub format_yuy2: usize,
    pub frame_vga: usize,
    pub frame_qvga: usize,
    pub format_mjpeg: usize,
    pub frame_mjpeg: usize,
    pub alt1_endpoint: usize,
    pub alt3_endpoint: usize,
}

pub const GUID_SUFFIX: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

pub fn interface(number: u8, alternate: u8, endpoints: u8, subclass: u8) -> Vec<u8> {
    vec![9, 4, number, alternate, endpoints, 0x0e, subclass, 0, 0]
}

pub fn iso_endpoint(address: u8, max_packet: u16) -> Vec<u8> {
    let [low, high] = max_packet.to_le_bytes();
    vec![7, 5, address, 0x05, low, high, 1]
}

pub fn bulk_endpoint(address: u8) -> Vec<u8> {
    vec![7, 5, address, 0x02, 0x00, 0x02, 0]
}

pub fn yuy2_format(index: u8, frames: u8) -> Vec<u8> {
    let mut d = vec![27, 0x24, 0x04, index, frames];
    d.extend_from_slice(b"YUY2");
    d.extend_from_slice(&GUID_SUFFIX);
    d.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
    assert_eq!(d.len(), 27);
    d
}

pub fn mjpeg_format(index: u8, frames: u8) -> Vec<u8> {
    vec![11, 0x24, 0x06, index, frames, 0x01, 1, 0, 0, 0, 0]
}

pub fn frame(
    subtype: u8,
    index: u8,
    size: (u16, u16),
    buffer: u32,
    default: u32,
    intervals: &[u32],
    continuous: bool,
) -> Vec<u8> {
    let mut d = vec![0, 0x24, subtype, index, 0];
    d.extend_from_slice(&size.0.to_le_bytes());
    d.extend_from_slice(&size.1.to_le_bytes());
    d.extend_from_slice(&1_000_000u32.to_le_bytes());
    d.extend_from_slice(&100_000_000u32.to_le_bytes());
    d.extend_from_slice(&buffer.to_le_bytes());
    d.extend_from_slice(&default.to_le_bytes());
    d.push(if continuous { 0 } else { intervals.len() as u8 });
    for interval in intervals {
        d.extend_from_slice(&interval.to_le_bytes());
    }
    d[0] = d.len() as u8;
    d
}

/// Assembles a configuration from its descriptors, filling in `wTotalLength`.
pub fn config(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![9, 2, 0, 0, 2, 1, 0, 0x80, 50];
    for part in parts {
        bytes.extend_from_slice(part);
    }
    let total = (bytes.len() as u16).to_le_bytes();
    bytes[2..4].copy_from_slice(&total);
    bytes
}

/// A control interface with a video control header, for a device built from parts.
pub fn control_parts() -> Vec<Vec<u8>> {
    vec![
        interface(CONTROL_INTERFACE, 0, 0, 1),
        vec![
            13,
            0x24,
            0x01,
            0x10,
            0x01,
            13,
            0,
            0,
            0x6c,
            0xdc,
            0x02,
            1,
            STREAM_INTERFACE,
        ],
    ]
}

pub fn webcam() -> Webcam {
    let mut control = vec![
        vec![
            13,
            0x24,
            0x01,
            0x10,
            0x01,
            0,
            0,
            0x00,
            0x6c,
            0xdc,
            0x02,
            1,
            STREAM_INTERFACE,
        ],
        // Camera terminal.
        vec![
            18, 0x24, 0x02, 1, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0x0e, 0x00,
        ],
        // Processing unit.
        vec![12, 0x24, 0x05, 2, 1, 0, 0, 2, 0x7f, 0x14, 0, 0],
        // Output terminal.
        vec![9, 0x24, 0x03, 3, 0x01, 0x01, 0, 2, 0],
    ];
    let vc_total: usize = control.iter().map(Vec::len).sum();
    control[0][5..7].copy_from_slice(&(vc_total as u16).to_le_bytes());

    let format_yuy2 = yuy2_format(1, 2);
    let frame_vga = frame(
        0x05,
        1,
        (640, 480),
        614_400,
        333_333,
        &[333_333, 666_666],
        false,
    );
    let frame_qvga = frame(
        0x05,
        2,
        (320, 240),
        153_600,
        500_000,
        &[333_333, 1_000_000, 166_667],
        true,
    );
    let colour = vec![6, 0x24, 0x0d, 1, 1, 4];
    let format_mjpeg = mjpeg_format(2, 1);
    let frame_mjpeg = frame(0x07, 1, (1280, 720), 921_600, 333_333, &[333_333], false);
    let stream_descriptors = [
        format_yuy2.clone(),
        frame_vga.clone(),
        frame_qvga.clone(),
        colour,
        format_mjpeg.clone(),
        frame_mjpeg.clone(),
    ];
    let stream_total = 14 + stream_descriptors.iter().map(Vec::len).sum::<usize>();
    let total = (stream_total as u16).to_le_bytes();
    let vs_header = vec![
        14, 0x24, 0x01, 2, total[0], total[1], ENDPOINT, 0, 3, 0, 0, 0, 1, 0,
    ];

    let mut parts = vec![
        // Interface association.
        vec![8, 0x0b, 0, 2, 0x0e, 0x03, 0, 0],
        interface(CONTROL_INTERFACE, 0, 1, 1),
    ];
    parts.append(&mut control);
    parts.push(vec![7, 5, 0x83, 0x03, 16, 0, 8]);
    parts.push(vec![5, 0x25, 0x03, 16, 0]);
    parts.push(interface(STREAM_INTERFACE, 0, 0, 2));
    parts.push(vs_header);
    parts.extend(stream_descriptors);
    parts.push(interface(STREAM_INTERFACE, 1, 1, 2));
    parts.push(iso_endpoint(ENDPOINT, 192));
    parts.push(interface(STREAM_INTERFACE, 2, 1, 2));
    parts.push(iso_endpoint(ENDPOINT, 944));
    parts.push(interface(STREAM_INTERFACE, 3, 1, 2));
    parts.push(iso_endpoint(ENDPOINT, 0x1400));

    let bytes = config(&parts);
    let offset = |needle: &[u8]| {
        bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("the fixture contains what it was built from")
    };
    let vc_header = offset(&[13, 0x24, 0x01, 0x10, 0x01]);
    let vs_header = offset(&[14, 0x24, 0x01, 2]);
    let alt1_endpoint = offset(&iso_endpoint(ENDPOINT, 192));
    let alt3_endpoint = offset(&iso_endpoint(ENDPOINT, 0x1400));
    let format_yuy2 = offset(&format_yuy2);
    let frame_vga = offset(&frame_vga);
    let frame_qvga = offset(&frame_qvga);
    let format_mjpeg = offset(&format_mjpeg);
    let frame_mjpeg = offset(&frame_mjpeg);
    Webcam {
        bytes,
        vc_header,
        vs_header,
        format_yuy2,
        frame_vga,
        frame_qvga,
        format_mjpeg,
        frame_mjpeg,
        alt1_endpoint,
        alt3_endpoint,
    }
}
