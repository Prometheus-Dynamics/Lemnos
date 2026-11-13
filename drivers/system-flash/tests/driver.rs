//! The first-flash driver on temp files: a target file stands in for the disk, and a fake mount
//! table stands in for `/proc/self/mountinfo`. Nothing here touches a real device or mount.

use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use ed25519_dalek::{Signer, SigningKey};
use lemnos_contract_indicator::{Indicator as IndicatorContract, NO_PROGRESS};
use lemnos_contract_system_flash::{FORMAT_RAW, FORMAT_XZ, Flash};
use lemnos_core::{
    CallerId, Config, ConfigValue, Decode, Dev, Health, JobId, JobState, Lifecycle, LocalCaller,
    MethodId, Reader, Reply, Result, Router, Status,
};
use lemnos_driver_system_flash::SystemFlash;
use lemnos_driver_system_flash::manifest::{self, DISK};
use lemnos_driver_system_flash::target::major_minor;
use lemnos_measure::test_alloc::{Counting, peak_bytes};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const BOARD: &str = "flash-test";
const SMALL_SHA: &str = "f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58";
const BIG_SHA: &str = "12909e5cc5d1435045c74802b576d464d7de0f020b192235a415030e642eccfd";
const SMALL: u64 = 16 * 1024;
const EIGHT_MIB: u64 = 8 << 20;
const SIGNED: &str = "signed.img.xz";

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "data", name]
        .iter()
        .collect();
    fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A scratch directory, removed when the test ends.
struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-flash-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The settings a test driver is started with.
struct Rig {
    dir: Dir,
    target: PathBuf,
    mountinfo: PathBuf,
    keys: Option<PathBuf>,
    allow: bool,
    require_signed: bool,
    board: &'static str,
    max_dict_mb: Option<i64>,
}

impl Rig {
    fn new(label: &str, target_bytes: u64) -> Self {
        let dir = Dir::new(label);
        let target = dir.path("disk.img");
        File::create(&target)
            .unwrap()
            .set_len(target_bytes)
            .unwrap();
        let mountinfo = dir.path("mountinfo");
        // A table with nothing on the target: a RAM-only flasher's root.
        fs::write(&mountinfo, "1 0 0:1 / / rw - rootfs rootfs rw\n").unwrap();
        Self {
            dir,
            target,
            mountinfo,
            keys: None,
            allow: true,
            require_signed: false,
            board: BOARD,
            max_dict_mb: None,
        }
    }

    /// Writes the key file the board trusts: `keys` lines, one per key given.
    fn trust(&mut self, keys: &[SigningKey]) {
        let mut text = String::new();
        for key in keys {
            text.push_str(&format!(
                "{} the flash test key\n",
                hex(key.verifying_key().as_bytes())
            ));
        }
        let path = self.dir.path("keys");
        fs::write(&path, text).unwrap();
        self.keys = Some(path);
    }

    /// The driver, started as the board file would start it, with no indicator.
    fn start(&self) -> SystemFlash {
        self.start_with(None)
    }

    /// The driver, started with `indicator` as its indicator need.
    fn start_with(&self, indicator: Option<Dev<dyn IndicatorContract>>) -> SystemFlash {
        self.try_start_with(indicator)
            .expect("the flash driver starts")
    }

    /// The driver's start, as the board file's settings give it.
    fn try_start_with(&self, indicator: Option<Dev<dyn IndicatorContract>>) -> Result<SystemFlash> {
        let target = self.target.to_str().unwrap().to_string();
        let mountinfo = self.mountinfo.to_str().unwrap().to_string();
        let keys = self.keys.as_ref().map(|k| k.to_str().unwrap().to_string());
        let mut entries: Vec<(&str, ConfigValue<'_>)> = vec![
            ("path", ConfigValue::Str(&target)),
            ("kind", ConfigValue::Str("file")),
            ("allow_first_flash", ConfigValue::Bool(self.allow)),
            ("require_signed", ConfigValue::Bool(self.require_signed)),
            ("mountinfo", ConfigValue::Str(&mountinfo)),
        ];
        if let Some(keys) = keys.as_ref() {
            entries.push(("keys", ConfigValue::Str(keys)));
        }
        if let Some(mb) = self.max_dict_mb {
            entries.push(("max_dict_mb", ConfigValue::Int(mb)));
        }
        let cfg = Config::new(&entries).on_board(self.board);
        let needs = lemnos_driver_system_flash::SystemFlashNeeds { indicator };
        <SystemFlash as Lifecycle>::start(needs, &cfg)
    }

