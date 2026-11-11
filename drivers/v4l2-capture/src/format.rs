//! The pixel formats a raw capture node names its frames by.

use lemnos_contract_camera_receiver::BayerOrder;
use lemnos_v4l2::FourCc;

/// The node's pixel format for raw frames of `depth` bits in `order` (`videodev2.h`).
#[must_use]
pub const fn fourcc(order: BayerOrder, depth: u8) -> Option<FourCc> {
    let code: &[u8; 4] = match (depth, order) {
        (8, BayerOrder::Mono) => b"GREY",
        (8, BayerOrder::Bggr) => b"BA81",
        (8, BayerOrder::Gbrg) => b"GBRG",
        (8, BayerOrder::Grbg) => b"GRBG",
        (8, BayerOrder::Rggb) => b"RGGB",
        (10, BayerOrder::Bggr) => b"BG10",
        (10, BayerOrder::Gbrg) => b"GB10",
        (10, BayerOrder::Grbg) => b"BA10",
        (10, BayerOrder::Rggb) => b"RG10",
        (12, BayerOrder::Bggr) => b"BG12",
        (12, BayerOrder::Gbrg) => b"GB12",
        (12, BayerOrder::Grbg) => b"BA12",
        (12, BayerOrder::Rggb) => b"RG12",
        (14, BayerOrder::Bggr) => b"BG14",
        (14, BayerOrder::Gbrg) => b"GB14",
        (14, BayerOrder::Grbg) => b"GR14",
        (14, BayerOrder::Rggb) => b"RG14",
        (16, BayerOrder::Bggr) => b"BYR2",
        (16, BayerOrder::Gbrg) => b"GB16",
        (16, BayerOrder::Grbg) => b"GR16",
        (16, BayerOrder::Rggb) => b"RG16",
        _ => return None,
    };
    Some(FourCc::new(code))
}

/// `order` from its board file spelling.
#[must_use]
pub fn parse_order(order: &str) -> Option<BayerOrder> {
    Some(match order {
        "mono" => BayerOrder::Mono,
        "rggb" => BayerOrder::Rggb,
        "grbg" => BayerOrder::Grbg,
        "gbrg" => BayerOrder::Gbrg,
        "bggr" => BayerOrder::Bggr,
        _ => return None,
    })
}
