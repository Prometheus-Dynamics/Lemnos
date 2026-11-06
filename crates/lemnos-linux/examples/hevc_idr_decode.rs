#![allow(clippy::print_stdout, clippy::print_stderr)]

//! Decodes one HEVC IDR frame on a stateless decoder (`rpi-hevc-dec` on the
//! BCM2712 board, `/dev/video19`) through the request API, and checks it
//! against a reference decode of the same frame.
//!
//! ```text
//! hevc_idr_decode STREAM.h265 [VIDEO_NODE] [REFERENCE.nv12]
//! ```
//!
//! The stream's first VPS, SPS, PPS and IDR slice are parsed with `lemnos-hevc`
//! and sent as the decoder's controls in one request, with the slice NAL unit
//! (start code in front) as the buffer. The decoder keeps NV12 in 128-byte
//! columns (`Nc12` in this kernel, two planes); the frame is read back as
//! linear NV12 at the visible width and compared byte for byte with
//! `REFERENCE.nv12` if given. `HEVC_OUT=path` also writes the linear frame.
//!
//! Needs root or access to the device nodes, and a stateless HEVC decoder
//! with the request API. Each step's time is printed.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use lemnos_hevc as hevc;
use lemnos_linux::v4l2::hevc::{self as conv, START_CODE_LEN};
use lemnos_linux::v4l2::m2m::{M2mSession, QueueSetup, Ready};
use lemnos_linux::v4l2::request::{self, Request};
use lemnos_linux::v4l2::{
    self, ExtControl, ExtValue, Format, Memory, PixFormatMplane, PlaneFormat, PlaneMemory,
    QueuePlane, fourcc,
};
use lemnos_linux_sys::v4l2::hevc as sys;
use lemnos_v4l2::FourCc;

/// The OUTPUT buffer: one slice NAL unit (the test stream's slice is ~4 KB).
const SOURCE_BYTES: u32 = 1 << 20;
/// Room for one unit's RBSP (the parameter sets here are far smaller).
const RBSP_MAX: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

fn io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn step(label: &str, started: Instant) {
    println!("  {label}: {:.1} us", started.elapsed().as_secs_f64() * 1e6);
}

fn unescape_unit(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = vec![0u8; RBSP_MAX];
    let n = hevc::unescape(raw, &mut out).map_err(io_error)?;
    out.truncate(n);
    Ok(out)
}

/// The decoder's capture planes for MMAP buffers: one entry per plane.
fn capture_planes() -> Vec<QueuePlane<'static>> {
    (0..2)
        .map(|_| QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used: 0,
            data_offset: 0,
        })
        .collect()
}