    fn file_bytes(&self) -> Vec<u8> {
        fs::read(&self.target).unwrap()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The transport manifest for an image of `size` bytes with `sha`, for `board`. `signature` is
/// the record's hex, when the sender puts it in the JSON.
fn manifest_json(board: &str, size: u64, sha: &str, signature: Option<&str>) -> Vec<u8> {
    let sig = signature.map_or(String::new(), |s| format!(r#","signature":"{s}""#));
    format!(
        r#"{{"schema":1,"board":"{board}","format":"{DISK}","image":{{"size":{size},"sha256":"{sha}"}}{sig}}}"#
    )
    .into_bytes()
}

/// The vector's key: the test seed (seven bytes of 0x07 repeated four times, i.e. 0x07 x 32).
fn vector_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

/// The record the vector's key signs for the small image (the brief's test vector).
fn vector_record() -> Vec<u8> {
    fixture("vector-record.bin")
}

/// A record signed by `key` for the given image, built by the same rules the board uses.
fn record_by(key: &SigningKey, board: &str, size: u64, sha: &str) -> Vec<u8> {
    let sha: [u8; 32] = hex_to(sha);
    let message = manifest::signed_message(board, DISK, size, &sha);
    let signature = key.sign(&message).to_bytes();
    manifest::record_bytes(&manifest::key_id(&key.verifying_key()), &signature)
}

fn hex_to(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, pair) in text.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    out
}

/// The job's status and its reply: the byte count, then the outcome code for a write.
fn job_result(driver: &mut SystemFlash, job: JobId) -> (Status, Vec<u8>) {
    let mut out = [0u8; 16];
    let (status, n) = {
        let mut reply = Reply::new(&mut out);
        let status = match driver.job(job, &mut reply) {
            JobState::Finished(status) => status,
            other => panic!("the job is not finished: {other:?}"),
        };
        (status, reply.len())
    };
    (status, out[..n].to_vec())
}

/// Runs a job to its end, ticking it while it runs; the job's status and its reply. The job is
/// read once to the end, so this is the only read of its result.
fn run_to_end(driver: &mut SystemFlash, job: JobId) -> (Status, Vec<u8>, u64) {
    let mut ticks = 0u64;
    loop {
        let mut out = [0u8; 16];
        let (state, n) = {
            let mut reply = Reply::new(&mut out);
            let state = driver.job(job, &mut reply);
            (state, reply.len())
        };
        match state {
            JobState::Running { .. } => {
                ticks += 1;
                lemnos_core::Lifecycle::tick(driver, 0);
            }
            JobState::Finished(status) => return (status, out[..n].to_vec(), ticks),
        }
    }
}

/// The bytes a write's result carries: the image's byte count, then the outcome code.
fn written(count: u64, code: u8) -> Vec<u8> {
    let mut out = count.to_le_bytes().to_vec();
    out.push(code);
    out
}

fn info(driver: &mut SystemFlash) -> String {
    let mut out = vec![0u8; 4096];
    let n = driver.info(&mut out).unwrap();
    String::from_utf8(out[..n].to_vec()).unwrap()
}

/// Streams `bytes` into the write job in pieces of `piece`, from offset 0, as the starter.
fn stream(driver: &mut SystemFlash, bytes: &[u8], piece: usize) -> std::result::Result<(), Status> {
    let mut at = 0;
    for chunk in bytes.chunks(piece) {
        driver.data(at, chunk)?;
        at += chunk.len() as u64;
    }
    Ok(())
}

/// A started driver whose manifest is accepted (unsigned) for `image`.
fn ready(rig: &Rig, image_sha: &str, image_size: u64) -> SystemFlash {
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(&manifest_json(rig.board, image_size, image_sha, None), &[])
        .expect("the manifest is accepted");
    driver
}

#[test]
fn an_xz_stream_is_written_verified_and_its_result_is_the_raw_count() {
    let rig = Rig::new("xz", SMALL + 4096);
    let stream_bytes = fixture("crc64.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    stream(&mut driver, &stream_bytes, 4096).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 1))
    );
    assert_eq!(
        &rig.file_bytes()[..SMALL as usize],
        fixture("small.img").as_slice()
    );
    assert!(info(&mut driver).contains("\"state\":\"written\""));

    let check = driver.verify().unwrap();
    let (status, reply, ticks) = run_to_end(&mut driver, check);
    assert!(ticks >= 1, "a verify ticks while it reads");
    assert_eq!((status, reply), (Status::OK, SMALL.to_le_bytes().to_vec()));
    assert!(info(&mut driver).contains("\"state\":\"verified\""));
}

#[test]
fn a_raw_image_is_written_as_it_comes() {
    let rig = Rig::new("raw", SMALL);
    let image = fixture("small.img");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    stream(&mut driver, &image, 1000).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 1))
    );
    assert_eq!(rig.file_bytes(), image);
}

