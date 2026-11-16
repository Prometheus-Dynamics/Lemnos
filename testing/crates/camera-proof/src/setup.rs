//! The camera chain's set-up, as the chain proof and the stall test both run it: the sensor
//! stopped and set up, the front end and the back end configured, their streams open, and every
//! configuration buffer written. [`Chain::start`] then starts the back end, the front end and the
//! sensor, in that order.

use std::sync::Arc;

use lemnos_client::{Client, DeviceEntry, FrameStream};
use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_receiver::CameraReceiver;
use lemnos_contract_camera_sensor::{CameraSensor, Controls, Mode};
use lemnos_contract_isp_pisp::{LAYOUT_BACK_END, LAYOUT_FRONT_END, OUTPUT_FRONT_END_0};
use lemnos_core::{Bound, Encode, Writer};
use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::fe::FrontEnd;
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    AGC_STATS_NUM_ZONES, BayerOrder as Order, BeCcmConfig, BeTilesConfig, Branch, FeAgcStatsConfig,
    FeAwbStatsConfig, FeConfig, FeRgbyConfig, ImageFormatConfig, Wire, fe_enable as en,
};
use lemnos_linux_stream::LinuxStreams;

/// The frame's width and height the chain is set up for: the sensor's mode 0.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 800;
/// RAW10's black level, moved to 16 bits.
const BLACK: u16 = 64 << 6;
/// Full-range BT.601, as the isp proof uses.
const CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33_554_432, 33_554_432],
};

/// What the set-up is told beyond the devices: the sensor's frame length and test pattern, when
/// the caller sets them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tuning {
    pub frame_length: Option<u32>,
    pub pattern: Option<u16>,
}

/// The streams one stage is driven through: its configuration, and its frames.
pub struct FrontStreams {
    pub cfg: FrameStream,
    pub stats: FrameStream,
    pub out: FrameStream,
}

pub struct BackStreams {
    pub cfg: FrameStream,
    /// The back end's `input` stream, when the board declares one. This chain never writes it: the
    /// front end's output 0 is lent to each job (`None`: the board declares none).
    pub input: Option<FrameStream>,
    pub out: FrameStream,
}

/// The chain, set up and not yet started.
pub struct Chain {
    pub sensor_name: String,
    pub receiver_name: String,
    pub front_name: String,
    pub back_name: String,
    pub sensor: Bound<dyn CameraSensor, lemnos_client::RemoteCaller>,
    pub receiver: Bound<dyn CameraReceiver, lemnos_client::RemoteCaller>,
    pub fe: Bound<dyn CameraIsp, lemnos_client::RemoteCaller>,
    pub be: Bound<dyn CameraIsp, lemnos_client::RemoteCaller>,
    pub mode: Mode,
    pub front: FrontStreams,
    pub back: BackStreams,
    /// The front end's output 0 stride and size, and the back end's input stride.
    pub fe_stride: u32,
    pub fe_size: u32,
    pub input_stride: u32,
    /// The back end's output 0 stride and size, and its tile count.
    pub be_stride: u32,
    pub be_size: u32,
    pub be_tiles: u32,
}

fn order(bayer: BayerOrder) -> Order {
    match bayer {
        BayerOrder::Rggb => Order::Rggb,
        BayerOrder::Grbg => Order::Grbg,
        BayerOrder::Gbrg => Order::Gbrg,
        BayerOrder::Bggr => Order::Bggr,
        BayerOrder::Mono => Order::Greyscale,
    }
}

/// The front end's configuration: output 0 on, 16-bit; AWB, AGC and focus statistics on, every
/// pixel counted, as `lemnos-fe-proof` builds it.
fn front_config(bayer: Order) -> Result<Vec<u8>, String> {
    let mut fe = FrontEnd::new(WIDTH as u16, HEIGHT as u16, bayer);
    fe.set_output_format(
        Branch::Zero,
        ImageFormatConfig {
            format: formats::BAYER16,
            ..Default::default()
        },
    );
    fe.set_rgby(FeRgbyConfig {
        gain_r: 306,
        gain_g: 601,
        gain_b: 117,
        ..Default::default()
    });
    fe.set_awb_stats(FeAwbStatsConfig {
        r_hi: u16::MAX,
        g_hi: u16::MAX,
        b_hi: u16::MAX,
        ..Default::default()
    });
    fe.set_agc_stats(FeAgcStatsConfig {
        weights: [0x11; AGC_STATS_NUM_ZONES / 2],
        ..Default::default()
    });
    fe.enable(
        en::output(0) | en::AWB_STATS | en::RGBY | en::AGC_STATS | en::CDAF_STATS,
        true,
    );
    let cfg = fe
        .prepare()
        .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let mut bytes = vec![0u8; FeConfig::SIZE];
    cfg.write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    Ok(bytes)
}

