use lemnos_flash_wire::frame::crc16_ccitt;
use lemnos_maskrom_rehearsal::face::{CHUNK, PAD, REQUEST, STAGE_471, STAGE_472, Stall};
use lemnos_maskrom_rehearsal::{
    Behaviour, Observed, Refused, Stand, Stub, WDT_471_US, face::RomFace,
};

/// The 471 image the tests send: its first bytes say what the model runs.
const RETURN_471: &[u8] = b"STUB-RETURN";
const SPIN_471: &[u8] = b"STUB-SPIN--";
const USBPLUG_472: &[u8] = b"USBPLUG-IMAGE";

fn classify(image: &[u8]) -> Option<Stub> {
    if image.starts_with(RETURN_471) {
        Some(Stub::ArmAndReturn)
    } else if image.starts_with(SPIN_471) {
        Some(Stub::ArmAndSpin)
    } else {
        None
    }
}

/// The host's side of a stage, written from the same cited framing: the payload, a pad byte when it is 4095 bytes into
/// its last chunk, the CRC-16 big-endian, and a lone pad request when it is 4094 bytes into its last chunk. Returns
/// the requests in order, each as its bytes.
fn host_requests(payload: &[u8]) -> Vec<Vec<u8>> {
    let mut wire = payload.to_vec();
    let mut lone_pad = false;
    match payload.len() % CHUNK {
        4095 => wire.push(PAD),
        4094 => lone_pad = true,
        _ => {}
    }
    let crc = crc16_ccitt(&wire).to_be_bytes();
    wire.extend_from_slice(&crc);
    let mut requests: Vec<Vec<u8>> = wire.chunks(CHUNK).map(<[u8]>::to_vec).collect();
    if lone_pad {
        requests.push(vec![PAD]);
    }
    requests
}

/// Sends a stage's requests through the face. The first refusal ends the stage, as a stall does.
fn send(face: &mut RomFace<'_>, index: u16, requests: &[Vec<u8>]) -> Result<(), Stall> {
    for request in requests {
        face.control_out(REQUEST, 0, index, request)?;
    }
    Ok(())
}

fn maskrom_stand(behaviour: Behaviour) -> Stand {
    let mut stand = Stand::from_running_system(behaviour);
    stand.enter_maskrom().unwrap();
    stand
}

#[test]
fn faithful_stages_are_accepted_at_every_boundary_length() {
    for len in [
        0usize, 1, 4093, 4094, 4095, 4096, 4097, 8190, 8191, 8192, 5000,
    ] {
        let mut stub = RETURN_471.to_vec();
        stub.resize(len.max(RETURN_471.len()), 0x5A);
        let mut stand = maskrom_stand(Behaviour::Faithful);
        let mut face = RomFace::new(&mut stand, &classify);
        send(&mut face, STAGE_471, &host_requests(&stub))
            .unwrap_or_else(|e| panic!("471 of {} bytes refused: {e:?}", stub.len()));
        send(&mut face, STAGE_472, &host_requests(USBPLUG_472))
            .unwrap_or_else(|e| panic!("472 after a 471 refused: {e:?}"));
    }
}

#[test]
fn a_spinning_471_is_accepted_and_its_watchdog_resets_the_board() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let mut face = RomFace::new(&mut stand, &classify);
        send(&mut face, STAGE_471, &host_requests(SPIN_471)).unwrap();
    }
    assert_eq!(stand.observe(WDT_471_US + 1), Observed::Reset);
    assert!(stand.booted());
}

#[test]
fn a_returning_471_keeps_the_watchdog_and_resets_inside_the_window() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let mut face = RomFace::new(&mut stand, &classify);
        send(&mut face, STAGE_471, &host_requests(RETURN_471)).unwrap();
    }
    assert_eq!(stand.observe(WDT_471_US - 1), Observed::NoReset);
    assert_eq!(stand.observe(2), Observed::Reset);
}

#[test]
fn a_bad_crc_in_the_first_chunk_is_refused_and_nothing_is_armed() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let mut requests = host_requests(&[SPIN_471, &[7u8; 5000][..]].concat());
        requests[0][10] ^= 0x40;
        let mut face = RomFace::new(&mut stand, &classify);
        assert_eq!(send(&mut face, STAGE_471, &requests), Err(Stall::Framing));
    }
    assert!(
        stand.in_maskrom(),
        "a refused stage leaves the ROM in maskrom"
    );
    assert_eq!(stand.observe(WDT_471_US * 2), Observed::NoReset);
}

#[test]
fn a_flipped_trailer_byte_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let mut requests = host_requests(SPIN_471);
        let last = requests.last_mut().unwrap();
        let n = last.len();
        last[n - 1] ^= 0x01;
        let mut face = RomFace::new(&mut stand, &classify);
        assert_eq!(send(&mut face, STAGE_471, &requests), Err(Stall::Framing));
    }
    assert!(stand.in_maskrom());
}

