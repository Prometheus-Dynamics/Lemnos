//! The bounce trace of the link check: the comm core's own log to the console, with the time and the
//! thread of each event, and each open, read and write on its link stream with its length and first
//! bytes. The boundary is the link check's (Orion is unchanged). Built only with the `trace` feature,
//! which `testing/qemu/run.sh` turns on with `LINK_CHECK_TRACE=1`; off by default.

use std::io;

use orion_link_serial::{ByteStream, Open};
use orion_link_usb::{UsbLink, UsbStream};

/// Starts the comm core's log on the console.
pub fn subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
        .with_ansi(false)
        // The thread of each event: the old and the new endpoint reader are different threads.
        .with_thread_ids(true)
        .with_thread_names(true)
        .try_init();
}

/// The link as the comm core's carrier sees it: each open, and each read and write on the stream, with
/// its length and first bytes.
pub struct Traced(pub UsbLink);

impl Open for Traced {
    type Stream = TracedStream;

    async fn open(&mut self) -> io::Result<TracedStream> {
        match Open::open(&mut self.0).await {
            Ok(stream) => {
                tracing::info!(target: "trace", "trace comm open: the carrier has a stream");
                Ok(TracedStream(stream))
            }
            Err(error) => {
                tracing::info!(target: "trace", %error, "trace comm open failed");
                Err(error)
            }
        }
    }

    fn describe(&self) -> String {
        format!("traced {}", Open::describe(&self.0))
    }
}

/// A stream the carrier reads and writes, logged.
pub struct TracedStream(UsbStream);

fn head(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(12)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl ByteStream for TracedStream {
    async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let result = ByteStream::read(&self.0, buf).await;
        match &result {
            Ok(len) => {
                tracing::info!(target: "trace", len = *len, head = %head(&buf[..*len]), "trace comm read");
            }
            Err(error) => {
                tracing::info!(target: "trace", %error, "trace comm read failed");
            }
        }
        result
    }

    async fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        let result = ByteStream::write(&self.0, bytes).await;
        match &result {
            Ok(len) => {
                tracing::info!(target: "trace", len = *len, head = %head(bytes), "trace comm write");
            }
            Err(error) => {
                tracing::info!(target: "trace", %error, "trace comm write failed");
            }
        }
        result
    }
}
