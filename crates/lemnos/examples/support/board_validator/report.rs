#![allow(clippy::print_stdout)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

impl CheckStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRecord {
    pub status: CheckStatus,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct ValidatorReport {
    records: Vec<CheckRecord>,
}

impl ValidatorReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(CheckStatus::Pass, name, detail);
    }

    pub fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(CheckStatus::Fail, name, detail);
    }

    pub fn skip(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.push(CheckStatus::Skip, name, detail);
    }

    pub fn has_failures(&self) -> bool {
        self.records
            .iter()
            .any(|record| record.status == CheckStatus::Fail)
    }

    pub fn print_summary(&self) {
        let passed = self
            .records
            .iter()
            .filter(|record| record.status == CheckStatus::Pass)
            .count();
        let failed = self
            .records
            .iter()
            .filter(|record| record.status == CheckStatus::Fail)
            .count();
        let skipped = self
            .records
            .iter()
            .filter(|record| record.status == CheckStatus::Skip)
            .count();

        println!();
        println!("validator summary: {passed} passed, {failed} failed, {skipped} skipped");
    }

    fn push(&mut self, status: CheckStatus, name: impl Into<String>, detail: impl Into<String>) {
        let record = CheckRecord {
            status,
            name: name.into(),
            detail: detail.into(),
        };
        println!(
            "[{}] {}: {}",
            record.status.as_str(),
            record.name,
            record.detail
        );
        self.records.push(record);
    }
}