#[test]
fn a_lone_pad_byte_that_is_not_zero_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let payload = vec![3u8; 4094];
        let mut requests = host_requests(&payload);
        assert_eq!(
            requests.len(),
            2,
            "a 4094 stage is a full request then a lone pad"
        );
        requests[1][0] = 0x01;
        let mut face = RomFace::new(&mut stand, &classify);
        assert_eq!(send(&mut face, STAGE_471, &requests), Err(Stall::Framing));
    }
}

#[test]
fn a_wrong_chunking_of_a_4095_stage_is_refused() {
    // The host drops the pad byte that a 4095 payload needs from the wire, but keeps the CRC that covers it: the
    // CRC then runs over one byte the ROM does not receive, and the stage no longer frames.
    let mut stand = maskrom_stand(Behaviour::Faithful);
    {
        let payload = vec![9u8; 4095];
        let mut padded = payload.clone();
        padded.push(PAD);
        let mut wire = payload.clone();
        wire.extend_from_slice(&crc16_ccitt(&padded).to_be_bytes());
        let requests: Vec<Vec<u8>> = wire.chunks(CHUNK).map(<[u8]>::to_vec).collect();
        let mut face = RomFace::new(&mut stand, &classify);
        assert_eq!(send(&mut face, STAGE_471, &requests), Err(Stall::Framing));
    }
}

#[test]
fn an_oversize_request_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    assert_eq!(
        face.control_out(REQUEST, 0, STAGE_471, &vec![0u8; CHUNK + 1]),
        Err(Stall::Oversize)
    );
}

#[test]
fn a_472_before_a_471_has_returned_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    assert_eq!(
        send(&mut face, STAGE_472, &host_requests(USBPLUG_472)),
        Err(Stall::Rom(Refused::NoStubReturned))
    );
}

#[test]
fn a_472_after_a_spinning_471_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    send(&mut face, STAGE_471, &host_requests(SPIN_471)).unwrap();
    assert_eq!(
        send(&mut face, STAGE_472, &host_requests(USBPLUG_472)),
        Err(Stall::Rom(Refused::NoStubReturned))
    );
}

#[test]
fn a_refusing_rom_refuses_the_472_it_is_given() {
    let mut stand = maskrom_stand(Behaviour::Refuses472);
    let mut face = RomFace::new(&mut stand, &classify);
    send(&mut face, STAGE_471, &host_requests(RETURN_471)).unwrap();
    assert_eq!(
        send(&mut face, STAGE_472, &host_requests(USBPLUG_472)),
        Err(Stall::Rom(Refused::Rom472))
    );
}

#[test]
fn no_reset_after_a_spin_is_visible_through_the_face() {
    let mut stand = maskrom_stand(Behaviour::NoResetAfterSpin);
    {
        let mut face = RomFace::new(&mut stand, &classify);
        send(&mut face, STAGE_471, &host_requests(SPIN_471)).unwrap();
    }
    assert_eq!(stand.observe(WDT_471_US * 2), Observed::NoReset);
}

#[test]
fn a_request_after_the_usbplug_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    send(&mut face, STAGE_471, &host_requests(RETURN_471)).unwrap();
    send(&mut face, STAGE_472, &host_requests(USBPLUG_472)).unwrap();
    assert_eq!(
        send(&mut face, STAGE_471, &host_requests(RETURN_471)),
        Err(Stall::Order)
    );
}

#[test]
fn a_stage_that_starts_before_the_open_one_ends_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    face.control_out(REQUEST, 0, STAGE_471, &vec![1u8; CHUNK])
        .unwrap();
    assert_eq!(
        face.control_out(REQUEST, 0, STAGE_472, &[0u8; 4]),
        Err(Stall::Order)
    );
}

#[test]
fn a_request_that_is_not_a_download_or_names_no_stage_is_refused() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    assert_eq!(
        face.control_out(0x0d, 0, STAGE_471, &[0; 4]),
        Err(Stall::NotADownload)
    );
    assert_eq!(
        face.control_out(REQUEST, 1, STAGE_471, &[0; 4]),
        Err(Stall::NotADownload)
    );
    assert_eq!(
        face.control_out(REQUEST, 0, 0x0473, &[0; 4]),
        Err(Stall::UnknownStage)
    );
}

#[test]
fn an_unknown_471_image_is_refused_because_the_model_cannot_run_it() {
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    assert_eq!(
        send(&mut face, STAGE_471, &host_requests(b"NOT-A-STUB")),
        Err(Stall::UnknownImage)
    );
}

#[test]
fn the_old_stand_in_passes_a_corrupt_471_that_the_face_refuses() {
    // The abstract model never looked at the bytes: its 471 entry takes any stage. The face refuses the same
    // corrupt frame, so the old stand-in would pass where the ROM stops.
    let mut old = maskrom_stand(Behaviour::Faithful);
    assert_eq!(old.send_471(Stub::ArmAndSpin), Ok(()));

    let mut requests = host_requests(SPIN_471);
    requests[0][3] ^= 0x80;
    let mut stand = maskrom_stand(Behaviour::Faithful);
    let mut face = RomFace::new(&mut stand, &classify);
    assert_eq!(send(&mut face, STAGE_471, &requests), Err(Stall::Framing));
    assert!(stand.in_maskrom());
}
