//! A job for the block, built only from images checked against the dma-bufs they sit in.
//!
//! The block reads and writes memory at the addresses it is given, with nothing between it and
//! the rest of memory. So a [`Job`] names only addresses the kernel answered for dma-bufs this
//! driver imported, and for each image the bytes it covers, which were checked to lie inside that
//! dma-buf by the size the kernel answers for it. The two images never overlap. Whatever programs
//! the block from a job must keep it inside [`Image::address`] and [`Image::bytes`].

use core::fmt;

use lemnos_contract_image_scaler::Filter;

/// A pixel format the block is driven with: one plane of `bytes_per_pixel` bytes a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// The little-endian four-character code a job names it by.
    pub fourcc: u32,
    /// Bytes a pixel takes.
    pub bytes_per_pixel: u32,
}

/// The largest image a job may have, input or output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Widest, in pixels.
    pub max_width: u32,
    /// Tallest, in pixels.
    pub max_height: u32,
}

/// Why an image or a job is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// A format the block isn't driven with, two formats in one job, or a filter it doesn't
    /// offer.
    Format,
    /// An empty image, or one past the limits.
    Size,
    /// A stride narrower than the image's row.
    Stride,
    /// The image needs more bytes than its dma-buf holds.
    Extent {
        /// Bytes the image covers.
        need: u64,
        /// The dma-buf's size.
        have: u64,
    },
    /// The image's bytes run past the end of the address space.
    Address,
    /// The input and the output overlap.
    Overlap,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format => f.write_str("a format or filter the block isn't driven with"),
            Self::Size => f.write_str("an empty image or one past the limits"),
            Self::Stride => f.write_str("a stride narrower than the row"),
            Self::Extent { need, have } => {
                write!(
                    f,
                    "the image covers {need} bytes and its dma-buf holds {have}"
                )
            }
            Self::Address => f.write_str("the image runs past the end of the address space"),
            Self::Overlap => f.write_str("the input and the output overlap"),
        }
    }
}

impl std::error::Error for JobError {}

/// An image as a caller describes it, before any memory is involved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per line.
    pub stride: u32,
    /// The pixel format.
    pub format: Format,
}

impl Shape {
    /// The bytes the image covers: every line at its full stride. Checks the size against
    /// `limits` and the stride against the row; overflow-safe.
    pub fn bytes(&self, limits: Limits) -> Result<u64, JobError> {
        let sized = (1..=limits.max_width).contains(&self.width)
            && (1..=limits.max_height).contains(&self.height);
        if !sized {
            return Err(JobError::Size);
        }
        let row = u64::from(self.width) * u64::from(self.format.bytes_per_pixel);
        if self.format.bytes_per_pixel == 0 || u64::from(self.stride) < row {
            return Err(JobError::Stride);
        }
        // Two 32-bit factors: no overflow in 64 bits.
        Ok(u64::from(self.stride) * u64::from(self.height))
    }

    /// Checks the image fits a dma-buf of `size` bytes, before anything is imported.
    pub fn fits(&self, limits: Limits, size: u64) -> Result<u64, JobError> {
        let need = self.bytes(limits)?;
        if need > size {
            return Err(JobError::Extent { need, have: size });
        }
        Ok(need)
    }
}

/// A dma-buf the block can reach: the address the kernel answered when it was imported, and its
/// size as the kernel answers it. Made only by this crate, from a real import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Imported {
    address: u64,
    size: u64,
}

impl Imported {
    pub(crate) fn new(address: u64, size: u64) -> Self {
        Self { address, size }
    }
}

/// An image in an imported dma-buf, checked to lie inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    shape: Shape,
    address: u64,
    bytes: u64,
}

impl Image {
    /// `shape` at the start of `memory`, if it fits.
    pub fn new(memory: Imported, shape: Shape, limits: Limits) -> Result<Self, JobError> {
        let bytes = shape.fits(limits, memory.size)?;
        if memory.address.checked_add(bytes).is_none() {
            return Err(JobError::Address);
        }
        Ok(Self {
            shape,
            address: memory.address,
            bytes,
        })
    }

    /// The image's shape.
    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Where the image starts, as the block addresses it.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// The bytes from [`address`](Self::address) the image covers; the block may touch no
    /// others for it.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    fn end(&self) -> u64 {
        // Checked in `new`.
        self.address + self.bytes
    }
}

/// One resize: `input` read, `output` written, in one format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    input: Image,
    output: Image,
    filter: Filter,
}

