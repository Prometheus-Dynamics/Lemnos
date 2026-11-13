//! A simulated producer for frame streams: frames with contents a test can check.

use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint};

/// Puts a frame's bytes into a buffer, as a device would.
pub type FrameWriter<'a> = dyn FnMut(u32, &[u8]) -> Result<bool, ErrorKind> + 'a;

/// Produces frames onto any [`StreamEndpoint`]. Each frame's contents and timestamp come from its
/// sequence number, so a consumer can check them. The sequence is the frame counter from this
/// source's start. The identity is what the caller names: 0 for a camera receiver, which takes no
/// job (as on hardware), or the job's identity for a producer that answers one. It uses one
/// scratch buffer, so after the first frame it allocates nothing.
#[derive(Debug)]
pub struct SimFrameSource {
    bytes: usize,
    sequence: u64,
    /// Set up with the length of a frame so filling it allocates nothing.
    scratch: Vec<u8>,
}

impl SimFrameSource {
    #[must_use]
    pub fn new(bytes: usize) -> Self {
        Self {
            bytes,
            sequence: 0,
            scratch: vec![0; bytes],
        }
    }

    /// The bytes of frame `sequence`.
    pub fn fill(sequence: u64, out: &mut [u8]) {
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = (sequence as usize).wrapping_mul(31).wrapping_add(index) as u8;
        }
    }

    /// The next sequence number this source will produce.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Takes a free buffer, fills it through `write` and publishes it with identity 0, as a
    /// camera receiver publishes. `false` when no buffer is free, which isn't an error and uses up
    /// no sequence number.
    pub fn produce(
        &mut self,
        stream: &mut impl StreamEndpoint<Error = ErrorKind>,
        write: &mut FrameWriter<'_>,
    ) -> Result<bool, ErrorKind> {
        self.produce_with_identity(stream, 0, write)
    }

    /// [`Self::produce`], publishing `identity` (the job the frame answers, for a producer that
    /// takes jobs).
    pub fn produce_with_identity(
        &mut self,
        stream: &mut impl StreamEndpoint<Error = ErrorKind>,
        identity: u64,
        write: &mut FrameWriter<'_>,
    ) -> Result<bool, ErrorKind> {
        // A frame needs a buffer the producer picks, so finding none is an ask: counted as refused on a
        // job-driven stream.
        let Some(buffer) = stream.ask_buffer()? else {
            return Ok(false);
        };
        let sequence = self.sequence;
        self.sequence += 1;
        Self::fill(sequence, &mut self.scratch);
        write(buffer, &self.scratch)?;
        stream.publish(StreamFrame {
            buffer,
            bytes: self.bytes as u32,
            sequence,
            identity,
            timestamp_ns: sequence * 1000,
        })?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_hal::mock::MockStream;

    #[test]
    fn frames_carry_what_their_sequence_says() {
        let mut stream = MockStream::new(2);
        let consumer = stream.clone();
        let mut source = SimFrameSource::new(8);
        let mut written = Vec::new();
        for _ in 0..2 {
            let made = source
                .produce(&mut stream, &mut |buffer, bytes| {
                    written.push((buffer, bytes.to_vec()));
                    Ok(true)
                })
                .unwrap();
            assert!(made);
        }
        assert_eq!(source.sequence(), 2);
        let first = consumer.take_frame().unwrap();
        assert_eq!(first.sequence, 0);
        assert_eq!(first.identity, 0, "a receiver's frames carry identity 0");
        assert_eq!(first.bytes, 8);
        assert_eq!(written[0].1, [0, 1, 2, 3, 4, 5, 6, 7]);
        let second = consumer.take_frame().unwrap();
        assert_eq!(second.timestamp_ns, 1000);
        assert_eq!(written[1].1[0], 31);
    }

    #[test]
    fn no_free_buffer_is_not_an_error_and_keeps_the_sequence() {
        let mut stream = MockStream::new(1);
        let mut source = SimFrameSource::new(1);
        let mut write = |_, _: &[u8]| Ok(true);
        assert!(source.produce(&mut stream, &mut write).unwrap());
        assert!(!source.produce(&mut stream, &mut write).unwrap());
        assert_eq!(source.sequence(), 1);
    }
}
