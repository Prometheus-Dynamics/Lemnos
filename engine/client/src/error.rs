use lemnos_core::Status;
use lemnos_wire::ring::RingError;
use lemnos_wire::{LEMNOS, WireError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not connected right now. The session keeps trying.
    Disconnected,
    /// The service answered no.
    Refused { status: Status, text: String },
    /// The service speaks another version of the protocol.
    Version { ours: u16, theirs: u16 },
    /// The service's core protocol differs from this client's: its core hash is not this
    /// client's. Nothing after the handshake's prefix was read. Only the core is compared here;
    /// see `docs/writing-a-driver.md`.
    LayoutMismatch {
        ours: u64,
        theirs: u64,
        theirs_lemnos: String,
    },
    /// A part of the wire (`part`, one of the feature parts) differs from this client's. Refused
    /// at the handshake, whatever the core. Nothing was called.
    PartMismatch {
        part: &'static str,
        ours: u64,
        theirs: u64,
    },
    /// The device's description gives contract `contract` another base hash than this client was
    /// built with: a base method's argument or result type differs. Nothing was called.
    ContractMismatch {
        contract: String,
        ours: u64,
        theirs: u64,
    },
    /// The device does not provide the contract at all. Nothing was called.
    ContractMissing { contract: String },
    /// The device's contract has no method `method` named `name`, which this client needs. The
    /// device's driver does not provide it. Nothing was called.
    MethodMissing {
        contract: String,
        method: u16,
        name: String,
    },
    /// The device's method `method` (`name`) has another shape than this client's. Nothing was
    /// called.
    MethodShape {
        contract: String,
        method: u16,
        name: String,
    },
    /// The device's method `method` (`name`) lacks an added field this client's method carries.
    /// Nothing was called.
    FieldMissing {
        contract: String,
        method: u16,
        name: String,
        field: String,
    },
    /// The service sent something that isn't the protocol.
    Wire(WireError),
    /// The handle predates a reconnect. Open it again.
    Stale,
    /// No answer in time.
    TimedOut,
    /// The destination buffer can't hold the value.
    TooSmall,
    /// Too many requests are waiting to be written.
    Full,
    /// A frame stream's shared memory broke the rules, or couldn't be set up. Only that stream is
    /// affected.
    Stream(RingError),
    /// Waiting for a frame failed in the operating system: the stream's doorbell is broken. Nothing
    /// was read; the frames are still in the ring.
    StreamWait(std::io::ErrorKind),
    /// The stream's buffers are dma-bufs, and the [`StreamPlatform`](crate::StreamPlatform) in
    /// use can't map them. The stream was given back.
    StreamUnsupported,
    /// A write to a stream that was taken to be read only
    /// ([`Session::open_stream`](crate::Session::open_stream)). Nothing was written.
    StreamReadOnly,
    /// A stream was asked for writable buffers ([`Session::open_stream_writable`](crate::Session::open_stream_writable))
    /// but a buffer's descriptor allows only read-only mappings (the host gave it read-only, or a
    /// seal forbids writes). Nothing was taken: the stream is given back.
    StreamBuffersReadOnly,
    /// A stream taken with [`Session::open_stream_unmapped`](crate::Session::open_stream_unmapped)
    /// maps none of its buffers, so its bytes can't be read or written through the stream: use
    /// the buffers' regions and descriptors instead. Nothing was read or written.
    StreamUnmapped,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Disconnected => f.write_str("not connected to the service"),
            Self::Refused { status, text } => write!(f, "refused ({}): {text}", status.0),
            Self::Version { ours, theirs } => {
                write!(
                    f,
                    "protocol {theirs} from the service, this client speaks {ours}"
                )
            }
            Self::LayoutMismatch {
                ours,
                theirs,
                theirs_lemnos,
            } => write!(
                f,
                "core protocol differs (hash {ours:#018x} here, {theirs:#018x} there; the service is Lemnos {theirs_lemnos}, this client is Lemnos {LEMNOS})"
            ),
            Self::PartMismatch { part, ours, theirs } => write!(
                f,
                "wire part `{part}` differs (hash {ours:#018x} here, {theirs:#018x} there)"
            ),
            Self::ContractMismatch {
                contract,
                ours,
                theirs,
            } => write!(
                f,
                "contract {contract}: a base method is laid out differently (the device's base hash {theirs:#018x}, this client's {ours:#018x})"
            ),
            Self::ContractMissing { contract } => {
                write!(f, "the device does not provide contract {contract}")
            }
            Self::MethodMissing {
                contract,
                method,
                name,
            } => write!(
                f,
                "contract {contract}: this client needs method {method} `{name}`; the device's driver does not provide it"
            ),
            Self::MethodShape {
                contract,
                method,
                name,
            } => write!(
                f,
                "contract {contract}: this client needs method {method} `{name}`; the device's driver has another shape"
            ),
            Self::FieldMissing {
                contract,
                method,
                name,
                field,
            } => write!(
                f,
                "contract {contract}: this client needs field `{field}` of method {method} `{name}`; the device's driver does not provide it"
            ),
            Self::Wire(error) => write!(f, "{error}"),
            Self::Stale => f.write_str("the handle predates a reconnect"),
            Self::TimedOut => f.write_str("no answer in time"),
            Self::TooSmall => f.write_str("the buffer is too small"),
            Self::Full => f.write_str("too many requests waiting"),
            Self::Stream(error) => write!(f, "the frame stream: {error}"),
            Self::StreamWait(kind) => write!(f, "waiting for a frame failed: {kind}"),
            Self::StreamUnmapped => {
                f.write_str("this stream maps none of its buffers: read them through their regions")
            }
            Self::StreamUnsupported => {
                f.write_str("the frame stream's buffers are dma-bufs, which can't be mapped here")
            }
            Self::StreamReadOnly => {
                f.write_str("the frame stream was taken to be read, not written")
            }
            Self::StreamBuffersReadOnly => {
                f.write_str("this stream's buffers are read-only: it can't be taken writable")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<WireError> for Error {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

impl From<Error> for Status {
    fn from(error: Error) -> Self {
        match error {
            Error::Disconnected => Status::NO_DEVICE,
            Error::Refused { status, .. } => status,
            Error::Version { .. } => Status::NOT_SUPPORTED,
            Error::LayoutMismatch { .. }
            | Error::PartMismatch { .. }
            | Error::ContractMismatch { .. }
            | Error::MethodShape { .. } => Status::VERSION_MISMATCH,
            Error::FieldMissing { .. } | Error::MethodMissing { .. } => Status::NOT_SUPPORTED,
            Error::ContractMissing { .. } => Status::NOT_FOUND,
            Error::Wire(_) => Status::IO,
            Error::Stale => Status::AGAIN,
            Error::TimedOut => Status::TIMED_OUT,
            Error::TooSmall => Status::BUFFER_TOO_SMALL,
            Error::Full => Status::BUSY,
            Error::Stream(_) | Error::StreamWait(_) => Status::IO,
            Error::StreamUnsupported | Error::StreamUnmapped => Status::NOT_SUPPORTED,
            Error::StreamReadOnly | Error::StreamBuffersReadOnly => Status::ACCESS,
        }
    }
}
