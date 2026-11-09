use lemnos_core::Status;
use lemnos_wire::WireError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not connected right now. The session keeps trying.
    Disconnected,
    /// The service answered no.
    Refused { status: Status, text: String },
    /// The service speaks another version of the protocol.
    Version { ours: u16, theirs: u16 },
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
            Self::Wire(error) => write!(f, "{error}"),
            Self::Stale => f.write_str("the handle predates a reconnect"),
            Self::TimedOut => f.write_str("no answer in time"),
            Self::TooSmall => f.write_str("the buffer is too small"),
            Self::Full => f.write_str("too many requests waiting"),
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
            Error::Wire(_) => Status::IO,
            Error::Stale => Status::AGAIN,
            Error::TimedOut => Status::TIMED_OUT,
            Error::TooSmall => Status::BUFFER_TOO_SMALL,
            Error::Full => Status::BUSY,
        }
    }
}
