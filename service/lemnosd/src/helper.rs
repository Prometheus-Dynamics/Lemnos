//! The gadget helper: the process that keeps root after the root phase, so the daemon can make
//! and bind its USB gadgets whenever they become possible, and never needs root itself.
//!
//! # Who runs it
//!
//! The root phase forks a small root parent (`supervisor.rs`, `ps` shows `lemnosd-gsuper`) before the
//! daemon drops to its user, and the parent forks the helper (`lemnosd-gadget`) and forks it again
//! when it dies, so a helper crash needs neither the daemon nor a person. The helper is the same
//! binary. It drops everything but `CAP_SYS_ADMIN` (which the FunctionFS mount and its unmount
//! need; configfs needs no capability for a root process that owns what it writes) and keeps uid 0,
//! the owner of the configfs tree and the mount point the setup makes. It ignores `SIGINT`,
//! `SIGTERM` and `SIGHUP`, so a signal to the daemon's process group does not take it, and it runs
//! under a seccomp allowlist ([`BASE_CALLS`]).
//!
//! # What it holds
//!
//! The gadgets of the board file, fixed when the root phase forks the parent: each gadget's
//! controller name, the owner of its mount, its settings and the board's serial source (the
//! identity device's settings). Nothing it does reads a path, a name, a string or a value from the
//! daemon after that. Every file it touches is an entry of a directory it opened once
//! (`lemnos_linux::Trees`, `lemnos_linux_sys::confined`).
//!
//! # The vocabulary
//!
//! A request is one line, `VERB INDEX SEQ`, where `VERB` is `setup`, `teardown`, `bind` or `unbind`,
//! `INDEX` a gadget's index in the fixed list and `SEQ` the daemon's count of its requests, both as
//! canonical decimal (no sign, no leading zero). Nothing else is acted on: any other line is
//! answered `0 refused` and nothing runs.
//!
//! - `setup N`: reads the board serial from its fixed source, and makes gadget `N`'s tree and
//!   FunctionFS mount, or adopts the tree that is there when it is the same (`Trees::setup`). Fails
//!   with the errno of the step that failed, when the controller, configfs or the module is not
//!   there, or `ENODATA` when the serial cannot be read yet.
//! - `bind N`: writes the controller's name to the gadget's `UDC`.
//! - `unbind N`: writes an empty line to it.
//! - `teardown N`: unbinds, removes the gadget's tree and unmounts its FunctionFS. The daemon sends
//!   it for every gadget when it stops.
//!
//! The reply is one line that starts with the request's `SEQ`: `SEQ ok`, `SEQ fail ERRNO REASON` (the
//! errno and a line of text for the daemon's log), or `SEQ refused REASON`. A successful `setup`
//! says whether the tree it made or adopted carries a USB serial string: `SEQ ok serial` or
//! `SEQ ok no-serial` (the closed set; the daemon takes any other `setup` reply as a failed setup,
//! by name, and any `ok` word after another verb likewise). The daemon reads
//! replies until it finds its own `SEQ`, so a reply to a request it gave up on, from a helper that
//! was slow or was restarted, is dropped. No file descriptor passes: the daemon opens `ep0` and the
//! endpoint files itself, as the FunctionFS mount is owned by its user.
//!
//! # Whom it acts for
//!
//! Only the daemon that started it. The socket pair was made by the root phase, so the kernel
//! records the daemon's process as the creator of both ends (`SO_PEERCRED`); the helper checks, for
//! every request, that the peer's process id and user id are the ones it was started for, and
//! refuses by name (`refused: not the lemnosd that started this helper`) when they are not. Nothing
//! else can reach it: the socket has no name in the file system.
//!
//! # When it dies
//!
//! The parent forks it again, after 1, 2, 4 and then 8 seconds, for as long as the daemon runs. A
//! request that was in flight is answered by nobody: the daemon's call fails, the gadget's driver
//! retries on its backoff, and the new helper adopts the tree the old one left.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::gadget::{Entries, Unready, settings_now};
use lemnos_linux::{
    GadgetHelper, Mounts, Outcome, Owner, SerialString, SharedHelper, SystemMounts, Trees, errno_of,
};
use lemnos_linux_sys::privilege::{self, CAP_SYS_ADMIN, Plan};
use lemnos_linux_sys::seccomp::{self, nr};
use lemnos_linux_sys::unix::peer_cred;

/// The process name the helper is given: what `ps` shows for it.
pub const PROCESS_NAME: &str = "lemnosd-gadget";
/// The most a request line may hold. The longest request is `teardown 999999 4294967295`.
const MAX_LINE: usize = 32;
/// How long the daemon waits for an answer before it gives the call up. The helper may be starting
/// again (the parent waits at most 8 s), so this is more than that.
const ANSWER_WAIT: Duration = Duration::from_secs(10);
/// The most of a reply's text the daemon keeps.
const MAX_REASON: usize = 160;
/// Signals the helper ignores, so that a signal to the daemon's process group does not take it.
const IGNORED: [i32; 3] = [SIGHUP, SIGINT, SIGTERM];
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
/// `EINVAL`, the errno of settings the driver refuses.
const EINVAL: i32 = 22;
/// `EIO`, the errno a reply without one of its own carries.
const EIO: i32 = 5;
/// `ENODATA`, the errno of a setup that waits for the board serial.
const ENODATA: i32 = 61;
/// What a refusal of a request from anyone else says.
pub const NOT_THE_HOLDER: &str = "not the lemnosd that started this helper";

/// The capabilities the helper keeps: the mount and its unmount.
pub const KEEP: &[u32] = &[CAP_SYS_ADMIN];

