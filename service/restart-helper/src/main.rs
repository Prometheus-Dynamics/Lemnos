//! `lemnos-restart`: the restart helper.
//!
//! Usage: `lemnos-restart --user NAME [--manager PATH]` under systemd, started by
//! `lemnos-restart.socket`: the listening socket comes from the service manager (`LISTEN_FDS=1`,
//! descriptor 3), and PATH is its private socket (`/run/systemd/private`).
//!
//! `lemnos-restart --user NAME --init --listen PATH` under BusyBox init, started by the init script:
//! the helper makes its own socket at PATH (root's, group NAME's group, mode 0660) and restarts the
//! board by `SIGTERM` to process 1.
//!
//! NAME is the one user besides root that may ask (the daemon's). Connections are served one at a
//! time, each one request.

use std::io::BufReader;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::poll::{POLLIN, PollFd, poll_many};
use lemnos_linux_sys::signal::{SIGTERM, SignalFd};
use lemnos_linux_sys::unix::{listener_from_activation, peer_cred};
use lemnos_linux_sys::user::user_ids;
use lemnos_restart::serve::{InitSignal, Pid1, Policy, Systemd, Watch, serve};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

/// How long a connected caller may take to send its request.
const REQUEST_WAIT: Duration = Duration::from_secs(2);
/// The first descriptor socket activation passes.
const LISTEN_FD: i32 = 3;

/// One line on stderr, which the journal keeps.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "lemnos-restart: {}", format_args!($($arg)*));
    }};
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            say!("{why}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut user = None;
    let mut manager = PathBuf::from("/run/systemd/private");
    let mut init = false;
    let mut listen: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--user" => user = args.next(),
            "--manager" => {
                manager = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--manager needs a path")?
            }
            "--init" => init = true,
            "--listen" => {
                listen = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or("--listen needs a path")?,
                )
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let user = user.ok_or("--user NAME is required: the one user besides root that may ask")?;
    let (allowed_uid, group) = user_ids(&user)
        .map_err(|e| format!("the user database: {e}"))?
        .ok_or_else(|| format!("there is no user {user:?}"))?;
    let listener = match &listen {
        Some(path) => own_socket(path, group)?,
        None => activated()?,
    };
    // SIGTERM is read, not taken: after a started reboot it is the system stopping.
    let signals = SignalFd::new(&[SIGTERM]).map_err(|e| format!("the signal descriptor: {e}"))?;
    let policy = Policy { allowed_uid };
    let mut pid1: Box<dyn Pid1> = if init {
        Box::new(InitSignal)
    } else {
        Box::new(Systemd { socket: manager })
    };
    let mut stopped = false;
    loop {
        if stopped {
            return Ok(());
        }
        // Idle: wait for a caller or the stop, whichever comes first.
        let mut fds = [
            PollFd::new(listener.as_fd(), POLLIN),
            PollFd::new(signals.as_fd(), POLLIN),
        ];
        poll_many(&mut fds, None).map_err(|e| format!("poll: {e}"))?;
        if fds[1].revents() != 0 {
            if matches!(signals.read(), Ok(Some(_))) {
                return Ok(());
            }
            continue;
        }
        if fds[0].revents() == 0 {
            continue;
        }
        let (stream, _) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("accept: {e}")),
        };
        let uid = match peer_cred(stream.as_fd()) {
            Ok(cred) => cred.uid,
            Err(e) => {
                say!("a caller's credentials could not be read: {e}");
                continue;
            }
        };
        if let Err(e) = stream.set_read_timeout(Some(REQUEST_WAIT)) {
            say!("{e}");
            continue;
        }
        let mut conn = Conn(BufReader::new(stream));
        let mut stop_seen = || {
            if matches!(signals.read(), Ok(Some(_))) {
                stopped = true;
            }
            stopped
        };
        let result = serve(
            &mut conn,
            uid,
            policy,
            pid1.as_mut(),
            Watch::default(),
            &mut stop_seen,
        );
        if let Err(e) = result {
            say!("a caller (uid {uid}): {e}");
        }
        if matches!(signals.read(), Ok(Some(_))) {
            stopped = true;
        }
    }
}

/// The listening socket from socket activation: exactly one descriptor, passed to this process.
fn activated() -> Result<UnixListener, String> {
    let fds = std::env::var("LISTEN_FDS").unwrap_or_default();
    let pid = std::env::var("LISTEN_PID").unwrap_or_default();
    if fds != "1" || pid != std::process::id().to_string() {
        return Err(format!(
            "started without its socket (LISTEN_FDS={fds:?}, LISTEN_PID={pid:?}): run it from lemnos-restart.socket, or give --listen"
        ));
    }
    listener_from_activation(LISTEN_FD).map_err(|e| format!("the activation socket: {e}"))
}

/// The helper's own socket at `path`: root's, group `group`, mode 0660, so only root and the
/// group's members can connect (and only root and the allowed user are answered).
fn own_socket(path: &std::path::Path, group: u32) -> Result<UnixListener, String> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::os::unix::fs::chown(path, Some(0), Some(group))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(listener)
}

/// A caller's connection, read through a buffer and written directly.
struct Conn(BufReader<std::os::unix::net::UnixStream>);

impl std::io::Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::BufRead for Conn {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.0.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.0.consume(n)
    }
}

impl std::io::Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.get_mut().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.get_mut().flush()
    }
}
