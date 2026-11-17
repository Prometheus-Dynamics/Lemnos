//! The arithmetic of the B8 capture: decode the records of each batch, and report what the chip
//! delivered. The binary (`main.rs`) only reads batches through the daemon and hands them here, so
//! everything the record checks is measured by code that is tested on its own.
//!
//! A record is the accelerometer contract's [`RECORD_LEN`] bytes: the stamp (`at_ns`) and the value.
//! A batch that is not a whole number of records is refused, not decoded: a batch is whole records
//! only, so a partial one is a driver or transport fault and the capture says so.

use lemnos_contract_accelerometer::{RECORD_LEN, Sample};
use lemnos_core::{Decode, Reader};

/// What one capture measured. Times are the daemon's host clock (`at_ns`), in nanoseconds.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Batch reads that were made.
    pub reads: u64,
    /// Batch reads that returned no records.
    pub empty_reads: u64,
    /// The largest number of records one read returned.
    pub max_per_read: usize,
    /// Records decoded.
    pub records: u64,
    /// Bytes that did not make a whole record, in batches that were refused.
    pub refused_bytes: u64,
    /// The stamps of the records, in order (for the gap and period figures).
    pub stamps: Vec<u64>,
    /// Records whose value equals the one before it.
    pub repeats: u64,
    /// The previous record's value, to count repeats across batches.
    last: Option<[f32; 3]>,
}

/// One batch's outcome.
#[derive(Debug, PartialEq)]
pub enum BatchError {
    /// `bytes` is not a whole number of records: the chip or the transport handed over part of one.
    Partial { bytes: usize },
    /// A record did not decode.
    Undecodable { index: usize },
}

impl Report {
    /// Records one batch read that returned `bytes` bytes into `batch`.
    pub fn add_batch(&mut self, batch: &[u8]) -> Result<(), BatchError> {
        self.reads += 1;
        if batch.is_empty() {
            self.empty_reads += 1;
            return Ok(());
        }
        if !batch.len().is_multiple_of(RECORD_LEN) {
            self.refused_bytes += batch.len() as u64;
            return Err(BatchError::Partial { bytes: batch.len() });
        }
        let count = batch.len() / RECORD_LEN;
        self.max_per_read = self.max_per_read.max(count);
        for (index, record) in batch.as_chunks::<RECORD_LEN>().0.iter().enumerate() {
            let sample = Sample::decode(&mut Reader::new(record))
                .map_err(|_| BatchError::Undecodable { index })?;
            let value = [sample.value.x, sample.value.y, sample.value.z];
            if self.last == Some(value) {
                self.repeats += 1;
            }
            self.last = Some(value);
            self.stamps.push(sample.at_ns);
            self.records += 1;
        }
        Ok(())
    }

    /// The average time between consecutive stamps, in nanoseconds (`None` with fewer than two).
    pub fn mean_period_ns(&self) -> Option<f64> {
        let (first, last) = (self.stamps.first()?, self.stamps.last()?);
        (self.stamps.len() >= 2).then(|| (last - first) as f64 / (self.stamps.len() - 1) as f64)
    }

    /// The largest gap between consecutive stamps, in nanoseconds.
    pub fn largest_gap_ns(&self) -> u64 {
        self.stamps
            .windows(2)
            .map(|pair| pair[1].saturating_sub(pair[0]))
            .max()
            .unwrap_or(0)
    }

    /// Gaps longer than `1.5` periods at `period_ns`: the records the chip did not deliver.
    pub fn misses(&self, period_ns: u64) -> u64 {
        let limit = period_ns + period_ns / 2;
        self.stamps
            .windows(2)
            .filter(|pair| pair[1].saturating_sub(pair[0]) > limit)
            .count() as u64
    }

    /// The span from the first stamp to the last, in nanoseconds.
    pub fn span_ns(&self) -> u64 {
        match (self.stamps.first(), self.stamps.last()) {
            (Some(first), Some(last)) => last - first,
            _ => 0,
        }
    }
}

