//! The board's security switch, served as `system.security/1` (`docs/slots.md`, "The security
//! switch"). One value per board, off by default. The truth is a record (`lemnos_slot_store`'s
//! `security` module); the readers see the view it renders. This driver is the one writer.
//!
//! # Wiring
//!
//! - `uses = { slots = "<id>", flash = "<id>", identity = "<id>", orion = "<id>" }` names the stakeholders:
//!   the `system-slots`, `system-flash` and `orion-precheck` devices, which serve
//!   `system.security-precondition/1`. Each answers only for itself. A board with neither slots nor flash
//!   bound judges on the trust root alone. `identity` is the `system-identity` device: its serial names
//!   this board in an off's authorization. `orion` is the `orion-precheck` device (`docs/drivers.md`): on
//!   a board whose `[security]` says `orion = true` it is asked for a change to on and off, and on such a
//!   board a change to on is refused by name while it is not bound. A board that says `orion = false` is
//!   not asked.
//!
//! `slots` and `flash` are optional. A board with slots and no flash device names `slots` only: `precheck`
//! and `set` ask `slots` alone, `flash` is not asked and is not a fault, and the switch adds no trust-root
//! answer of its own while `slots` answers. Naming a device the board does not have is refused at
//! validation by name.
//!
//! On a board whose `[security]` says `orion = false`, `state` says `orion: not covered by this switch on
//! this board (the board file says orion = false)` and a `set` to on replies the same sentence after
//! `note: `.
//!
//! ```toml
//! [[devices]]
//! id = "security"
//! driver = "system-security"
//! uses = { slots = "slots", flash = "flash", identity = "identity" }
//! # no config: record, keys and orion come from the board file's [security] table
//! ```
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `record` | required | The record file (two copies; `docs/slots.md`). On a board with slots it moves to the boot partition once I5i1 lands. |
//! | `keys` | none | The board's one trust root (`[security] keys`, given by the host to this device and to `system-slots` and `system-flash`): the keys that verify slots, flash images and an off's authorization. No keys: off needs no authorization, and `state` says `trust root absent`. Read at each change, and by `state`. |
//! | `orion` | `false` | Whether Orion ships on this board: its answer is then needed for `on`, and `on` is refused unless Orion says yes, and while no `orion` device is bound. |
//!
//! The view is not configurable: it is rendered into `/run/board`, which the image links `/etc/board`
//! to. A board file that names `run` (or `security_view`) is refused: no board file can point the
//! switch's readers or its render somewhere else.

pub mod flow;

use std::path::PathBuf;

use flow::{Answer, Board, Flow, ORION_NOT_COVERED, SwitchRecord, judge, lines_of, trust_root};
use lemnos_contract_security::Security;
use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_contract_system_identity::{MAX_TEXT as IDENTITY_MAX, SystemIdentity};
use lemnos_core::{Config, ConfigError, Dev, Key, Kind, Lifecycle, Result, Status, driver};
use lemnos_slot_store::boot_node;
use lemnos_slot_store::security::{BootSwitch, PlainFile, RUN_DIR};

/// The `record` value that names the boot partition's `SECURE.REC` (slot boards).
const BOOT_RECORD: &str = "boot";

/// The most a stakeholder's answer may say.
const ANSWER_MAX: usize = lemnos_contract_security_precondition::TEXT_MAX;

#[driver("system-security", provides = [Security])]
pub struct SystemSecurity {
    /// The `system-slots` device, when the board has slots.
    #[needs(optional)]
    slots: Option<Dev<dyn SecurityPrecondition>>,
    /// The `system-flash` device, when the board has a first-flash target.
    #[needs(optional)]
    flash: Option<Dev<dyn SecurityPrecondition>>,
    /// The `system-identity` device, when the board has one: its serial names the board.
    #[needs(optional)]
    identity: Option<Dev<dyn SystemIdentity>>,
    /// The `orion-precheck` device, when the board has one: Orion's answer for the switch.
    #[needs(optional)]
    orion: Option<Dev<dyn SecurityPrecondition>>,
    flow: Flow,
    keys: Option<PathBuf>,
}

/// The settings the driver reads. The board file gives none of them: the host puts the `[security]`
/// table in as `record`, `keys` and `orion` when it binds the device (a device that names one is
/// refused there), so a board file's `config` for this device holds nothing. `record` is checked
/// when the device starts, as the host gives it.
const KEYS: &[Key] = &[
    Key::optional("record", Kind::Str),
    Key::optional("keys", Kind::Str),
    Key::optional("orion", Kind::Bool),
];

impl Lifecycle for SystemSecurity {
    type Needs = SystemSecurityNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)
    }

    fn start(needs: SystemSecurityNeeds, cfg: &Config<'_>) -> Result<Self> {
        let record = cfg.str("record").ok_or(Status::INVALID_ARGUMENT)?;
        if cfg.get("run").is_some() || cfg.get("security_view").is_some() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let board_orion = cfg.bool("orion").unwrap_or(false);
        // `record = "boot"`: the boot partition's `SECURE.REC`, on the node the slots driver finds from
        // the same `boot_sources` (`docs/slots.md`, "The security switch"). A node that cannot be found
        // is kept as the record's reason: the switch then reads on, reported.
        let record = if record == BOOT_RECORD {
            let node = match cfg.list("boot_sources") {
                Some(items) => boot_node::node_for_items(items),
                None => Err("boot_sources is not set, so the boot partition can't be found".into()),
            };
            match node {
                Ok(node) => SwitchRecord::Boot(BootSwitch::new(node)),
                Err(why) => SwitchRecord::Unreachable(format!(
                    "the switch's record can't be reached ({why}): the switch reads as on"
                )),
            }
        } else {
            SwitchRecord::File(PlainFile::new(record))
        };
        let mut flow = Flow::open_record(record, RUN_DIR, board_orion);
        flow.bind_orion(needs.orion.is_some());
        let mut dev = Self {
            slots: needs.slots,
            flash: needs.flash,
            identity: needs.identity,
            orion: needs.orion,
            flow,
            keys: cfg.str("keys").map(PathBuf::from),
        };
        // The start-up repair: the boot side's marks follow the record, whatever a crash left.
        let on = dev.flow.switch().on;
        if let Err(text) = apply_to(dev.slots.as_mut(), dev.flash.as_mut(), on) {
            dev.flow
                .note(vec![format!("boot mark not repaired: {text}")]);
        }
        Ok(dev)
    }
}

