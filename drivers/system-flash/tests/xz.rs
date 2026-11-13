//! The xz decoder on streams `xz` itself wrote (the fixtures in `tests/data`, made with xz 5.8.2).

use std::path::PathBuf;

use lemnos_driver_system_flash::xz::{Decoder, Error};
use lemnos_measure::test_alloc::{Counting, peak_bytes};
use sha2::{Digest, Sha256};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn data(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "data", name]
        .iter()
        .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Decodes `input` pushed in pieces of `piece` bytes, returning the image (stream 0) only.
fn decode(input: &[u8], piece: usize, max_dict: u64) -> Result<Vec<u8>, Error> {
    decode_both(input, piece, max_dict).map(|(image, _)| image)
}

/// Decodes `input` in pieces: the image (stream 0) and the record (stream 1), and how many streams
/// the decoder read.
fn decode_both(input: &[u8], piece: usize, max_dict: u64) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut decoder = Decoder::new(max_dict);
    let (mut image, mut record) = (Vec::new(), Vec::new());
    for chunk in input.chunks(piece.max(1)) {
        decoder.push(chunk, &mut |stream, bytes| {
            if stream == 0 {
                image.extend_from_slice(bytes);
            } else {
                record.extend_from_slice(bytes);
            }
            Ok(())
        })?;
    }
    decoder.finish()?;
    Ok((image, record))
}

const EIGHT_MIB: u64 = 8 << 20;

#[test]
fn every_check_kind_decodes_to_the_image_whatever_the_piece_size() {
    let image = data("small.img");
    for name in [
        "crc64.img.xz",
        "crc32.img.xz",
        "sha256.img.xz",
        "none.img.xz",
        "multi.img.xz",
    ] {
        let input = data(name);
        for piece in [1, 7, 512, 4096, usize::MAX] {
            let out = decode(&input, piece, EIGHT_MIB)
                .unwrap_or_else(|e| panic!("{name} at {piece}: {e:?}"));
            assert_eq!(out, image, "{name} pushed in {piece}-byte pieces");
        }
    }
}

#[test]
fn a_multi_megabyte_stream_decodes_to_its_hash() {
    let out = decode(&data("big.img.xz"), 4096, EIGHT_MIB).unwrap();
    assert_eq!(out.len(), 8 << 20);
    let digest: String = Sha256::digest(&out)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest,
        "12909e5cc5d1435045c74802b576d464d7de0f020b192235a415030e642eccfd"
    );
}

#[test]
fn a_dictionary_over_the_limit_is_refused_before_anything_is_decoded() {
    let input = data("dict16.img.xz");
    assert_eq!(
        decode(&input, 4096, EIGHT_MIB),
        Err(Error::Dictionary {
            mib: 16,
            limit_mib: 8
        })
    );
    // At its own size, the same stream is fine.
    assert_eq!(decode(&input, 4096, 16 << 20).unwrap(), data("small.img"));
}

#[test]
fn a_truncated_stream_is_refused_at_the_end() {
    let input = data("truncated.img.xz");
    let mut decoder = Decoder::new(EIGHT_MIB);
    decoder.push(&input, &mut |_, _| Ok(())).unwrap();
    assert!(matches!(decoder.finish(), Err(Error::Malformed(_))));
}

#[test]
fn a_flipped_byte_in_the_data_fails_the_block_check() {
    let mut input = data("crc64.img.xz");
    input[100] ^= 0x40;
    assert!(decode(&input, 4096, EIGHT_MIB).is_err());
}

#[test]
fn a_flipped_byte_in_the_stream_header_is_refused() {
    let mut input = data("crc64.img.xz");
    input[0] = 0x00;
    assert_eq!(
        decode(&input, 4096, EIGHT_MIB),
        Err(Error::Malformed("the image is not an xz stream"))
    );
}