#[test]
fn a_wrong_hash_fails_the_job_and_says_so() {
    let rig = Rig::new("hash", SMALL);
    let mut driver = ready(&rig, &"0".repeat(64), SMALL);
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    let last = stream(&mut driver, &fixture("small.img"), 4096);
    assert_eq!(last, Err(Status::INVALID_ARGUMENT));
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("SHA-256"));
}

#[test]
fn a_truncated_stream_fails_at_its_end() {
    let rig = Rig::new("truncated", SMALL + 4096);
    let stream_bytes = fixture("truncated.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("cut short"));
}

#[test]
fn a_cancel_mid_way_leaves_the_target_partial_and_a_rerun_writes_it_again() {
    let rig = Rig::new("cancel", SMALL + 4096);
    let stream_bytes = fixture("crc64.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    driver
        .data(0, &stream_bytes[..stream_bytes.len() / 2])
        .unwrap();
    driver.job_cancel(job);
    assert_eq!(job_result(&mut driver, job).0, Status::CANCELLED);
    let text = info(&mut driver);
    assert!(text.contains("\"state\":\"cancelled\""));
    assert!(text.contains("partial image"));
    assert_eq!(
        driver.verify(),
        Err(Status::NOT_FOUND),
        "a cancelled write is no finished image"
    );

    // Nothing is kept: the next write starts again from byte 0.
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    stream(&mut driver, &stream_bytes, 4096).unwrap();
    assert_eq!(job_result(&mut driver, job).0, Status::OK);
    assert_eq!(
        &rig.file_bytes()[..SMALL as usize],
        fixture("small.img").as_slice()
    );
}

#[test]
fn an_out_of_order_chunk_changes_nothing_and_the_stream_goes_on() {
    let rig = Rig::new("order", SMALL);
    let image = fixture("small.img");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    assert_eq!(
        driver.data(4096, &image[4096..8192]),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(info(&mut driver).contains("\"state\":\"writing\""));
    stream(&mut driver, &image, 4096).unwrap();
    assert_eq!(job_result(&mut driver, job).0, Status::OK);
}

#[test]
fn only_the_starter_may_send_the_stream() {
    let rig = Rig::new("starter", SMALL);
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    driver.write(SMALL, FORMAT_RAW).unwrap();
    driver.caller(CallerId(2));
    assert_eq!(driver.data(0, &[0u8; 16]), Err(Status::NOT_PERMITTED));
}

#[test]
fn an_image_larger_than_the_target_is_refused_before_anything_is_written() {
    let rig = Rig::new("small-target", SMALL / 2);
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::from_errno(28)));
    assert!(
        rig.file_bytes().iter().all(|&b| b == 0),
        "nothing was written"
    );
}

#[test]
fn an_xz_dictionary_over_the_limit_is_refused_when_the_block_starts() {
    let rig = Rig::new("dict", SMALL + 4096);
    let stream_bytes = fixture("dict16.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes[..100], 100),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("dictionary is 16 MiB"));
}

#[test]
fn a_board_may_lower_the_dictionary_limit_but_never_raise_it() {
    let mut rig = Rig::new("dict-raise", SMALL);
    rig.max_dict_mb = Some(16);
    assert!(
        rig.try_start_with(None).is_err(),
        "16 MiB is over the 8 MiB ceiling"
    );
    rig.max_dict_mb = Some(0);
    assert!(
        rig.try_start_with(None).is_err(),
        "0 is under the 1 MiB floor"
    );
    rig.max_dict_mb = Some(8);
    let mut driver = rig.start();
    assert!(info(&mut driver).contains("\"max_dict_mb\":8"));
}

#[test]
fn a_board_limit_below_the_stream_refuses_the_stream_at_its_block() {
    let mut rig = Rig::new("dict-low", SMALL + 4096);
    rig.max_dict_mb = Some(1);
    let stream_bytes = fixture("dict16.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("over the board's limit of 1 MiB"));
}

#[test]
fn a_target_that_holds_the_running_system_is_refused() {
    let rig = Rig::new("running", SMALL);
    let meta = fs::metadata(&rig.target).unwrap();
    let (major, minor) = major_minor(meta.dev());
    fs::write(
        &rig.mountinfo,
        format!("1 0 0:1 / / rw - rootfs rootfs rw\n2 1 {major}:{minor} / /run/root rw - ext4 /dev/x rw\n"),
    )
    .unwrap();
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    assert!(info(&mut driver).contains("\"writable\":false"));
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::NOT_PERMITTED));
    assert!(info(&mut driver).contains("running system"));
    assert!(rig.file_bytes().iter().all(|&b| b == 0));
}

#[test]
fn without_allow_first_flash_nothing_is_written() {
    let mut rig = Rig::new("allow", SMALL);
    rig.allow = false;
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    assert!(info(&mut driver).contains("allow_first_flash is not set"));
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::NOT_PERMITTED));
}