/// The TSV rows of the report, as the record's checks read them (key, status, value, note).
pub fn rows(report: &Report, period_ns: u64, elapsed_ns: u64) -> String {
    let mut out = String::new();
    let mut row = |key: &str, value: String, note: &str| {
        out.push_str(&format!("{key}\tmeasured\t{value}\t{note}\n"));
    };
    row(
        "b8.records",
        report.records.to_string(),
        "records decoded from the FIFO",
    );
    row("b8.reads", report.reads.to_string(), "batch reads made");
    row(
        "b8.empty_reads",
        report.empty_reads.to_string(),
        "batch reads that returned no records",
    );
    row(
        "b8.max_per_read",
        report.max_per_read.to_string(),
        "the most records one read returned",
    );
    row(
        "b8.refused_bytes",
        report.refused_bytes.to_string(),
        "bytes in batches that were not whole records",
    );
    row(
        "b8.repeats",
        report.repeats.to_string(),
        "records equal to the one before",
    );
    row(
        "b8.misses",
        report.misses(period_ns).to_string(),
        "gaps over 1.5 periods",
    );
    row(
        "b8.largest_gap_ms",
        format!("{:.3}", report.largest_gap_ns() as f64 / 1e6),
        "largest gap between two stamps",
    );
    if let Some(period) = report.mean_period_ns() {
        row(
            "b8.mean_period_us",
            format!("{:.2}", period / 1e3),
            "average between consecutive stamps",
        );
    }
    row(
        "b8.span_vs_elapsed_ms",
        format!("{:.3}", (report.span_ns() as f64 - elapsed_ns as f64) / 1e6),
        "first to last stamp, minus the capture's own elapsed time",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_core::{Encode, Vector3, Writer};

    /// A record as the driver writes it: the stamp and the value, in the contract's encoding.
    fn record(at_ns: u64, x: f32, y: f32, z: f32) -> [u8; RECORD_LEN] {
        let sample = Sample {
            at_ns,
            value: Vector3 { x, y, z },
        };
        let mut buf = [0u8; RECORD_LEN];
        let mut writer = Writer::new(&mut buf);
        sample.encode(&mut writer).unwrap();
        buf
    }

    fn batch(records: &[[u8; RECORD_LEN]]) -> Vec<u8> {
        records.iter().flat_map(|r| r.iter().copied()).collect()
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::a_whole_batch_decodes_in_order_with_its_stamps
    #[test]
    fn a_whole_batch_decodes_in_order_with_its_stamps() {
        let mut report = Report::default();
        let bytes = batch(&[record(1_000, 0.1, 0.2, 9.8), record(6_000, 0.1, 0.2, 9.7)]);
        report.add_batch(&bytes).unwrap();
        assert_eq!(report.records, 2);
        assert_eq!(report.stamps, vec![1_000, 6_000]);
        assert_eq!(report.max_per_read, 2);
        assert_eq!(report.mean_period_ns(), Some(5_000.0));
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::a_partial_batch_is_refused_and_counted_not_decoded
    /// Fails on a tool that decoded the leading whole records of a partial batch: this one refuses it.
    #[test]
    fn a_partial_batch_is_refused_and_counted_not_decoded() {
        let mut report = Report::default();
        let mut bytes = batch(&[record(1_000, 0.0, 0.0, 9.8)]);
        bytes.push(0xab); // a torn record
        assert_eq!(
            report.add_batch(&bytes),
            Err(BatchError::Partial {
                bytes: RECORD_LEN + 1
            })
        );
        assert_eq!(report.records, 0, "nothing from a refused batch is decoded");
        assert_eq!(report.refused_bytes, (RECORD_LEN + 1) as u64);
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::an_empty_read_is_counted_and_is_not_a_record
    #[test]
    fn an_empty_read_is_counted_and_is_not_a_record() {
        let mut report = Report::default();
        report.add_batch(&[]).unwrap();
        report.add_batch(&[]).unwrap();
        assert_eq!(
            (report.reads, report.empty_reads, report.records),
            (2, 2, 0)
        );
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::misses_are_gaps_over_one_and_a_half_periods
    #[test]
    fn misses_are_gaps_over_one_and_a_half_periods() {
        let mut report = Report::default();
        // 5 ms period: gaps of 5, 5, 20 (a miss), 5.
        let bytes = batch(&[
            record(0, 1.0, 0.0, 0.0),
            record(5_000_000, 2.0, 0.0, 0.0),
            record(10_000_000, 3.0, 0.0, 0.0),
            record(30_000_000, 4.0, 0.0, 0.0),
            record(35_000_000, 5.0, 0.0, 0.0),
        ]);
        report.add_batch(&bytes).unwrap();
        assert_eq!(report.misses(5_000_000), 1);
        assert_eq!(report.largest_gap_ns(), 20_000_000);
    }

    /// testing/crates/bmi088-batch/src/lib.rs::tests::repeats_count_across_batches
    #[test]
    fn repeats_count_across_batches() {
        let mut report = Report::default();
        report
            .add_batch(&batch(&[record(0, 1.0, 1.0, 1.0)]))
            .unwrap();
        report
            .add_batch(&batch(&[
                record(5, 1.0, 1.0, 1.0),
                record(10, 2.0, 1.0, 1.0),
            ]))
            .unwrap();
        assert_eq!(report.repeats, 1);
    }
}
