//! Embedded data off a metadata node, into the embedded stream: one line a frame, copied.
//!
//! A stock V4L2 metadata node (`V4L2_BUF_TYPE_META_CAPTURE`) carries a sensor's embedded data
//! as one buffer per frame, with the frame's sequence on it. This module takes those lines off
//! the node and publishes each on the receiver's `embedded` stream, carrying the node's sequence
//! number. Nothing about a line's layout is read here: its bytes are the sensor's, and the
//! sensor's library crate decodes them.
//!
//! A line is copied, never shared: the node's memory is its own and a stream buffer is the
//! consumer's, so each line goes through `StreamEndpoint::write`. A line that is bad, empty, or
//! finds no free buffer is not published, and the node's buffer goes back to the node either way.

use lemnos_core::{Result, Status};
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint};

use lemnos_v4l2::FourCc;

use crate::hw::{Hardware, Wanted};

/// Buffers a metadata node is started with: every one queued, so no line waits for a buffer.
pub const LINE_BUFFERS: u32 = 4;

/// Opens the metadata node for lines of `bytes` in data format `format`, and starts it with
/// [`LINE_BUFFERS`] buffers queued. `INVALID_ARGUMENT` for a line of no bytes; an error from the
/// node when it can't give a buffer of the line's size.
pub fn start(node: &mut dyn Hardware, format: FourCc, bytes: u32) -> Result<()> {
    if bytes == 0 {
        return Err(Status::INVALID_ARGUMENT);
    }
    let granted = node.open(&Wanted {
        input: None,
        fourcc: format,
        width: 0,
        height: 0,
        stride: 0,
        size: bytes,
        mode: 0,
    })?;
    if granted.size < bytes {
        return Err(Status::INVALID_ARGUMENT);
    }
    node.stream_on(LINE_BUFFERS)
}

/// Moves every filled line the node has into `stream`, one stream buffer per line, and gives the
/// node's buffer back. Returns how many lines were published. `NOT_SUPPORTED` is never returned
/// here; an error means the node or the stream failed and the caller should stop.
pub fn pump(
    node: &mut dyn Hardware,
    stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
) -> Result<u32> {
    let mut published = 0;
    while let Some(frame) = node.dequeue()? {
        let line = publish_line(node, stream, &frame);
        node.requeue(frame.index)?;
        if line? {
            published += 1;
        }
    }
    Ok(published)
}

