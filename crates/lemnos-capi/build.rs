//! Gives the shared library a SONAME with the surface's major version,
//! `liblemnos_capi.so.<LEMNOS_ABI_MAJOR>`, so the loader refuses a library of
//! another major for a program linked against this one.

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
}