#[test]
fn a_write_needs_a_manifest_first() {
    let rig = Rig::new("nomanifest", SMALL);
    let mut driver = rig.start();
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::NOT_FOUND));
}

#[test]
fn the_decoder_stays_bounded_over_a_multi_megabyte_stream() {
    let rig = Rig::new("memory", EIGHT_MIB);
    let stream_bytes = fixture("big.img.xz");
    let mut driver = ready(&rig, BIG_SHA, EIGHT_MIB);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    let (result, peak) = peak_bytes(|| stream(&mut driver, &stream_bytes, 4096));
    assert_eq!(result, Ok(()));
    assert_eq!(job_result(&mut driver, job).0, Status::OK);
    // The dictionary (8 MiB) and a chunk or two of buffering; nothing that grows with the image.
    assert!(peak <= (8 << 20) + (256 << 10), "peak {peak} bytes");
}

/// The eight rows of the signature table, through the manifest call (the record is the argument)
/// and, where the file matters, through a whole write.
#[test]
fn an_unsigned_image_is_accepted_and_not_checked_when_signing_is_off() {
    let rig = Rig::new("row1", SMALL);
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    assert!(info(&mut driver).contains("\"signed\":\"not_checked_unsigned\""));
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    stream(&mut driver, &fixture("small.img"), 4096).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 1))
    );
}

