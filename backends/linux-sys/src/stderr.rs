//! Log lines written to file descriptor 2 without `std::io::stderr`'s lock.
//!
//! A process forked from a thread while another thread of it holds the stderr lock inherits that
//! lock, held by a thread the child does not have. A write through `std::io::stderr` in the child
//! then waits on the lock in a futex call, which a seccomp filter that allows only the helper's
//! calls kills the child for (SIGSYS). A `write` call takes no lock, so a forked helper logs
//! through this instead.

/// Writes `text` and a newline to standard error, in one `write` loop. A failed write is dropped:
/// a log line is not worth the process.
pub fn line(text: &str) {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(b'\n');
    let mut rest: &[u8] = &bytes;
    while !rest.is_empty() {
        // SAFETY: `rest` is a live slice for the whole call, so its pointer and length are valid
        // for reading; descriptor 2 is only written, and a bad descriptor is an error return.
        let written = unsafe { libc::write(2, rest.as_ptr().cast(), rest.len()) };
        if written < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        rest = &rest[written as usize..];
    }
}
