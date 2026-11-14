//! The security switch's flow, without the host: judging the stakeholders' answers, checking the
//! authorization that turns the switch off, writing the record (the truth), rendering the view. The
//! driver calls these with the answers and facts it collected.

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use lemnos_core::Status;
use lemnos_slot_store::security::{self, PlainFile, Store, Switch, View};
use lemnos_slot_store::signing::{self, Record, security_switch_message};

/// One stakeholder's answer: its name and its lines (`missing: ...` or `busy: ...`). No lines is
/// nothing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub from: &'static str,
    pub lines: Vec<String>,
}

/// The fact that stops a change, or `None` when it may go ahead.
pub type Refusal = (Status, String);

/// How many earlier generations an authorization is tried against, to say which one it was made
/// for. A bounded scan: past it, the reason names no generation.
pub const PROBE_MAX: u64 = 4096;

/// The board's facts a read or a change of the switch needs: its trust root's key file and its
/// identity (the serial, `system.identity/1`'s `serial`; `None` when the board has none).
#[derive(Debug, Clone, Copy, Default)]
pub struct Board<'a> {
    pub keys: Option<&'a Path>,
    pub serial: Option<&'a str>,
}

/// A key the board holds, read from its key file: its key id (SHA-256 of the public key), the key,
/// and whose it is.
#[derive(Debug, Clone)]
pub struct Held {
    pub id: [u8; 32],
    pub key: VerifyingKey,
    pub owner: String,
}

/// The lines a check names, for the state and the reply: `ok`, or one line per fact.
#[must_use]
pub fn lines_of(judged: &std::result::Result<(), Refusal>) -> Vec<String> {
    match judged {
        Ok(()) => vec!["ok".into()],
        Err((_, text)) => text.lines().map(str::to_string).collect(),
    }
}

/// Judges a change to `on`. `answers` are the stakeholders' lines (already named by their
/// stakeholder), `orion` is whether Orion ships on this board. The authorization for an off is
/// checked by [`authorize_off`], not here.
///
/// # Errors
///
/// `BUSY` when a stakeholder reports work in progress (either direction). `NOT_PERMITTED` when
/// turning on is stopped by a missing fact, or by Orion (see the interim below).
pub fn judge(on: bool, orion: bool, answers: &[Answer]) -> std::result::Result<(), Refusal> {
    let mut busy = Vec::new();
    let mut missing = Vec::new();
    for answer in answers {
        for line in &answer.lines {
            if line.starts_with("busy:") {
                busy.push(format!("{}: {line}", answer.from));
            } else if line.starts_with("missing:") {
                missing.push(format!("{}: {line}", answer.from));
            }
        }
    }
    if !busy.is_empty() {
        return Err((Status::BUSY, busy.join("\n")));
    }
    if !on {
        return Ok(());
    }
    // ORION-R3: the contact point Orion's side builds to (designed here, not wired; see
    // docs/slots.md, "The Orion precheck"). The driver asks the Orion bridge's device, bound as
    // `uses = { orion = "<device>" }`, through the same contract the slots and flash answer:
    //   contract `system.security-precondition/1`, method 1 `precondition(on: bool, into: &mut [u8])`.
    // Answers: a text of `missing: ...` / `busy: ...` lines, which refuse (busy either way); no such
    // line, the empty text, is a yes; a status error (no answer, a deadline, `NOT_SUPPORTED`) is a
    // refusal, `missing: it did not answer`. Nothing is assumed about Orion, and a refusal here never
    // changes the switch. Until the device is bound, with Orion on the board, ON is refused by name.
    if orion {
        missing.push(
            "missing: Orion cannot be asked at this revision: the switch cannot be turned on while Orion is on this board"
                .into(),
        );
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err((Status::NOT_PERMITTED, missing.join("\n")))
    }
}

/// The message an authorization signs for this board: the serial, the target `off`, the generation.
fn off_message(serial: &str, generation: u64) -> Vec<u8> {
    security_switch_message(serial.as_bytes(), false, generation)
}

fn verifies(key: &VerifyingKey, message: &[u8], signature: &[u8; 64]) -> bool {
    key.verify_strict(message, &Signature::from_bytes(signature))
        .is_ok()
}

