//! [`Held`]: what a driver owns of a resource once it has bound, and how the resource goes back
//! where it came from when the driver is dropped.

use core::ops::{Deref, DerefMut};

/// Where a lent resource lives when nobody holds it. A host with no allocator keeps each bus in
/// a `static` place like this; dropping the [`Held`] that was lent from it puts the bus back, so
/// the next driver bound to it can take it again.
pub trait Home<T: ?Sized>: Sync {
    /// Takes the resource back. Called once for each time it was lent, when its holder goes.
    fn take_back(&self, resource: &'static mut T);
}

/// Something a driver owns once it has bound: a bus, a line, a stream's end.
///
/// It is the same type with and without an allocator, so code that names it builds either way.
/// With one (feature `alloc`), a host usually boxes the resource ([`Held::boxed`]) and dropping
/// the `Held` frees it. Without one, the resource is a `static` the firmware owns, lent out
/// with [`Held::lent`]: dropping the `Held` hands the reference back to the [`Home`] it was
/// lent from. Either way a dropped driver gives up everything it held, and a host can bind
/// another in its place.
pub struct Held<T: ?Sized + 'static>(Inner<T>);

enum Inner<T: ?Sized + 'static> {
    #[cfg(feature = "alloc")]
    Boxed(alloc::boxed::Box<T>),
    /// `None` only while it is being given back.
    Lent(Option<&'static mut T>, &'static dyn Home<T>),
}

impl<T: ?Sized> Held<T> {
    /// A resource lent from `home`, which gets it back when this is dropped.
    pub fn lent(resource: &'static mut T, home: &'static dyn Home<T>) -> Self {
        Self(Inner::Lent(Some(resource), home))
    }

    /// A resource this owns outright.
    #[cfg(feature = "alloc")]
    pub fn boxed(resource: alloc::boxed::Box<T>) -> Self {
        Self(Inner::Boxed(resource))
    }
}

#[cfg(feature = "alloc")]
impl<T: ?Sized> From<alloc::boxed::Box<T>> for Held<T> {
    fn from(resource: alloc::boxed::Box<T>) -> Self {
        Self::boxed(resource)
    }
}

impl<T: ?Sized> Deref for Held<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match &self.0 {
            #[cfg(feature = "alloc")]
            Inner::Boxed(resource) => resource,
            Inner::Lent(Some(resource), _) => resource,
            Inner::Lent(None, _) => given_back(),
        }
    }
}

/// Only `drop` empties a lent `Held`, and nothing reads it after: this is never reached. It
/// stops where it is instead of panicking, so a firmware pays nothing for a message.
#[cold]
fn given_back() -> ! {
    #[allow(clippy::empty_loop)]
    loop {}
}

impl<T: ?Sized> DerefMut for Held<T> {
    fn deref_mut(&mut self) -> &mut T {
        match &mut self.0 {
            #[cfg(feature = "alloc")]
            Inner::Boxed(resource) => resource,
            Inner::Lent(Some(resource), _) => resource,
            Inner::Lent(None, _) => given_back(),
        }
    }
}

impl<T: ?Sized> Drop for Held<T> {
    fn drop(&mut self) {
        if let Inner::Lent(resource, home) = &mut self.0
            && let Some(resource) = resource.take()
        {
            home.take_back(resource);
        }
    }
}

impl<T: ?Sized> core::fmt::Debug for Held<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Held")
    }
}

/// Exclusive use of a value from a shared, `'static` place, supplied by the firmware.
///
/// On one core this is usually a critical section around a `RefCell`; on a host, a mutex. A
/// device's calls run inside its lock, and a device can call another inside its own, so locks
/// nest: a lock that is already held must answer `None` rather than wait for itself.
pub trait Lock: Sync {
    type Inner;

    /// Runs `f` on the value, or returns `None` when the value is in use (a call into the same
    /// device that hasn't returned).
    fn with<R>(&self, f: impl FnOnce(&mut Self::Inner) -> R) -> Option<R>;
}
