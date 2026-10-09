//! Mock raw lines and PWM channels ([`crate::raw`]). Clones share state, so a
//! test keeps one to drive inputs, inject edges and inspect what the code
//! under test did with another.

use super::alloc::collections::VecDeque;
use super::alloc::vec::Vec;
use super::{Shared, with};
use crate::ErrorKind;
use crate::raw::{Direction, Edge, LineConfig, PwmConfig, RawLine, RawPwm};

#[derive(Debug, Default)]
struct LineState {
    config: LineConfig,
    /// Physical input level (what an outside circuit drives).
    input: bool,
    /// Logical value of an output.
    output: bool,
    edges: VecDeque<Edge>,
    configs: Vec<LineConfig>,
    seq: u32,
    fail: Option<ErrorKind>,
}

/// A GPIO line: inputs read the level set with [`MockLine::drive`] (edges
/// injected with [`MockLine::edge`]), outputs record what is written.
#[derive(Debug, Clone, Default)]
pub struct MockLine {
    state: Shared<LineState>,
}

impl MockLine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drives the physical input level.
    pub fn drive(&self, level: bool) {
        with(&self.state, |s| s.input = level);
    }

    /// Changes the input level and queues the edge an input configured for
    /// it would report.
    pub fn edge(&self, rising: bool, timestamp_ns: u64) {
        with(&self.state, |s| {
            s.input = rising != s.config.active_low;
            s.seq += 1;
            let seq = s.seq;
            s.edges.push_back(Edge {
                rising,
                timestamp_ns,
                seq,
            });
        });
    }

    /// The current configuration.
    pub fn config(&self) -> LineConfig {
        with(&self.state, |s| s.config)
    }

    /// Every configuration applied, in order.
    pub fn configs(&self) -> Vec<LineConfig> {
        with(&self.state, |s| s.configs.clone())
    }

    /// The physical level an output drives (`None` for an input).
    pub fn level(&self) -> Option<bool> {
        with(&self.state, |s| {
            (s.config.direction == Direction::Output).then_some(s.output != s.config.active_low)
        })
    }

    /// Every operation fails with `kind` until cleared (`None`).
    pub fn fail(&self, kind: Option<ErrorKind>) {
        with(&self.state, |s| s.fail = kind);
    }
}

impl RawLine for MockLine {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if let Some(kind) = s.fail {
                return Err(kind);
            }
            if config.direction == Direction::Output {
                s.output = config.initial;
            }
            s.config = *config;
            s.configs.push(*config);
            Ok(())
        })
    }

    fn get(&mut self) -> Result<bool, ErrorKind> {
        with(&self.state, |s| {
            if let Some(kind) = s.fail {
                return Err(kind);
            }
            Ok(match s.config.direction {
                Direction::Output => s.output,
                Direction::Input => s.input != s.config.active_low,
            })
        })
    }

    fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            if let Some(kind) = s.fail {
                return Err(kind);
            }
            if s.config.direction != Direction::Output {
                return Err(ErrorKind::InvalidInput);
            }
            s.output = value;
            Ok(())
        })
    }

    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        with(&self.state, |s| Ok(s.edges.pop_front()))
    }
}

#[derive(Debug, Default)]
struct PwmState {
    config: PwmConfig,
    history: Vec<PwmConfig>,
}

/// A PWM channel that records every configuration.
#[derive(Debug, Clone, Default)]
pub struct MockPwm {
    state: Shared<PwmState>,
}

impl MockPwm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every configuration applied, in order.
    pub fn history(&self) -> Vec<PwmConfig> {
        with(&self.state, |s| s.history.clone())
    }

    pub fn current(&self) -> PwmConfig {
        with(&self.state, |s| s.config)
    }
}

impl RawPwm for MockPwm {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        if config.duty_ns > config.period_ns {
            return Err(ErrorKind::InvalidInput);
        }
        with(&self.state, |s| {
            s.config = *config;
            s.history.push(*config);
        });
        Ok(())
    }

    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        Ok(with(&self.state, |s| s.config))
    }
}

/// One recorded SPI segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiRecord {
    pub config: crate::raw::SpiConfig,
    pub tx: Vec<u8>,
    pub rx_len: usize,
}

#[derive(Debug, Default)]
struct SpiState {
    transactions: Vec<Vec<SpiRecord>>,
    responses: VecDeque<u8>,
}

/// A raw SPI device ([`crate::raw::RawSpi`]) that records each transaction's
/// segments and answers from queued bytes (0 when empty). Clones share it.
#[derive(Debug, Clone, Default)]
pub struct MockRawSpi {
    state: Shared<SpiState>,
}

impl MockRawSpi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues bytes the device sends.
    pub fn respond(&self, bytes: &[u8]) {
        with(&self.state, |s| s.responses.extend(bytes.iter().copied()));
    }

    pub fn transactions(&self) -> Vec<Vec<SpiRecord>> {
        with(&self.state, |s| s.transactions.clone())
    }
}

impl crate::raw::RawSpi for MockRawSpi {
    fn transfer(&mut self, segments: &mut [crate::raw::SpiSegment<'_>]) -> Result<(), ErrorKind> {
        with(&self.state, |s| {
            let mut log = Vec::with_capacity(segments.len());
            for segment in segments.iter_mut() {
                log.push(SpiRecord {
                    config: segment.config,
                    tx: segment.tx.to_vec(),
                    rx_len: segment.rx.len(),
                });
                for b in segment.rx.iter_mut() {
                    *b = s.responses.pop_front().unwrap_or(0);
                }
            }
            s.transactions.push(log);
        });
        Ok(())
    }
}
