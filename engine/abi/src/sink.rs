use core::ffi::c_void;

/// Where a call writes a message, usually the text of an error: a C callback
/// and its context, in the same shape as the graph runtime's plugin string
/// sink. The callee may write several times; the caller appends. A null
/// `write` drops the message. Not part of [`LAYOUT_HASH`](crate::LAYOUT_HASH):
/// its size is the target's pointer width.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MessageSink {
    pub context: *mut c_void,
    pub write: Option<extern "C" fn(context: *mut c_void, text: *const u8, len: usize)>,
}

impl MessageSink {
    /// A sink that drops everything.
    pub const NONE: Self = Self {
        context: core::ptr::null_mut(),
        write: None,
    };

    /// Hands `text` (UTF-8, not NUL-terminated) to the callback, valid for the
    /// callback's duration only.
    pub fn write(&self, text: &str) {
        if let Some(write) = self.write {
            write(self.context, text.as_ptr(), text.len());
        }
    }

    /// Writes a formatted message without allocating: in pieces, as
    /// `core::fmt` produces them.
    pub fn write_fmt(&self, args: core::fmt::Arguments<'_>) {
        struct Pieces<'a>(&'a MessageSink);
        impl core::fmt::Write for Pieces<'_> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.0.write(s);
                Ok(())
            }
        }
        let _ = core::fmt::write(&mut Pieces(self), args);
    }
}
