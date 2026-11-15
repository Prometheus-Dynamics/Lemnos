//! The boot ROM stage, against a scripted device: its stage, loading an image, starting it, and the
//! start's outcome settled by observation. The address and the image are the caller's; the two
//! addresses below are the ones the vendor tool's notes record, passed in as data.

mod common;

use common::*;
use lemnos_k230_usbboot::UsbError;
use lemnos_k230_usbboot::bootrom::{self, DOWNLOAD_PID, DOWNLOAD_VID, START_WAIT_MS, Stage};
use lemnos_k230_usbboot::packet::STAGE_IDENT;
use lemnos_k230_usbboot::sim::{Answer, ScriptedIo, Transfer};

/// The two load addresses the vendor notes record (0x80360000 and 0x80300000).
const ADDRESSES: [u32; 2] = [0x8036_0000, 0x8030_0000];

/// The answers of a load of `pages` pages: the address, each page, then `start`.
fn load_answers(pages: usize, start: Answer) -> Vec<Answer> {
    let mut answers = vec![Answer::Ok];
    answers.extend(std::iter::repeat_n(Answer::Ok, pages));
    answers.push(start);
    answers
}

fn download() -> Result<(u16, u16), UsbError> {
    Ok((DOWNLOAD_VID, DOWNLOAD_PID))
}

#[test]
fn the_stage_is_told_by_its_cpu_info() {
    let mut io = ScriptedIo::new([
        cpu_info("K230"),
        cpu_info("Uboot Stage for K230"),
        cpu_info(STAGE_IDENT),
        cpu_info("something else"),
    ]);
    assert_eq!(bootrom::stage(&mut io).unwrap(), Stage::BootRom);
    assert_eq!(bootrom::stage(&mut io).unwrap(), Stage::Burner);
    assert_eq!(bootrom::stage(&mut io).unwrap(), Stage::Lemnos);
    assert_eq!(
        bootrom::stage(&mut io).unwrap(),
        Stage::Unknown("something else".into())
    );
}

#[test]
fn an_image_goes_in_pages_then_the_start_then_the_wait_then_the_identity_probe() {
    for address in ADDRESSES {
        let image: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let mut answers = load_answers(3, Answer::Disconnect);
        answers.push(cpu_info(STAGE_IDENT));
        let mut io = ScriptedIo::new(answers).reattach(download());
        bootrom::load_and_start(&mut io, &image, address).unwrap();

        let (high, low) = ((address >> 16) as u16, (address & 0xffff) as u16);
        assert_eq!(
            io.records().to_vec(),
            vec![
                Transfer::ControlOut {
                    request: 1,
                    value: high,
                    index: low,
                    data: Vec::new()
                },
                Transfer::BulkOut(image[..1000].to_vec()),
                Transfer::BulkOut(image[1000..2000].to_vec()),
                Transfer::BulkOut(image[2000..].to_vec()),
                Transfer::ControlOut {
                    request: 4,
                    value: high,
                    index: low,
                    data: Vec::new()
                },
                Transfer::WaitForDevice(START_WAIT_MS),
                Transfer::ControlIn {
                    request: 0,
                    value: 0,
                    index: 0,
                    len: 32
                },
            ],
            "{address:#010x}"
        );
    }
}

#[test]
fn a_start_that_disconnects_and_a_stage_that_comes_back_is_success() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Disconnect)
            .into_iter()
            .chain([cpu_info(STAGE_IDENT)]),
    )
    .reattach(download());
    bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap();
}

#[test]
fn a_start_that_returns_normally_and_a_stage_that_comes_back_is_success_too() {
    // The rule holds for a normal return: the start's result is not what decides.
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Ok)
            .into_iter()
            .chain([cpu_info(STAGE_IDENT)]),
    )
    .reattach(download());
    bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap();
}

#[test]
fn a_start_that_stalls_is_settled_by_the_device_not_by_the_stall() {
    // Real-failing: the stall alone is not taken as failure, and the observation finds the ROM again.
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Stall)
            .into_iter()
            .chain([cpu_info("K230")]),
    )
    .reattach(download());
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("answered again"), "{error}");
}

#[test]
fn no_re_enumeration_before_the_deadline_is_a_timeout_naming_the_wait() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(load_answers(1, Answer::Disconnect))
        .reattach(Err(UsbError::Timeout(START_WAIT_MS)));
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert_eq!(error, UsbError::Timeout(START_WAIT_MS));
}

#[test]
fn a_device_back_with_other_ids_is_refused_by_its_ids() {
    let image = vec![7u8; 10];
    let mut io =
        ScriptedIo::new(load_answers(1, Answer::Disconnect)).reattach(Ok((0x1234, DOWNLOAD_PID)));
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("1234:0230"), "{error}");
}

#[test]
fn the_boot_rom_answering_again_is_a_failure_that_says_the_image_did_not_run() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Disconnect)
            .into_iter()
            .chain([cpu_info("K230")]),
    )
    .reattach(download());
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("answered again"), "{error}");
}

#[test]
fn the_vendor_burner_answering_is_a_failure_naming_the_wrong_image() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Disconnect)
            .into_iter()
            .chain([cpu_info("Uboot Stage for K230")]),
    )
    .reattach(download());
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("vendor burner"), "{error}");
}

#[test]
fn an_identity_text_that_is_not_the_stages_is_a_failure_naming_the_text() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Disconnect)
            .into_iter()
            .chain([cpu_info("something else")]),
    )
    .reattach(download());
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("identity"), "{error}");
    assert!(error.to_string().contains("something else"), "{error}");
}

#[test]
fn a_device_that_leaves_the_bus_again_after_it_re_enumerates_is_a_failure() {
    let image = vec![7u8; 10];
    let mut io = ScriptedIo::new(
        load_answers(1, Answer::Disconnect)
            .into_iter()
            .chain([Answer::Disconnect]),
    )
    .reattach(download());
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert!(error.to_string().contains("left the bus again"), "{error}");
}

#[test]
fn a_reset_mid_upload_is_an_error_and_no_start_is_sent() {
    let image = vec![1u8; 2500];
    let mut io = ScriptedIo::new([Answer::Ok, Answer::Ok, Answer::Disconnect]);
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[1]).unwrap_err();
    assert_eq!(error, UsbError::Disconnected);
    assert!(io.is_gone());
}

#[test]
fn a_stalled_page_stops_the_load_with_the_stall() {
    let image = vec![2u8; 1500];
    let mut io = ScriptedIo::new([Answer::Ok, Answer::Stall]);
    let error = bootrom::load_and_start(&mut io, &image, ADDRESSES[0]).unwrap_err();
    assert_eq!(error, UsbError::Stall);
    assert_eq!(io.records().len(), 2, "no page after the stall, no start");
}

#[test]
fn an_address_outside_the_range_or_an_empty_image_is_refused_before_any_transfer() {
    let mut io = ScriptedIo::new([]);
    let outside = bootrom::load_and_start(&mut io, &[1u8; 4], 0x8000_0000).unwrap_err();
    assert!(outside.to_string().contains("outside"), "{outside}");
    let empty = bootrom::load_and_start(&mut io, &[], ADDRESSES[0]).unwrap_err();
    assert!(empty.to_string().contains("no image"), "{empty}");
    assert!(io.records().is_empty());
}