#[test]
fn a_signed_image_under_a_known_key_verifies_and_is_written_with_its_record() {
    let mut rig = Rig::new("row2", SMALL + 4096);
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    let report = info(&mut driver);
    assert!(report.contains("\"signed\":\"verified\""), "{report}");
    assert!(
        report.contains("\"signer\":\"the flash test key\""),
        "{report}"
    );
    let stream_bytes = fixture(SIGNED);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    stream(&mut driver, &stream_bytes, 4096).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 0))
    );
    assert_eq!(
        &rig.file_bytes()[..SMALL as usize],
        fixture("small.img").as_slice()
    );
}

#[test]
fn a_known_key_with_a_bad_signature_is_refused_when_signing_is_off() {
    let mut rig = Rig::new("row3", SMALL);
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    let mut bad = vector_record();
    bad[60] ^= 0x10;
    assert_eq!(
        driver.manifest(&manifest_json(rig.board, SMALL, SMALL_SHA, None), &bad),
        Err(Status::NOT_PERMITTED)
    );
    assert!(info(&mut driver).contains("\"signed\":\"refused\""));
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::NOT_FOUND));
}

#[test]
fn an_unknown_key_is_accepted_and_not_checked_when_signing_is_off() {
    // The vector's key is not on the list: the signature can't be checked, and it is said so.
    let other = SigningKey::from_bytes(&[9; 32]);
    let mut rig = Rig::new("row4", SMALL);
    rig.trust(&[other]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    assert!(info(&mut driver).contains("\"signed\":\"not_checked_unknown_key\""));
    // A board with no key list at all: every signed image is from an unknown key.
    let bare = Rig::new("row4-bare", SMALL);
    let mut driver = bare.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(bare.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    assert!(info(&mut driver).contains("\"signed\":\"not_checked_unknown_key\""));
}

#[test]
fn a_signed_image_under_an_unknown_key_is_accepted_and_reported_not_checked() {
    let other = SigningKey::from_bytes(&[9; 32]);
    let mut rig = Rig::new("row4b", SMALL + 4096);
    rig.trust(&[other]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    let report = info(&mut driver);
    assert!(
        report.contains("\"signed\":\"not_checked_unknown_key\""),
        "{report}"
    );
    let stream_bytes = fixture(SIGNED);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    stream(&mut driver, &stream_bytes, 4096).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 2))
    );
}

#[test]
fn an_unsigned_image_is_refused_before_any_write_when_signing_is_required() {
    let mut rig = Rig::new("row5", SMALL);
    rig.require_signed = true;
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    assert_eq!(
        driver.manifest(&manifest_json(rig.board, SMALL, SMALL_SHA, None), &[]),
        Err(Status::NOT_PERMITTED)
    );
    assert!(info(&mut driver).contains("only signed images"));
    assert_eq!(driver.write(SMALL, FORMAT_RAW), Err(Status::NOT_FOUND));
    assert!(rig.file_bytes().iter().all(|&b| b == 0));
}

#[test]
fn an_unknown_key_is_refused_before_any_write_when_signing_is_required() {
    let other = SigningKey::from_bytes(&[9; 32]);
    let mut rig = Rig::new("row6", SMALL);
    rig.require_signed = true;
    rig.trust(&[other]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    let refused = driver.manifest(
        &manifest_json(rig.board, SMALL, SMALL_SHA, None),
        &vector_record(),
    );
    assert_eq!(refused, Err(Status::NOT_PERMITTED));
    assert!(info(&mut driver).contains("unknown key"));
    // With no key list at all, every signed image is an unknown key.
    let mut bare = Rig::new("row6b", SMALL);
    bare.require_signed = true;
    let mut driver = bare.start();
    driver.caller(CallerId(1));
    assert_eq!(
        driver.manifest(
            &manifest_json(bare.board, SMALL, SMALL_SHA, None),
            &vector_record()
        ),
        Err(Status::NOT_PERMITTED)
    );
}

#[test]
fn a_known_key_with_a_bad_signature_is_refused_when_signing_is_required() {
    let mut rig = Rig::new("row7", SMALL);
    rig.require_signed = true;
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    let mut bad = vector_record();
    bad[100] ^= 0x01;
    assert_eq!(
        driver.manifest(&manifest_json(rig.board, SMALL, SMALL_SHA, None), &bad),
        Err(Status::NOT_PERMITTED)
    );
    assert!(info(&mut driver).contains("does not verify"));
}

#[test]
fn a_known_key_that_verifies_is_accepted_when_signing_is_required() {
    let mut rig = Rig::new("row8", SMALL + 4096);
    rig.require_signed = true;
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    assert!(info(&mut driver).contains("\"signed\":\"verified\""));
    let stream_bytes = fixture(SIGNED);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    stream(&mut driver, &stream_bytes, 4096).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(SMALL, 0))
    );
}

