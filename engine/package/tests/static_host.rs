//! A statically linked program has no dynamic loader: it can load no package, and says so
//! before it looks at a file.
//!
//! The program is `tests/static-host`, built here for `x86_64-unknown-linux-musl`, where the C
//! library is linked statically, and run against a real package. Where that target is not
//! installed there is nothing to run, and the test passes without having shown anything.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{fixtures, scratch};
use lemnos_package::{Loader, Mode, Refusal, can_load};

const TARGET: &str = "x86_64-unknown-linux-musl";

/// The program built for [`TARGET`], or `None` where the target is not installed.
fn static_host() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/static-host/Cargo.toml");
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap().join("package-static-host");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target")
        .arg(TARGET)
        .arg("--target-dir")
        .arg(&target);
    for outer in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_TARGET_DIR"] {
        command.env_remove(outer);
    }
    let output = command.output().expect("cargo runs");
    let said = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && said.contains("target may not be installed") {
        assert!(
            std::env::var_os("LEMNOS_REQUIRE_MUSL").is_none(),
            "{TARGET} is not installed"
        );
        return None;
    }
    assert!(
        output.status.success(),
        "the program did not build:\n{said}"
    );
    Some(target.join(TARGET).join("debug/lemnos-package-static-host"))
}

/// Whether the program asks for a dynamic loader: a `PT_INTERP` program header.
fn asks_for_a_loader(program: &[u8]) -> bool {
    let word = |at: usize| u64::from_le_bytes(program[at..at + 8].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(program[at..at + 2].try_into().unwrap());
    let (phoff, phentsize, phnum) = (word(32) as usize, half(54) as usize, half(56) as usize);
    (0..phnum).any(|index| {
        let at = phoff + index * phentsize;
        u32::from_le_bytes(program[at..at + 4].try_into().unwrap()) == 3
    })
}

#[test]
fn a_statically_linked_program_refuses_every_package_and_says_why() {
    // This test binary is the ordinary kind: it can load, and does.
    assert!(can_load());
    assert!(Loader::new(Mode::Open).load(&fixtures().good).is_ok());
    assert_eq!(
        Refusal::StaticHost.to_string(),
        "this program is statically linked and can't load driver packages: \
         use a dynamically linked build"
    );

    let Some(program) = static_host() else {
        return;
    };
    assert!(
        !asks_for_a_loader(&std::fs::read(&program).unwrap()),
        "the program is not statically linked"
    );
    let dir = scratch("static-host");
    let package = dir.join("good.so");
    std::fs::copy(&fixtures().good, &package).unwrap();
    std::fs::write(dir.join("junk.so"), "not a shared object").unwrap();
    let output = Command::new(&program).arg(&package).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let said = String::from_utf8(output.stdout).unwrap();
    let reason = Refusal::StaticHost.to_string();
    // The same reason for a good package and for a file that is none: neither was looked at.
    assert_eq!(
        said.lines().collect::<Vec<_>>(),
        [
            "can load: false",
            &format!("refused: {reason}"),
            "scan: read loaded 0 refused 2",
            &format!("scan refused: {reason}"),
            &format!("scan refused: {reason}"),
        ]
    );
    let _ = std::fs::remove_dir_all(dir);
}
