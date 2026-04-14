#![forbid(unsafe_code)]

//! Packer: accumulate samples into fixed-size frames.
//!
//! The SDRplay callback delivers variable-size batches; downstream
//! consumers (audio sink, recorder) want fixed-size frames.

/// Accumulates `T` samples until `frame_size` is reached, then emits
/// one complete frame at a time.
///
/// This is the Rust-native equivalent of C++ `dsp::buffer::Packer<T>`.
pub struct Packer<T: Clone> {
    buffer: Vec<T>,
    frame_size: usize,
}

impl<T: Clone> Packer<T> {
    pub fn new(frame_size: usize) -> Self {
        assert!(frame_size > 0, "frame_size must be > 0");
        Self {
            buffer: Vec::with_capacity(frame_size * 2),
            frame_size,
        }
    }

    /// Push samples in. Returns any complete frames that have accumulated.
    /// Partial frames stay in the internal buffer.
    pub fn push(&mut self, samples: &[T]) -> Vec<Vec<T>> {
        self.buffer.extend_from_slice(samples);
        let mut frames = Vec::new();
        while self.buffer.len() >= self.frame_size {
            frames.push(self.buffer[..self.frame_size].to_vec());
            self.buffer.drain(..self.frame_size);
        }
        frames
    }

    /// How many samples are buffered (incomplete frame).
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Discard all buffered samples.
    pub fn flush(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn exact_multiple_produces_correct_frames() {
        let mut packer = Packer::new(4);
        let frames = packer.push(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], vec![1, 2, 3, 4]);
        assert_eq!(frames[1], vec![5, 6, 7, 8]);
        assert_eq!(packer.pending(), 0);
    }

    #[test]
    fn partial_batch_buffered() {
        let mut packer = Packer::new(4);
        let frames = packer.push(&[1, 2, 3]);
        assert_eq!(frames.len(), 0);
        assert_eq!(packer.pending(), 3);
        let frames2 = packer.push(&[4, 5]);
        assert_eq!(frames2.len(), 1);
        assert_eq!(frames2[0], vec![1, 2, 3, 4]);
        assert_eq!(packer.pending(), 1);
    }

    #[test]
    fn flush_clears_buffer() {
        let mut packer = Packer::new(4);
        packer.push(&[1, 2, 3]);
        packer.flush();
        assert_eq!(packer.pending(), 0);
    }

    proptest! {
        #[test]
        fn packer_total_samples_conserved(
            frame_size in 1usize..=32,
            batches in proptest::collection::vec(
                proptest::collection::vec(0i32..=100, 0..=64),
                1..=10
            )
        ) {
            let mut packer = Packer::new(frame_size);
            let total_in: usize = batches.iter().map(|b| b.len()).sum();
            let mut total_out = 0usize;

            for batch in &batches {
                let frames = packer.push(batch);
                for frame in &frames {
                    assert_eq!(frame.len(), frame_size);
                    total_out += frame.len();
                }
            }

            // All output frames have correct size
            // Samples in = samples out + pending
            assert_eq!(total_out + packer.pending(), total_in);
        }
    }
}
