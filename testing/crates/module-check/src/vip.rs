//! `lemnos-module-check vip <device> <heap>`: the VIP9000 driver's Linux side
//! (`lemnos-driver-vip9000`'s claim over its node in `lemnos.ko`) end to end against the
//! harness's fake block. Runs are posted on a played job stream, as the daemon's would be: a
//! copy through two slots lands byte for byte, a program reaching past its slot comes back
//! failed and the claim goes on, a program the module refuses is refused, a run the fake hangs
//! on comes back failed, runs in a row allocate nothing, and a second claim after the first works.

use std::io::Write;

use lemnos_contract_npu::{JOB_BYTES, RESULT_BYTES, RunJob, RunResult, RunStatus};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_driver_vip9000::claim::{Claim, Service};
use lemnos_driver_vip9000::linux::{LinuxModule, Settings};
use lemnos_driver_vip9000::slot_address;
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::Report;

const BUFFERS: u32 = 8;
const BUFFER_BYTES: usize = 8192;

/// The fake block's copy (its states 0x5000 to 0x5002), then a NOP.
fn copy(src: u32, dst: u32, len: u32) -> Vec<u8> {
    [0x0803_5000u32, src, dst, len, 3 << 27, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect()
}

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn slot(n: u16) -> u32 {
    slot_address(n, 1).map_or(0, |r| r.start as u32)
}

fn post(stream: &MockStream, identity: u64, program: u32, result: u32) {
    // Sized by the contract's own job size, and the encode must succeed: a job cut short decodes
    // as a result of 0 on the driver's side, which hides the cause.
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    RunJob {
        identity,
        program,
        result,
    }
    .encode(&mut writer)
    .expect("a job fits its job size");
    let used = writer.len();
    assert_eq!(used, JOB_BYTES, "a run is a job of the contract's size");
    stream.post_job(&bytes[..used]);
}

/// Serves one round and takes its one completion.
fn one(claim: &mut Claim<LinuxModule>, stream: &mut MockStream) -> Option<(u64, RunStatus)> {
    if claim.serve(stream, 0) != Ok(1) {
        return None;
    }
    let frame = stream.take_frame()?;
    // The record is as long as the driver says it wrote, not a guessed size.
    let contents = stream.contents(frame.buffer);
    let record = contents.get(..frame.bytes as usize)?;
    if frame.bytes as usize != RESULT_BYTES {
        return None;
    }
    let status = RunResult::decode(&mut Reader::new(record)).ok()?.status;
    stream.return_buffer(frame.buffer);
    Some((frame.identity, status))
}

pub fn run(device: &str, heap: &str) -> u32 {
    let mut report = Report { failures: 0 };
    let Ok(settings) = Settings::new(device, heap) else {
        report.check(false, "vip: settings");
        return report.failures;
    };
    let mut service = Service::new(LinuxModule::new(settings));
    let mut stream = MockStream::with_bytes(BUFFERS, BUFFER_BYTES);
    match service.claim(0) {
        Err(e) => report.check(false, &format!("vip: claim ({e:?})")),
        Ok(mut claim) => {
            // Buffer 0 holds programs, 1 and 2 the slots, 3 and up results.
            claim.serve(&mut stream, 0).ok();
            let program = copy(slot(0), slot(1), 6000);
            stream.fill(0, &program);
            let p = claim.load(&mut stream, 0, program.len() as u32);
            report.check(p.is_ok(), &format!("vip: a copy program loads ({p:?})"));
            let p = p.unwrap_or(0);
            let input: Vec<u8> = (0..6000u32).map(|i| (i * 7 + 3) as u8).collect();
            stream.fill(1, &input);
            let attached = claim.attach(&mut stream, p, 0, 1, 6000).is_ok()
                && claim.attach(&mut stream, p, 1, 2, 6000).is_ok();
            report.check(attached, "vip: two slots attached");
            post(&stream, 1, p, 3);
            let done = one(&mut claim, &mut stream);
            report.check(
                done == Some((1, RunStatus::Done))
                    && stream.contents(2).get(..6000) == Some(&input[..]),
                &format!("vip: a copy across two pages lands byte for byte ({done:?})"),
            );

            // Past the slot's last page: the module's tables don't reach it.
            let past = copy(slot(0), slot(1) + 2 * 4096, 64);
            stream.fill(0, &past);
            let q = claim.load(&mut stream, 0, past.len() as u32).unwrap_or(0);
            let ok = claim.attach(&mut stream, q, 0, 1, 6000).is_ok()
                && claim.attach(&mut stream, q, 1, 2, 6000).is_ok();
            post(&stream, 2, q, 4);
            let done = one(&mut claim, &mut stream);
            report.check(
                ok && done == Some((2, RunStatus::Failed)) && !claim.lost(),
                &format!("vip: a program past its slot fails and the claim stays ({done:?})"),
            );
            post(&stream, 3, p, 3);
            let done = one(&mut claim, &mut stream);
            report.check(
                done == Some((3, RunStatus::Done)),
                &format!("vip: the next run goes on ({done:?})"),
            );

            // The module's check: an END is only ever the module's own.
            let end = words(&[2 << 27, 0]);
            stream.fill(0, &end);
            let refused = claim.load(&mut stream, 0, end.len() as u32);
            report.check(
                refused == Err(Status::INVALID_ARGUMENT),
                &format!("vip: a program holding END is refused ({refused:?})"),
            );

            // A run the fake hangs on: the module stops it.
            let hang = words(&[0x0801_5003, 0]);
            stream.fill(0, &hang);
            let h = claim.load(&mut stream, 0, hang.len() as u32).unwrap_or(0);
            post(&stream, 4, h, 4);
            let done = one(&mut claim, &mut stream);
            report.check(
                done == Some((4, RunStatus::Failed)) && !claim.lost(),
                &format!("vip: a run the block hangs on comes back failed ({done:?})"),
            );
            report.check(
                claim.unload(h).is_ok() && claim.unload(q).is_ok(),
                "vip: unloads",
            );

            // Posting and taking the frame are the played consumer's side, which allocates:
            // only the claim's round is measured.
            let mut n = 0;
            for i in 0..20 {
                post(&stream, 100 + i, p, 3);
                let (served, allocated) = allocations(|| claim.serve(&mut stream, 0));
                n += allocated;
                let done = stream.take_frame().map(|f| {
                    stream.return_buffer(f.buffer);
                    f.bytes
                });
                if served != Ok(1) || done != Some(RESULT_BYTES as u32) {
                    report.failures += 1;
                }
            }
            report.check(n == 0, &format!("vip: 20 runs allocated nothing ({n})"));
            report.check(
                claim.counters().jobs == 22 && claim.counters().errors == 2,
                &format!("vip: counted ({:?})", claim.counters()),
            );
        }
    }
    let again = service.claim(0);
    report.check(again.is_ok(), "vip: a second claim after the first");
    drop(again);
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-module-check vip: {} failed",
        report.failures
    );
    report.failures
}