/// The system calls the helper may make once its filter is installed: exactly what its four verbs
/// make on a Linux system (configfs and FunctionFS through `*at` calls on directory descriptors, the
/// FunctionFS mount and its unmount, the socket's reads and writes and its peer's credentials, the
/// allocator and the runtime's signal set-up, and the exit). Any other call kills the helper
/// (`SIGSYS`). The list is checked by `every_verb_runs_under_the_filter` against fake roots, and is
/// written out in `docs/daemon.md`.
pub const BASE_CALLS: &[i64] = &[
    nr::READ,
    nr::WRITE,
    nr::CLOSE,
    nr::OPENAT,
    nr::OPENAT2,
    nr::MKDIRAT,
    nr::UNLINKAT,
    nr::SYMLINKAT,
    nr::READLINKAT,
    nr::NEWFSTATAT,
    nr::FSTAT,
    nr::FSTATFS,
    nr::STATX,
    nr::GETDENTS64,
    nr::LSEEK,
    nr::FSYNC,
    nr::FCHOWNAT,
    nr::FCHMODAT,
    nr::MOUNT,
    nr::UMOUNT2,
    nr::GETSOCKOPT,
    nr::RECVFROM,
    nr::SENDTO,
    nr::MMAP,
    nr::MUNMAP,
    nr::MREMAP,
    nr::MPROTECT,
    nr::MADVISE,
    nr::BRK,
    nr::RT_SIGACTION,
    nr::RT_SIGPROCMASK,
    nr::RT_SIGRETURN,
    nr::SIGALTSTACK,
    nr::CLOCK_GETTIME,
    nr::GETRANDOM,
    nr::EXIT,
    nr::EXIT_GROUP,
    nr::FCNTL,
];

/// The calls the helper may make: [`BASE_CALLS`], and on x86_64 the calls libc makes under their own
/// names there.
#[must_use]
pub fn calls() -> Vec<i64> {
    seccomp::calls_for_helper(BASE_CALLS)
}

/// The verbs of the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Setup,
    Teardown,
    Bind,
    Unbind,
}

impl Verb {
    /// The verb as a request spells it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Teardown => "teardown",
            Self::Bind => "bind",
            Self::Unbind => "unbind",
        }
    }
}

/// One gadget as the helper holds it: fixed at fork.
pub struct Gadget {
    /// The controller's name, as `/sys/class/udc` lists it (`udc:` removed).
    pub udc: String,
    /// The user and group the FunctionFS mount is made for: the daemon's.
    pub owner: Owner,
    /// The board file's `[board] id`, which the derived addresses are salted with.
    pub board: String,
    /// The gadget device's settings, without the serial keys the daemon writes.
    pub config: Entries,
    /// The identity device's settings: the serial source, which the helper reads at each setup.
    pub identity: Option<Entries>,
}

/// One request: its verb, the gadget's index and the daemon's sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub verb: Verb,
    pub index: u32,
    pub seq: u32,
}

/// Reads one number as canonical decimal: no sign, no leading zero, at most `digits` digits.
fn canonical(text: &str, digits: usize) -> Option<u32> {
    let ok = !text.is_empty()
        && text.len() <= digits
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if ok { text.parse().ok() } else { None }
}

/// Reads one request from `line` (the bytes before its newline). Only the closed vocabulary is
/// accepted: `VERB INDEX SEQ`, each number as canonical decimal.
#[must_use]
pub fn parse(line: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split(' ');
    let verb = match parts.next()? {
        "setup" => Verb::Setup,
        "teardown" => Verb::Teardown,
        "bind" => Verb::Bind,
        "unbind" => Verb::Unbind,
        _ => return None,
    };
    let index = canonical(parts.next()?, 6)?;
    let seq = canonical(parts.next()?, 10)?;
    if parts.next().is_some() {
        return None;
    }
    Some(Request { verb, index, seq })
}

/// What the helper does to a gadget. The system's implementation is [`System`]; a test passes a
/// fake that records the calls.
pub trait Ops {
    fn setup(&mut self, gadget: &Gadget) -> io::Result<SerialString>;
    fn teardown(&mut self, gadget: &Gadget) -> io::Result<()>;
    fn bind(&mut self, gadget: &Gadget) -> io::Result<()>;
    fn unbind(&mut self, gadget: &Gadget) -> io::Result<()>;
}

/// The system's gadget operations, on directories opened once (configfs, the FunctionFS mount
/// points' directory, the controllers' directory) and the system's mounts.
pub struct System<M: Mounts = SystemMounts> {
    trees: Trees,
    mounts: M,
}

impl System<SystemMounts> {
    /// The system's roots: configfs at `/sys/kernel/config`, the FunctionFS mount points under
    /// `/dev/usb-ffs`, the controllers at `/sys/class/udc`. A configfs directory that is not a
    /// mounted configfs is taken as not there yet.
    #[must_use]
    pub fn new() -> Self {
        Self::over(
            Trees::new(&crate::gadget::system_roots()).verifying_configfs(),
            SystemMounts,
        )
    }
}

impl Default for System<SystemMounts> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Mounts> System<M> {
    /// Operations on `trees`, with `mounts` as the mount calls.
    #[must_use]
    pub fn over(trees: Trees, mounts: M) -> Self {
        Self { trees, mounts }
    }

    /// Opens the directories that can be opened now; the others are tried again at each setup.
    pub fn open(&mut self) {
        let _ = self.trees.open();
    }
}

/// A setup that cannot go on, as the reply carries it.
fn unready(why: Unready) -> io::Error {
    match why {
        Unready::Serial(reason) => lemnos_linux::gadget_fault(
            ENODATA,
            format!("the board serial is not readable yet: {reason}"),
        ),
        Unready::Refused(reason) => {
            lemnos_linux::gadget_fault(EINVAL, format!("the settings are refused: {reason}"))
        }
    }
}

impl<M: Mounts> Ops for System<M> {
    fn setup(&mut self, gadget: &Gadget) -> io::Result<SerialString> {
        let (set, functions) = settings_now(gadget).map_err(unready)?;
        let outcome = self.trees.setup_for_helper(
            &gadget.udc,
            gadget.owner,
            set.bytes(),
            functions.as_slice(),
            &mut self.mounts,
        )?;
        match outcome {
            Outcome::Made => {}
            Outcome::Adopted => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd-gadget: gadget {}: the tree that was there is the board file's: adopted",
                    gadget.udc
                );
            }
            Outcome::Remade => {
                let _ = writeln!(
                    io::stderr(),
                    "lemnosd-gadget: gadget {}: the tree that was there differed and was unbound: made again",
                    gadget.udc
                );
            }
        }
        // Told from the tree as it is now, so an adopted tree's own string counts.
        self.trees.serial_string(&gadget.udc)
    }

    fn teardown(&mut self, gadget: &Gadget) -> io::Result<()> {
        self.trees.teardown(&gadget.udc, &mut self.mounts)
    }

    fn bind(&mut self, gadget: &Gadget) -> io::Result<()> {
        self.trees.bind(&gadget.udc)
    }

    fn unbind(&mut self, gadget: &Gadget) -> io::Result<()> {
        self.trees.unbind(&gadget.udc)
    }
}

