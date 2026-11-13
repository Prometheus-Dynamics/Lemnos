//! The scaler against a node that plays the device, surfaces over real memory, and finding the
//! node. No device node is opened.

use std::cell::Cell;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::device::find_in;
use crate::{
    Channel, Crop, Device, Job, Node, Output, PixelFormat, RunError, Scaler, State, Surface,
    backoff_ns,
};
use lemnos_core::{Config, ConfigError, ConfigValue, Lifecycle};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// What the played device saw, and what it is to fail next.
#[derive(Default)]
struct Log {
    opens: Cell<u32>,
    selected: Cell<u8>,
    runs: Cell<u32>,
    releases: Cell<u32>,
    fail_open: Cell<Option<i32>>,
    fail_select: Cell<Option<i32>>,
    fail_run: Cell<Option<i32>>,
    fail_release: Cell<Option<i32>>,
}

struct Played(Rc<Log>);

fn failed(errno: &Cell<Option<i32>>) -> io::Result<()> {
    errno
        .take()
        .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
}

impl Device for Played {
    fn select_offline(&mut self, mask: u8) -> io::Result<()> {
        failed(&self.0.fail_select)?;
        self.0.selected.set(self.0.selected.get() | mask);
        Ok(())
    }

    fn run(&mut self, _job: &mut Job<'_>) -> io::Result<()> {
        failed(&self.0.fail_run)?;
        self.0.runs.set(self.0.runs.get() + 1);
        Ok(())
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        failed(&self.0.fail_release)?;
        self.0.releases.set(self.0.releases.get() + 1);
        Ok(())
    }
}

impl Drop for Played {
    fn drop(&mut self) {
        // The kernel drops the selection when the file closes.
        self.0.selected.set(0);
    }
}

fn scaler(channels: u8) -> (Scaler<Played, impl FnMut() -> io::Result<Played>>, Rc<Log>) {
    let log = Rc::new(Log::default());
    let held = Rc::clone(&log);
    let open = move || {
        failed(&held.fail_open)?;
        held.opens.set(held.opens.get() + 1);
        Ok(Played(Rc::clone(&held)))
    };
    (Scaler::new(open, channels), log)
}

/// A sealed memfd of `len` bytes: memory the caller really holds, sized the way a dma-buf is.
fn memory(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"rkvpss-test", len).expect("memfd")
}

struct Buffers {
    input: OwnedFd,
    output: OwnedFd,
}

fn buffers() -> Buffers {
    Buffers {
        input: memory(PixelFormat::Nv12.frame_len(1920, 1080)),
        output: memory(PixelFormat::Nv12.frame_len(640, 360)),
    }
}

fn job(buffers: &Buffers, channel: Channel) -> Job<'_> {
    let source =
        Surface::new(buffers.input.as_fd(), PixelFormat::Nv12, 1920, 1080, 1920).expect("input");
    let target =
        Surface::new(buffers.output.as_fd(), PixelFormat::Nv12, 640, 360, 640).expect("output");
    let output = Output {
        channel,
        crop: Crop::full(1920, 1080),
        target,
        flip: false,
    };
    Job::new(source, &[output]).expect("job")
}

const MS: u64 = 1_000_000;

#[test]
fn a_surface_is_measured_against_the_memory_behind_it() {
    let exact = PixelFormat::Nv12.frame_len(640, 480);
    let fits = memory(exact);
    let short = memory(exact - 1);
    assert!(Surface::new(fits.as_fd(), PixelFormat::Nv12, 640, 480, 640).is_ok());
    let error =
        Surface::new(short.as_fd(), PixelFormat::Nv12, 640, 480, 640).expect_err("one byte short");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    // A wider stride needs more memory than the same image packed.
    assert!(Surface::new(fits.as_fd(), PixelFormat::Nv12, 640, 480, 656).is_err());
}

#[test]
fn the_first_job_opens_the_node_and_selects_the_channels() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit() | Channel::Ch1.bit());
    assert_eq!(scaler.state(), State::Closed);
    assert_eq!(log.opens.get(), 0);
    let b = buffers();
    scaler.run(0, &mut job(&b, Channel::Ch1)).expect("run");
    scaler.run(1, &mut job(&b, Channel::Ch0)).expect("run");
    assert_eq!(scaler.state(), State::Running);
    assert_eq!(log.opens.get(), 1);
    assert_eq!(log.selected.get(), 0b11);
    assert_eq!(log.runs.get(), 2);
}