impl Job {
    /// A job, if the images share a format and don't overlap.
    pub fn new(input: Image, output: Image, filter: Filter) -> Result<Self, JobError> {
        if input.shape.format != output.shape.format {
            return Err(JobError::Format);
        }
        if input.address < output.end() && output.address < input.end() {
            return Err(JobError::Overlap);
        }
        Ok(Self {
            input,
            output,
            filter,
        })
    }

    /// The image read.
    pub fn input(&self) -> &Image {
        &self.input
    }

    /// The image written.
    pub fn output(&self) -> &Image {
        &self.output
    }

    /// How output pixels are chosen.
    pub fn filter(&self) -> Filter {
        self.filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGB: Format = Format {
        fourcc: u32::from_le_bytes(*b"RG24"),
        bytes_per_pixel: 3,
    };
    const LIMITS: Limits = Limits {
        max_width: 4096,
        max_height: 4096,
    };

    fn shape(width: u32, height: u32, stride: u32) -> Shape {
        Shape {
            width,
            height,
            stride,
            format: RGB,
        }
    }

    #[test]
    fn an_image_covers_every_line_at_its_stride() {
        assert_eq!(shape(10, 4, 30).bytes(LIMITS), Ok(120));
        assert_eq!(shape(10, 4, 32).bytes(LIMITS), Ok(128));
        assert_eq!(shape(10, 4, 29).bytes(LIMITS), Err(JobError::Stride));
        let zero = Shape {
            format: Format {
                bytes_per_pixel: 0,
                ..RGB
            },
            ..shape(10, 4, 30)
        };
        assert_eq!(zero.bytes(LIMITS), Err(JobError::Stride));
        assert_eq!(shape(0, 4, 30).bytes(LIMITS), Err(JobError::Size));
        assert_eq!(shape(10, 0, 30).bytes(LIMITS), Err(JobError::Size));
        assert_eq!(shape(4097, 4, u32::MAX).bytes(LIMITS), Err(JobError::Size));
        // The largest numbers a job can carry don't overflow.
        let wide = Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
        };
        assert_eq!(
            shape(u32::MAX / 3, u32::MAX, u32::MAX).bytes(wide),
            Ok(u64::from(u32::MAX) * u64::from(u32::MAX))
        );
    }

    #[test]
    fn an_image_must_lie_inside_its_dma_buf() {
        let memory = |size| Imported::new(0x1000_0000, size);
        assert!(Image::new(memory(120), shape(10, 4, 30), LIMITS).is_ok());
        assert_eq!(
            Image::new(memory(119), shape(10, 4, 30), LIMITS),
            Err(JobError::Extent {
                need: 120,
                have: 119
            })
        );
        // A padded stride counts on every line.
        assert!(Image::new(memory(120), shape(10, 4, 31), LIMITS).is_err());
        let image = Image::new(memory(4096), shape(10, 4, 30), LIMITS).expect("image");
        assert_eq!((image.address(), image.bytes()), (0x1000_0000, 120));
        let top = Imported::new(u64::MAX - 100, 4096);
        assert_eq!(
            Image::new(top, shape(10, 4, 30), LIMITS),
            Err(JobError::Address)
        );
    }

    #[test]
    fn a_job_keeps_one_format_and_two_separate_images() {
        let at = |address, w, h| {
            Image::new(Imported::new(address, 1 << 20), shape(w, h, w * 3), LIMITS).expect("image")
        };
        let input = at(0x10_0000, 64, 64);
        assert!(Job::new(input, at(0x20_0000, 32, 32), Filter::Bilinear).is_ok());
        // The output's first byte is the input's last.
        let last = 0x10_0000 + 64 * 64 * 3 - 1;
        assert_eq!(
            Job::new(input, at(last, 32, 32), Filter::Bilinear),
            Err(JobError::Overlap)
        );
        // Right after it is fine, and so is right before.
        assert!(Job::new(input, at(last + 1, 32, 32), Filter::Bilinear).is_ok());
        assert!(Job::new(input, at(0x10_0000 - 32 * 32 * 3, 32, 32), Filter::Area).is_ok());
        assert_eq!(
            Job::new(input, at(0x10_0000 - 32 * 32 * 3 + 1, 32, 32), Filter::Area),
            Err(JobError::Overlap)
        );
        let other = Image::new(
            Imported::new(0x40_0000, 1 << 20),
            Shape {
                format: Format {
                    fourcc: u32::from_le_bytes(*b"AR24"),
                    bytes_per_pixel: 4,
                },
                ..shape(8, 8, 32)
            },
            LIMITS,
        )
        .expect("image");
        assert_eq!(Job::new(input, other, Filter::Area), Err(JobError::Format));
    }
}
