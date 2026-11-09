//! A simulated producer for frame streams: frames with contents a test can check.

use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint};

/// Puts a frame's bytes into a buffer, as a device would.
pub type FrameWriter<'a> = dyn FnMut(u32, &[u8]) -> Result<bool, ErrorKind> + 'a;

/// Produces frames onto any [`StreamEndpoint`]. Each frame's contents, identity and timestamp come
/// from its sequence number, so a consumer can check all of them. It uses one scratch buffer, so
/// after the first frame it allocates nothing.
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

    /// What frame `sequence` carries as its identity. Means nothing to anyone but the test.
    #[must_use]
    pub const fn identity(sequence: u64) -> u64 {
        sequence ^ 0xFEED_0000_0000_0000
    }

    /// The next sequence number this source will produce.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Takes a free buffer, fills it through `write` and publishes it. `false` when no buffer is
    /// free, which isn't an error and uses up no sequence number.
    pub fn produce(
        &mut self,
        stream: &mut impl StreamEndpoint<Error = ErrorKind>,
        write: &mut FrameWriter<'_>,
    ) -> Result<bool, ErrorKind> {
        let Some(buffer) = stream.next_buffer()? else {
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
            identity: Self::identity(sequence),
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
        assert_eq!(first.identity, SimFrameSource::identity(0));
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