/// The back end's configuration: Bayer to NV12 at the input size, as `lemnos-isp-proof` builds it.
fn back_config(input: ImageFormatConfig, bayer: BayerOrder) -> Result<BeTilesConfig, String> {
    BackEnd::simple_bayer(
        input,
        order(bayer),
        BLACK,
        (1.6, 1.0, 1.6),
        None,
        formats::NV12,
        Some(CSC),
    )
    .prepare()
    .map_err(|e| format!("the configuration library refused: {e:?}"))
}

/// A front-end job: output 0 into `buffer`, its configuration in `buffer` too.
pub fn post_front(cfg: &mut FrameStream, identity: u64, buffer: u32) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_FRONT_END,
        config_buffer: buffer,
        input_buffer: NO_BUFFER,
        outputs: [NO_BUFFER, NO_BUFFER, buffer, NO_BUFFER],
        input_lent: false,
        lent_buffer: 0,
    };
    post_job(cfg, &job)
}

/// A back-end job with configuration buffer `slot`, reading the front-end buffer `lent` (a frame of
/// the front end's output 0, lent: no copy), and writing output 0 buffer `output`.
pub fn post_back(
    cfg: &mut FrameStream,
    identity: u64,
    slot: u32,
    lent: u32,
    output: u32,
) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: slot,
        input_buffer: NO_BUFFER,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        input_lent: true,
        lent_buffer: lent,
    };
    post_job(cfg, &job)
}

/// A back-end job whose input is the buffer `slot` of the back end's own `input` stream, which the
/// caller has written (the copy path, for a host whose buffers are memfds and cannot be lent), and
/// that writes output 0 buffer `output`.
pub fn post_back_copied(
    cfg: &mut FrameStream,
    identity: u64,
    slot: u32,
    output: u32,
) -> Result<(), String> {
    let job = IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: slot,
        input_buffer: slot,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        input_lent: false,
        lent_buffer: 0,
    };
    post_job(cfg, &job)
}

/// Posts one job on a configuration stream. `Err` names a full queue or a refused post.
pub fn post_job(cfg: &mut FrameStream, job: &IspJob) -> Result<(), String> {
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
    match cfg.post_job(buf.get(..n).unwrap_or(&[])) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the job queue is full".into()),
        Err(e) => Err(format!("post: {e:?}")),
    }
}

