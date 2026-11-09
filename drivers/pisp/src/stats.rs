//! Front end statistics decoded from the buffer the hardware writes.
//!
//! The buffer's bytes are untrusted: it may be the wrong length, and any field may hold any
//! value. A short buffer is an error; a counter that wraps is handled, not a panic.

use crate::math;
use crate::uapi::*;

impl RawAwbZone {
    /// Mean `(r, g, b)` per counted pixel, `None` when nothing was counted.
    pub fn mean(&self) -> Option<(f64, f64, f64)> {
        (self.counted > 0).then(|| {
            let n = f64::from(self.counted);
            (
                f64::from(self.r_sum) / n,
                f64::from(self.g_sum) / n,
                f64::from(self.b_sum) / n,
            )
        })
    }
}

impl RawAgcZone {
    /// Mean Y per counted pixel.
    pub fn mean(&self) -> Option<f64> {
        (self.counted > 0).then(|| self.y_sum as f64 / f64::from(self.counted))
    }
}

/// Statistics for one frame. Large (23 KiB): keep one and decode into it with [`Self::parse_into`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statistics {
    /// The decoded buffer, in the hardware's layout.
    pub raw: RawStatistics,
}

impl Default for Statistics {
    fn default() -> Self {
        Self {
            raw: RawStatistics::ZERO,
        }
    }
}

/// A statistics buffer shorter than the structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadStatsLength(pub usize);

impl core::fmt::Display for BadStatsLength {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "statistics buffer is {} bytes, expected at least {}",
            self.0,
            RawStatistics::SIZE
        )
    }
}

impl core::error::Error for BadStatsLength {}

impl Statistics {
    /// Decodes a statistics buffer into `self`, allocating nothing. On error `self` is unchanged.
    pub fn parse_into(&mut self, bytes: &[u8]) -> Result<(), BadStatsLength> {
        self.raw
            .read_from(bytes)
            .map_err(|_| BadStatsLength(bytes.len()))
    }

    /// Totals over all AWB zones. Sums wrap, as the counters of a garbage buffer may.
    pub fn awb_total(&self) -> RawAwbZone {
        self.raw
            .awb
            .zones
            .iter()
            .fold(RawAwbZone::default(), |a, z| RawAwbZone {
                r_sum: a.r_sum.wrapping_add(z.r_sum),
                g_sum: a.g_sum.wrapping_add(z.g_sum),
                b_sum: a.b_sum.wrapping_add(z.b_sum),
                counted: a.counted.wrapping_add(z.counted),
            })
    }

    /// Pixels in the histogram.
    pub fn histogram_count(&self) -> u64 {
        self.raw.agc.histogram.iter().map(|&c| u64::from(c)).sum()
    }

    /// Mean histogram bin (0..1024), `None` for an empty histogram.
    pub fn histogram_mean(&self) -> Option<f64> {
        let n = self.histogram_count();
        (n > 0).then(|| {
            let s: f64 = self
                .raw
                .agc
                .histogram
                .iter()
                .enumerate()
                .map(|(i, &c)| (i as f64 + 0.5) * f64::from(c))
                .sum();
            s / n as f64
        })
    }

    /// The histogram bin below which `q` (0..1) of the pixels lie.
    pub fn histogram_quantile(&self, q: f64) -> Option<usize> {
        let n = self.histogram_count();
        if n == 0 {
            return None;
        }
        let target = math::ceil(q.clamp(0.0, 1.0) * n as f64) as u64;
        let mut acc = 0u64;
        for (i, &c) in self.raw.agc.histogram.iter().enumerate() {
            acc += u64::from(c);
            if acc >= target.max(1) {
                return Some(i);
            }
        }
        Some(AGC_STATS_NUM_BINS - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_of(raw: &RawStatistics) -> Vec<u8> {
        let mut v = vec![0; RawStatistics::SIZE];
        raw.write_to(&mut v).unwrap();
        v
    }

    #[test]
    fn parses_a_synthetic_buffer() {
        let mut raw = RawStatistics::ZERO;
        raw.awb.zones[33] = RawAwbZone {
            r_sum: 300,
            g_sum: 600,
            b_sum: 150,
            counted: 3,
        };
        raw.agc.histogram[10] = 4;
        raw.agc.histogram[20] = 4;
        raw.agc.floating[0] = RawAgcZone {
            y_sum: 1000,
            counted: 10,
            pad: 0,
        };
        raw.cdaf.foms[63] = 7;
        let mut s = Statistics::default();
        s.parse_into(&bytes_of(&raw)).unwrap();
        assert_eq!(s.raw.awb.zones[33].mean(), Some((100.0, 200.0, 50.0)));
        assert_eq!(s.awb_total().counted, 3);
        assert_eq!(s.histogram_count(), 8);
        assert_eq!(s.histogram_mean(), Some(15.5));
        assert_eq!(s.histogram_quantile(0.5), Some(10));
        assert_eq!(s.histogram_quantile(0.51), Some(20));
        assert_eq!(s.raw.agc.floating[0].mean(), Some(100.0));
        assert_eq!(s.raw.cdaf.foms[63], 7);
    }

    #[test]
    fn a_short_buffer_is_an_error_and_changes_nothing() {
        let mut s = Statistics::default();
        s.raw.cdaf.foms[0] = 5;
        assert_eq!(s.parse_into(&[0; 10]).unwrap_err(), BadStatsLength(10));
        assert_eq!(
            s.parse_into(&[0; RawStatistics::SIZE - 1]).unwrap_err(),
            BadStatsLength(RawStatistics::SIZE - 1)
        );
        assert_eq!(s.raw.cdaf.foms[0], 5);
    }

    #[test]
    fn a_longer_buffer_reads_the_structure_and_ignores_the_rest() {
        let mut raw = RawStatistics::ZERO;
        raw.agc.histogram[1] = 9;
        let mut bytes = bytes_of(&raw);
        bytes.extend_from_slice(&[0xff; 64]);
        let mut s = Statistics::default();
        s.parse_into(&bytes).unwrap();
        assert_eq!(s.histogram_count(), 9);
    }

    #[test]
    fn saturated_counts_do_not_overflow() {
        // A buffer of 0xFF bytes (garbage or uninitialised): the zone counts add up past u32
        // and wrap like the sums instead of panicking.
        let mut s = Statistics::default();
        s.parse_into(&[0xFF; RawStatistics::SIZE]).unwrap();
        assert_eq!(s.awb_total().counted, u32::MAX.wrapping_mul(1024));
        assert_eq!(s.histogram_quantile(f64::NAN), Some(0));
        assert_eq!(s.histogram_quantile(2.0), Some(AGC_STATS_NUM_BINS - 1));
    }

    #[test]
    fn an_empty_histogram_has_no_mean_or_quantile() {
        let s = Statistics::default();
        assert_eq!(s.histogram_mean(), None);
        assert_eq!(s.histogram_quantile(0.5), None);
        assert_eq!(s.raw.awb.zones[0].mean(), None);
        assert_eq!(s.raw.agc.floating[0].mean(), None);
    }
}