impl SystemSecurity {
    /// The board's serial, from the identity device; `None` when there is none or it does not
    /// answer.
    fn serial(&mut self) -> Option<String> {
        let dev = self.identity.as_mut()?;
        let mut buffer = [0u8; IDENTITY_MAX];
        let n = dev.serial(&mut buffer).ok()?;
        String::from_utf8(buffer[..n].to_vec()).ok()
    }

    /// Every stakeholder's answer for a change to `on`, and the switch's own trust root when no
    /// stakeholder is bound.
    fn answers(&mut self, on: bool) -> Vec<Answer> {
        let mut answers = Vec::new();
        if let Some(dev) = self.slots.as_mut() {
            answers.push(ask("slots", dev, on));
        }
        if let Some(dev) = self.flash.as_mut() {
            answers.push(ask("flash", dev, on));
        }
        // Asked only on a board whose file says Orion ships: a board without it is not asked.
        if self.flow.orion()
            && let Some(dev) = self.orion.as_mut()
        {
            answers.push(ask("orion", dev, on));
        }
        if on && self.slots.is_none() && self.flash.is_none() && !trust_root(self.keys.as_deref()) {
            answers.push(Answer {
                from: "security",
                lines: vec![
                    "missing: no trust root: the board names no key file that has a valid line (`keys` in the board file's [security] table)"
                        .into(),
                ],
            });
        }
        answers
    }
}

/// One stakeholder's answer. A stakeholder that does not answer stops the change: nothing is
/// assumed about it.
fn ask(from: &'static str, dev: &mut Dev<dyn SecurityPrecondition>, on: bool) -> Answer {
    let mut buffer = [0u8; ANSWER_MAX];
    let lines = match dev.precondition(on, &mut buffer) {
        Ok(n) => String::from_utf8_lossy(&buffer[..n])
            .lines()
            .filter(|line| line.starts_with("missing:") || line.starts_with("busy:"))
            .map(str::to_string)
            .collect(),
        Err(status) => vec![format!("missing: it did not answer ({status:?})")],
    };
    Answer { from, lines }
}

/// Makes each bound stakeholder's boot-side mark match `on` (its `apply`). The first failure is
/// reported by name; the others are not tried after it.
fn apply_to(
    slots: Option<&mut Dev<dyn SecurityPrecondition>>,
    flash: Option<&mut Dev<dyn SecurityPrecondition>>,
    on: bool,
) -> std::result::Result<(), String> {
    for (name, dev) in [("slots", slots), ("flash", flash)] {
        if let Some(dev) = dev {
            dev.apply(on)
                .map_err(|status| format!("{name} did not apply the mark: {status:?}"))?;
        }
    }
    Ok(())
}

/// Writes `text` into the caller's buffer, or says it is too small.
fn copy_out(text: &str, into: &mut [u8]) -> Result<usize> {
    let bytes = text.as_bytes();
    into.get_mut(..bytes.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(bytes);
    Ok(bytes.len())
}

impl Security for SystemSecurity {
    fn state(&mut self, into: &mut [u8]) -> Result<usize> {
        self.flow.retry_render();
        let serial = self.serial();
        let board = Board {
            keys: self.keys.as_deref(),
            serial: serial.as_deref(),
        };
        let mut text = self.flow.state_lines(board).join("\n");
        text.push('\n');
        copy_out(&text, into)
    }

    fn precheck(&mut self, on: bool, into: &mut [u8]) -> Result<usize> {
        let answers = self.answers(on);
        let mut lines = lines_of(&judge(on, self.flow.orion_unasked(), &answers));
        if !on && trust_root(self.keys.as_deref()) {
            lines.push(
                "note: off needs an authorization made for the current generation (request_off says what it signs)"
                    .into(),
            );
        }
        self.flow.note(lines.clone());
        let mut text = lines.join("\n");
        text.push('\n');
        copy_out(&text, into)
    }

    fn set(&mut self, on: bool, authorization: &[u8], into: &mut [u8]) -> Result<usize> {
        let answers = self.answers(on);
        let serial = self.serial();
        let board = Board {
            keys: self.keys.as_deref(),
            serial: serial.as_deref(),
        };
        let mut boot = |on: bool| apply_to(self.slots.as_mut(), self.flash.as_mut(), on);
        self.flow
            .set(on, &answers, board, authorization, &mut boot)
            .map_err(|(status, _)| status)?;
        // A change to on is made; say what the switch does not cover. With Orion on the board the
        // change is refused above, so this is the `orion = false` board only.
        if on && !self.flow.orion() {
            let mut text = format!("note: {ORION_NOT_COVERED}");
            text.push('\n');
            return copy_out(&text, into);
        }
        Ok(0)
    }

    fn request_off(&mut self, into: &mut [u8]) -> Result<usize> {
        let serial = self.serial();
        let board = Board {
            keys: self.keys.as_deref(),
            serial: serial.as_deref(),
        };
        let mut text = self.flow.request_off_lines(board).join("\n");
        text.push('\n');
        copy_out(&text, into)
    }
}