/// The reply to a `setup` whose tree carries a serial string.
const REPLY_SERIAL: &str = "ok serial";
/// The reply to a `setup` whose tree has none.
const REPLY_NO_SERIAL: &str = "ok no-serial";

/// The fake-free part of one request: what it does, and its reply (after the sequence number).
fn act(verb: Verb, gadget: &Gadget, ops: &mut dyn Ops) -> String {
    let result = match verb {
        Verb::Setup => ops.setup(gadget).map(|serial| match serial {
            SerialString::Set => REPLY_SERIAL,
            SerialString::Absent => REPLY_NO_SERIAL,
        }),
        Verb::Teardown => ops.teardown(gadget).map(|()| "ok"),
        Verb::Bind => ops.bind(gadget).map(|()| "ok"),
        Verb::Unbind => ops.unbind(gadget).map(|()| "ok"),
    };
    match result {
        Ok(reply) => reply.to_owned(),
        Err(error) => reply_fail(&error),
    }
}

/// A `fail` reply: the errno, then one line of printable text.
fn reply_fail(error: &io::Error) -> String {
    let errno = errno_of(error).unwrap_or(EIO);
    let text: String = error
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(MAX_REASON)
        .collect();
    format!("fail {errno} {text}")
}

/// One line from the stream, without its newline. A line longer than [`MAX_LINE`] is read to its
/// end and refused.
enum Line {
    Eof,
    Long,
    Text(Vec<u8>),
}

fn read_line(stream: &mut UnixStream) -> io::Result<Line> {
    let mut line = Vec::with_capacity(MAX_LINE);
    let mut long = false;
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Ok(Line::Eof);
        }
        if byte[0] == b'\n' {
            return Ok(if long { Line::Long } else { Line::Text(line) });
        }
        if line.len() < MAX_LINE {
            line.push(byte[0]);
        } else {
            long = true;
        }
    }
}

/// Whose requests the helper acts on.
pub trait Holder {
    /// `Ok` when the peer is the process the helper was started for; otherwise the reason.
    ///
    /// # Errors
    ///
    /// The peer is someone else, or cannot be told.
    fn check(&self, stream: &UnixStream) -> Result<(), String>;
}

/// The daemon that made the socket pair: its process id and user id, as the kernel recorded them
/// for the pair (`SO_PEERCRED`). The root phase makes the pair as root, so the user id is 0.
pub struct Daemon {
    /// The daemon's process id.
    pub pid: i32,
    /// The user id the daemon made the pair as.
    pub uid: u32,
}

impl Holder for Daemon {
    fn check(&self, stream: &UnixStream) -> Result<(), String> {
        let peer = peer_cred(stream.as_fd()).map_err(|error| {
            format!("{NOT_THE_HOLDER}: its credentials cannot be read ({error})")
        })?;
        if peer.pid == self.pid && peer.uid == self.uid {
            Ok(())
        } else {
            Err(format!(
                "{NOT_THE_HOLDER} (peer pid {} uid {}, expected pid {} uid {})",
                peer.pid, peer.uid, self.pid, self.uid
            ))
        }
    }
}

/// Answers requests until the daemon's end closes. A request outside the vocabulary runs nothing,
/// and neither does one from a peer that is not `holder`.
///
/// # Errors
///
/// The socket fails for a reason other than the daemon's end closing.
pub fn serve(
    stream: &mut UnixStream,
    gadgets: &[Gadget],
    ops: &mut dyn Ops,
    holder: &dyn Holder,
) -> io::Result<()> {
    loop {
        let (seq, reply) = match read_line(stream)? {
            Line::Eof => return Ok(()),
            Line::Long => (0, "refused a request that is too long".to_owned()),
            Line::Text(line) => match parse(&line) {
                None => (0, "refused a request outside the vocabulary".to_owned()),
                Some(request) => {
                    let reply = match holder.check(stream) {
                        Err(why) => format!("refused {why}"),
                        Ok(()) => match gadgets.get(request.index as usize) {
                            Some(gadget) => act(request.verb, gadget, ops),
                            None => "refused a gadget that is not in the list".to_owned(),
                        },
                    };
                    (request.seq, reply)
                }
            },
        };
        stream.write_all(format!("{seq} {reply}\n").as_bytes())?;
    }
}

/// The helper process's body, run in the forked child of the parent: names itself, ignores the
/// signals that would take it down with the group, drops to what it keeps, narrows its system
/// calls, and serves until the daemon's end closes. Never returns.
pub fn run_child(mut stream: UnixStream, gadgets: &[Gadget], daemon: Daemon) -> ! {
    let mut ok = privilege::set_name(PROCESS_NAME).is_ok() && privilege::die_with_parent().is_ok();
    for signal in IGNORED {
        ok &= privilege::ignore_signal(signal).is_ok();
    }
    let plan = Plan {
        uid: 0,
        gid: 0,
        groups: Vec::new(),
        keep: KEEP.to_vec(),
    };
    ok &= privilege::drop_to(&plan).is_ok();
    ok &= privilege::observe()
        .map(|now| privilege::verify(&plan, &now).is_ok())
        .unwrap_or(false);
    if !ok {
        let _ = writeln!(
            io::stderr(),
            "{PROCESS_NAME}: cannot take its name, its signals or its capabilities: ending"
        );
        std::process::exit(1);
    }
    // The directories it works in are opened now, once, where they can be (configfs may not be
    // mounted yet: the first setup that finds it there opens it). Before the filter, which
    // allows the calls a setup makes and no others.
    let mut ops = System::new();
    ops.open();
    // Last, once every step above has run: from here the helper makes only the calls in `calls()`.
    // A filter that cannot be installed ends the helper, rather than letting it run unfiltered.
    if let Err(error) = seccomp::install(&calls()) {
        let _ = writeln!(
            io::stderr(),
            "{PROCESS_NAME}: cannot install its filter ({error}): ending"
        );
        std::process::exit(1);
    }
    let code = match serve(&mut stream, gadgets, &mut ops, &daemon) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(
                io::stderr(),
                "{PROCESS_NAME}: the socket failed ({error}): ending"
            );
            1
        }
    };
    std::process::exit(code);
}

