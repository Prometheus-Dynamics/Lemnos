//! An addressable LED strip on the Raspberry Pi RP1 `ws2812-pio` character
//! device (`/dev/ledsN`), as a device-model light.

use crate::SysfsError;
use lemnos_device::{
    Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, Pixels, Quantity, Rgbw,
    check_control,
};
use lemnos_drivers_ws2812::{StripConfig, encode_rp1};
use lemnos_hal::ErrorKind;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// Control indices of [`LIGHT_INFO`].
pub const CONTROL_BRIGHTNESS: usize = 0;
pub const CONTROL_COLOR: usize = 1;

/// A light: controls `brightness` (0..=1000 ‰, applied to every frame) and
/// `color` (`0xRRGGBB` on every LED); frames through `Pixels::show`.
pub static LIGHT_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Light,
    "ws2812",
    &[],
    &[
        ControlInfo::new("brightness", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("color", Quantity::Color, 0, 0, 0xff_ffff),
    ],
);

/// A WS2812/SK6812 strip behind the RP1 `ws2812-pio` driver. The kernel
/// driver gamma-corrects and sends GRB or GRBW (its `rgbw` overlay
/// parameter); this side writes the 4-byte-per-LED layout with the strip's
/// index offset and brightness. Frames are encoded into a buffer allocated
/// once, so showing a frame does not allocate.
#[derive(Debug)]
pub struct Ws2812Pio {
    path: PathBuf,
    config: StripConfig,
    file: Option<File>,
    buffer: Vec<u8>,
    color: u32,
}

impl Ws2812Pio {
    pub fn new(path: impl Into<PathBuf>, config: StripConfig) -> Self {
        Self {
            path: path.into(),
            buffer: vec![0; config.rp1_len()],
            config,
            file: None,
            color: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> &StripConfig {
        &self.config
    }

    fn open(&mut self) -> Result<&File, SysfsError> {
        if self.file.is_none() {
            let file = OpenOptions::new()
                .write(true)
                .open(&self.path)
                .map_err(|e| SysfsError::io(&self.path, "open", &e))?;
            self.file = Some(file);
        }
        Ok(self.file.as_ref().expect("opened above"))
    }

    /// Writes a frame (logical order).
    pub fn write_frame(&mut self, pixels: &[Rgbw]) -> Result<(), SysfsError> {
        let config = self.config;
        let len = encode_rp1(&config, pixels, &mut self.buffer);
        if len == 1 {
            // A one-byte write sets the driver's brightness instead.
            return Err(SysfsError::new(
                ErrorKind::Configuration,
                &self.path,
                "a strip needs at least one 4-byte LED",
            ));
        }
        let path = self.path.clone();
        let buffer = std::mem::take(&mut self.buffer);
        let result = self.open().and_then(|file| {
            file.write_all_at(&buffer[..len], 0)
                .map_err(|e| SysfsError::io(&path, "write", &e))
        });
        self.buffer = buffer;
        if result.is_err() {
            // Reopen next time: the device may have been re-probed.
            self.file = None;
        }
        result
    }
}

impl Device for Ws2812Pio {
    type Error = SysfsError;

    fn info(&self) -> &'static DeviceInfo {
        &LIGHT_INFO
    }

    /// Opens the device and turns the strip off.
    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<SysfsError>> {
        self.write_frame(&[]).map_err(DeviceError::Driver)
    }
}

impl Control for Ws2812Pio {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<SysfsError>> {
        check_control(&LIGHT_INFO, index, value)?;
        if index == CONTROL_BRIGHTNESS {
            self.config.brightness = ((value * 255 + 500) / 1000) as u8;
            Ok(value)
        } else {
            self.color = value as u32;
            let fill = [Rgbw::rgb(self.color); 1];
            let count = usize::from(self.config.count);
            let frame: Vec<Rgbw> = fill.iter().copied().cycle().take(count).collect();
            self.write_frame(&frame).map_err(DeviceError::Driver)?;
            Ok(value)
        }
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<SysfsError>> {
        check_control::<SysfsError>(&LIGHT_INFO, index, 0)?;
        Ok(if index == CONTROL_BRIGHTNESS {
            (i32::from(self.config.brightness) * 1000 + 127) / 255
        } else {
            self.color as i32
        })
    }
}

impl Pixels for Ws2812Pio {
    fn pixel_count(&self) -> usize {
        usize::from(self.config.count)
    }

    fn show(&mut self, pixels: &[Rgbw]) -> Result<(), DeviceError<SysfsError>> {
        self.write_frame(pixels).map_err(DeviceError::Driver)
    }
}