#[test]
fn a_malformed_record_is_refused_in_every_row() {
    for require in [false, true] {
        let mut rig = Rig::new("malformed", SMALL);
        rig.require_signed = require;
        rig.trust(&[vector_key()]);
        let mut driver = rig.start();
        driver.caller(CallerId(1));
        let record = vector_record();
        let mut magic = record.clone();
        magic[0] = b'X';
        let mut alg = record.clone();
        alg[8] = 2;
        for bad in [&record[..107], &magic[..], &alg[..]] {
            assert_eq!(
                driver.manifest(&manifest_json(rig.board, SMALL, SMALL_SHA, None), bad),
                Err(Status::INVALID_ARGUMENT),
                "require_signed {require}"
            );
        }
        assert!(info(&mut driver).contains("malformed signature record"));
    }
}

#[test]
fn a_trailing_record_that_differs_from_the_argument_fails_the_job() {
    // The file carries the vector's record; the manifest was given an unknown key's record instead.
    let other = SigningKey::from_bytes(&[9; 32]);
    let rig = Rig::new("differs", SMALL + 4096);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    let other_record = record_by(&other, rig.board, SMALL, SMALL_SHA);
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &other_record,
        )
        .unwrap();
    let stream_bytes = fixture(SIGNED);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("the image's trailing signature is not the one given"));
}

#[test]
fn a_record_given_for_a_file_with_none_fails_the_job() {
    // The manifest signs the image, but the file it comes in carries no record: they must agree.
    let mut rig = Rig::new("given-none", SMALL + 4096);
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    driver
        .manifest(
            &manifest_json(rig.board, SMALL, SMALL_SHA, None),
            &vector_record(),
        )
        .unwrap();
    let stream_bytes = fixture("crc64.img.xz");
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("the image's trailing signature is not the one given"));
}

#[test]
fn a_file_with_a_record_given_no_record_fails_the_job() {
    let rig = Rig::new("nogiven", SMALL + 4096);
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let stream_bytes = fixture(SIGNED);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
}

