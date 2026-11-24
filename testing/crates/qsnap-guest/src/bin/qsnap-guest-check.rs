//! The guest for qsnap's own check (testing/qsnap/check.sh), run as the first process: says
//! ready, prints the run's line, and acts on it. `save <name>`: asks for the state to be kept, then
//! prints which side of the save it is on and the line a restore got. `payload`: unpacks the
//! payload disk under /payload and prints the file `hello` from it. Every line it prints for the
//! check starts with `qsnap-guest:`. Powers off at the end.

use std::path::Path;

use lemnos_linux_sys::guest;
use lemnos_qsnap_guest::{Saved, ready, save, unpack_payload};

/// A line on standard output (the console), which the check reads.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

fn main() {
    let _ = guest::mount("devtmpfs", "/dev", "devtmpfs");
    let _ = guest::mount("proc", "/proc", "proc");
    let _ = guest::mount("sysfs", "/sys", "sysfs");
    match ready() {
        Ok(line) => {
            say!("qsnap-guest: line {line:?}");
            act(&line);
        }
        Err(error) => say!("qsnap-guest: FAIL no control port: {error}"),
    }
    let _ = guest::power_off();
}

fn act(line: &str) {
    let mut words = line.split_whitespace();
    match words.next() {
        Some("save") => {
            let name = words.next().unwrap_or("mid");
            match save(name) {
                Ok(Saved::Kept(dir)) => say!("qsnap-guest: kept as {dir}"),
                Ok(Saved::Restored(line)) => {
                    say!("qsnap-guest: restored with {line:?}");
                    act(&line);
                }
                Err(error) => say!("qsnap-guest: FAIL save: {error}"),
            }
        }
        Some("payload") => match unpack_payload(Path::new("/payload")) {
            Ok(n) => say!(
                "qsnap-guest: payload {n} file(s), hello {:?}",
                std::fs::read_to_string("/payload/hello")
                    .unwrap_or_default()
                    .trim()
            ),
            Err(error) => say!("qsnap-guest: FAIL payload: {error}"),
        },
        // The first disk the spec gives the guest (vda): `write <word>` puts the word at its start,
        // `read` prints what is there.
        Some("write") => {
            let word = words.next().unwrap_or("");
            let wrote = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/vda")
                .and_then(|mut disk| {
                    std::io::Write::write_all(&mut disk, format!("{word:<16}").as_bytes())?;
                    disk.sync_all()
                });
            say!("qsnap-guest: wrote {word:?}: {wrote:?}");
            if let Some(next) = words.next() {
                act(&std::iter::once(next)
                    .chain(words)
                    .collect::<Vec<_>>()
                    .join(" "));
            }
        }
        Some("read") => {
            let mut head = [0u8; 16];
            let read = std::fs::File::open("/dev/vda")
                .and_then(|mut disk| std::io::Read::read_exact(&mut disk, &mut head));
            say!(
                "qsnap-guest: disk holds {:?} ({read:?})",
                String::from_utf8_lossy(&head).trim_end_matches(['\0', ' '])
            );
        }
        _ => {}
    }
}
