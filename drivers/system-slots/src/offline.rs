//! The first slot of a data partition, written offline (M36): what one install, one trial and one
//! confirm of that slot as the default leave on the data partition, with no daemon running.
//!
//! There is no second writer. The bundle goes through the daemon's own install, its trial and its
//! confirm, the same driver methods a board runs. The boot partition is the daemon's own record
//! code, run over an in-memory copy of the image that is never written back ([`OfflineRecord`]).
//! What differs from a board is only what the host would otherwise supply: the security switch,
//! the clock, the free space, and the boot record's restart (there is no board to restart). Each
//! is a parameter here.
//!
//! The boot image is read and checked, never changed: its default pointer and its bay for the
//! slot are compared with what the writer produced, and a difference refuses the whole output.
//!
//! Free space: the writer applies no reserve. The output (the slot and its records, the sum of
//! their file bytes) must fit in `--data-size`, and nothing more is asked. The reserve a live
//! install keeps back (`reserve_mb` in the board file) is not the writer's to apply: `--verify`
//! prints it so the operator sees what the board will keep once it boots.

use super::*;
// The driver's own `Result` takes a status; the writer's errors are words.
use core::result::Result;
use lemnos_slot_store::bays::{self, ROLES};
use lemnos_slot_store::boot_raw::{Boot, BootRaw, Pointer};
use lemnos_slot_store::bootimage::Mem;
use std::sync::Arc;

/// The bundle is handed to the installer in pieces of this size, as a sender would stream it.
const STREAM: usize = 64 * 1024;

/// The board facts the writer checks the bundle against. The board file gives them; the offline
/// writer never reads the host.
#[derive(Debug, Clone)]
pub struct OfflineBoard {
    /// The board's id (`[board] id`): the bundle must be built for it.
    pub id: String,
    /// The prefix of a slot's kernel arguments (`boot_args`).
    pub boot_args: String,
    /// The board's `boot_sources` entries, each `source=path`.
    pub boot_sources: Vec<String>,
    /// The bay sizes the board file names (`[boot] bay_*`).
    pub layout: bays::Layout,
    /// Whether the board requires its bundles to be signed (`signature = "required"`, the default).
    /// An unsigned bundle is refused when it is; an unknown key is refused either way.
    pub signature_required: bool,
    /// The free space live installs keep back (`reserve_mb` on the board's system-slots device,
    /// else [`crate::DEFAULT_RESERVE`]). Reported by `--verify`; the offline writer never applies it.
    pub reserve: u64,
}

/// Everything the writer is given. Its output is a function of these fields alone.
pub struct FirstSlot<'a> {
    /// The directory the data partition's tree is written to. It must not exist; it is created
    /// here and removed again when the write is refused.
    pub out: &'a Path,
    /// The slot bundle's bytes.
    pub bundle: &'a [u8],
    /// The boot partition's image, as it is on the board's boot disk.
    pub boot_image: Vec<u8>,
    /// The trust root: the keys a signed bundle is checked against.
    pub keys: Vec<TrustedKey>,
    /// The keys file's name as the refusals give it (the path the caller was given).
    pub keys_name: &'a str,
    /// The board the bundle must be for.
    pub board: OfflineBoard,
    /// The time every stamp in the output carries, `YYYY-MM-DDTHH:MM:SSZ`.
    pub time: &'a str,
    /// The bytes the data partition has for the tree: the slot and its records must fit in it.
    pub free: u64,
}

/// The boot record of the offline writer: the daemon's raw boot-partition writers, run over an
/// in-memory copy of the image. A write changes the copy and nothing else; the copy is never
/// written back, so the boot partition is never touched. A restart has no board to restart, so it
/// does nothing; the trial is armed in the copy, then confirmed.
pub struct OfflineRecord {
    boot: Arc<Mutex<Boot<Mem>>>,
}

fn io_of(message: String) -> std::io::Error {
    std::io::Error::other(message)
}

impl BootRecord for OfflineRecord {
    fn set_default(&mut self, id: &str) -> std::io::Result<()> {
        self.lock().set_pointer(Pointer::Default, id).map_err(io_of)
    }

    fn set_trial(&mut self, id: &str, attempts: u8) -> std::io::Result<()> {
        self.lock().set_trial(id, attempts).map_err(io_of)
    }