impl Chain {
    /// Sets the chain up on the four named devices: the sensor's mode 0, its frame length and test
    /// pattern as `tuning` says, the front end and back end configured for it, their streams open
    /// and each configuration buffer written. Nothing is started.
    ///
    /// # Errors
    /// A device is missing or refuses a call, a stream cannot be opened, or a configuration is
    /// refused. The message names which.
    #[allow(clippy::too_many_lines)]
    pub fn open(
        client: &Client,
        list: &[DeviceEntry],
        names: [&str; 4],
        tuning: Tuning,
    ) -> Result<Self, String> {
        let [sensor_name, receiver_name, front_name, back_name] = names;
        let index = |name: &str| {
            list.iter()
                .find(|d| d.name == name)
                .map(|d| d.index)
                .ok_or(format!("no device {name}"))
        };
        let fe_index = index(front_name)?;
        let be_index = index(back_name)?;
        let mut sensor = client
            .open::<dyn CameraSensor>(sensor_name)
            .map_err(|e| format!("{sensor_name}: {e}"))?;
        let receiver = client
            .open::<dyn CameraReceiver>(receiver_name)
            .map_err(|e| format!("{receiver_name}: {e}"))?;
        let mut fe = client
            .open::<dyn CameraIsp>(front_name)
            .map_err(|e| format!("{front_name}: {e}"))?;
        let mut be = client
            .open::<dyn CameraIsp>(back_name)
            .map_err(|e| format!("{back_name}: {e}"))?;

        let _ = sensor.stop();
        let _ = fe.stop();
        let _ = be.stop();
        sensor
            .select_mode(0)
            .map_err(|s| format!("select_mode: {s:?}"))?;
        let mode = sensor.mode(0).map_err(|s| format!("mode: {s:?}"))?;
        if let Some(lines) = tuning.frame_length {
            sensor
                .set_controls(Controls {
                    frame_length: Some(lines),
                    ..Controls::default()
                })
                .map_err(|s| format!("set_controls: {s:?}"))?;
        }
        if let Some(code) = tuning.pattern {
            sensor
                .set_test_pattern(code)
                .map_err(|s| format!("set_test_pattern: {s:?}"))?;
        }

        let mut input = ImageFormatConfig {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut input, 64);
        fe.set_input(IspInput {
            width: WIDTH,
            height: HEIGHT,
            bit_depth: 16,
            bayer: mode.bayer,
        })
        .map_err(|s| format!("front end set_input: {s:?}"))?;
        let fe_layout = fe
            .set_output(
                OUTPUT_FRONT_END_0,
                IspOutput {
                    width: WIDTH,
                    height: HEIGHT,
                    fourcc: u32::from_le_bytes(*b"BYR2"),
                    pad_bytes: 0,
                },
            )
            .map_err(|s| format!("front end set_output: {s:?}"))?;
        if i64::from(fe_layout.stride) != i64::from(input.stride) {
            return Err(format!(
                "front-end output stride {} is not the back end's input stride {}",
                fe_layout.stride, input.stride
            ));
        }
        be.set_input(IspInput {
            width: WIDTH,
            height: HEIGHT,
            bit_depth: 16,
            bayer: mode.bayer,
        })
        .map_err(|s| format!("back end set_input: {s:?}"))?;
        let be_layout = be
            .set_output(
                0,
                IspOutput {
                    width: WIDTH,
                    height: HEIGHT,
                    fourcc: u32::from_le_bytes(*b"NV12"),
                    pad_bytes: 0,
                },
            )
            .map_err(|s| format!("back end set_output: {s:?}"))?;
        let fe_bytes = front_config(order(mode.bayer))?;
        let be_config = back_config(input, mode.bayer)?;
        if i64::from(be_config.config.output_format[0].image.stride) != i64::from(be_layout.stride)
        {
            return Err(format!(
                "back-end configuration's output stride {} is not its layout's {}",
                be_config.config.output_format[0].image.stride, be_layout.stride
            ));
        }
        let mut be_bytes = vec![0u8; BeTilesConfig::SIZE];
        be_config
            .write_to(&mut be_bytes)
            .map_err(|_| "configuration size".to_string())?;

        let platform = Arc::new(LinuxStreams);
        let front = FrontStreams {
            cfg: client
                .open_stream_writable(fe_index, "config", platform.clone())
                .map_err(|e| format!("front end config stream: {e}"))?,
            stats: client
                .open_stream(fe_index, "statistics", platform.clone())
                .map_err(|e| format!("front end statistics stream: {e}"))?,
            out: client
                .open_stream(fe_index, "output0", platform.clone())
                .map_err(|e| format!("front end output0 stream: {e}"))?,
        };
        let back = BackStreams {
            cfg: client
                .open_stream_writable(be_index, "config", platform.clone())
                .map_err(|e| format!("back end config stream: {e}"))?,
            input: client
                .open_stream_writable(be_index, "input", platform.clone())
                .ok(),
            out: client
                .open_stream(be_index, "output0", platform)
                .map_err(|e| format!("back end output0 stream: {e}"))?,
        };
        let mut chain = Self {
            sensor_name: sensor_name.to_string(),
            receiver_name: receiver_name.to_string(),
            front_name: front_name.to_string(),
            back_name: back_name.to_string(),
            sensor,
            receiver,
            fe,
            be,
            mode,
            front,
            back,
            fe_stride: fe_layout.stride,
            fe_size: fe_layout.size,
            input_stride: u32::try_from(input.stride).unwrap_or(0),
            be_stride: be_layout.stride,
            be_size: be_layout.size,
            be_tiles: be_config.num_tiles,
        };
        // Every buffer of each configuration stream holds the configuration: a job names one of them.
        for buffer in 0..chain.front.out.layout().buffers {
            chain
                .front
                .cfg
                .write(buffer, 0, &fe_bytes)
                .map_err(|e| format!("front end config write: {e:?}"))?;
        }
        for buffer in 0..chain.back.cfg.layout().buffers {
            chain
                .back
                .cfg
                .write(buffer, 0, &be_bytes)
                .map_err(|e| format!("back end config write: {e:?}"))?;
        }
        Ok(chain)
    }

    /// Starts the back end, the front end and the sensor, in that order.
    ///
    /// # Errors
    /// A stage refuses its start. The message names which.
    pub fn start(&mut self) -> Result<(), String> {
        self.be
            .start()
            .map_err(|s| format!("back end start: {s:?}"))?;
        self.fe
            .start()
            .map_err(|s| format!("front end start: {s:?}"))?;
        self.sensor
            .start()
            .map_err(|s| format!("sensor start: {s:?}"))?;
        Ok(())
    }
}
