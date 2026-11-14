//! The receiver stand-in is the only simulated driver; the PiSP drivers are the real ones. It
//! must declare what the real receiver declares, or a proof that passes against it could pass
//! against a chain no board runs.

use lemnos_camera_sim::SimReceiver;
use lemnos_core::Driver;
use lemnos_driver_rp1_csi2::Rp1Csi2;

#[test]
fn the_receiver_stand_in_declares_what_the_real_receiver_declares() {
    // Driver name, every need (name, kind, optional, stream kind) and every contract it provides.
    assert_eq!(SimReceiver::MANIFEST, Rp1Csi2::MANIFEST);
}

#[test]
fn every_pixel_of_the_reference_frame_is_the_frame_the_front_end_writes() {
    use lemnos_camera_sim::scene::{HEIGHT, RAW10_STRIDE, WIDTH, frame16, pixel, reference_raw10};
    let raw = reference_raw10();
    assert_eq!(raw.len(), RAW10_STRIDE * HEIGHT as usize);
    let wide = frame16(WIDTH as usize * 2, WIDTH as usize * 2 * HEIGHT as usize);
    for (y, x) in [(0u32, 0u32), (0, 5), (7, 1279), (799, 640), (400, 63)] {
        let group = (y as usize * RAW10_STRIDE) + (x as usize / 4) * 5;
        let low = raw[group + 4] >> (2 * (x % 4)) & 3;
        let ten = u16::from(raw[group + (x as usize % 4)]) << 2 | u16::from(low);
        assert_eq!(ten, pixel(x, y));
        let at = (y as usize * WIDTH as usize + x as usize) * 2;
        assert_eq!(u16::from_le_bytes([wide[at], wide[at + 1]]), ten << 6);
    }
}