/// Checks the authorization that turns the switch off, for a switch at `generation` on the board
/// whose serial is `serial`, against the keys it holds. The record is the same 108-byte form as
/// every signature (`LMNSSIG1`), over [`security_switch_message`].
///
/// A refusal names the fact that stops it. When the signature is well formed and by a held key but
/// does not verify, the reason says what it was made for, as far as this board can tell: another
/// state (on), an earlier generation (scanned up to [`PROBE_MAX`]), or else another board or a
/// corrupt record (the message binds the serial, and this board cannot see another's).
///
/// # Errors
///
/// `INVALID_ARGUMENT` for a malformed authorization; `NOT_PERMITTED` for everything else.
pub fn authorize_off(
    bytes: &[u8],
    held: &[Held],
    serial: Option<&str>,
    generation: u64,
) -> std::result::Result<(), Refusal> {
    let refuse = |status, text: String| Err((status, text));
    let Some(serial) = serial else {
        return refuse(
            Status::NOT_PERMITTED,
            "missing: this board has no serial, so no authorization can name it: the switch cannot be turned off by software (reflash over USB boot, docs/slots.md)".into(),
        );
    };
    if bytes.is_empty() {
        return refuse(
            Status::NOT_PERMITTED,
            format!(
                "missing: no authorization given: off needs one signed under this board's trust root for generation {generation} (lemnos-ctl security DEVICE request-off)"
            ),
        );
    }
    let record: Record = match signing::parse_record(bytes) {
        Ok(record) => record,
        Err(why) => {
            return refuse(
                Status::INVALID_ARGUMENT,
                format!(
                    "malformed authorization: {why}: a 108-byte signature record is needed (LMNSSIG1, algorithm 1, zero padding)"
                ),
            );
        }
    };
    let Some(key) = held.iter().find(|held| held.id == record.key_id) else {
        return refuse(
            Status::NOT_PERMITTED,
            "signed by a key this board does not hold: the key id is not on this board's trust root".into(),
        );
    };
    if verifies(
        &key.key,
        &off_message(serial, generation),
        &record.signature,
    ) {
        return Ok(());
    }
    if verifies(
        &key.key,
        &security_switch_message(serial.as_bytes(), true, generation),
        &record.signature,
    ) {
        return refuse(
            Status::NOT_PERMITTED,
            "made for another state: it turns the switch on, not off".into(),
        );
    }
    for earlier in (0..generation).rev().take(PROBE_MAX as usize) {
        if verifies(&key.key, &off_message(serial, earlier), &record.signature) {
            return refuse(
                Status::NOT_PERMITTED,
                format!(
                    "made for generation {earlier}, the switch is at {generation}: the switch changed after it was signed"
                ),
            );
        }
    }
    refuse(
        Status::NOT_PERMITTED,
        "signature does not verify for this board: it was made for another board, or it is corrupted".into(),
    )
}

/// The switch's state on this device: the record, the view, and the last check.
pub struct Flow {
    record: PlainFile,
    render_dir: PathBuf,
    orion: bool,
    switch: Switch,
    /// The record could not be read as a value (corrupt or unreadable): the switch reads on.
    corrupt: bool,
    /// The view was not rendered; the record is still the truth and the render is retried.
    view_pending: bool,
    /// The last check or change, one line each (`ok`, or what stopped it).
    last: Vec<String>,
    /// Test seam: a simulated power cut stops the change before this step runs.
    #[cfg(test)]
    fault: Option<Step>,
}

/// The steps of a change, in the order they run. A crash before a step leaves the earlier ones done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The boot side's mark (the stakeholders' `apply`).
    Boot,
    /// The record (the truth).
    Record,
    /// The view the readers read.
    View,
}

/// The boot side's mark for `on`: the stakeholders' `apply`, as one callback. `Err` names what failed.
pub type Boot<'a> = &'a mut dyn FnMut(bool) -> Result<(), String>;

impl Flow {
    /// Opens the record and renders the view. Absent = off, generation 0. Corrupt or unreadable =
    /// on, reported.
    #[must_use]
    pub fn open(record: impl Into<PathBuf>, render_dir: impl Into<PathBuf>, orion: bool) -> Self {
        let record = PlainFile::new(record);
        let (switch, corrupt) = match record.read() {
            Ok(Some(switch)) => (switch, false),
            Ok(None) => (
                Switch {
                    on: false,
                    generation: 0,
                },
                false,
            ),
            Err(_) => (
                Switch {
                    on: true,
                    generation: 0,
                },
                true,
            ),
        };
        let mut flow = Self {
            record,
            render_dir: render_dir.into(),
            orion,
            switch,
            corrupt,
            view_pending: false,
            last: vec!["ok".into()],
            #[cfg(test)]
            fault: None,
        };
        flow.render();
        flow
    }

    /// The switch as it stands.
    #[must_use]
    pub const fn switch(&self) -> Switch {
        self.switch
    }

    /// Whether the view is waiting to be rendered.
    #[must_use]
    pub const fn view_pending(&self) -> bool {
        self.view_pending
    }

    /// Whether Orion ships on this board.
    #[must_use]
    pub const fn orion(&self) -> bool {
        self.orion
    }

    /// Records the last check's lines (the precheck's reply, or a refusal's text).
    pub fn note(&mut self, lines: Vec<String>) {
        self.last = lines;
    }

