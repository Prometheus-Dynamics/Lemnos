//! FLASH-EXTENT: the whole-image flash path (`k230-card flash`, the job at the job address) writes only inside the
//! extent its job declares, and the stage refuses a piece that has no declaration, a piece that is not wholly
//! inside it, and an extent that runs past the card's end. The flash path has no protected-sector list: a real flash
//! writes the GPT and the boot headers. The bench's declared range and protected sectors (`bounds`) do not apply.
//!
//! The tests build each job's header by the wire layout (`piece.rs`, `HEADER_LEN` words), not through the `Job` type,
//! so the same bytes are checked whether or not the stage reads the extent. The model is the card of record
//! (`card_model`): a piece is run through the stage's real `piece::run` over the model's `sd::Card`.

mod card_model;

use lemnos_k230_usbboot_stage::format::crc32;
use lemnos_k230_usbboot_stage::piece::{self, Job, Op, PieceError};

use card_model::{BLOCKS, ModelHost, card, pattern};

/// The job header's magic, `LPC1`.
const MAGIC: u32 = 0x3143_504c;
/// The header's flags word: bit 0 is FAST; bit 1 says the extent words are declared (FLASH-EXTENT).
const EXTENT_FLAG: u32 = 1 << 1;

const NO_EXTENT: &str = "piece refused: no declared extent";
const OUTSIDE: &str = "piece refused: the piece is not wholly inside the declared extent";
const PAST_CARD: &str = "piece refused: the declared extent runs past the card";
const EMPTY: &str = "piece refused: the declared extent is empty";

/// A job's header, as the host sends it: sixteen little-endian words, the last a CRC of the first fifteen.
/// `extent` is the whole-image declaration (first sector, sector count), or `None` for a job without one.
fn header(
    op: Op,
    lba: u64,
    count: u32,
    data: &[u8],
    cid: [u8; 16],
    extent: Option<(u32, u32)>,
) -> [u8; 64] {
    let mut words = [0u32; 16];
    words[0] = MAGIC;
    words[1] = op as u32;
    words[2] = lba as u32;
    words[3] = (lba >> 32) as u32;
    words[4] = count;
    words[5] = if data.is_empty() { 0 } else { crc32(data) };
    words[6] = if extent.is_some() { EXTENT_FLAG } else { 0 };
    words[7] = BLOCKS as u32;
    words[8] = (BLOCKS >> 32) as u32;
    for i in 0..4 {
        words[9 + i] =
            u32::from_be_bytes([cid[4 * i], cid[4 * i + 1], cid[4 * i + 2], cid[4 * i + 3]]);
    }
    if let Some((first, count)) = extent {
        words[13] = first;
        words[14] = count;
    }
    let mut out = [0u8; 64];
    for (i, word) in words.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
    }
    let crc = crc32(&out[..60]);
    out[60..].copy_from_slice(&crc.to_le_bytes());
    out
}

/// What the stage does with one piece: `Ok` when the piece is accepted and run, else the refusal's text. The card is
/// opened as the stage opens it, so a refused piece has been through `Card::init` and nothing else.
fn stage_piece(host: &ModelHost, header: [u8; 64], payload: &[u8]) -> Result<(), String> {
    let job = Job::parse(&header).map_err(str::to_string)?;
    let mut card = card(host);
    piece::run(&mut card, &job, payload, &mut |_| {})
        .map(|_| ())
        .map_err(|e| match e {
            PieceError::Refused(text) => text.to_string(),
            other => format!("{other:?}"),
        })
}

fn cid(host: &ModelHost) -> [u8; 16] {
    card(host).cid()
}

fn sectors(count: u32, fill: u8) -> Vec<u8> {
    vec![fill; count as usize * 512]
}

const DECLARED: (u32, u32) = (1_048_576, 4);

#[test]
fn a_piece_with_no_declared_extent_is_refused_and_writes_nothing() {
    for op in [Op::Write, Op::Zero] {
        let host = ModelHost::card_of_record();
        let data = if op == Op::Write {
            sectors(1, 0xc3)
        } else {
            Vec::new()
        };
        let h = header(op, 1_048_576, 1, &data, cid(&host), None);
        assert_eq!(
            stage_piece(&host, h, &data),
            Err(NO_EXTENT.to_string()),
            "{op:?}"
        );
        assert!(host.writes().is_empty(), "{op:?}: the card was not touched");
    }
}

