//! A consumer reads a stream's buffer count and size from the device's description before it
//! attaches, so it can size the hold it declares without opening the stream twice. Nothing is
//! attached or allocated by the read. Before the layouts were described, a consumer could learn
//! the count only from an attached stream.

mod common;

use std::sync::Arc;

use common::start_with;
use lemnos_client::{Error, Session, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_wire::StreamLayout;

const BOARD: &str = "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
                     [devices.streams.frames]\nbuffers = 6\nbuffer_bytes = 4096\n\
                     [devices.streams.other]\nbuffers = 2\nbuffer_bytes = 128\n";

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

#[test]
fn a_streams_buffer_count_is_described_before_anything_is_attached_or_allocated() {
    let running = start_with(BOARD);
    let host = running.service.as_ref().unwrap().host();
    let mut session = Session::connect(&running.socket, "consumer").unwrap();

    let described = session.describe(0).unwrap();
    assert_eq!(
        described.stream_layout("frames"),
        Some(&StreamLayout {
            name: "frames".to_string(),
            buffers: 6,
            buffer_bytes: 4096
        })
    );
    assert_eq!(
        described
            .stream_layout("other")
            .map(|l| (l.buffers, l.buffer_bytes)),
        Some((2, 128))
    );
    // Nothing attached and no memory made by the read (the board keeps no standby).
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(
        (
            stats.attached,
            stats.attachments,
            stats.standby_bytes,
            stats.held_by_driver
        ),
        (false, 0, 0, 0),
        "{stats:?}"
    );

    // The consumer's own rule sizes its hold from the count; one attach, with that hold.
    let buffers = described.stream_layout("frames").unwrap().buffers;
    let hold = buffers - (buffers / 2).max(2);
    let stream = session
        .open_stream_held(0, "frames", hold, platform())
        .unwrap();
    assert_eq!(stream.buffer_count(), buffers);
    assert_eq!(host.stream_stats(0, "frames").unwrap().attachments, 1);
    drop(stream);

    // A hold that doesn't fit the count is refused at attach, by name, and attaches nothing.
    let refused = session.open_stream_held(0, "other", 1, platform()).err();
    assert!(
        matches!(&refused, Some(Error::Refused { status, text }) if *status == Status::BUSY && text.contains("\"other\"")),
        "{refused:?}"
    );
    assert_eq!(host.stream_stats(0, "other").unwrap().attachments, 0);
}