    /// Changes the switch once the answers have been judged. The order keeps the boot side's mark never
    /// less strict than the record, through a crash before any step (`docs/slots.md`, "The order of a
    /// change"):
    ///
    /// - to on: the boot mark, then the record, then the view;
    /// - to off: the record, then the view, then the boot mark.
    ///
    /// A failed render leaves the view pending (the record stands). A failed boot mark stops a change to on
    /// before the record is written; for a change to off it is reported, and the mark stays on (stricter)
    /// until the next start repairs it.
    ///
    /// An off on a board with a trust root also needs the authorization, checked against the record's
    /// generation as it stands, before any step runs. An authorization given with an on, or with an off
    /// on a board with no trust root, is ignored, and the reply says so.
    ///
    /// # Errors
    ///
    /// The refusal from [`judge`] or [`authorize_off`], `IO` when the record cannot be written, or `IO`
    /// when the boot mark cannot be written or cleared.
    pub fn set(
        &mut self,
        on: bool,
        answers: &[Answer],
        board: Board<'_>,
        authorization: &[u8],
        boot: Boot<'_>,
    ) -> Result<(), Refusal> {
        self.retry_render();
        let result = self.change(on, answers, board, authorization, boot);
        match &result {
            Ok(notes) => {
                let mut lines = vec!["ok".to_string()];
                lines.extend(notes.iter().cloned());
                self.note(lines);
            }
            Err(refusal) => self.note(lines_of(&Err(refusal.clone()))),
        }
        result.map(|_| ())
    }

    fn change(
        &mut self,
        on: bool,
        answers: &[Answer],
        board: Board<'_>,
        authorization: &[u8],
        boot: Boot<'_>,
    ) -> Result<Vec<String>, Refusal> {
        judge(on, self.orion, answers)?;
        let mut notes = Vec::new();
        if on {
            if !authorization.is_empty() {
                notes.push("note: authorization ignored: turning the switch on needs none".into());
            }
        } else {
            let held = held_keys(board.keys);
            if held.is_empty() {
                if !authorization.is_empty() {
                    notes.push(
                        "note: authorization ignored: this board holds no trust root, so off needs none"
                            .into(),
                    );
                }
            } else {
                self.check_off(&held, board.serial, authorization)?;
            }
        }
        if on {
            self.mark(on, boot, "the switch is unchanged")?;
            self.write_record(on)?;
            self.show_view()?;
        } else {
            self.write_record(on)?;
            self.show_view()?;
            self.mark(
                on,
                boot,
                "the switch is off, but the boot side's mark was not cleared, so boot stays strict until the next start",
            )?;
        }
        self.corrupt = false;
        Ok(notes)
    }

    /// The boot side's mark for `on`. `what` says what the caller is left with when it fails.
    fn mark(&mut self, on: bool, boot: Boot<'_>, what: &str) -> Result<(), Refusal> {
        self.at(Step::Boot)?;
        boot(on).map_err(|text| (Status::IO, format!("missing: boot mark: {text}; {what}")))
    }

    /// The record first: the switch's truth, written durably.
    fn write_record(&mut self, on: bool) -> Result<(), Refusal> {
        self.at(Step::Record)?;
        match self.record.write(on) {
            Ok(generation) => {
                self.switch = Switch { on, generation };
                Ok(())
            }
            Err(error) => Err((
                Status::IO,
                format!("missing: the record was not written: {error}"),
            )),
        }
    }

    /// The view the readers read, from the record. A failed render is pending, not a failed change.
    fn show_view(&mut self) -> Result<(), Refusal> {
        self.at(Step::View)?;
        self.render();
        Ok(())
    }

    /// Test seam: stops here when the simulated power cut is at `step`. Never fails in a shipped build.
    fn at(&self, step: Step) -> Result<(), Refusal> {
        let _ = (self, step);
        #[cfg(test)]
        if self.fault == Some(step) {
            return Err((Status::IO, format!("injected: power cut before {step:?}")));
        }
        Ok(())
    }

    /// The start-up repair: the boot side's mark is made to match the record (its value is the switch's).
    /// The view was rendered from the record when the device opened.
    ///
    /// # Errors
    ///
    /// The boot mark's failure, as text.
    pub fn repair(&mut self, boot: Boot<'_>) -> Result<(), String> {
        boot(self.switch.on)
    }

    /// The authorization check for an off, against the record's generation as it is now.
    fn check_off(
        &self,
        held: &[Held],
        serial: Option<&str>,
        authorization: &[u8],
    ) -> std::result::Result<(), Refusal> {
        let generation = match self.record.read() {
            Ok(Some(current)) => current.generation,
            Ok(None) => 0,
            Err(error) => {
                return Err((
                    Status::NOT_PERMITTED,
                    format!(
                        "missing: the record is not readable, so its generation is unknown and no authorization can name it: {error}"
                    ),
                ));
            }
        };
        authorize_off(authorization, held, serial, generation)
    }

