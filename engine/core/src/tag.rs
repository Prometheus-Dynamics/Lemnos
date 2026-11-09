/// What a device provides, as `name/major`, such as `example.block/1`.
///
/// The engine only compares these. Only the contract beside the driver gives
/// one a meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(&'static str);

impl Tag {
    /// Panics, at compile time in a constant, on anything but `name/major`
    /// with a decimal major.
    pub const fn new(tag: &'static str) -> Self {
        let bytes = tag.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i] != b'/' {
            i += 1;
        }
        assert!(i > 0 && i + 1 < bytes.len(), "a tag is name/major");
        let mut j = i + 1;
        while j < bytes.len() {
            assert!(bytes[j].is_ascii_digit(), "a tag's major is a number");
            j += 1;
        }
        Self(tag)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }

    pub fn name(self) -> &'static str {
        self.0.split('/').next().unwrap_or(self.0)
    }

    pub fn major(self) -> u32 {
        self.0
            .rsplit('/')
            .next()
            .and_then(|major| major.parse().ok())
            .unwrap_or(0)
    }
}

impl core::fmt::Display for Tag {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}