#[test]
fn bits_past_the_last_channel_are_dropped() {
    let (scaler, _log) = scaler(0xff);
    assert_eq!(scaler.channels(), 0b11_1111);
}

#[test]
fn a_job_on_a_channel_the_scaler_was_not_given_is_not_run() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    match scaler.run(0, &mut job(&b, Channel::Ch2)) {
        Err(RunError::NotOurs { channels }) => assert_eq!(channels, Channel::Ch2.bit()),
        other => panic!("{other:?}"),
    }
    assert_eq!(log.opens.get(), 0);
    assert_eq!(scaler.state(), State::Closed);
}

#[test]
fn a_job_the_kernel_refuses_leaves_the_node_open() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    log.fail_run.set(Some(libc::EINVAL));
    assert!(matches!(
        scaler.run(0, &mut job(&b, Channel::Ch0)),
        Err(RunError::Refused(_))
    ));
    assert_eq!(scaler.state(), State::Running);
    scaler.run(1, &mut job(&b, Channel::Ch0)).expect("run");
    assert_eq!(log.opens.get(), 1);
}

#[test]
fn a_device_failure_backs_off_doubling_to_8_s_and_never_gives_up() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    scaler.run(0, &mut job(&b, Channel::Ch0)).expect("run");
    let mut now = 1_000 * MS;
    let expected = [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000, 8_000, 8_000];
    for (attempt, delay_ms) in expected.into_iter().enumerate() {
        // The block times out (the driver's EAGAIN after 500 ms).
        log.fail_run.set(Some(libc::EAGAIN));
        let retry_at_ns = match scaler.run(now, &mut job(&b, Channel::Ch0)) {
            Err(RunError::Device { retry_at_ns, .. }) => retry_at_ns,
            other => panic!("attempt {attempt}: {other:?}"),
        };
        assert_eq!(retry_at_ns, now + delay_ms * MS, "attempt {attempt}");
        assert_eq!(log.selected.get(), 0, "the node was closed");
        let opens = log.opens.get();
        // Until it is due, the device is not touched.
        match scaler.run(retry_at_ns - 1, &mut job(&b, Channel::Ch0)) {
            Err(RunError::BackingOff { retry_at_ns: at }) => assert_eq!(at, retry_at_ns),
            other => panic!("{other:?}"),
        }
        assert_eq!(log.opens.get(), opens);
        assert!(matches!(scaler.state(), State::BackingOff { .. }));
        now = retry_at_ns;
    }
    assert_eq!(backoff_ns(u32::MAX), 8_000 * MS);
    // When due, it opens again, selects its channels again and runs; that clears the backoff.
    let opens = log.opens.get();
    scaler.run(now, &mut job(&b, Channel::Ch0)).expect("run");
    assert_eq!(log.opens.get(), opens + 1);
    assert_eq!(log.selected.get(), Channel::Ch0.bit());
    assert_eq!(scaler.state(), State::Running);
    log.fail_run.set(Some(libc::EIO));
    match scaler.run(now, &mut job(&b, Channel::Ch0)) {
        Err(RunError::Device { retry_at_ns, .. }) => assert_eq!(retry_at_ns, now + 250 * MS),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failed_open_or_selection_backs_off_too() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    log.fail_open.set(Some(libc::ENOENT));
    assert!(matches!(
        scaler.run(0, &mut job(&b, Channel::Ch0)),
        Err(RunError::Device { retry_at_ns, .. }) if retry_at_ns == 250 * MS
    ));
    // The camera path is running: selecting the channel is refused, and the node is closed.
    log.fail_select.set(Some(libc::EBUSY));
    assert!(matches!(
        scaler.run(250 * MS, &mut job(&b, Channel::Ch0)),
        Err(RunError::Device { retry_at_ns, .. }) if retry_at_ns == 750 * MS
    ));
    assert_eq!(log.runs.get(), 0);
    assert!(matches!(
        scaler.state(),
        State::BackingOff { failures: 2, .. }
    ));
    scaler
        .run(750 * MS, &mut job(&b, Channel::Ch0))
        .expect("run");
    assert_eq!(log.runs.get(), 1);
}