/// Reads a `Nc12` frame (luma, then chroma, each in 128-byte columns of
/// `h` rows, or `h / 2` for chroma) into linear NV12 of `w` columns.
fn linear_nv12(luma: &[u8], chroma: &[u8], w: usize, h: usize) -> Vec<u8> {
    const COL: usize = 128;
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    for row in 0..h {
        for x in 0..w {
            out.push(luma[(x / COL) * COL * h + row * COL + x % COL]);
        }
    }
    for row in 0..h / 2 {
        for k in 0..w {
            out.push(chroma[(k / COL) * COL * (h / 2) + row * COL + k % COL]);
        }
    }
    out
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let stream_path = args
        .get(1)
        .ok_or_else(|| io::Error::other("usage: hevc_idr_decode STREAM [NODE] [REFERENCE]"))?;
    let node = args.get(2).map(String::as_str).unwrap_or("/dev/video19");
    let reference = args.get(3);

    // Parse the first VPS, SPS, PPS and IDR slice.
    let parse_started = Instant::now();
    let stream = std::fs::read(stream_path)?;
    let (mut sps, mut pps, mut slice) = (None, None, None);
    for nal in hevc::nal_units(&stream) {
        match nal.unit_type {
            32 => {
                let rbsp = unescape_unit(nal.raw)?;
                hevc::parse_vps(&rbsp).map_err(io_error)?;
            }
            33 if sps.is_none() => {
                let rbsp = unescape_unit(nal.raw)?;
                sps = Some(hevc::parse_sps(&rbsp).map_err(io_error)?);
            }
            34 if pps.is_none() => {
                let rbsp = unescape_unit(nal.raw)?;
                pps = Some(hevc::parse_pps(&rbsp).map_err(io_error)?);
            }
            19 | 20 if slice.is_none() => slice = Some(nal),
            _ => {}
        }
    }
    let sps = sps.ok_or_else(|| io::Error::other("no SPS in the stream"))?;
    let pps = pps.ok_or_else(|| io::Error::other("no PPS in the stream"))?;
    let nal = slice.ok_or_else(|| io::Error::other("no IDR slice in the stream"))?;
    let rbsp = unescape_unit(nal.raw)?;
    let header = hevc::parse_slice_header(&rbsp, nal.unit_type, &sps, &pps).map_err(io_error)?;
    let (w, h) = (sps.width, sps.height);
    let sps_ctrl = conv::sps_control(&sps)?;
    let pps_ctrl = conv::pps_control(&pps)?;
    let mut frame = Vec::with_capacity(START_CODE_LEN + nal.raw.len());
    frame.extend_from_slice(&[0, 0, 1]);
    frame.extend_from_slice(nal.raw);
    let idr = conv::idr_slice(
        &header,
        nal.raw,
        nal.unit_type,
        nal.temporal_id_plus1,
        frame.len(),
    )?;
    println!(
        "stream {stream_path}: {w}x{h}, {} bytes, IDR slice {} bytes, slice data at buffer byte {}",
        stream.len(),
        nal.raw.len(),
        idr.slice.data_byte_offset
    );
    step("parse and convert", parse_started);

    // The device.
    let caps = v4l2::query(node)?;
    if !caps.is_m2m_multiplanar() {
        return Err(io::Error::other(format!(
            "{node} is not a multi-planar M2M device"
        )));
    }
    println!("node {node}: driver {}", caps.driver);
    let media = request::media_device_for(node)?
        .ok_or_else(|| io::Error::other(format!("no media device lists {node}")))?;

    // Two queues, MMAP. The driver keeps the capture queue's layout (`Nc12`)
    // when NV12 is asked for with two planes.
    let open_started = Instant::now();
    let mut source_planes = [PlaneFormat {
        size_image: 0,
        bytes_per_line: 0,
    }; 8];
    source_planes[0] = PlaneFormat {
        size_image: SOURCE_BYTES,
        bytes_per_line: 0,
    };
    let mut capture_planes_fmt = [PlaneFormat {
        size_image: 0,
        bytes_per_line: 0,
    }; 8];
    capture_planes_fmt[0] = PlaneFormat {
        size_image: w * h,
        bytes_per_line: w,
    };
    capture_planes_fmt[1] = PlaneFormat {
        size_image: w * h / 2,
        bytes_per_line: w,
    };
    let mut s = M2mSession::open(
        node,
        QueueSetup {
            format: Format::Multi(PixFormatMplane {
                width: w,
                height: h,
                fourcc: FourCc(sys::PIX_FMT_HEVC_SLICE),
                field: 0,
                colorspace: 0,
                planes: source_planes[..1].to_vec(),
                ..Default::default()
            }),
            memory: Memory::Mmap,
            buffers: 2,
            crop: None,
        },
        QueueSetup {
            format: Format::Multi(PixFormatMplane {
                width: w,
                height: h,
                fourcc: FourCc::new(b"NV12"),
                field: 0,
                colorspace: 0,
                planes: capture_planes_fmt[..2].to_vec(),
                ..Default::default()
            }),
            memory: Memory::Mmap,
            buffers: 4,
            crop: None,
        },
    )?;
    step("open (both queues, MMAP)", open_started);

    let stream_started = Instant::now();
    for i in 0..s.capture_buffers() {
        s.queue_capture(i, &capture_planes())?;
    }
    s.stream_on()?;
    step("stream on", stream_started);

    // The request: the parameters as controls, then the slice as the buffer.
    let req = Request::allocate(File::open(&media)?)?;
    let controls_started = Instant::now();
    s.set_request_controls(
        &req,
        &[
            ExtControl {
                id: sys::CID_SPS,
                value: ExtValue::Bytes(sps_ctrl.as_bytes()),
            },
            ExtControl {
                id: sys::CID_PPS,
                value: ExtValue::Bytes(pps_ctrl.as_bytes()),
            },
            ExtControl {
                id: sys::CID_SLICE_PARAMS,
                value: ExtValue::Bytes(idr.slice.as_bytes()),
            },
            ExtControl {
                id: sys::CID_DECODE_PARAMS,
                value: ExtValue::Bytes(idr.decode.as_bytes()),
            },
            ExtControl {
                id: sys::CID_DECODE_MODE,
                value: ExtValue::Int(sys::DECODE_MODE_FRAME_BASED),
            },
            ExtControl {
                id: sys::CID_START_CODE,
                value: ExtValue::Int(sys::START_CODE_ANNEX_B),
            },
        ],
    )?;
    step("request controls", controls_started);

    let index = s
        .free_output()
        .ok_or_else(|| io::Error::other("no free source buffer"))?;
    s.output_plane_mut(index, 0)
        .ok_or_else(|| io::Error::other("no source plane"))?[..frame.len()]
        .copy_from_slice(&frame);
    let queued = Instant::now();
    s.queue_output_in_request(
        index,
        &[QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used: frame.len() as u32,
            data_offset: 0,
        }],
        Duration::ZERO,
        &req,
    )?;
    req.queue()?;
    if !req.wait(Some(TIMEOUT))? {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the request did not complete",
        ));
    }
    step("request run", queued);
    let ready = s.wait(
        Ready {
            capture: true,
            ..Ready::default()
        },
        Some(TIMEOUT),
    )?;
    if !ready.capture {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "no decoded frame"));
    }
    let done = s
        .dequeue_capture()?
        .ok_or_else(|| io::Error::other("no decoded frame"))?;
    step("queue to decoded frame", queued);

    // Read the frame back as linear NV12 at the visible width.
    let (pixfmt, luma_stride) = match s.capture_format() {
        Format::Multi(m) => (m.fourcc.0, m.planes[0].bytes_per_line),
        Format::Single(p) => (p.fourcc.0, p.bytes_per_line),
        Format::Meta(_) => unreachable!("a session has no metadata queue"),
    };
    if pixfmt != fourcc(b"Nc12") || luma_stride != w.div_ceil(128) * 128 {
        return Err(io::Error::other(
            "the capture layout is not the 128-byte columns this reads",
        ));
    }
    let luma = s
        .capture_plane(done.index, 0)
        .ok_or_else(|| io::Error::other("no luma plane"))?;
    let chroma = s
        .capture_plane(done.index, 1)
        .ok_or_else(|| io::Error::other("no chroma plane"))?;
    let out = linear_nv12(luma, chroma, w as usize, h as usize);
    let distinct = {
        let mut seen = [false; 256];
        for &v in &out[..(w * h) as usize] {
            seen[usize::from(v)] = true;
        }
        seen.iter().filter(|&&b| b).count()
    };
    println!(
        "  luma: {distinct} distinct values, first row {:?}",
        &out[..8]
    );
    if let Ok(path) = std::env::var("HEVC_OUT") {
        std::fs::write(path, &out)?;
    }
    if distinct < 2 {
        return Err(io::Error::other("the decoded frame is blank"));
    }
    if let Some(path) = reference {
        let want = std::fs::read(Path::new(path))?;
        if want.len() != out.len() {
            return Err(io::Error::other(format!(
                "reference is {} bytes, the frame is {}",
                want.len(),
                out.len()
            )));
        }
        let differ = out.iter().zip(&want).filter(|(a, b)| a != b).count();
        println!("  against {path}: {differ} of {} bytes differ", out.len());
        if differ != 0 {
            return Err(io::Error::other(
                "the decoded frame differs from the reference",
            ));
        }
    }
    s.queue_capture(done.index, &capture_planes())?;
    s.stream_off()?;
    println!("done");
    Ok(())
}
