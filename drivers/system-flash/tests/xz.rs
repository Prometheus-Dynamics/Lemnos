//! The xz decoder on streams `xz` itself wrote (the fixtures in `tests/data`, made with xz 5.8.2).

use std::path::PathBuf;

use lemnos_contract_system_flash::{XZ_MAX_BLOCKS, XZ_MAX_DICT_MIB};
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

#[test]
fn the_signed_and_unsigned_builder_files_decode_in_the_builders_piece_sizes() {
    // The image builders' files (signed.img.xz: image and record; small.img.xz: unsigned) must decode
    // to the same bytes whether they arrive in 1-, 7-, 4096-byte or whole pieces.
    let small = data("small.img");
    for piece in [1, 7, 4096, usize::MAX] {
        let (image, record) = decode_both(&data("signed.img.xz"), piece, EIGHT_MIB).unwrap();
        assert_eq!(image, small, "signed image at {piece}");
        assert_eq!(record.len(), 108, "signed record at {piece}");
        let unsigned = decode(&data("crc64.img.xz"), piece, EIGHT_MIB).unwrap();
        assert_eq!(unsigned, small, "unsigned image at {piece}");
    }
}

/// The builder's threaded stream (`threaded-64m.img.xz`): `xz -6 --check=crc64 --block-size=64MiB
/// -T2` on its 160 MiB raw image. `xz -lvv`, as made:
///
/// ```text
/// Streams: 1   Blocks: 3   Check: CRC64   Stream Padding: 0 B
/// Block 1: uncompressed 67,108,864   header 20   CRC64   --lzma2=dict=8MiB
/// Block 2: uncompressed 67,108,864   header 20   CRC64   --lzma2=dict=8MiB
/// Block 3: uncompressed 33,554,432   header 20   CRC64   --lzma2=dict=8MiB
/// Memory needed: 9 MiB   Sizes in headers: Yes
/// ```
const THREADED_LEN: u64 = 167_772_160;
const THREADED_SHA: &str = "326c678268e0a2ca05ce03d3c5cc2b0737f6137bb484a53017a998cfd507a16f";
/// The recipe's threaded stream (`threaded-recipe-64m.img.xz`): the same command on the recipe's
/// image (`make-fixtures.sh`). Also 3 blocks, CRC64, 8 MiB dictionaries, sized headers.
const RECIPE_SHA: &str = "4f587dfdc8118827f76d17c2a9718f7c777aaf608d86006c987347400e6edb6c";
const BLOCK: u64 = 64;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes `input` in pieces, hashing the image (stream 0) as it comes: its length and SHA-256.
/// Nothing is kept, so a 160 MiB image is not held in memory.
fn decode_digest(input: &[u8], piece: usize) -> Result<(u64, String), Error> {
    let mut decoder = Decoder::new(EIGHT_MIB);
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    for chunk in input.chunks(piece.max(1)) {
        decoder.push(chunk, &mut |stream, bytes| {
            if stream == 0 {
                hasher.update(bytes);
                count += bytes.len() as u64;
            }
            Ok(())
        })?;
    }
    decoder.finish()?;
    Ok((count, hex(&hasher.finalize())))
}

#[test]
fn the_builder_threaded_stream_decodes_to_its_hash_in_an_odd_and_a_whole_piece() {
    // Two decodes of the 160 MiB image in a debug build take about 20 seconds, so the sizes here
    // are the ones that matter for block boundaries: 7 (odd) and whole. The 4096-byte pieces, and
    // both images, are in a_threaded_stream_stays_within_its_dictionary_plus_256_kib.
    let input = data("threaded-64m.img.xz");
    for piece in [7, usize::MAX] {
        let (count, digest) =
            decode_digest(&input, piece).unwrap_or_else(|e| panic!("threaded at {piece}: {e:?}"));
        assert_eq!(count, THREADED_LEN, "threaded length at {piece}");
        assert_eq!(digest, THREADED_SHA, "threaded hash at {piece}");
    }
}