/// The daemon's end of the helper: one request at a time, each answered before the next. A call the
/// helper does not answer in time fails and nothing else changes: the helper may be starting again,
/// and its parent keeps its end of the socket, so the next call is served by the new one. Only a
/// socket whose other end is gone for good (the parent was killed) fails every call after it.
pub struct Client {
    stream: UnixStream,
    wait: Duration,
    dead: Option<String>,
    /// The sequence number of the last request.
    seq: u32,
    /// Each gadget's controller name and index, as the board's gadget devices name them.
    gadgets: Vec<(String, u32)>,
}

impl Client {
    /// The daemon's end, with the names the gadgets were forked with.
    ///
    /// # Errors
    ///
    /// The socket cannot be given its wait.
    pub fn new(stream: UnixStream, gadgets: Vec<(String, u32)>) -> io::Result<Self> {
        Self::waiting(stream, gadgets, ANSWER_WAIT)
    }

    /// As [`Client::new`], with the wait for an answer given.
    ///
    /// # Errors
    ///
    /// The socket cannot be given its wait.
    pub fn waiting(
        stream: UnixStream,
        gadgets: Vec<(String, u32)>,
        wait: Duration,
    ) -> io::Result<Self> {
        stream.set_read_timeout(Some(wait))?;
        Ok(Self {
            stream,
            wait,
            dead: None,
            seq: 0,
            gadgets,
        })
    }

    /// Each gadget's controller name and index, for the board.
    #[must_use]
    pub fn gadgets(&self) -> &[(String, u32)] {
        &self.gadgets
    }

    fn name_of(&self, index: u32) -> &str {
        self.gadgets
            .iter()
            .find(|(_, i)| *i == index)
            .map_or("?", |(udc, _)| udc.as_str())
    }

    /// Sends one request and reads its answer, passing over answers to earlier requests. The
    /// reason of a failed answer is written to the error output, as the daemon's log, with the verb
    /// and the gadget.
    fn call(&mut self, verb: Verb, index: u32) -> io::Result<Reply> {
        if let Some(why) = &self.dead {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, why.clone()));
        }
        self.seq = self.seq.checked_add(1).unwrap_or(1);
        let seq = self.seq;
        let request = format!("{} {index} {seq}\n", verb.word());
        if let Err(error) = self.stream.write_all(request.as_bytes()) {
            return Err(self.gone(&error));
        }
        let answer = loop {
            match read_answer(&mut self.stream) {
                Ok((got, text)) if got == seq => break text,
                // An answer to a request the daemon gave up on, or the rest of a line cut by a
                // timeout: not this call's.
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    let why = format!(
                        "the gadget helper did not answer {} for {} within {} ms: it may be starting again",
                        verb.word(),
                        self.name_of(index),
                        self.wait.as_millis()
                    );
                    let _ = writeln!(io::stderr(), "lemnosd: {why}");
                    return Err(io::Error::new(io::ErrorKind::TimedOut, why));
                }
                Err(error) => return Err(self.gone(&error)),
            }
        };
        // The closed set: `ok` for every verb, and for `setup` only the two that say what the tree
        // carries. Anything else that is not a `fail` is not accepted.
        let accepted = match (verb, answer.as_str()) {
            (Verb::Setup, REPLY_SERIAL) => Some(Reply::Serial(SerialString::Set)),
            (Verb::Setup, REPLY_NO_SERIAL) => Some(Reply::Serial(SerialString::Absent)),
            (Verb::Teardown | Verb::Bind | Verb::Unbind, "ok") => Some(Reply::Done),
            _ => None,
        };
        if let Some(reply) = accepted {
            return Ok(reply);
        }
        if answer == "ok" || answer.starts_with("ok ") {
            // An `ok` that is not one of the words this verb has: a failed call, by name.
            let why = format!(
                "the gadget helper's reply to {} is not in its vocabulary: `{answer}`",
                verb.word()
            );
            let _ = writeln!(
                io::stderr(),
                "lemnosd: gadget {}: {why}",
                self.name_of(index)
            );
            return Err(io::Error::other(why));
        }
        if let Some(rest) = answer.strip_prefix("fail ") {
            let (errno, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            let errno: i32 = errno.parse().unwrap_or(EIO);
            let _ = writeln!(
                io::stderr(),
                "lemnosd: gadget {}: {} failed: {reason}",
                self.name_of(index),
                verb.word()
            );
            return Err(io::Error::from_raw_os_error(errno));
        }
        let _ = writeln!(
            io::stderr(),
            "lemnosd: gadget {}: {} {answer}",
            self.name_of(index),
            verb.word()
        );
        Err(io::Error::other(format!("the gadget helper {answer}")))
    }

    /// The socket's other end is gone for good: the helper's parent ended. Said once.
    fn gone(&mut self, error: &io::Error) -> io::Error {
        let why = format!(
            "the gadget helper's parent is gone ({error}): the USB gadget cannot be set up again until the daemon restarts"
        );
        let _ = writeln!(io::stderr(), "lemnosd: {why}");
        self.dead = Some(why.clone());
        io::Error::new(io::ErrorKind::BrokenPipe, why)
    }

    /// Takes every gadget down, for good (the daemon is stopping). Each failure is reported, and
    /// the rest are still tried.
    pub fn teardown_all(&mut self) {
        for index in 0..self.gadgets.len() {
            let _ = self.call(Verb::Teardown, index as u32);
        }
    }
}

/// What an accepted reply said.
enum Reply {
    /// `ok`.
    Done,
    /// `ok serial` or `ok no-serial`.
    Serial(SerialString),
}

/// The helper's answer: its sequence number, then the rest of the line, without its newline. A line
/// that does not start with a number is an answer to nothing: `0`.
fn read_answer(stream: &mut UnixStream) -> io::Result<(u32, String)> {
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the helper's end closed",
            ));
        }
        if byte[0] == b'\n' {
            let text = String::from_utf8_lossy(&line).into_owned();
            let (seq, rest) = text.split_once(' ').unwrap_or((&text, ""));
            return Ok((seq.parse().unwrap_or(0), rest.to_owned()));
        }
        if line.len() < MAX_REASON + 48 {
            line.push(byte[0]);
        }
    }
}

impl GadgetHelper for Client {
    fn setup(&mut self, index: u32) -> io::Result<SerialString> {
        match self.call(Verb::Setup, index)? {
            Reply::Serial(serial) => Ok(serial),
            Reply::Done => Err(io::Error::other(
                "the gadget helper's setup reply named no serial",
            )),
        }
    }

    fn bind(&mut self, index: u32) -> io::Result<()> {
        self.call(Verb::Bind, index).map(|_| ())
    }

