//! The layouts and request numbers written out by hand, and the requests against a file that is
//! not the subdevice (each fails cleanly, touching nothing). No device node is opened.

use core::mem::{offset_of, size_of};

use crate::*;

#[test]
fn the_buffer_sizes_are_the_kernels() {
    // The numbers `layout.rs` checks against the C compiler, as the driver sees them.
    assert_eq!(LOAD_BYTES, 30464);
    assert_eq!(SAVE_BYTES, 27136);
    assert_eq!(SAVE_LOAD_STATISTICS_BYTES, 13056);
    assert_eq!(STATISTICS_BYTES, 40192);
}

#[test]
fn a_request_number_past_255_carries_into_the_type_byte_as_in_c() {
    // `_IOWR('V', 192 + 70, struct isp_table_reg_map)`: number 262 is 0x106, so the type byte
    // reads 'W' (0x57) and the number 6. `_IOC_NR` of it is 6, and that is what the kernel sees.
    assert_eq!(VIDIOC_VIN_ISP_LOAD_REG >> 30, 3, "read and write");
    assert_eq!((VIDIOC_VIN_ISP_LOAD_REG >> 16) & 0x3fff, 16, "size");
    assert_eq!((VIDIOC_VIN_ISP_LOAD_REG >> 8) & 0xff, u32::from(b'W'));
    assert_eq!(VIDIOC_VIN_ISP_LOAD_REG & 0xff, 6);
    assert_eq!(VIDIOC_VIN_ISP_STAT_REQ & 0xff, 224);
    assert_eq!((VIDIOC_VIN_ISP_STAT_REQ >> 8) & 0xff, u32::from(b'V'));
}

#[cfg(target_pointer_width = "64")]
#[test]
fn the_pointer_structures_have_the_kernels_shape() {
    assert_eq!(size_of::<TableRegMap>(), 16);
    assert_eq!(offset_of!(TableRegMap, size), 8);
    assert_eq!(size_of::<StatData>(), 24);
    assert_eq!(offset_of!(StatData, buf_size), 8);
    assert_eq!(offset_of!(StatData, frame_number), 12);
    assert_eq!(offset_of!(StatData, config_counter), 16);
}

#[cfg(target_os = "linux")]
mod calls {
    use std::fs::File;
    use std::os::fd::AsFd;

    use lemnos_measure::test_alloc::allocations;

    use crate::call::{load_arg, received, stat_arg};
    use crate::*;

    #[global_allocator]
    static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

    #[test]
    fn each_pointer_is_the_borrowed_buffer_and_its_whole_length() {
        let image = Box::new([7u8; LOAD_BYTES]);
        let arg = load_arg(&image);
        assert_eq!(arg.addr.cast_const().cast::<u8>(), image.as_ptr());
        assert_eq!(arg.size as usize, LOAD_BYTES);

        let mut into = Box::new([0u8; STATISTICS_BYTES]);
        let arg = stat_arg(&mut into);
        assert_eq!(arg.buf.cast::<u8>(), into.as_mut_ptr());
        assert_eq!(arg.buf_size as usize, STATISTICS_BYTES);
    }

    #[test]
    fn an_answer_claiming_more_than_the_buffer_is_refused() {
        let mut into = Box::new([0u8; STATISTICS_BYTES]);
        let mut answer = stat_arg(&mut into);
        answer.buf_size = SAVE_BYTES as u32;
        answer.frame_number = 9;
        assert_eq!(
            received(&answer).unwrap(),
            Received {
                frame_number: 9,
                bytes: STATISTICS_BYTES
            }
        );
        answer.buf_size = SAVE_BYTES as u32 + 1;
        assert_eq!(
            received(&answer).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        answer.buf_size = u32::MAX;
        assert_eq!(
            received(&answer).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_file_that_is_not_the_subdevice_refuses_every_request() {
        let file = File::open("/dev/null").unwrap();
        let fd = file.as_fd();
        let image = Box::new([0u8; LOAD_BYTES]);
        let mut into = Box::new([0u8; STATISTICS_BYTES]);
        let enotty = Some(libc::ENOTTY);
        assert_eq!(load(fd, &image).unwrap_err().raw_os_error(), enotty);
        assert_eq!(configure_statistics(fd).unwrap_err().raw_os_error(), enotty);
        assert_eq!(
            enable_statistics(fd, true).unwrap_err().raw_os_error(),
            enotty
        );
        assert_eq!(
            request_statistics(fd, &mut into)
                .unwrap_err()
                .raw_os_error(),
            enotty
        );
        assert!(into.iter().all(|&b| b == 0), "nothing written");
    }

    #[test]
    fn a_request_allocates_nothing() {
        let file = File::open("/dev/null").unwrap();
        let image = Box::new([0u8; LOAD_BYTES]);
        let mut into = Box::new([0u8; STATISTICS_BYTES]);
        let ((), allocated) = allocations(|| {
            for _ in 0..100 {
                let _ = load(file.as_fd(), &image);
                let _ = request_statistics(file.as_fd(), &mut into);
            }
        });
        assert_eq!(allocated, 0);
    }
}
