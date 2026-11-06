//! Conversions between the session's formats (`lemnos-v4l2`'s) and the sys
//! calls' (`lemnos_linux_sys::v4l2`'s). The sys calls take plain, copyable
//! structures with a fixed plane array; the session keeps `lemnos-v4l2`'s
//! `FourCc` and plane list. The fields that only `lemnos-v4l2` carries (flags,
//! encodings, quantization, transfer function) are not sent to the kernel by
//! the sys calls, so they read back as zero.

use std::io;

use lemnos_linux_sys::v4l2 as sys;
use lemnos_v4l2::FourCc;
use lemnos_v4l2::v4l2::{Format, PixFormat, PixFormatMplane, PlaneFormat};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The number of memory planes a format's buffers have.
pub(super) fn plane_count(format: &Format) -> usize {
    match format {
        Format::Single(_) | Format::Meta(_) => 1,
        Format::Multi(m) => m.planes.len(),
    }
}

/// Converts a session format for a sys call. Metadata formats are not sessions.
pub(super) fn to_sys(format: &Format) -> io::Result<sys::Format> {
    match format {
        Format::Single(p) => Ok(sys::Format::Single(sys::PixFormat {
            width: p.width,
            height: p.height,
            pixelformat: p.fourcc.0,
            field: p.field,
            bytes_per_line: p.bytes_per_line,
            size_image: p.size_image,
            colorspace: p.colorspace,
        })),
        Format::Multi(m) => {
            if !(1..=sys::MAX_PLANES).contains(&m.planes.len()) {
                return Err(invalid("a multi-planar format has 1 to 8 planes"));
            }
            let mut planes = [sys::PlaneFormat::default(); sys::MAX_PLANES];
            for (dst, src) in planes.iter_mut().zip(&m.planes) {
                *dst = sys::PlaneFormat {
                    size_image: src.size_image,
                    bytes_per_line: src.bytes_per_line,
                };
            }
            Ok(sys::Format::Multi(sys::PixFormatMplane {
                width: m.width,
                height: m.height,
                pixelformat: m.fourcc.0,
                field: m.field,
                colorspace: m.colorspace,
                planes,
                num_planes: m.planes.len() as u32,
            }))
        }
        Format::Meta(_) => Err(invalid("a metadata format is not a session's format")),
    }
}

/// Converts a format the sys calls returned.
pub(super) fn from_sys(format: sys::Format) -> Format {
    match format {
        sys::Format::Single(p) => Format::Single(PixFormat {
            width: p.width,
            height: p.height,
            fourcc: FourCc(p.pixelformat),
            field: p.field,
            bytes_per_line: p.bytes_per_line,
            size_image: p.size_image,
            colorspace: p.colorspace,
            ..PixFormat::default()
        }),
        sys::Format::Multi(m) => {
            let n = (m.num_planes as usize).min(sys::MAX_PLANES);
            Format::Multi(PixFormatMplane {
                width: m.width,
                height: m.height,
                fourcc: FourCc(m.pixelformat),
                field: m.field,
                colorspace: m.colorspace,
                planes: m.planes[..n]
                    .iter()
                    .map(|p| PlaneFormat {
                        bytes_per_line: p.bytes_per_line,
                        size_image: p.size_image,
                    })
                    .collect(),
                ..PixFormatMplane::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_multi_planar_formats_round_trip() {
        let single = Format::Single(PixFormat {
            width: 64,
            height: 48,
            fourcc: FourCc::new(b"RGB3"),
            field: 1,
            bytes_per_line: 192,
            size_image: 9216,
            colorspace: 4,
            ..PixFormat::default()
        });
        let back = from_sys(to_sys(&single).unwrap());
        assert_eq!(back, single);

        let multi = Format::Multi(PixFormatMplane {
            width: 640,
            height: 480,
            fourcc: FourCc::new(b"NV12"),
            field: 1,
            colorspace: 4,
            planes: vec![
                PlaneFormat {
                    bytes_per_line: 640,
                    size_image: 307_200,
                },
                PlaneFormat {
                    bytes_per_line: 640,
                    size_image: 153_600,
                },
            ],
            ..PixFormatMplane::default()
        });
        let back = from_sys(to_sys(&multi).unwrap());
        assert_eq!(back, multi);
        assert_eq!(plane_count(&back), 2);
    }

    #[test]
    fn metadata_and_plane_counts_out_of_range_are_refused() {
        let meta = Format::Meta(lemnos_v4l2::v4l2::MetaFormat::default());
        assert!(to_sys(&meta).is_err());
        let none = Format::Multi(PixFormatMplane::default());
        assert!(to_sys(&none).is_err());
        let nine = Format::Multi(PixFormatMplane {
            planes: vec![PlaneFormat::default(); 9],
            ..PixFormatMplane::default()
        });
        assert!(to_sys(&nine).is_err());
    }
}