#[test]
fn stop_closes_the_node_and_forgets_the_backoff() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    log.fail_run.set(Some(libc::EIO));
    assert!(scaler.run(0, &mut job(&b, Channel::Ch0)).is_err());
    scaler.stop();
    assert_eq!(scaler.state(), State::Closed);
    scaler
        .run(1, &mut job(&b, Channel::Ch0))
        .expect("run at once");
    scaler.stop();
    assert_eq!(log.selected.get(), 0);
    assert_eq!(scaler.state(), State::Closed);
}

#[test]
fn buffers_are_released_only_from_an_open_node() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    scaler.release_buffers(0).expect("nothing held");
    assert_eq!(log.opens.get(), 0);
    let b = buffers();
    scaler.run(0, &mut job(&b, Channel::Ch0)).expect("run");
    scaler.release_buffers(1).expect("release");
    assert_eq!(log.releases.get(), 1);
    log.fail_release.set(Some(libc::EIO));
    assert!(matches!(
        scaler.release_buffers(2),
        Err(RunError::Device { .. })
    ));
    assert!(matches!(scaler.state(), State::BackingOff { .. }));
}

#[test]
fn a_job_on_an_open_node_allocates_nothing() {
    let (mut scaler, log) = scaler(Channel::Ch0.bit());
    let b = buffers();
    scaler.run(0, &mut job(&b, Channel::Ch0)).expect("open");
    let ((), allocated) = allocations(|| {
        for now in 1..1_001 {
            scaler.run(now, &mut job(&b, Channel::Ch0)).expect("run");
        }
    });
    assert_eq!(allocated, 0, "building and running a job");
    assert_eq!(log.runs.get(), 1_001);
}

#[test]
fn a_missing_node_is_an_error() {
    let error = Node::open("/nonexistent/video9").expect_err("no such device");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn the_node_is_found_by_name() {
    let root = std::env::temp_dir().join(format!("rkvpss-find-{}", std::process::id()));
    for (node, name) in [
        ("video0", "rkisp_mainpath\n"),
        ("video7", "rkvpss-offline\n"),
    ] {
        let dir = root.join(node);
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("name"), name).expect("write");
    }
    let found = find_in(&root);
    fs::remove_dir_all(&root).expect("clean up");
    assert_eq!(found, Some(PathBuf::from("/dev/video7")));
}

#[test]
fn a_kernel_without_the_node_finds_nothing() {
    assert_eq!(find_in(Path::new("/nonexistent/video4linux")), None);
}

#[test]
fn a_file_that_is_not_the_node_is_refused() {
    let path = std::env::temp_dir().join(format!("rkvpss-plain-{}", std::process::id()));
    File::create(&path).expect("create");
    let refused = Node::open(&path);
    fs::remove_file(&path).expect("clean up");
    assert_eq!(
        refused.expect_err("a plain file").kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn check_config_names_the_bad_setting() {
    fn check(
        entries: &'static [(&'static str, ConfigValue<'static>)],
    ) -> core::result::Result<(), ConfigError<'static>> {
        <crate::Rkvpss as Lifecycle>::check_config(&Config::new(entries))
    }
    assert!(
        check(&[
            ("channel", ConfigValue::Int(5)),
            ("node", ConfigValue::Str("/dev/v"))
        ])
        .is_ok()
    );
    assert!(check(&[("channel", ConfigValue::Int(0))]).is_ok());
    assert_eq!(check(&[]).unwrap_err().key, "channel");
    assert_eq!(
        check(&[("channel", ConfigValue::Int(6))]).unwrap_err().key,
        "channel"
    );
    assert_eq!(
        check(&[("channel", ConfigValue::Int(-1))]).unwrap_err().key,
        "channel"
    );
    assert_eq!(
        check(&[
            ("channel", ConfigValue::Int(0)),
            ("node", ConfigValue::Str(""))
        ])
        .unwrap_err()
        .key,
        "node"
    );
    let bogus = check(&[
        ("channel", ConfigValue::Int(0)),
        ("nod", ConfigValue::Str("/dev/v")),
    ])
    .unwrap_err();
    assert_eq!((bogus.key, bogus.suggestion), ("nod", Some("node")));
    assert_eq!(
        check(&[("channel", ConfigValue::Str("0"))])
            .unwrap_err()
            .key,
        "channel"
    );
}
