//! The PiSP back end's version read-back (`LEMNOS_IOC_PISP_BE_VERSION`) on the fake block: the
//! value the probe read and accepted, read through the control node, for the block the name finds.
//!
//! Usage: `lemnos-pisp-be-version-check /dev/lemnos <device> <expected version>`. Port of
//! `kernel/test/lemnos_pisp_be_version_check.c` (CZ-GUEST-3). The expected version is parsed as
//! `strtoul(.., 0)` does: hex with `0x`, octal with a leading zero, otherwise decimal.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::iowr;
use lemnos_linux_sys::module;

/// `_IOWR('L', 0xb2, struct lemnos_pisp_be_version)`: the block in, the version register out.
const PISP_BE_VERSION: u32 = iowr::<[u8; 8]>(b'L', 0xb2);

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn fail(line: &str) -> ExitCode {
    let _ = writeln!(io::stderr(), "{line}");
    ExitCode::from(1)
}

/// `strtoul(text, NULL, 0)`: optional blanks and sign, then `0x` hex, a leading `0` octal, or
/// decimal, up to the first byte that does not fit. Truncated to 32 bits by the caller.
fn strtoul0(text: &str) -> u64 {
    let b = text.trim_start().as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let (radix, start) = if b.len() > i + 1 && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        (16, i + 2)
    } else if b.len() > i && b[i] == b'0' {
        (8, i)
    } else {
        (10, i)
    };
    let mut value: u64 = 0;
    for &c in &b[start..] {
        let digit = match c {
            b'0'..=b'9' => u64::from(c - b'0'),
            b'a'..=b'f' => u64::from(c - b'a' + 10),
            b'A'..=b'F' => u64::from(c - b'A' + 10),
            _ => break,
        };
        if digit >= radix {
            break;
        }
        value = value.wrapping_mul(radix).wrapping_add(digit);
    }
    if neg { value.wrapping_neg() } else { value }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        let _ = writeln!(
            io::stderr(),
            "usage: {} /dev/lemnos <device> <version>",
            args.first()
                .map_or("lemnos-pisp-be-version-check", String::as_str)
        );
        return ExitCode::from(2);
    }
    let node: OwnedFd = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&args[1])
    {
        Ok(f) => OwnedFd::from(f),
        Err(e) => return fail(&format!("open: {e}")),
    };
    let block = module::find_block(node.as_fd(), &args[2])
        .ok()
        .flatten()
        .unwrap_or(0);
    if block == 0 {
        return fail(&format!("no block {}", args[2]));
    }
    let mut arg = [0u8; 8];
    arg[0..4].copy_from_slice(&block.to_ne_bytes());
    if let Err(e) = guest::ioctl_bytes(node.as_fd(), PISP_BE_VERSION, &mut arg) {
        return fail(&format!("LEMNOS_IOC_PISP_BE_VERSION: {e}"));
    }
    let hw = u32::from_ne_bytes([arg[4], arg[5], arg[6], arg[7]]);
    let want = strtoul0(&args[3]) as u32;
    if hw != want {
        say(&format!("version 0x{hw:08x}, want 0x{want:08x}"));
        return ExitCode::from(1);
    }
    say(&format!("version 0x{hw:08x} read back for block {block}"));
    ExitCode::SUCCESS
}