    fn unbind(&mut self, index: u32) -> io::Result<()> {
        self.call(Verb::Unbind, index).map(|_| ())
    }
}

/// The helper as the daemon's backend holds it: one shared client.
#[must_use]
pub fn shared(client: Arc<Mutex<Client>>) -> SharedHelper {
    client
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_linux::Roots;
    use std::path::{Path, PathBuf};

    /// `SIGSYS`, what the kernel sends a process that makes a call its filter does not allow.
    const SIGSYS_TEST: i32 = 31;
    /// `ENOENT`, as the kernel answers a step that finds nothing.
    const ENOENT_TEST: i32 = 2;

    /// A holder that accepts every peer, or refuses every one.
    struct Anyone;
    impl Holder for Anyone {
        fn check(&self, _: &UnixStream) -> Result<(), String> {
            Ok(())
        }
    }

    /// A board with one gadget per controller name, as the root phase describes them.
    fn gadgets(names: &[&str]) -> Vec<Gadget> {
        let mut text = String::from(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alpha\"\n",
        );
        for name in names {
            text.push_str(&format!(
                "[[devices]]\nid = \"g-{name}\"\ndriver = \"usb-gadget\"\n\
                 uses = {{ function = \"udc:{name}\" }}\n\
                 config = {{ vendor = 0x1209, product = 1, interfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\" }}\n"
            ));
        }
        let board = lemnos_host::board::parse(&text).expect("the test board parses");
        let (gadgets, refused) = crate::gadget::describe(&board, Owner { uid: 987, gid: 987 });
        assert!(refused.is_empty(), "{refused:?}");
        gadgets
    }

    /// Records each call, and fails the verb it is given with the errno.
    #[derive(Default)]
    struct Recorded {
        calls: Vec<(Verb, String)>,
        fail: Option<(Verb, i32)>,
        /// Whether setup says the tree carries a serial string.
        serial: bool,
    }

    impl Recorded {
        fn act(&mut self, verb: Verb, gadget: &Gadget) -> io::Result<()> {
            self.calls.push((verb, gadget.udc.clone()));
            match self.fail {
                Some((failing, errno)) if failing == verb => {
                    Err(io::Error::from_raw_os_error(errno))
                }
                _ => Ok(()),
            }
        }
    }

    impl Ops for Recorded {
        fn setup(&mut self, gadget: &Gadget) -> io::Result<SerialString> {
            self.act(Verb::Setup, gadget)?;
            Ok(if self.serial {
                SerialString::Set
            } else {
                SerialString::Absent
            })
        }
        fn teardown(&mut self, gadget: &Gadget) -> io::Result<()> {
            self.act(Verb::Teardown, gadget)
        }
        fn bind(&mut self, gadget: &Gadget) -> io::Result<()> {
            self.act(Verb::Bind, gadget)
        }
        fn unbind(&mut self, gadget: &Gadget) -> io::Result<()> {
            self.act(Verb::Unbind, gadget)
        }
    }

    #[test]
    fn only_the_closed_vocabulary_parses() {
        for ok in [
            "setup 0 1",
            "teardown 12 9",
            "bind 3 4294967295",
            "unbind 0 0",
            "setup 999999 100",
        ] {
            assert!(parse(ok.as_bytes()).is_some(), "{ok}");
        }
        assert_eq!(
            parse(b"bind 3 17"),
            Some(Request {
                verb: Verb::Bind,
                index: 3,
                seq: 17
            })
        );
        for bad in [
            "",
            "setup",
            "setup ",
            "setup 1",
            "setup 1 ",
            " setup 1 1",
            "setup  1 1",
            "setup 1  1",
            "setup -1 1",
            "setup +1 1",
            "setup 01 1",
            "setup 1 01",
            "setup 1 1 ",
            "setup 1 1 1",
            "SETUP 1 1",
            "setup 1000000 1",
            "setup 1 99999999999",
            "setup 0x1 1",
            "setup /dev/x 1",
            "bind udc0 1",
            "setup\t1 1",
            "set 1 1",
            "setup 1 1\u{0}",
            "setup 1 1\r",
            "unbind - 1",
            "setup 1 -",
        ] {
            assert_eq!(parse(bad.as_bytes()), None, "{bad:?}");
        }
    }

    /// Runs `lines` through the helper's loop, each as one request, and returns what it did and
    /// every reply line.
    fn run_with(
        lines: &[Vec<u8>],
        gadgets: &[Gadget],
        holder: &dyn Holder,
    ) -> (Vec<(Verb, String)>, String) {
        let (mut daemon, mut helper) = UnixStream::pair().unwrap();
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(line);
            bytes.push(b'\n');
        }
        let writer = std::thread::spawn(move || {
            daemon.write_all(&bytes).unwrap();
            daemon.shutdown(std::net::Shutdown::Write).unwrap();
            // The replies are read by nobody but the test: let them queue, then drain them.
            let mut replies = Vec::new();
            daemon.read_to_end(&mut replies).unwrap();
            String::from_utf8(replies).unwrap()
        });
        let mut ops = Recorded::default();
        serve(&mut helper, gadgets, &mut ops, holder).unwrap();
        drop(helper);
        (ops.calls, writer.join().unwrap())
    }

    fn run(lines: &[Vec<u8>], gadgets: &[Gadget]) -> Vec<(Verb, String)> {
        run_with(lines, gadgets, &Anyone).0
    }

    /// Requests from a small alphabet of the words, numbers and bytes the vocabulary and its
    /// near misses are made of, plus raw bytes. A fixed sequence, so every run is the same.
    fn requests(count: usize) -> Vec<Vec<u8>> {
        const PIECES: [&[u8]; 22] = [
            b"setup",
            b"teardown",
            b"bind",
            b"unbind",
            b"set",
            b" ",
            b" ",
            b" ",
            b"0",
            b"1",
            b"2",
            b"9",
            b"00",
            b"-",
            b"+",
            b"/",
            b"x",
            b"\0",
            b"\r",
            b"\t",
            b"999999",
            b"\xff",
        ];
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..count)
            .map(|_| {
                // Near misses of the vocabulary: a real verb and index with a suffix or a
                // different spelling of the index, the shapes a path would take.
                if next() % 3 == 0 {
                    const VERBS: [&str; 4] = ["setup", "teardown", "bind", "unbind"];
                    const SUFFIXES: [&str; 8] = ["", "/x", "/../y", " ", "0", "x", "\r", ".."];
                    let verb = VERBS[(next() % 4) as usize];
                    let index = next() % 4;
                    let suffix = SUFFIXES[(next() % SUFFIXES.len() as u64) as usize];
                    let spelled = if next() % 4 == 0 {
                        format!("0{index}")
                    } else {
                        index.to_string()
                    };
                    let seq = next() % 5000;
                    return format!("{verb} {spelled} {seq}{suffix}").into_bytes();
                }
                let pieces = 1 + (next() % 5) as usize;
                let mut line = Vec::new();
                for _ in 0..pieces {
                    line.extend_from_slice(PIECES[(next() % PIECES.len() as u64) as usize]);
                }
                if next() % 16 == 0 {
                    // A long line of valid words: refused, whatever it spells.
                    line = format!("setup {} 1", "0".repeat(40)).into_bytes();
                }
                line
            })
            .collect()
    }

    #[test]
    fn a_request_outside_the_vocabulary_runs_nothing_and_the_rest_run_as_asked() {
        let gadgets = gadgets(&["udc0", "udc1"]);
        let lines = requests(4000);
        let calls = run(&lines, &gadgets);
        // The oracle is the canonical spelling: a line is acted on exactly when it is one of the
        // four verbs, an index in range and a sequence number, each spelled as the helper spells it.
        let expected: Vec<(Verb, String)> = lines
            .iter()
            .filter_map(|line| {
                let request = parse(line)?;
                let spelled = format!("{} {} {}", request.verb.word(), request.index, request.seq);
                if spelled.as_bytes() != line.as_slice() {
                    return None;
                }
                gadgets
                    .get(request.index as usize)
                    .map(|gadget| (request.verb, gadget.udc.clone()))
            })
            .collect();
        assert_eq!(calls, expected);
        assert!(!calls.is_empty(), "the sequence reaches the vocabulary");
    }

    #[test]
    fn an_index_past_the_list_and_a_long_line_are_refused_and_each_reply_carries_its_sequence() {
        let gadgets = gadgets(&["udc0"]);
        let long = format!("unbind {} 5", "0".repeat(40)).into_bytes();
        let (calls, replies) = run_with(
            &[b"setup 1 7".to_vec(), long, b"setup 0 9".to_vec()],
            &gadgets,
            &Anyone,
        );
        assert_eq!(calls, [(Verb::Setup, "udc0".to_owned())]);
        let lines: Vec<&str> = replies.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("7 refused"), "{lines:?}");
        assert!(lines[1].starts_with("0 refused"), "{lines:?}");
        assert_eq!(lines[2], "9 ok no-serial");
    }

    #[test]
    fn a_peer_that_is_not_the_daemon_that_started_the_helper_is_refused_by_name() {
        let gadgets = gadgets(&["udc0"]);
        // The pair was made by this process: its credentials are the peer's.
        let me = i32::try_from(std::process::id()).unwrap();
        let uid = unsafe_free_uid();
        let right = Daemon { pid: me, uid };
        let (calls, replies) = run_with(&[b"setup 0 1".to_vec()], &gadgets, &right);
        assert_eq!(calls.len(), 1);
        assert_eq!(replies, "1 ok no-serial\n");
        // Another process id, or another user: nothing runs, and the reply says why.
        for wrong in [
            Daemon { pid: me + 1, uid },
            Daemon {
                pid: me,
                uid: uid + 1,
            },
        ] {
            let (calls, replies) = run_with(
                &[
                    b"setup 0 1".to_vec(),
                    b"teardown 0 2".to_vec(),
                    b"bind 9 3".to_vec(),
                ],
                &gadgets,
                &wrong,
            );
            assert!(calls.is_empty(), "{calls:?}");
            for (i, line) in replies.lines().enumerate() {
                assert!(
                    line.starts_with(&format!("{} refused {NOT_THE_HOLDER}", i + 1)),
                    "{line}"
                );
            }
            assert_eq!(replies.lines().count(), 3);
        }
    }

    /// The user id of this process, which is what the socket pair records as its maker's.
    fn unsafe_free_uid() -> u32 {
        let (a, _b) = UnixStream::pair().unwrap();
        peer_cred(a.as_fd()).unwrap().uid
    }

    #[test]
    fn a_failed_step_answers_its_errno_and_the_daemon_gets_it_back() {
        let gadgets = gadgets(&["udc0", "udc1"]);
        let (daemon, mut helper) = UnixStream::pair().unwrap();
        let helper_thread = std::thread::spawn(move || {
            let mut ops = Recorded {
                calls: Vec::new(),
                fail: Some((Verb::Setup, ENOENT_TEST)),
                serial: false,
            };
            serve(&mut helper, &gadgets, &mut ops, &Anyone).unwrap();
            ops.calls
        });
        let mut client = Client::new(daemon, vec![("udc0".into(), 0), ("udc1".into(), 1)]).unwrap();
        let error = client.setup(0).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ENOENT_TEST));
        client.bind(1).unwrap();
        // Out of the list: refused, and the call fails without a helper step.
        let refused = client.setup(7).unwrap_err();
        assert!(refused.to_string().contains("refused"), "{refused}");
        drop(client);
        assert_eq!(
            helper_thread.join().unwrap(),
            [
                (Verb::Setup, "udc0".to_owned()),
                (Verb::Bind, "udc1".to_owned())
            ]
        );
    }

    /// A client whose helper answers its next request with `reply` (after the sequence number).
    fn answered(verb: Verb, reply: &str) -> io::Result<Reply> {
        let (daemon, mut helper) = UnixStream::pair().unwrap();
        let reply = format!("1 {reply}\n");
        let answering = std::thread::spawn(move || {
            let mut request = [0u8; 16];
            let mut got = 0;
            loop {
                got += helper.read(&mut request[got..]).unwrap();
                if request[..got].ends_with(b"\n") {
                    break;
                }
            }
            helper.write_all(reply.as_bytes()).unwrap();
            helper
        });
        let mut client = Client::new(daemon, vec![("udc0".into(), 0)]).unwrap();
        let result = client.call(verb, 0);
        drop(answering.join().unwrap());
        result
    }

    #[test]
    fn the_setup_reply_says_whether_the_tree_has_a_serial_and_nothing_else_is_accepted() {
        assert!(matches!(
            answered(Verb::Setup, "ok serial"),
            Ok(Reply::Serial(SerialString::Set))
        ));
        assert!(matches!(
            answered(Verb::Setup, "ok no-serial"),
            Ok(Reply::Serial(SerialString::Absent))
        ));
        // Outside the closed set: a failed setup, named. Bare `ok` is not a setup reply either.
        for bad in [
            "ok",
            "ok serials",
            "ok Serial",
            "ok serial now",
            "ok  serial",
            "ok maybe",
            "ok no-serial ",
        ] {
            let error = answered(Verb::Setup, bad)
                .err()
                .unwrap_or_else(|| panic!("`{bad}`"));
            assert!(
                error.to_string().contains("not in its vocabulary"),
                "`{bad}`: {error}"
            );
        }
        // The other verbs say `ok` only.
        for verb in [Verb::Bind, Verb::Unbind, Verb::Teardown] {
            assert!(matches!(answered(verb, "ok"), Ok(Reply::Done)));
            for bad in ["ok serial", "ok no-serial"] {
                let error = answered(verb, bad).err().unwrap();
                assert!(
                    error.to_string().contains("not in its vocabulary"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn a_served_setup_replies_by_what_the_tree_carries() {
        for (serial, word) in [(true, "ok serial"), (false, "ok no-serial")] {
            let gadgets = gadgets(&["udc0"]);
            let (daemon, mut helper) = UnixStream::pair().unwrap();
            let helper_thread = std::thread::spawn(move || {
                let mut ops = Recorded {
                    serial,
                    ..Recorded::default()
                };
                serve(&mut helper, &gadgets, &mut ops, &Anyone).unwrap();
            });
            let mut daemon = daemon;
            daemon.write_all(b"setup 0 1\n").unwrap();
            let (seq, text) = read_answer(&mut daemon).unwrap();
            assert_eq!((seq, text.as_str()), (1, word));
            drop(daemon);
            helper_thread.join().unwrap();
        }
    }

    #[test]
    fn a_reply_to_a_request_given_up_on_is_dropped_and_a_helper_that_is_slow_is_not_dead() {
        let (daemon, mut helper) = UnixStream::pair().unwrap();
        let mut client =
            Client::waiting(daemon, vec![("udc0".into(), 0)], Duration::from_millis(100)).unwrap();
        // Nobody answers the first request in time: the call fails, and nothing is latched.
        let first = client.setup(0).unwrap_err();
        assert_eq!(first.kind(), io::ErrorKind::TimedOut);
        assert!(client.dead.is_none());
        // The restarted helper reads the old request, and answers it late, then the new one.
        let mut line = [0u8; 10];
        helper.read_exact(&mut line).unwrap();
        assert_eq!(&line, b"setup 0 1\n");
        helper.write_all(b"1 fail 11 late\n").unwrap();
        // The second call finds its own number past the stale answer.
        let answering = std::thread::spawn(move || {
            let mut request = [0u8; 9];
            helper.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"bind 0 2\n");
            helper.write_all(b"2 ok\n").unwrap();
            helper
        });
        client.bind(0).unwrap();
        drop(answering.join().unwrap());
    }

    #[test]
    fn a_socket_whose_other_end_is_gone_fails_every_call_with_the_reason_once() {
        let (daemon, helper) = UnixStream::pair().unwrap();
        drop(helper);
        let mut client = Client::new(daemon, vec![("udc0".into(), 0)]).unwrap();
        let first = client.setup(0).unwrap_err();
        assert_eq!(first.kind(), io::ErrorKind::BrokenPipe);
        assert!(first.to_string().contains("parent is gone"), "{first}");
        // No second write: the parent is gone, and the answer is the same reason.
        let second = client.bind(0).unwrap_err();
        assert_eq!(second.to_string(), first.to_string());
    }

    /// Mounts that do nothing and say what they were asked, as a fake.
    #[derive(Default)]
    struct NoMounts {
        mounted: Vec<String>,
    }

    impl Mounts for NoMounts {
        fn functionfs(&mut self, source: &str, _target: &Path, options: &str) -> io::Result<()> {
            self.mounted.push(format!("{source} {options}"));
            Ok(())
        }
        fn unmount(&mut self, _target: &Path) -> io::Result<()> {
            Ok(())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lemnosd-helper-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in configfs, mount directory and controller directory under `dir`.
    fn stand_in(dir: &Path, controller: &str) -> (PathBuf, PathBuf, PathBuf) {
        let (configfs, mounts, udc) = (dir.join("config"), dir.join("usb-ffs"), dir.join("udc"));
        std::fs::create_dir_all(&configfs).unwrap();
        std::fs::create_dir_all(udc.join(controller)).unwrap();
        (configfs, mounts, udc)
    }

    fn system_over(dir: &Path, controller: &str) -> System<NoMounts> {
        let (configfs, mounts, udc) = stand_in(dir, controller);
        let roots = Roots {
            configfs: &configfs,
            mounts: &mounts,
            udc_class: &udc,
        };
        System::over(Trees::new(&roots), NoMounts::default())
    }

    /// A board with an identity device whose serial is the file at `serial`, and a gadget that
    /// derives its NCM addresses from it.
    fn deriving(serial: &Path) -> Vec<Gadget> {
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alpha\"\n\
             [[devices]]\nid = \"identity\"\ndriver = \"system-identity\"\n\
             config = {{ serial = \"device-tree\", serial_path = \"{}\" }}\n\
             [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\n\
             uses = {{ function = \"udc:udc0\" }}\n\
             config = {{ vendor = 0x1209, product = 1, identity = \"identity\", \
             interfaces = \"function ncm derive\\ninterface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\" }}\n",
            serial.display()
        );
        let board = lemnos_host::board::parse(&text).expect("the test board parses");
        let (gadgets, refused) = crate::gadget::describe(&board, Owner { uid: 987, gid: 987 });
        assert!(refused.is_empty(), "{refused:?}");
        gadgets
    }

    #[test]
    fn a_serial_that_cannot_be_read_yet_fails_the_setup_and_the_retry_that_can_read_it_makes_the_tree()
     {
        let dir = scratch("serial");
        let serial = dir.join("serial-number");
        let gadgets = deriving(&serial);
        assert!(
            gadgets[0].identity.is_some(),
            "the helper holds the serial source"
        );
        let mut system = system_over(&dir, "udc0");
        // The source is not there: the setup fails with the reason, for as long as it is not.
        for attempt in 0..3 {
            let error = system.setup(&gadgets[0]).unwrap_err();
            assert_eq!(
                errno_of(&error),
                Some(ENODATA),
                "attempt {attempt}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains("the board serial is not readable yet"),
                "{error}"
            );
            assert!(
                !dir.join("config/usb_gadget/lemnos-udc0/functions/ncm.usb0")
                    .exists()
            );
        }
        let reply = reply_fail(&system.setup(&gadgets[0]).unwrap_err());
        assert!(
            reply.starts_with("fail 61 the board serial is not readable yet: "),
            "{reply}"
        );
        // The source appears: the next setup reads it and makes the tree, with no restart.
        std::fs::write(&serial, b"0123456789abcdef\0").unwrap();
        system.setup(&gadgets[0]).unwrap();
        let host = std::fs::read_to_string(
            dir.join("config/usb_gadget/lemnos-udc0/functions/ncm.usb0/host_addr"),
        )
        .unwrap();
        let pair = lemnos_driver_usb_gadget::derive::macs("alpha", "0123456789abcdef").unwrap();
        assert_eq!(
            host.trim(),
            pair.host
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        );
        let serial_string = std::fs::read_to_string(
            dir.join("config/usb_gadget/lemnos-udc0/strings/0x409/serialnumber"),
        )
        .unwrap();
        assert_eq!(serial_string.trim(), "0123456789abcdef");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_setup_over_the_tree_that_is_there_adopts_it_and_a_different_one_that_is_bound_is_not_remade()
     {
        let dir = scratch("adopt");
        let serial = dir.join("serial-number");
        std::fs::write(&serial, b"0123456789abcdef\0").unwrap();
        let gadgets = deriving(&serial);
        let mut system = system_over(&dir, "udc0");
        system.setup(&gadgets[0]).unwrap();
        assert_eq!(system.mounts.mounted.len(), 1);
        // The helper starts again (a new `System` on the same trees): the same tree is adopted,
        // nothing is mounted again, and a bound gadget stays bound.
        std::fs::write(dir.join("config/usb_gadget/lemnos-udc0/UDC"), b"udc0\n").unwrap();
        std::fs::write(dir.join("usb-ffs/udc0/ep0"), b"").unwrap();
        let mut again = system_over(&dir, "udc0");
        again.setup(&gadgets[0]).unwrap();
        assert!(again.mounts.mounted.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("config/usb_gadget/lemnos-udc0/UDC")).unwrap(),
            "udc0\n"
        );
        // The serial changed under a bound tree: it differs, and is left as it is.
        std::fs::write(&serial, b"fedcba9876543210\0").unwrap();
        let error = again.setup(&gadgets[0]).unwrap_err();
        assert_eq!(errno_of(&error), Some(16), "{error}");
        assert!(
            error.to_string().contains("not remade while bound"),
            "{error}"
        );
        // Unbound, the same difference is made again.
        std::fs::write(dir.join("config/usb_gadget/lemnos-udc0/UDC"), b"\n").unwrap();
        again.setup(&gadgets[0]).unwrap();
        let serial_string = std::fs::read_to_string(
            dir.join("config/usb_gadget/lemnos-udc0/strings/0x409/serialnumber"),
        )
        .unwrap();
        assert_eq!(serial_string.trim(), "fedcba9876543210");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Each verb, run under the production filter against fake roots: one forked child per verb,
    /// so a call the filter does not allow kills that verb's child and names it. The mounts are
    /// fakes: this PC makes no mount, and the filter's allowance of `mount` and `umount2` is read
    /// from the list.
    #[test]
    fn every_verb_runs_under_the_filter_and_a_forbidden_call_ends_the_child() {
        let dir = scratch("filter");
        let (configfs, mounts, udc) = stand_in(&dir, "udc0");
        std::fs::create_dir_all(configfs.join("usb_gadget")).unwrap();
        let serial = dir.join("serial-number");
        std::fs::write(&serial, b"0123456789abcdef\0").unwrap();
        let gadgets = deriving(&serial);
        let gadget = &gadgets[0];
        for verb in [
            Verb::Setup,
            Verb::Bind,
            Verb::Unbind,
            Verb::Teardown,
            Verb::Setup,
        ] {
            let exit = seccomp::run_forked(|| {
                let roots = Roots {
                    configfs: &configfs,
                    mounts: &mounts,
                    udc_class: &udc,
                };
                let mut system = System::over(Trees::new(&roots), NoMounts::default());
                system.open();
                if seccomp::install(&calls()).is_err() {
                    return 2;
                }
                // Any answer is fine here: only whether the child survives the filter is tested.
                let _ = match verb {
                    Verb::Setup => system.setup(gadget).map(|_| ()),
                    Verb::Bind => system.bind(gadget),
                    Verb::Unbind => system.unbind(gadget),
                    Verb::Teardown => system.teardown(gadget),
                };
                0
            })
            .unwrap();
            assert_eq!(
                exit,
                seccomp::Exit::Code(0),
                "{} under the filter",
                verb.word()
            );
        }
        assert!(calls().contains(&nr::MOUNT) && calls().contains(&nr::UMOUNT2));
        // The socket: std reads a Unix stream with `recvfrom` and writes it with `sendto`, so the
        // serving loop itself runs under the filter, requests in and replies out.
        let one = deriving(&serial);
        let (mut daemon, mut helper) = UnixStream::pair().unwrap();
        daemon.write_all(b"bind 0 1\nbogus\n").unwrap();
        daemon.shutdown(std::net::Shutdown::Write).unwrap();
        let served = seccomp::run_forked(|| {
            if seccomp::install(&calls()).is_err() {
                return 2;
            }
            let mut ops = Recorded::default();
            i32::from(serve(&mut helper, &one, &mut ops, &Anyone).is_err())
        })
        .unwrap();
        assert_eq!(served, seccomp::Exit::Code(0), "serving under the filter");
        drop(helper);
        let mut replies = String::new();
        daemon.read_to_string(&mut replies).unwrap();
        assert_eq!(replies.lines().count(), 2, "{replies}");
        assert!(replies.starts_with("1 ok\n0 refused"), "{replies}");
        // A call the list does not allow: the child is killed before it runs.
        let forbidden = seccomp::run_forked(|| {
            if seccomp::install(&calls()).is_err() {
                return 2;
            }
            let _ = std::process::id();
            0
        })
        .unwrap();
        assert_eq!(forbidden, seccomp::Exit::Signal(SIGSYS_TEST));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
