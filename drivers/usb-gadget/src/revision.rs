//! The bcdDevice policy: the release a gadget presents, tied to what the link presents.
//!
//! Windows keeps the state of a device's OS descriptors per vendor, product and release (the
//! `usbflags` key). A device that enumerated once with other descriptors under the same release
//! keeps the stale answer, and no driver install clears it. So every change to what the link
//! presents takes a new release, and this module makes forgetting one a test failure.
//!
//! What the link presents, for this purpose, is its interfaces and endpoints (the configuration
//! of the descriptor set, which the FunctionFS function's interface lives in), the function kinds
//! of the setup (FunctionFS, NCM, ACM, in order), and the Microsoft OS 1.0 constants of
//! [`ms_os`]. Not the board's strings, its vendor and product numbers (which are already part of
//! the cache key), or the NCM addresses and interface name.
//!
//! [`REVISIONS`] lists each release with the [`fingerprint`] of the link it presented. The default
//! release is the last one. The test fails when the fingerprint of the current link differs from
//! the last entry: then a new entry with a higher release and the new fingerprint is appended.

use lemnos_hal::usb_descriptors::{GadgetFunction, ms_os};
use sha2::{Digest, Sha256};

/// Each release with its fingerprint, oldest first. The first entry is the release every gadget
/// presented before the link's OS descriptors existed; its fingerprint is not recomputed.
pub const REVISIONS: &[(u16, [u8; 8])] = &[
    (0x0100, [0; 8]),
    (0x0200, [130, 157, 21, 33, 61, 119, 189, 109]),
];

/// The bcdDevice range reserved to the managed identity (vendor 0x1d6b, product 0x0104): every
/// release it presents is in `RESERVED_MIN..=RESERVED_MAX`, and the first release of the other
/// identity (0x1d6b:0103, another team's bring-up images) is `OTHER_MIN`. The file
/// `contracts/usb-link.toml` states the same ranges; a test checks they agree.
pub const RESERVED_MIN: u16 = 0x0100;
/// See [`RESERVED_MIN`].
pub const RESERVED_MAX: u16 = 0x0aff;
/// See [`RESERVED_MIN`]: the first release of the other identity.
pub const OTHER_MIN: u16 = 0x0b00;

/// The release a gadget presents when its board gives none: the last revision.
pub const RELEASE: u16 = REVISIONS[REVISIONS.len() - 1].0;

/// The fingerprint of the link `set` (a full descriptor set, as [`crate::description`] builds it)
/// and `functions` present. The device descriptor is not read: its `bcdDevice` is what this
/// names, and its vendor and product are in the cache key already.
pub fn fingerprint(set: &[u8], functions: &[GadgetFunction]) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"lemnos link revision 1\0");
    for text in [ms_os::LINK_GUID, ms_os::LINK_GUID_PROPERTY, ms_os::QW_SIGN] {
        hasher.update(text.as_bytes());
        hasher.update([0]);
    }
    hasher.update([ms_os::VENDOR_CODE]);
    hasher.update(ms_os::LINK_COMPATIBLE_ID);
    hasher.update(ms_os::NCM_COMPATIBLE_ID);
    for function in functions {
        hasher.update([match function {
            GadgetFunction::Ffs => 1,
            GadgetFunction::Ncm { .. } => 2,
            GadgetFunction::Acm => 3,
        }]);
    }
    // The full-speed configuration: `wTotalLength` is at 20..22 of the device's 18 bytes plus
    // the configuration's 2, and its interfaces and endpoints come after its 9-byte header.
    let total = set
        .get(20..22)
        .map_or(0, |len| usize::from(u16::from_le_bytes([len[0], len[1]])));
    hasher.update(set.get(27..(18 + total).min(set.len())).unwrap_or(&[]));
    let digest = hasher.finalize();
    let mut out = [0; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::description::{Identity, build};
    use lemnos_hal::usb_descriptors::UsbDescriptors;

    const INTERFACES: &str = "function ffs\ninterface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\nfunction ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\nfunction acm\n";

    fn link(interfaces: &str) -> ([u8; 8], Vec<GadgetFunction>, Vec<u8>) {
        let identity = Identity {
            vendor: crate::LINK_VENDOR,
            product: crate::LINK_PRODUCT,
            release: RELEASE,
            class: 0xef,
            subclass: 0x02,
            protocol: 0x01,
            manufacturer: None,
            product_name: None,
            serial: None,
            max_power_ma: 100,
            self_powered: false,
        };
        let set = build(&identity, interfaces).unwrap();
        let functions = crate::description::functions(interfaces, None).unwrap();
        let bytes = set.bytes().to_vec();
        UsbDescriptors::parse(&bytes).unwrap();
        (
            fingerprint(&bytes, functions.as_slice()),
            functions.as_slice().to_vec(),
            bytes,
        )
    }

    #[test]
    fn the_current_link_is_the_last_revision() {
        let (current, _, _) = link(INTERFACES);
        let (release, pinned) = REVISIONS[REVISIONS.len() - 1];
        assert_eq!(
            current, pinned,
            "the link changed: add a new REVISIONS entry with a release above {release:#06x} and fingerprint {current:?}"
        );
    }

    #[test]
    fn every_release_is_inside_the_managed_reservation() {
        // 0x0b00 and up belong to the other identity; below 0x0100 is not a release we use.
        for (release, _) in REVISIONS {
            assert!(
                (RESERVED_MIN..=RESERVED_MAX).contains(release),
                "release {release:#06x} is outside the managed range {RESERVED_MIN:#06x}..={RESERVED_MAX:#06x}"
            );
            assert!(
                *release < OTHER_MIN,
                "release {release:#06x} is at or above {OTHER_MIN:#06x}"
            );
            assert!(*release >= 0x0100, "release {release:#06x} is below 0x0100");
        }
        assert_eq!(OTHER_MIN, 0x0b00);
        assert_eq!((RESERVED_MIN, RESERVED_MAX), (0x0100, 0x0aff));
    }

    #[test]
    fn releases_increase_so_a_changed_link_takes_a_new_one() {
        assert!(REVISIONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(RELEASE, REVISIONS[REVISIONS.len() - 1].0);
    }

    #[test]
    fn every_function_list_has_the_link_whatever_the_board_names() {
        // The link is added when the board does not name it, so no gadget of ours is built
        // without it: the daemon's, the harness's and any other all go through `functions`.
        let bare = crate::description::functions(
            "interface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\n",
            None,
        )
        .unwrap();
        assert_eq!(bare.as_slice(), &[GadgetFunction::Ffs]);
        let named = crate::description::functions(INTERFACES, None).unwrap();
        assert_eq!(named.as_slice()[0], GadgetFunction::Ffs);
        assert_eq!(named.as_slice().len(), 3);
    }

    #[test]
    fn the_fingerprint_moves_with_the_link_and_not_with_the_board() {
        let (base, _, _) = link(INTERFACES);
        let (other_endpoints, _, _) = link(
            "function ffs\ninterface 0xff 0 0 link\nendpoint in interrupt 8 10\nendpoint out bulk\nfunction ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\nfunction acm\n",
        );
        assert_ne!(base, other_endpoints, "an endpoint change must move it");
        let (no_acm, _, _) = link(
            "function ffs\ninterface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\nfunction ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\n",
        );
        assert_ne!(base, no_acm, "a function change must move it");
        let (renamed, _, _) = link(&INTERFACES.replace("link", "pipe"));
        assert_eq!(base, renamed, "an interface name is not part of the link");
    }
}