    fn pending(&self) -> std::io::Result<Option<(String, u8)>> {
        let trial = self.lock().trial().map_err(io_of)?;
        Ok(trial.map(|t| (t.slot, t.attempts)))
    }

    fn set_attempts(&mut self, attempts: u8) -> std::io::Result<()> {
        let mut boot = self.lock();
        if let Some(trial) = boot.trial().map_err(io_of)? {
            boot.set_trial(&trial.slot, attempts).map_err(io_of)?;
        }
        Ok(())
    }

    fn clear_trial(&mut self) -> std::io::Result<()> {
        self.lock().clear_trial().map_err(io_of)
    }

    fn restart(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn install(&self, slot: &str, payloads: &[&[u8]; 5]) -> std::io::Result<()> {
        self.lock()
            .install_slot(slot, payloads)
            .map(|_| ())
            .map_err(io_of)
    }

    fn free(&self, slot: &str) -> std::io::Result<()> {
        self.lock().free_slot(slot).map_err(io_of)
    }

    fn slot_bay(&self, id: &str) -> std::io::Result<(String, Vec<(String, u64)>)> {
        self.lock().slot_bay(id).map_err(io_of)
    }
}

impl OfflineRecord {
    fn lock(&self) -> std::sync::MutexGuard<'_, Boot<Mem>> {
        self.boot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Writes the data partition's tree for the first slot of a board into `input.out`, and returns
/// the slot's id. Refusals are returned by name, and leave no `out` behind.
///
/// # Errors
///
/// The reason the bundle, the boot image or the free space refuses; `out` exists before the call.
pub fn write_first_slot(input: FirstSlot<'_>) -> Result<String, String> {
    if input.out.exists() {
        return Err(format!(
            "{} exists: the data partition is written to a new directory only",
            input.out.display()
        ));
    }
    // The signing policy is decided before the output directory is made, so a refusal writes nothing.
    check_signing(&input)?;
    // The boot image as the board has it. The writer's copy is the one its record writes into;
    // this one is the reference the result is compared with.
    let reference =
        Boot::on(Mem(input.boot_image.clone())).map_err(|e| format!("the boot image: {e}"))?;
    let image_layout = reference
        .bay_layout()
        .map_err(|e| format!("the boot image's bays: {e}"))?;
    check_layout(&image_layout, &input.board.layout)?;
    let copy = Arc::new(Mutex::new(
        Boot::on(Mem(input.boot_image.clone())).map_err(|e| format!("the boot image: {e}"))?,
    ));
    let record = OfflineRecord { boot: copy.clone() };

    fs::create_dir(input.out).map_err(|e| format!("{}: {e}", input.out.display()))?;
    let mut pair = Pair { reference, copy };
    let result = write_into(&input, record, &mut pair)
        .and_then(|id| fix_modes(input.out).map(|()| id))
        .and_then(|id| fit(input.out, input.free, id));
    if result.is_err() {
        // The directory this call created, with nothing in it that is kept.
        let _ = fs::remove_dir_all(input.out);
    }
    result
}

/// The written tree's bytes, checked against the data partition's free bytes: the slot and its
/// records together, no reserve. Refused with both sizes named.
fn fit(out: &Path, free: u64, id: String) -> Result<String, String> {
    let need = tree_bytes(out);
    if need > free {
        return Err(format!(
            "no room for this slot: the slot and its records need {need} bytes, --data-size gives {free} bytes: give the data partition more room"
        ));
    }
    Ok(id)
}

/// The bytes of every file under `root`: the sum of their lengths, directories not counted.
pub fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0_u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// The signature policy of the offline writer, on the outcome word the install recorded. An unknown
/// key is refused for every board; an unsigned bundle only when the board requires signing.
fn signature_policy(word: &str, bundle: &[u8], input: &FirstSlot<'_>) -> Result<(), String> {
    use lemnos_slot_store::signing::{NOT_CHECKED_UNKNOWN_KEY, NOT_CHECKED_UNSIGNED};
    match word {
        NOT_CHECKED_UNKNOWN_KEY => {
            let key = bundle_key_id(bundle).unwrap_or_else(|| "(unreadable)".into());
            Err(unknown_key_refusal(&key, input.keys_name))
        }
        NOT_CHECKED_UNSIGNED if input.board.signature_required => Err(UNSIGNED_REFUSAL.into()),
        _ => Ok(()),
    }
}

/// The key id the bundle's `slot.sig` names, in hex. `None` when the bundle has no such member or
/// the record does not parse.
pub fn bundle_key_id(bundle: &[u8]) -> Option<String> {
    use lemnos_slot_store::bundle::hex;
    let record = lemnos_slot_store::signing::parse_record(&bundle_sig(bundle)?).ok()?;
    Some(hex(&record.key_id))
}

/// The bundle's `slot.sig` member's bytes. `None` when the bundle has no such member.
fn bundle_sig(bundle: &[u8]) -> Option<Vec<u8>> {
    use lemnos_slot_store::tar::{Kind, Sink, Tar};
    #[derive(Default)]
    struct SlotSig {
        inside: bool,
        found: Option<Vec<u8>>,
    }
    impl Sink for SlotSig {
        fn begin(&mut self, name: &str, kind: Kind, _size: u64) -> Result<(), String> {
            self.inside = kind == Kind::File && name == "slot.sig";
            if self.inside {
                self.found = Some(Vec::new());
            }
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
            if let Some(buf) = self.found.as_mut().filter(|_| self.inside) {
                buf.extend_from_slice(bytes);
            }
            Ok(())
        }
        fn end(&mut self) -> Result<(), String> {
            self.inside = false;
            Ok(())
        }
    }
    let mut sink = SlotSig::default();
    Tar::default().feed(bundle, &mut sink).ok()?;
    sink.found
}

/// The signing policy before anything is written: a bundle signed by a key the keys list does not
/// hold, or unsigned on a board that requires signing, is refused. A malformed record is left to the
/// install, which names it.
fn check_signing(input: &FirstSlot<'_>) -> Result<(), String> {
    use lemnos_slot_store::bundle::hex;
    use lemnos_slot_store::signing;
    let Some(sig) = bundle_sig(input.bundle) else {
        return if input.board.signature_required {
            Err(UNSIGNED_REFUSAL.into())
        } else {
            Ok(())
        };
    };
    let Ok(record) = signing::parse_record(&sig) else {
        return Ok(());
    };
    let held = input
        .keys
        .iter()
        .any(|k| signing::key_id(k.key.as_bytes()) == record.key_id);
    if held {
        Ok(())
    } else {
        Err(unknown_key_refusal(&hex(&record.key_id), input.keys_name))
    }
}

const UNSIGNED_REFUSAL: &str = "the bundle is unsigned, and this board requires signed bundles: sign it, or set signature = \"not-required\" on the board's system-slots device";

fn unknown_key_refusal(key: &str, keys_name: &str) -> String {
    format!(
        "the bundle is signed by key id {key}, which {keys_name} does not hold: a bundle this board cannot check is refused"
    )
}

/// The modes the output carries whatever the umask of the process: 0644 for files, 0755 for
/// directories, the output directory included. The daemon's own writes take the umask they run
/// under, so the writer sets them after the fact, and the bytes of the tree do not depend on it.
fn fix_modes(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let set = |path: &Path, mode: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    set(root, 0o755)?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|e| format!("{}: {e}", path.display()))?
                .is_dir()
            {
                set(&path, 0o755)?;
                stack.push(path);
            } else {
                set(&path, 0o644)?;
            }
        }
    }
    Ok(())
}