#[test]
fn a_junk_second_stream_is_refused_by_name() {
    let rig = Rig::new("junk", SMALL + 4096);
    let stream_bytes = fixture("junk-second.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("not a first-flash image"));
}

#[test]
fn a_third_stream_is_refused_by_name() {
    let rig = Rig::new("third", SMALL + 4096);
    let stream_bytes = fixture("third.img.xz");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    assert_eq!(
        stream(&mut driver, &stream_bytes, 4096),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    assert!(info(&mut driver).contains("not a first-flash image"));
}

#[test]
fn a_manifest_for_another_board_is_refused_before_any_signature_is_checked() {
    let mut rig = Rig::new("board", SMALL);
    rig.require_signed = true;
    rig.trust(&[vector_key()]);
    let mut driver = rig.start();
    driver.caller(CallerId(1));
    let bad = vec![0u8; 3];
    assert_eq!(
        driver.manifest(
            &manifest_json("another-board", SMALL, SMALL_SHA, None),
            &bad
        ),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(info(&mut driver).contains("this image is for board another-board"));
}

#[test]
fn a_verify_finds_a_target_changed_after_the_write() {
    let rig = Rig::new("verify", SMALL);
    let image = fixture("small.img");
    let mut driver = ready(&rig, SMALL_SHA, SMALL);
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    stream(&mut driver, &image, 4096).unwrap();
    assert_eq!(job_result(&mut driver, job).0, Status::OK);
    let mut bytes = rig.file_bytes();
    bytes[100] ^= 1;
    fs::write(&rig.target, &bytes).unwrap();
    let check = driver.verify().unwrap();
    assert_eq!(run_to_end(&mut driver, check).0, Status::INVALID_ARGUMENT);
}

/// An indicator that records each state it is set to.
struct Recorder(std::sync::Mutex<Vec<(String, u16)>>);

impl Router for Recorder {
    fn call(
        &self,
        _device: u32,
        method: MethodId,
        args: &[u8],
        _reply: &mut [u8],
    ) -> Result<usize> {
        assert_eq!(method.method, 1, "only set is called");
        let mut from = Reader::new(args);
        let state = <&str>::decode(&mut from)?.to_string();
        let progress = u16::decode(&mut from)?;
        self.0.lock().unwrap().push((state, progress));
        Ok(0)
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

fn recorded(recorder: &Recorder) -> Vec<String> {
    recorder
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(s, _)| s.clone())
        .collect()
}

#[test]
fn a_write_shows_receiving_then_writing_then_verifying_and_ends_done() {
    // Several LZMA2 chunks, so writing is shown before the stream is over: a chunk is written as it
    // is decoded, and the last piece shows verifying.
    let recorder: &'static Recorder = Box::leak(Box::new(Recorder(Default::default())));
    let rig = Rig::new("indicator-ok", EIGHT_MIB);
    let stream_bytes = fixture("big.img.xz");
    let indicator = Dev::new(LocalCaller::new(recorder, 0, 0));
    let mut driver = rig.start_with(Some(indicator));
    driver.caller(CallerId(1));
    driver
        .manifest(&manifest_json(rig.board, EIGHT_MIB, BIG_SHA, None), &[])
        .unwrap();
    let job = driver.write(stream_bytes.len() as u64, FORMAT_XZ).unwrap();
    // Small pieces: the stream is 1.6 KB, so a piece of 4 KB would be the whole of it.
    stream(&mut driver, &stream_bytes, 128).unwrap();
    assert_eq!(
        job_result(&mut driver, job),
        (Status::OK, written(EIGHT_MIB, 1))
    );
    let states = recorded(recorder);
    assert_eq!(
        states.first().map(String::as_str),
        Some("receiving"),
        "{states:?}"
    );
    let writing = states
        .iter()
        .position(|s| s == "writing")
        .expect("writing is shown");
    let verifying = states
        .iter()
        .position(|s| s == "verifying")
        .expect("verifying is shown");
    assert!(writing < verifying, "{states:?}");
    assert_eq!(states.last().map(String::as_str), Some("done"));
    assert_eq!(states.iter().filter(|s| *s == "done").count(), 1);
    let _ = NO_PROGRESS;
}

#[test]
fn a_refused_image_ends_on_failed() {
    let recorder: &'static Recorder = Box::leak(Box::new(Recorder(Default::default())));
    let rig = Rig::new("indicator-bad", SMALL);
    let mut driver = rig.start_with(Some(Dev::new(LocalCaller::new(recorder, 0, 0))));
    driver.caller(CallerId(1));
    driver
        .manifest(&manifest_json(rig.board, SMALL, &"0".repeat(64), None), &[])
        .unwrap();
    let job = driver.write(SMALL, FORMAT_RAW).unwrap();
    let _ = stream(&mut driver, &fixture("small.img"), 4096);
    assert_eq!(job_result(&mut driver, job).0, Status::INVALID_ARGUMENT);
    let states = recorded(recorder);
    assert_eq!(
        states.last().map(String::as_str),
        Some("failed"),
        "{states:?}"
    );
}