#[test]
fn a_piece_wholly_or_partly_outside_the_declared_extent_is_refused_whole() {
    // Outside entirely, the GPT, a boot header, and two straddling pieces (one at the extent's start, one at its end).
    let cases: [(u64, u32); 5] = [
        (2048, 1),
        (0, 1),
        (1_048_575, 2),
        (1_048_579, 2),
        (1_048_575, 8),
    ];
    for op in [Op::Write, Op::Zero] {
        for (lba, count) in cases {
            let host = ModelHost::card_of_record();
            let data = if op == Op::Write {
                sectors(count, 0xc3)
            } else {
                Vec::new()
            };
            let h = header(op, lba, count, &data, cid(&host), Some(DECLARED));
            assert_eq!(
                stage_piece(&host, h, &data),
                Err(OUTSIDE.to_string()),
                "{op:?} {lba}+{count}"
            );
            assert!(
                host.writes().is_empty(),
                "{op:?} {lba}+{count}: nothing written"
            );
        }
    }
}

#[test]
fn the_write_test_outside_the_declared_extent_is_refused_and_writes_nothing() {
    let host = ModelHost::card_of_record();
    let data = sectors(1, 0x5a);
    let h = header(Op::Test, 2048, 1, &data, cid(&host), Some(DECLARED));
    assert_eq!(stage_piece(&host, h, &data), Err(OUTSIDE.to_string()));
    assert!(host.writes().is_empty());
}

#[test]
fn an_extent_that_runs_past_the_card_end_is_refused_before_any_write() {
    // The piece is inside the card; the extent is not: it names the card's last eight sectors and eight past them.
    let host = ModelHost::card_of_record();
    let data = sectors(8, 0xc3);
    let h = header(
        Op::Write,
        BLOCKS - 8,
        8,
        &data,
        cid(&host),
        Some((BLOCKS as u32 - 8, 16)),
    );
    assert_eq!(stage_piece(&host, h, &data), Err(PAST_CARD.to_string()));
    assert!(host.writes().is_empty());
    // An extent that starts past the card is the same refusal.
    let h = header(
        Op::Zero,
        BLOCKS - 1,
        1,
        &[],
        cid(&host),
        Some((BLOCKS as u32, 1)),
    );
    assert_eq!(stage_piece(&host, h, &[]), Err(PAST_CARD.to_string()));
    assert!(host.writes().is_empty());
}

#[test]
fn an_empty_extent_is_refused() {
    let host = ModelHost::card_of_record();
    let data = sectors(1, 0xc3);
    let h = header(
        Op::Write,
        1_048_576,
        1,
        &data,
        cid(&host),
        Some((1_048_576, 0)),
    );
    assert_eq!(stage_piece(&host, h, &data), Err(EMPTY.to_string()));
    assert!(host.writes().is_empty());
}

#[test]
fn a_whole_image_flash_inside_its_declared_extent_passes_and_reads_back_equal() {
    // The extent is the whole card. The flash writes the primary GPT, the boot headers and the backup's place, which
    // the bench's protected list would refuse; on the flash path it is written and read back equal.
    let host = ModelHost::card_of_record();
    let extent = Some((0u32, BLOCKS as u32));
    let plan: [(u64, u32); 4] = [(0, 16), (2048, 8), (3072, 8), (BLOCKS - 33, 8)];
    let cid = cid(&host);
    for (i, (lba, count)) in plan.into_iter().enumerate() {
        let data: Vec<u8> = (0..count)
            .flat_map(|s| pattern(lba + u64::from(s)))
            .collect();
        let h = header(Op::Write, lba, count, &data, cid, extent);
        assert_eq!(
            stage_piece(&host, h, &data),
            Ok(()),
            "piece {i}: {lba}+{count}"
        );
        for s in 0..count {
            let at = lba + u64::from(s);
            assert_eq!(host.sector(at), pattern(at), "read back at {at}");
        }
    }
}