/// The reference boot image and the writer's copy, as one value for the final checks.
struct Pair {
    reference: Boot<Mem>,
    copy: Arc<Mutex<Boot<Mem>>>,
}

fn write_into(
    input: &FirstSlot<'_>,
    record: OfflineRecord,
    pair: &mut Pair,
) -> Result<String, String> {
    let layout = Layout::new(input.out, input.out.join(".boot-not-mounted"));
    let mut slots =
        SystemSlots::with_volumes(layout.clone(), Box::new(record), input.keys.clone(), None);
    slots.boot_args = input.board.boot_args.clone();
    slots.boot_sources = input.board.boot_sources.clone();
    slots.board_id = input.board.id.clone();
    // The install's own room check does not apply here: the bundle is not the tree (the tree is what
    // the install keeps), and the tree is checked against --data-size once it is written (`fit`).
    slots.offline_free = Some(u64::MAX);
    // The offline writer applies no reserve.
    slots.reserve = 0;
    // Nothing is running yet: the first slot is tried, then the board boots into it.
    slots.running = Some(String::new());
    let id = security::with_enforced(false, || {
        store::with_clock(input.time, || drive(&mut slots, input.bundle, &layout))
    })?;
    // The signature policy: decided on the outcome the install recorded, before anything is kept.
    let word = store::Meta::load(&layout, &id).get("SIGNATURE").to_string();
    signature_policy(&word, input.bundle, input)?;
    // The boot image: the slot's bay must hold it, and the default must name a bay that holds
    // it. The bay check is `check_bay`, the one call site of it.
    check_bay(&mut pair.reference, &id)?;
    let mut copy = pair
        .copy
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let default = copy
        .pointer(Pointer::Default)
        .map_err(|e| format!("the boot image's default.txt: {e}"))?;
    let expected = pair.reference.pointer(Pointer::Default).ok().flatten();
    if default != expected || copy.trial().map_err(|e| e.to_string())?.is_some() {
        // The pointer names the slot in the default's bay; the words name the bay.
        let slot = expected.unwrap_or_default();
        let bay = pair
            .reference
            .bay_of(&slot)
            .map(bays::set_name)
            .unwrap_or(slot);
        return Err(format!(
            "the boot image's default.txt names {bay}, which does not hold {id}"
        ));
    }
    drop(copy);
    // The installer's staging directory is not part of the partition.
    let _ = fs::remove_dir(layout.images().join(install::INCOMING));
    Ok(id)
}

