//! The caller's side: what the slots driver uses to check and start a restart through the helper.

use crate::proto::{self, Answer, Verb};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The socket the packaged unit listens on.
pub const DEFAULT_SOCKET: &str = "/run/lemnos-restart.sock";
/// How long a check's answer, or a reboot's first answer, may take.
pub const ANSWER_WAIT: Duration = Duration::from_secs(5);

/// The helper at its socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub socket: PathBuf,
}

/// A nonce no other request of this process uses: the process id, a count and the time.
fn nonce() -> String {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut s = format!("{:x}{n:x}{t:x}", std::process::id());
    s.truncate(proto::MAX_NONCE);
    s
}

fn helper(socket: &Path, e: io::Error) -> String {
    format!("the restart helper at {}: {e}", socket.display())
}

impl Client {
    fn ask(&self, verb: Verb) -> Result<(BufReader<UnixStream>, String, Answer), String> {
        let stream = UnixStream::connect(&self.socket).map_err(|e| helper(&self.socket, e))?;
        stream
            .set_read_timeout(Some(ANSWER_WAIT))
            .map_err(|e| helper(&self.socket, e))?;
        let nonce = nonce();
        (&stream)
            .write_all(format!("{} {nonce}\n", verb.word()).as_bytes())
            .map_err(|e| helper(&self.socket, e))?;
        let mut reader = BufReader::new(stream);
        let answer = read_answer(&mut reader, &nonce)
            .map_err(|e| format!("the restart helper at {}: {e}", self.socket.display()))?;
        Ok((reader, nonce, answer))
    }

    /// Whether a restart would be accepted, before anything is written: the same socket and the
    /// same helper as the restart, which asks the service manager everything but the start itself.
    pub fn check(&self) -> Result<(), String> {
        match self.ask(Verb::Check)?.2 {
            Answer::Ok => Ok(()),
            Answer::Refused(why) => Err(format!("the restart helper refused the restart: {why}")),
            other => Err(format!(
                "the restart helper answered a check with {other:?}"
            )),
        }
    }

    /// Asks for the reboot. `Ok` once the helper accepted it: what follows is read from the
    /// returned [`Pending`].
    pub fn reboot(&self) -> Result<Pending, String> {
        let (reader, nonce, answer) = self.ask(Verb::Reboot)?;
        match answer {
            Answer::Accepted => {
                reader
                    .get_ref()
                    .set_nonblocking(true)
                    .map_err(|e| helper(&self.socket, e))?;
                Ok(Pending {
                    reader,
                    nonce,
                    line: Vec::new(),
                })
            }
            Answer::Refused(why) => Err(format!("the restart helper refused the restart: {why}")),
            other => Err(format!(
                "the restart helper answered a reboot with {other:?}"
            )),
        }
    }
}

fn read_answer<R: BufRead>(reader: &mut R, nonce: &str) -> Result<Answer, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return Err("it closed the connection without an answer".to_string()),
        Ok(_) => {}
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            return Err(format!("no answer within {} ms", ANSWER_WAIT.as_millis()));
        }
        Err(e) => return Err(e.to_string()),
    }
    if !line.ends_with('\n') {
        return Err(format!("it answered {line:?} without a newline"));
    }
    line.pop();
    proto::parse_answer(&line, nonce)
}

/// What a reboot's helper has said since it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Nothing yet.
    Waiting,
    /// The system is stopping: the restart is happening.
    Stopping,
    /// The reboot did not start, or the system did not start stopping: the reason.
    Failed(String),
}

/// A reboot the helper accepted, whose outcome is still to come on the same connection.
pub struct Pending {
    reader: BufReader<UnixStream>,
    nonce: String,
    line: Vec<u8>,
}

impl Pending {
    /// Reads what has arrived, without waiting. The connection closing with no outcome is a
    /// failure with that reason: the helper stops only with the system, and a stopping system was
    /// said first.
    pub fn poll(&mut self) -> Progress {
        let mut byte = [0u8; 1];
        loop {
            match self.reader.read(&mut byte) {
                Ok(0) => {
                    return Progress::Failed(
                        "the restart helper closed the connection without saying the system is stopping"
                            .to_string(),
                    );
                }
                Ok(_) => {
                    if byte[0] != b'\n' {
                        if self.line.len() > 512 {
                            return Progress::Failed(
                                "the restart helper's answer is too long".into(),
                            );
                        }
                        self.line.push(byte[0]);
                        continue;
                    }
                    let line = String::from_utf8_lossy(&self.line).into_owned();
                    self.line.clear();
                    return match proto::parse_answer(&line, &self.nonce) {
                        Ok(Answer::Stopping) => Progress::Stopping,
                        Ok(Answer::Failed(why)) => Progress::Failed(why),
                        Ok(other) => Progress::Failed(format!(
                            "the restart helper answered {other:?} after accepting"
                        )),
                        Err(why) => Progress::Failed(why),
                    };
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Progress::Waiting,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Progress::Failed(format!("the restart helper: {e}")),
            }
        }
    }
}
