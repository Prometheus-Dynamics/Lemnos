//! The real `lemnos-orion` binary, started with an environment, its log read line by line.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::daemon::wait_until;

/// One bridge process this check started, and the lines it logged so far.
pub struct Bridge {
    /// Kept so the process is a child of this check for as long as the check runs.
    _child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Bridge {
    /// Starts `program` with the daemon's socket, the comm core's socket and, when given, the
    /// `LEMNOS_ORION_LINKS` line (the `NAME=value` text `tree --bridge-env` wrote).
    pub fn start(
        program: &Path,
        lemnosd: &Path,
        comm: &Path,
        links: Option<&str>,
    ) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("LEMNOSD_SOCKET", lemnosd)
            .env("ORION_ADDRESS", comm)
            .stderr(Stdio::piped());
        if let Some(line) = links {
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| format!("not a NAME=value line: {line:?}"))?;
            command.env(name, value);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("start lemnos-orion: {error}"))?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                say!("     bridge: {line}");
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        Ok(Self {
            _child: child,
            lines,
        })
    }

    /// How many lines logged so far contain `text`.
    pub fn count(&self, text: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|line| line.contains(text))
            .count()
    }

    /// Whether `text` has been logged at least `times` times within `within`.
    pub fn wait_for(&self, text: &str, times: usize, within: Duration) -> bool {
        wait_until(within, || self.count(text) >= times)
    }
}
