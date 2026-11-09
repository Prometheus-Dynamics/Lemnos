//! One copy of the surface per process. Two copies (a program that links the
//! static library and also loads a library linked to `liblemnos_capi.so.1`)
//! would each keep a handle table, and a handle from one could name another
//! client in the other. So the first copy to open a client claims the process,
//! and any other copy's `lemnos_client_open` is refused with `-EBUSY`.
//!
//! The claim is an abstract unix socket named for the process: its pid
//! namespace and pid. Any copy in the process sees the same name, and the
//! kernel frees it when the process ends. It is close-on-exec, so a program the
//! process executes claims for itself. Where `/proc` is missing the check is
//! skipped, since a copy cannot be told from another without it.

use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::sync::OnceLock;

/// This process's claim name, or `None` without `/proc`.
fn name() -> Option<String> {
    let ns = std::fs::read_link("/proc/self/ns/pid").ok()?;
    let ns = ns
        .to_str()?
        .strip_prefix("pid:[")?
        .strip_suffix(']')?
        .to_owned();
    Some(format!("lemnos-capi.{ns}.{}", std::process::id()))
}

/// Binds `name`; refused with `AddrInUse` when something else holds it.
pub(crate) fn claim(name: &str) -> io::Result<UnixListener> {
    UnixListener::bind_addr(&SocketAddr::from_abstract_name(name)?)
}

/// Whether this copy is the process's copy of the surface (claiming the
/// process the first time). Refused once: a copy that lost stays refused.
pub(crate) fn this_copy() -> io::Result<()> {
    static CLAIM: OnceLock<Result<Option<UnixListener>, io::ErrorKind>> = OnceLock::new();
    let claimed = CLAIM.get_or_init(|| match name() {
        None => Ok(None),
        Some(name) => claim(&name).map(Some).map_err(|e| e.kind()),
    });
    match claimed {
        Ok(_) => Ok(()),
        Err(io::ErrorKind::AddrInUse) => Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            "another copy of the Lemnos C surface is in this process (static and shared at once)",
        )),
        // The claim could not be made (sockets refused here): do not refuse
        // the caller for it.
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_of_one_name_is_refused() {
        let name = format!("lemnos-capi-test.{}", std::process::id());
        let first = claim(&name).expect("first claim");
        let second = claim(&name).expect_err("second claim");
        assert_eq!(second.kind(), io::ErrorKind::AddrInUse);
        drop(first);
        // A process another test is forking can hold the socket until it execs.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while let Err(error) = claim(&name) {
            assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
            assert!(std::time::Instant::now() < until, "never freed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// In a static build this crate's copy is the process's: it claims the
    /// process and keeps it. (With `shared`, the library is, so this test would
    /// itself be a second copy.)
    #[cfg(not(feature = "shared"))]
    #[test]
    fn the_first_copy_claims_the_process_and_keeps_it() {
        this_copy().expect("first copy");
        this_copy().expect("still the copy");
        let ours = name_for_test();
        assert_eq!(
            claim(&ours).expect_err("held by this copy").kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[cfg(not(feature = "shared"))]
    fn name_for_test() -> String {
        name().expect("/proc on the test host")
    }
}