    /// Renders the view for the current switch. A failure leaves the view pending.
    pub fn render(&mut self) {
        match security::render_at(&self.render_dir, self.switch) {
            Ok(()) => self.view_pending = false,
            Err(_) => self.view_pending = true,
        }
    }

    /// Retries a pending render (called before each change and each state read).
    pub fn retry_render(&mut self) {
        if self.view_pending {
            self.render();
        }
    }

    /// The state, one fact per line: the switch and its generation, the view, the board's serial,
    /// the trust root and the keys it holds, and the last check.
    #[must_use]
    pub fn state_lines(&self, board: Board<'_>) -> Vec<String> {
        let held = held_keys(board.keys);
        let mut lines = vec![
            format!("switch {}", if self.switch.on { "on" } else { "off" }),
            format!("generation {}", self.switch.generation),
            format!(
                "view {}",
                if self.view_pending {
                    "pending"
                } else {
                    "current"
                }
            ),
            format!("orion {}", if self.orion { "present" } else { "absent" }),
            format!("serial {}", board.serial.unwrap_or("absent")),
            format!(
                "trust root {}",
                if held.is_empty() { "absent" } else { "present" }
            ),
        ];
        for key in &held {
            lines.push(format!("key {} {}", hex(&key.id), key.owner));
        }
        if self.corrupt {
            lines.push("record corrupt: the switch reads on".into());
        }
        // What the readers read: the view file, through the `/etc/board` link. It can differ from the
        // record (a view edited or torn by hand); this line says so.
        lines.push(
            match security::read_at(&self.render_dir.join(security::FILE_NAME)) {
                View::Absent => "view absent: readers read off".into(),
                View::Set(view) => format!(
                    "view {} generation {}: readers read {}",
                    if view.on { "on" } else { "off" },
                    view.generation,
                    if view.on { "on" } else { "off" }
                ),
                View::Corrupt => "view corrupt: readers read on, whatever the record says; \
                `lemnos-ctl security render --record FILE --dir DIR` rewrites it from the record"
                    .into(),
            },
        );
        lines.push("last:".into());
        lines.extend(self.last.iter().map(|line| format!("  {line}")));
        lines
    }

    /// What an authorization to turn the switch off must sign, from the board's own facts (the
    /// `request_off` reply). Without a trust root, one line says off needs none.
    #[must_use]
    pub fn request_off_lines(&self, board: Board<'_>) -> Vec<String> {
        let held = held_keys(board.keys);
        if held.is_empty() {
            return vec!["no trust root: off needs no authorization".into()];
        }
        let Some(serial) = board.serial else {
            return vec![
                "missing: this board has no serial, so no authorization can name it: reflash over USB boot (docs/slots.md)".into(),
            ];
        };
        let generation = match self.record.read() {
            Ok(Some(current)) => current.generation,
            Ok(None) => 0,
            Err(_) => {
                return vec![
                    "missing: the record is not readable, so its generation is unknown".into(),
                ];
            }
        };
        let mut lines = vec![
            "target off".to_string(),
            format!("generation {generation}"),
            format!("serial {serial}"),
            format!("message {}", hex(&off_message(serial, generation))),
        ];
        for key in &held {
            lines.push(format!("key {} {}", hex(&key.id), key.owner));
        }
        lines.push(
            "sign the message with Ed25519 under one of the keys above: the authorization is the 108-byte LMNSSIG1 record (algorithm 1, key id, signature)".into(),
        );
        lines
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The keys the board holds, from its key file: one per line, 64 hex digits, a space, then the
/// owner. Blank lines, lines starting with `#`, and lines that are not a key are skipped. A missing
/// file or no path holds no key.
#[must_use]
pub fn held_keys(path: Option<&Path>) -> Vec<Held> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (hex_key, owner) = line.split_once(char::is_whitespace)?;
            let owner = owner.trim();
            if owner.is_empty() || hex_key.len() != 64 {
                return None;
            }
            let bytes = unhex32(hex_key)?;
            let key = VerifyingKey::from_bytes(&bytes).ok()?;
            Some(Held {
                id: signing::key_id(&bytes),
                key,
                owner: owner.to_string(),
            })
        })
        .collect()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    for (at, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(2 * at..2 * at + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Whether a trust root is installed at `path`: at least one key this board holds (see
/// [`held_keys`]).
#[must_use]
pub fn trust_root(path: Option<&Path>) -> bool {
    !held_keys(path).is_empty()
}

#[cfg(test)]
mod tests;