/// Copies the node's line for `frame` into a free stream buffer and publishes it. Whether it was
/// published.
fn publish_line(
    node: &mut dyn Hardware,
    stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
    frame: &crate::hw::Frame,
) -> Result<bool> {
    if frame.bad || frame.bytes == 0 {
        return Ok(false);
    }
    let Some(buffer) = stream.next_buffer().map_err(|_| Status::IO)? else {
        // Nobody has given a buffer: the line is dropped, as a frame is when no buffer is free.
        return Ok(false);
    };
    let data = node.payload(frame, 0)?;
    let written = stream.write(buffer, 0, data).map_err(|_| Status::IO)?;
    let bytes = if written {
        u32::try_from(data.len()).map_err(|_| Status::IO)?
    } else {
        // The stream would not take the copy: the buffer goes back empty, which is not a frame.
        0
    };
    stream
        .publish(StreamFrame {
            buffer,
            bytes,
            sequence: u64::from(frame.sequence),
            identity: 0,
            timestamp_ns: frame.timestamp_ns,
        })
        .map_err(|_| Status::IO)?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use lemnos_core::{Result, Status};
    use lemnos_hal::blocks::BufferHandle;
    use lemnos_hal::mock::MockStream;

    use crate::hw::{Frame, Granted, Graph, Hardware, Wanted};

    /// A metadata node that serves what it is given and records the requeues.
    #[derive(Default)]
    struct Node {
        filled: Vec<(Frame, Vec<u8>)>,
        requeued: Vec<u32>,
        payload_of: Option<Vec<u8>>,
    }

    impl Hardware for Node {
        fn open(&mut self, _wanted: &Wanted) -> Result<Granted> {
            Err(Status::NOT_SUPPORTED)
        }
        fn stream_on(&mut self, _buffers: u32) -> Result<()> {
            Ok(())
        }
        fn stream_on_importing(&mut self, _slots: u32) -> Result<()> {
            Err(Status::NOT_SUPPORTED)
        }
        fn queue_handles(&mut self, _index: u32, _planes: &[BufferHandle<'_>]) -> Result<()> {
            Err(Status::NOT_SUPPORTED)
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
        fn dequeue(&mut self) -> Result<Option<Frame>> {
            if self.filled.is_empty() {
                return Ok(None);
            }
            let (frame, data) = self.filled.remove(0);
            self.payload_of = Some(data);
            Ok(Some(frame))
        }
        fn payload(&self, _frame: &Frame, _plane: usize) -> Result<&[u8]> {
            self.payload_of.as_deref().ok_or(Status::IO)
        }
        fn requeue(&mut self, index: u32) -> Result<()> {
            self.requeued.push(index);
            Ok(())
        }
        fn graph(&mut self) -> Result<Graph> {
            Err(Status::NOT_SUPPORTED)
        }
    }

    fn line(index: u32, sequence: u32, data: Vec<u8>, bad: bool) -> (Frame, Vec<u8>) {
        let bytes = data.len() as u32;
        (
            Frame {
                index,
                sequence,
                timestamp_ns: 500 + u64::from(sequence),
                bad,
                planes: 1,
                bytes,
            },
            data,
        )
    }

    #[test]
    fn each_line_is_copied_onto_the_stream_with_its_frames_sequence() {
        let stream = MockStream::with_bytes(4, 256);
        let mut node = Node {
            filled: vec![
                line(0, 7, vec![1; 256], false),
                line(1, 8, vec![2; 128], false),
            ],
            ..Node::default()
        };
        let mut s = stream.clone();
        assert_eq!(super::pump(&mut node, &mut s), Ok(2));
        let first = stream.take_frame().expect("the first line is published");
        assert_eq!((first.buffer, first.bytes, first.sequence), (0, 256, 7));
        assert_eq!(stream.contents(0), vec![1; 256]);
        let second = stream.take_frame().expect("the second line is published");
        assert_eq!((second.buffer, second.bytes, second.sequence), (1, 128, 8));
        assert_eq!(stream.contents(1)[..128], [2; 128]);
        assert_eq!(
            node.requeued,
            [0, 1],
            "each node buffer goes back to the node"
        );
    }

    #[test]
    fn a_bad_or_empty_line_is_not_published_and_its_buffer_goes_back() {
        let stream = MockStream::with_bytes(4, 256);
        let mut node = Node {
            filled: vec![line(0, 1, vec![9; 64], true), line(1, 2, Vec::new(), false)],
            ..Node::default()
        };
        let mut s = stream.clone();
        assert_eq!(super::pump(&mut node, &mut s), Ok(0));
        assert!(stream.take_frame().is_none());
        assert_eq!(node.requeued, [0, 1]);
    }

    #[test]
    fn a_line_finds_no_free_buffer_and_is_dropped_not_held() {
        let stream = MockStream::with_bytes(1, 256);
        let mut s = stream.clone();
        let mut first = Node {
            filled: vec![line(0, 1, vec![3; 32], false)],
            ..Node::default()
        };
        assert_eq!(super::pump(&mut first, &mut s), Ok(1));
        // The one buffer is the consumer's now: the next line has nowhere to go.
        let mut second = Node {
            filled: vec![line(0, 2, vec![4; 32], false)],
            ..Node::default()
        };
        assert_eq!(super::pump(&mut second, &mut s), Ok(0));
        assert_eq!(
            second.requeued,
            [0],
            "the node's buffer is given back regardless"
        );
    }
}
