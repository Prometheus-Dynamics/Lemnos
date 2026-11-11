//! Getting a driver package into this process (feature `load`, Linux).
//!
//! Two steps, and nothing else here: [`seal`] copies a file's bytes into memory nobody can
//! change, and [`load`] has the dynamic loader map that memory and returns the package's
//! [`PackageDescriptor`]. A caller checks the bytes (a signature, the file's format) between the
//! two, so what it checked is exactly what is loaded: the path is never opened again.
//!
//! This is the only place Lemnos calls the dynamic loader, and it is for Lemnos driver packages
//! only. [`load`] can look up one symbol, [`PACKAGE_ENTRY`], and returns nothing else of the
//! library: there is no way through it to a function of a library that is not a package.
//!
//! A package is never unloaded. The descriptor, the tables and every string a package hands the
//! host point into it, so they are `'static` in fact. A package refused after it was loaded
//! stays mapped and is never called.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::string::String;
use std::sync::Mutex;

use crate::package::{PACKAGE_ENTRY, PackageDescriptor};

/// A copy of `bytes` in memory that can no longer be written, grown or shrunk by anyone, this
/// process included.
pub fn seal(bytes: &[u8]) -> io::Result<OwnedFd> {
    let flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // SAFETY: the name is a NUL-terminated literal; `memfd_create` takes nothing else by pointer.
    let fd = unsafe { libc::memfd_create(c"lemnos-package".as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: `F_ADD_SEALS` takes an integer and touches no memory of this process.
    let sealed = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) };
    if sealed < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedFd::from(file))
}

/// Whether this program can load a shared object at all. One built with a statically linked C
/// library (the default on musl targets) has no dynamic loader in it: its `dlopen` is a stub
/// that always fails, or one that half works. [`load`] refuses there without calling it.
pub const fn supported() -> bool {
    !cfg!(target_feature = "crt-static")
}

/// What [`load`] says in a program that is not [`supported`].
pub const STATIC_PROGRAM: &str =
    "this program is statically linked and has no dynamic loader to load a package with";

/// Why [`load`] returned no descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The dynamic loader refused the file, in its words. Nothing was loaded.
    Refused(String),
    /// The file was loaded and has no [`PACKAGE_ENTRY`] symbol. It stays mapped, unused.
    NoEntry,
}

/// One load at a time, so each load's error is its own.
static LOADING: Mutex<()> = Mutex::new(());

/// Maps the sealed file behind `fd` into this process and returns its package descriptor.
///
/// The loader runs the file's initialisers, as it does for any shared object: check what `fd`
/// holds first. `fd` is never closed once the file is loaded, because the loader knows a library
/// by the name it was opened under, and that name is the descriptor's number: a number that came
/// round again would be taken for the library already loaded.
pub fn load(fd: OwnedFd) -> Result<&'static PackageDescriptor, LoadError> {
    if !supported() {
        return Err(LoadError::Refused(String::from(STATIC_PROGRAM)));
    }
    let _one = LOADING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = std::format!("/proc/self/fd/{}", fd.as_raw_fd());
    let path = CString::new(path).map_err(|_| LoadError::Refused(String::new()))?;
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        return Err(LoadError::Refused(last_error()));
    }
    // From here the library is loaded for good, under this descriptor's number.
    let _ = fd.into_raw_fd();
    // SAFETY: `library` is the handle `dlopen` just returned, never closed, and the name is a
    // NUL-terminated constant.
    let symbol = unsafe { libc::dlsym(library, PACKAGE_ENTRY.as_ptr()) };
    let symbol = symbol.cast::<PackageDescriptor>();
    if symbol.is_null() || !symbol.is_aligned() {
        return Err(LoadError::NoEntry);
    }
    // SAFETY: the symbol is the package's `PackageDescriptor` static, the one thing a package
    // exports under this name, in a library that is never unloaded: it is valid for `'static`,
    // and nothing writes to it.
    Ok(unsafe { &*symbol })
}

/// The dynamic loader's last error on this thread.
fn last_error() -> String {
    // SAFETY: `dlerror` takes no arguments.
    let text = unsafe { libc::dlerror() };
    if text.is_null() {
        return String::from("the dynamic loader gave no reason");
    }
    // SAFETY: a non-null `dlerror` result is a NUL-terminated string, valid until this thread's
    // next call into the loader, and it is copied here before any.
    let text = unsafe { core::ffi::CStr::from_ptr(text) };
    String::from(text.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::FileExt;

    use super::*;

    #[test]
    fn a_sealed_copy_holds_the_bytes_and_takes_no_more() {
        let sealed = seal(b"some bytes").unwrap();
        let mut file = File::from(sealed);
        let mut back = [0; 10];
        file.read_exact_at(&mut back, 0).unwrap();
        assert_eq!(&back, b"some bytes");
        assert!(file.write_all(b"more").is_err());
        assert!(file.write_at(b"S", 0).is_err());
        assert!(file.set_len(4).is_err());
        assert!(file.set_len(40).is_err());
        file.read_exact_at(&mut back, 0).unwrap();
        assert_eq!(&back, b"some bytes");
    }

    #[test]
    fn the_loader_refuses_what_is_not_a_shared_object_and_loads_nothing() {
        let sealed = seal(b"not a shared object").unwrap();
        let Err(LoadError::Refused(why)) = load(sealed) else {
            panic!("loaded text");
        };
        assert!(!why.is_empty());
        // In a statically linked test binary the reason is that, and no loader was called.
        assert_eq!(supported(), !cfg!(target_feature = "crt-static"));
        assert_eq!(why == STATIC_PROGRAM, !supported());
    }
}
