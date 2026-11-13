//! The security switch's flow, without the host: judging the stakeholders' answers, writing the
//! record (the truth), rendering the view. The driver calls these with the answers it collected.

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_core::Status;
use lemnos_slot_store::security::{self, PlainFile, Store, Switch, View};

/// One stakeholder's answer: its name and its lines (`missing: ...` or `busy: ...`). No lines is
/// nothing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub from: &'static str,
    pub lines: Vec<String>,
}

/// The fact that stops a change, or `None` when it may go ahead.
pub type Refusal = (Status, String);

/// The lines a check names, for the state and the reply: `ok`, or one line per fact.
#[must_use]
pub fn lines_of(judged: &Result<(), Refusal>) -> Vec<String> {
    match judged {
        Ok(()) => vec!["ok".into()],
        Err((_, text)) => text.lines().map(str::to_string).collect(),
    }
}

/// Judges a change to `on`. `answers` are the stakeholders' lines (already named by their
/// stakeholder), `orion` is whether Orion ships on this board, `trust_root` whether the device's
/// own trust root is present (checked by the caller).
///
/// # Errors
///
/// `BUSY` when a stakeholder reports work in progress (either direction). `NOT_PERMITTED` when
/// turning on is stopped by a missing fact, or by Orion (see the interim below).
pub fn judge(on: bool, orion: bool, answers: &[Answer]) -> Result<(), Refusal> {
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
    /// # Errors
    ///
    /// The refusal from [`judge`], `IO` when the record cannot be written, or `IO` when the boot mark cannot
    /// be written or cleared.
    pub fn set(&mut self, on: bool, answers: &[Answer], boot: Boot<'_>) -> Result<(), Refusal> {
        self.retry_render();
        let result = self.change(on, answers, boot);
        match &result {
            Ok(()) => self.note(vec!["ok".into()]),
            Err(refusal) => self.note(lines_of(&Err(refusal.clone()))),
        }
        result
    }

    fn change(&mut self, on: bool, answers: &[Answer], boot: Boot<'_>) -> Result<(), Refusal> {
        judge(on, self.orion, answers)?;
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
        Ok(())
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

    /// The state, one fact per line.
    #[must_use]
    pub fn state_lines(&self) -> Vec<String> {
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
        ];
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
}

/// Whether a trust root is installed at `path`: a key file with at least one line of 64 hex
/// digits, a space and an owner (the rule `system-slots` and `system-flash` read the same way).
#[must_use]
pub fn trust_root(path: Option<&Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return false;
        }
        match line.split_once(' ') {
            Some((key, owner)) => {
                key.len() == 64
                    && key.bytes().all(|b| b.is_ascii_hexdigit())
                    && !owner.trim().is_empty()
            }
            None => false,
        }
    })
}

#[cfg(test)]
mod tests;