#[test]
fn a_threaded_stream_stays_within_its_dictionary_plus_256_kib() {
    // Both threaded images in 4096-byte pieces: the length and SHA-256 of each, and the peak.
    for (name, sha) in [
        ("threaded-64m.img.xz", THREADED_SHA),
        ("threaded-recipe-64m.img.xz", RECIPE_SHA),
    ] {
        let input = data(name);
        let (result, peak) = peak_bytes(|| decode_digest(&input, 4096));
        let (count, digest) = result.unwrap();
        assert_eq!((count, digest.as_str()), (THREADED_LEN, sha), "{name}");
        assert!(peak <= (8 << 20) + (256 << 10), "{name}: peak {peak} bytes");
    }
}

#[test]
fn a_stream_of_exactly_the_block_limit_is_accepted() {
    // 8192 blocks of 64 zero bytes: the largest stream the index may list.
    assert_eq!(XZ_MAX_BLOCKS, 8192);
    let zeros = vec![0u8; (XZ_MAX_BLOCKS as u64 * BLOCK) as usize];
    for piece in [7, 4096, usize::MAX] {
        assert_eq!(
            decode(&data("blocks-8192.img.xz"), piece, EIGHT_MIB).unwrap(),
            zeros,
            "8192 blocks at {piece}"
        );
    }
}

#[test]
fn a_stream_over_the_block_limit_is_refused_by_name() {
    // 8193 blocks: the decoder refuses the 8193rd when its data ends, before the index.
    assert_eq!(
        decode(&data("blocks-8193.img.xz"), 4096, EIGHT_MIB),
        Err(Error::Unsupported(
            "the xz stream has more blocks than the board takes"
        ))
    );
    assert_eq!(
        decode(&data("blocks-8193.img.xz"), usize::MAX, EIGHT_MIB),
        Err(Error::Unsupported(
            "the xz stream has more blocks than the board takes"
        ))
    );
}

#[test]
fn the_contract_dictionary_limit_is_the_decoders_limit() {
    // The decoder is given 8 MiB by the driver's ceiling; the contract's constant is that ceiling.
    assert_eq!(XZ_MAX_DICT_MIB << 20, EIGHT_MIB);
    assert_eq!(
        decode(&data("dict16.img.xz"), 4096, XZ_MAX_DICT_MIB << 20),
        Err(Error::Dictionary {
            mib: 16,
            limit_mib: 8
        })
    );
}

/// The four-block stream's image: its length and sha256 (`multi-block.img.xz`, made by
/// `make-fixtures.sh`: three 64 MiB blocks and a short fourth).
const MULTI_LEN: u64 = 203_424_257;
const MULTI_SHA: &str = "d452e842e04ae0d07d79907d5ee388c552cf7de3e97ea9519d7a8519cbe636f6";

/// Decodes `input` in pieces, hashing each stream's bytes as they come (nothing is kept): the
/// length and sha256 of stream 0, and the bytes of every other stream.
fn digest_both(input: &[u8], piece: usize) -> Result<(u64, String, Vec<u8>), Error> {
    use sha2::{Digest, Sha256};
    let mut decoder = Decoder::new(EIGHT_MIB);
    let (mut hasher, mut count, mut record) = (Sha256::new(), 0u64, Vec::new());
    for chunk in input.chunks(piece.max(1)) {
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
    Ok((count, digest, record))
}

#[test]
fn a_four_block_stream_decodes_to_its_image_whatever_the_piece_size() {
    for piece in [4096, 1 << 20] {
        let (count, digest, record) = digest_both(&data("multi-block.img.xz"), piece).unwrap();
        assert_eq!(
            (count, digest.as_str()),
            (MULTI_LEN, MULTI_SHA),
            "piece {piece}"
        );
        assert!(record.is_empty());
    }
}

#[test]
fn a_four_block_signed_file_is_the_image_then_one_record_stream() {
    let (count, digest, record) = digest_both(&data("multi-block-signed.img.xz"), 4096).unwrap();
    assert_eq!((count, digest.as_str()), (MULTI_LEN, MULTI_SHA));
    assert_eq!(record.len(), 108);
}

#[test]
fn a_flipped_byte_inside_a_later_block_fails_its_check() {
    let mut input = data("multi-block.img.xz");
    let middle = input.len() / 2;
    input[middle] ^= 0x40;
    assert!(digest_both(&input, 4096).is_err());
}
