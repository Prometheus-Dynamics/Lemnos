//! The job checks: what a caller's job must pass before any of its numbers reach a queue. Every
//! expected value here is written out by hand.

use lemnos_contract_camera_isp::{IspJob, NO_BUFFER};
use lemnos_core::Status;
use lemnos_measure::test_alloc::allocations;

use crate::job::{self, Checked, Limits};
use crate::{Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP30_PARAMS};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// An ISP30 with 4 config buffers of 16 KiB, 3 main path buffers and the self path off.
fn limits() -> Limits {
    Limits::new(Generation::Isp30, 4, 16384, [Some(3), None]).expect("limits")
}

fn job(config_buffer: u32, outputs: [u32; 4]) -> IspJob {
    IspJob {
        identity: 77,
        layout: LAYOUT_ISP30_PARAMS,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs,
    }
}

const NONE4: [u32; 4] = [NO_BUFFER; 4];

#[test]
fn a_job_inside_what_the_driver_holds_passes() {
    assert_eq!(
        job::check(&job(3, [2, NO_BUFFER, NO_BUFFER, NO_BUFFER]), &limits()),
        Some(Checked {
            identity: 77,
            config_buffer: 3,
            outputs: [Some(2), None],
        })
    );
    // Statistics only: no image this frame.
    assert!(job::check(&job(0, NONE4), &limits()).is_some());
}

#[test]
fn a_config_buffer_past_the_config_stream_is_refused() {
    assert_eq!(job::check(&job(4, NONE4), &limits()), None);
    assert_eq!(job::check(&job(u32::MAX - 1, NONE4), &limits()), None);
}

#[test]
fn an_output_buffer_past_its_stream_is_refused() {
    assert_eq!(
        job::check(&job(0, [3, NO_BUFFER, NO_BUFFER, NO_BUFFER]), &limits()),
        None
    );
    assert_eq!(
        job::check(
            &job(0, [u32::MAX - 1, NO_BUFFER, NO_BUFFER, NO_BUFFER]),
            &limits()
        ),
        None
    );
}

#[test]
fn a_buffer_for_an_output_that_is_off_or_missing_is_refused() {
    // The self path is off.
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, 0, NO_BUFFER, NO_BUFFER]), &limits()),
        None
    );
    // The ISP has two outputs.
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, NO_BUFFER, 0, NO_BUFFER]), &limits()),
        None
    );
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, NO_BUFFER, NO_BUFFER, 0]), &limits()),
        None
    );
}

#[test]
fn another_layout_or_an_input_buffer_is_refused() {
    let mut j = job(0, NONE4);
    j.layout = LAYOUT_ISP21_PARAMS;
    assert_eq!(job::check(&j, &limits()), None);
    let mut j = job(0, NONE4);
    j.input_buffer = 0;
    assert_eq!(job::check(&j, &limits()), None);
}

#[test]
fn a_config_stream_too_small_for_the_parameters_is_refused() {
    assert_eq!(
        Limits::new(Generation::Isp35, 4, 13064, [Some(2), None]).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert!(Limits::new(Generation::Isp35, 4, 13065, [Some(2), None]).is_ok());
}

#[test]
fn parameters_are_copied_whole_or_not_at_all() {
    let n = Generation::Isp21.config_layout().bytes as usize;
    let from: Vec<u8> = (0..n + 10).map(|i| i as u8).collect();
    let mut into = vec![0xee; n + 4];
    assert_eq!(job::copy_params(Generation::Isp21, &from, &mut into), Ok(n));
    assert_eq!(&into[..n], &from[..n]);
    assert_eq!(&into[n..], &[0xee; 4]);

    // The caller's buffer is shorter than the parameters.
    let mut into = vec![0; n];
    assert_eq!(
        job::copy_params(Generation::Isp21, &from[..n - 1], &mut into),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(into.iter().all(|&b| b == 0));
    // The kernel's buffer is.
    let mut into = vec![0; n - 1];
    assert_eq!(
        job::copy_params(Generation::Isp21, &from, &mut into),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

#[test]
fn a_dequeued_buffer_is_read_only_as_far_as_it_was_mapped_and_fits() {
    let mapped = [7u8; 64];
    assert_eq!(job::filled(&mapped, 40, 48).map(<[u8]>::len), Some(40));
    assert_eq!(job::filled(&mapped, 64, 64).map(<[u8]>::len), Some(64));
    // The kernel reports more than it mapped.
    assert_eq!(job::filled(&mapped, 65, 1 << 20), None);
    assert_eq!(job::filled(&mapped, usize::MAX, u32::MAX), None);
    // The stream buffer is smaller.
    assert_eq!(job::filled(&mapped, 49, 48), None);
}

#[test]
fn the_per_frame_checks_allocate_nothing() {
    let limits = limits();
    let j = job(1, [0, NO_BUFFER, NO_BUFFER, NO_BUFFER]);
    let n = Generation::Isp30.config_layout().bytes as usize;
    let from = vec![1u8; n];
    let mut into = vec![0u8; n];
    let mapped = vec![2u8; 128];
    let (ok, allocated) = allocations(|| {
        let checked = job::check(&j, &limits).is_some();
        let copied = job::copy_params(Generation::Isp30, &from, &mut into).is_ok();
        let out = job::filled(&mapped, 100, 128).is_some();
        checked && copied && out
    });
    assert!(ok);
    assert_eq!(allocated, 0);
}
