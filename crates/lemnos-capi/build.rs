//! Gives the shared library a SONAME with the surface's major version,
//! `liblemnos_capi.so.<LEMNOS_ABI_MAJOR>`, so the loader refuses a library of
//! another major for a program linked against this one.
//!
//! With the `shared` feature, finds the library the wrapper links: in
//! `LEMNOS_CAPI_LIB_DIR` (an installed one, as an image's sysroot has), or,
//! unset, one this script builds from this crate without `shared`, for
//! development and the crate's own tests (which then find it by an rpath).

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=include/lemnos.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let header = std::fs::read_to_string("include/lemnos.h").expect("include/lemnos.h");
    let major = header
        .lines()
        .find_map(|l| l.strip_prefix("#define LEMNOS_ABI_MAJOR "))
        .map(str::trim)
        .expect("LEMNOS_ABI_MAJOR in include/lemnos.h");
    assert!(
        !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()),
        "LEMNOS_ABI_MAJOR is not a number: {major:?}"
    );
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,liblemnos_capi.so.{major}");
    if std::env::var_os("CARGO_FEATURE_SHARED").is_some() {
        shared(major);
    }
}

fn shared(major: &str) {
    println!("cargo:rerun-if-env-changed=LEMNOS_CAPI_LIB_DIR");
    let name = format!("liblemnos_capi.so.{major}");
    let dir = match std::env::var_os("LEMNOS_CAPI_LIB_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            assert!(
                dir.join(&name).exists(),
                "LEMNOS_CAPI_LIB_DIR={} has no {name}",
                dir.display()
            );
            dir
        }
        None => {
            let dir = build_library(&name);
            // The crate's own tests run on it from where it was built.
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
            dir
        }
    };
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-env=LEMNOS_CAPI_SHARED_DIR={}", dir.display());
}

/// Builds this crate's cdylib (without `shared`) into `OUT_DIR` and returns
/// the directory holding it as `name`.
fn build_library(name: &str) -> PathBuf {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    for src in ["src", "../lemnos-abi/src", "../lemnos-ipc/src"] {
        println!("cargo:rerun-if-changed={}", manifest.join(src).display());
    }
    let target = std::env::var("TARGET").expect("TARGET");
    let host = std::env::var("HOST").expect("HOST");
    let target_dir = out.join("library");
    let mut cargo = Command::new(std::env::var_os("CARGO").expect("CARGO"));
    cargo
        .args([
            "build",
            "--lib",
            "--offline",
            "-p",
            "lemnos-capi",
            "--manifest-path",
        ])
        .arg(manifest.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir);
    if target != host {
        cargo.args(["--target", &target]);
    }
    // The outer build's feature and target settings are not this build's.
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("CARGO_FEATURE_") || key == "CARGO_TARGET_DIR" {
            cargo.env_remove(key);
        }
    }
    let status = cargo.status().expect("cargo");
    assert!(status.success(), "building {name} for the shared feature");
    let built = if target == host {
        target_dir.join("debug")
    } else {
        target_dir.join(&target).join("debug")
    }
    .join("liblemnos_capi.so");
    let dir = out.join("lib");
    std::fs::create_dir_all(&dir).expect("lib dir");
    copy(&built, &dir.join(name));
    dir
}

fn copy(from: &Path, to: &Path) {
    std::fs::copy(from, to)
        .unwrap_or_else(|e| panic!("{} to {}: {e}", from.display(), to.display()));
}