/// Check 11: the boot image's bay for `id` holds the slot. TODO(M35): compare the bay's payload
/// hashes with the bundle's payloads, and refuse a different build (the bay record's
/// `SHA-256` per file). Until M35 merges, only the slot id is compared, and a bay whose payloads
/// differ from the bundle's passes here. The bay sizes are checked by [`check_layout`].
///
/// # Errors
///
/// The reason the bay does not hold `id`.
pub fn check_bay(boot: &mut Boot<Mem>, id: &str) -> Result<(), String> {
    boot.bay_of(id).map(drop).map_err(|_| {
        format!("the boot image holds no bay for slot {id}: build the image from this bundle")
    })
}

/// Opens the boot image's bays for [`check_bay`] and the layout check: what `--verify` needs.
///
/// # Errors
///
/// The reason the image cannot be read as a boot partition.
pub fn open_boot_image(image: &[u8]) -> Result<Boot<Mem>, String> {
    Boot::on(Mem(image.to_vec())).map_err(|e| format!("the boot image: {e}"))
}

/// The boot image's bays are the board file's: the same five roles, the same sizes.
pub fn check_layout(image: &bays::Layout, board: &bays::Layout) -> Result<(), String> {
    for (i, role) in ROLES.iter().enumerate() {
        if image.bytes[i] != board.bytes[i] {
            return Err(format!(
                "the boot image's {} bay is {} bytes; the board file's [boot] says {}: the image was built for another layout",
                role.role, image.bytes[i], board.bytes[i]
            ));
        }
    }
    Ok(())
}

/// The install, the trial and the confirm, through the driver's own methods.
fn drive(slots: &mut SystemSlots, bundle: &[u8], layout: &Layout) -> Result<String, String> {
    let job = slots
        .install(bundle.len() as u64, false, false)
        .map_err(|status| refusal(layout, status))?;
    for (i, piece) in bundle.chunks(STREAM).enumerate() {
        slots
            .write((i * STREAM) as u64, piece)
            .map_err(|status| refusal(layout, status))?;
    }
    let mut out = [0u8; 256];
    let mut reply = Reply::new(&mut out);
    let id = match slots.job(job, &mut reply) {
        JobState::Finished(Status::OK) => {
            let len = reply.len();
            String::from_utf8_lossy(&out[..len]).into_owned()
        }
        JobState::Finished(status) => return Err(refusal(layout, status)),
        JobState::Running { .. } => {
            return Err("the install did not finish: the bundle is short".into());
        }
    };
    slots
        .trial(&id, false, false, false)
        .map_err(|status| refusal(layout, status))?;
    // The board boots into the trial, and the trial works: the same as a restart would do.
    slots.running = Some(id.clone());
    slots
        .confirm_as(resolve::By::Auto)
        .map_err(|status| refusal(layout, status))?;
    Ok(id)
}

/// The reason a step refused: the state's words when the driver gave some, else the status.
fn refusal(layout: &Layout, status: Status) -> String {
    let state = State::load(layout);
    if state.error.is_empty() {
        format!("the driver refused the step: {status:?}")
    } else {
        state.error
    }
}