#[test]
fn data_after_the_footer_is_refused_but_zero_padding_is_not() {
    let mut padded = data("crc64.img.xz");
    padded.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(decode(&padded, 4096, EIGHT_MIB).unwrap(), data("small.img"));
    let mut junk = data("crc64.img.xz");
    junk.extend_from_slice(b"junk");
    assert!(matches!(
        decode(&junk, 4096, EIGHT_MIB),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn a_sink_refusal_ends_the_decode_with_its_status() {
    let mut decoder = Decoder::new(EIGHT_MIB);
    let refused = decoder.push(&data("crc64.img.xz"), &mut |_, _| {
        Err(lemnos_core::Status::from_errno(28))
    });
    assert_eq!(
        refused,
        Err(Error::Sink(lemnos_core::Status::from_errno(28)))
    );
}

const NOT_AN_IMAGE: &str = "not a first-flash image";

#[test]
fn a_signed_file_is_the_image_then_one_record_stream() {
    let (image, record) = decode_both(&data("signed.img.xz"), 4096, EIGHT_MIB).unwrap();
    assert_eq!(image, data("small.img"));
    assert_eq!(record, data("vector-record.bin"));
    assert_eq!(record.len(), 108);
}

#[test]
fn the_image_alone_has_no_record_and_the_second_stream_may_come_in_any_pieces() {
    let (image, record) = decode_both(&data("crc64.img.xz"), 1, EIGHT_MIB).unwrap();
    assert_eq!((image.len(), record.len()), (16384, 0));
    for piece in [1, 9, usize::MAX] {
        let (_, record) = decode_both(&data("signed.img.xz"), piece, EIGHT_MIB).unwrap();
        assert_eq!(record.len(), 108, "piece {piece}");
    }
}

#[test]
fn a_third_stream_is_refused_by_name() {
    assert_eq!(
        decode(&data("third.img.xz"), 4096, EIGHT_MIB),
        Err(Error::Malformed(NOT_AN_IMAGE))
    );
}

#[test]
fn a_second_stream_is_handed_on_for_the_driver_to_judge() {
    // Junk as a second stream is the driver's to refuse (it is not a record); the decoder gives it.
    let (image, record) = decode_both(&data("junk-second.img.xz"), 4096, EIGHT_MIB).unwrap();
    assert_eq!(image, data("small.img"));
    assert_eq!(record, b"junk");
}

#[test]
fn data_after_the_image_that_is_not_a_stream_is_refused_by_name() {
    let mut input = data("crc64.img.xz");
    input.extend_from_slice(&[0, 0, 0, 0, b'j', b'u', b'n', b'k']);
    assert_eq!(
        decode(&input, 4096, EIGHT_MIB),
        Err(Error::Malformed(NOT_AN_IMAGE))
    );
}

#[test]
fn a_second_stream_must_follow_padding_in_fours() {
    let mut input = data("crc64.img.xz");
    input.push(0);
    let signed = data("signed.img.xz");
    input.extend_from_slice(&signed[data("crc64.img.xz").len()..]);
    assert_eq!(
        decode(&input, 4096, EIGHT_MIB),
        Err(Error::Malformed(NOT_AN_IMAGE))
    );
}

#[test]
fn the_decoder_stays_bounded_over_a_two_stream_file() {
    // The big image, then the record stream. The image is hashed as it comes, not kept: the peak
    // is the decoder's own (its dictionary and a little more).
    let mut input = data("big.img.xz");
    let signed = data("signed.img.xz");
    input.extend_from_slice(&signed[data("crc64.img.xz").len()..]);
    let (result, peak) = peak_bytes(|| -> Result<(u64, Vec<u8>, String), Error> {
        let mut decoder = Decoder::new(EIGHT_MIB);
        let mut hasher = Sha256::new();
        let mut count = 0u64;
        let mut record = Vec::new();
        for chunk in input.chunks(4096) {
            decoder.push(chunk, &mut |stream, bytes| {
                if stream == 0 {
                    hasher.update(bytes);
                    count += bytes.len() as u64;
                } else {
                    record.extend_from_slice(bytes);
                }
                Ok(())
            })?;
        }
        decoder.finish()?;
        let digest = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok((count, record, digest))
    });
    let (count, record, digest) = result.unwrap();
    assert_eq!(count, 8 << 20);
    assert_eq!(
        digest,
        "12909e5cc5d1435045c74802b576d464d7de0f020b192235a415030e642eccfd"
    );
    assert_eq!(record, data("vector-record.bin"));
    assert!(peak <= (8 << 20) + (256 << 10), "peak {peak} bytes");
}
