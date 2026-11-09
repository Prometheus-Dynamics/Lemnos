/// A reference to an object behind the boundary: a slot index in the low 32
/// bits and the slot's generation in the high 32. A generation is never 0, so
/// 0 is never issued: it is [`Handle::NULL`]. Releasing an object bumps its
/// slot's generation, so a handle kept past the release is refused
/// ([`Status::STALE_HANDLE`](crate::Status::STALE_HANDLE)), not taken for the
/// slot's next object.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(pub u64);

impl Handle {
    pub const NULL: Self = Self(0);

    pub const fn new(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }

    pub const fn index(self) -> u32 {
        self.0 as u32
    }

    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub const fn is_null(self) -> bool {
        self.generation() == 0
    }
}
