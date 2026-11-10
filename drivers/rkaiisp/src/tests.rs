//! The layouts, every parameter check, and the ring against a played device. No device node is
//! opened.

use std::cell::RefCell;
use std::io;
use std::mem::{offset_of, size_of};
use std::rc::Rc;

use lemnos_measure::test_alloc::allocations;

use crate::uapi::{
    self, AibnrQueue, CHANNELS, IspbufInfo, KwtCfg, LEVELS, MAX_RUNCNT, ModelCfg, ParamInfo,
    Params, chn_src, model_mode,
};
use crate::{Aiisp, Device, ParamError, RunError, Settings, State, backoff_ns, params};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

// ---- Layouts ---------------------------------------------------------------------------------

#[test]
fn layouts_match_the_c_compiler() {
    // The full set is asserted at compile time in `layout.rs`; these are the ones a caller
    // builds buffers from, as `gcc` gave them for the vendor headers.
    assert_eq!(size_of::<Params>(), 2752);
    assert_eq!(offset_of!(Params, model_cfg), 156);
    assert_eq!(offset_of!(Params, kwt_cfg), 2620);
    assert_eq!(size_of::<ModelCfg>(), 308);
    assert_eq!(size_of::<KwtCfg>(), 96);
    assert_eq!(size_of::<ParamInfo>(), 16);
    assert_eq!(size_of::<IspbufInfo>(), 548);
    assert_eq!(size_of::<AibnrQueue>(), uapi::QUEUE_BUF_BYTES);
    assert_eq!(uapi::QUEUE_BUF_BYTES, 40);
    assert_eq!(uapi::CMD_SET_PARAM_INFO, 0x4010_56c0);
    assert_eq!(uapi::CMD_QUEUE_BUF, 0x4028_56c3);
    assert_eq!(uapi::CMD_CLEAR_IQPARAMS, 0x56c7);
    assert_eq!(uapi::EVENT_AIISP_DONE, 0x0800_0001);
}

#[test]
fn the_kernel_buffer_size_is_its_own_formula_or_none_past_32_bits() {
    assert_eq!(uapi::param_buffer_bytes(16 * 1024, 8), Some(2752 + 131_072));
    assert_eq!(uapi::param_buffer_bytes(u32::MAX / 8 + 1, 8), None);
    assert_eq!(uapi::param_buffer_bytes(u32::MAX / 8, 8), None);
}

// ---- A parameter buffer ----------------------------------------------------------------------

const WEIGHTS: usize = 4096;

fn put(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn run_field(run: usize, field: usize, index: usize) -> usize {
    offset_of!(Params, model_cfg) + run * size_of::<ModelCfg>() + field + index * 4
}

fn kwt(field: usize, run: usize) -> usize {
    offset_of!(Params, kwt_cfg) + field + run * 4
}

/// Two runs of a four-level model reading the ISP's plane on channel 0, each with 2048 bytes of
/// weights after the head.
fn valid() -> Vec<u8> {
    let mut b = vec![0; size_of::<Params>() + WEIGHTS];
    put(&mut b, offset_of!(Params, model_runcnt), 2);
    put(
        &mut b,
        offset_of!(Params, model_mode),
        model_mode::SINGLE_MODE,
    );
    put(
        &mut b,
        offset_of!(Params, module_update),
        uapi::MODEL_UPDATE,
    );
    for run in 0..2 {
        put(
            &mut b,
            run_field(run, offset_of!(ModelCfg, sw_aiisp_level_num), 0),
            4,
        );
        put(
            &mut b,
            run_field(run, offset_of!(ModelCfg, sw_aiisp_lv_mode), 3),
            2,
        );
        put(
            &mut b,
            run_field(run, offset_of!(ModelCfg, sw_mi_chn_en), 0),
            1,
        );
        put(
            &mut b,
            run_field(run, offset_of!(ModelCfg, mi_chn_src), 0),
            chn_src::ISP_IIR,
        );
        let offset = (size_of::<Params>() + run * 2048) as u32;
        put(&mut b, kwt(offset_of!(KwtCfg, kwt_offet), run), offset);
        put(&mut b, kwt(offset_of!(KwtCfg, kwt_size), run), 2048);
    }
    b
}

const CAPACITY: usize = 1 << 20;

fn check(b: &[u8]) -> Result<params::Checked, ParamError> {
    params::check(b, MAX_RUNCNT, CAPACITY)
}

fn field_error(b: &[u8]) -> (u32, &'static str, u32) {
    match check(b) {
        Err(ParamError::Field { run, field, value }) => (run, field, value),
        other => panic!("expected a field error, got {other:?}"),
    }
}

#[test]
fn a_valid_buffer_passes_and_reports_its_head() {
    let b = valid();
    let checked = check(&b).expect("valid");
    assert_eq!(checked.runs, 2);
    assert_eq!(checked.mode, model_mode::SINGLE_MODE);
    assert_eq!(checked.update, uapi::MODEL_UPDATE);
}

#[test]
fn length_is_between_the_head_and_the_kernel_buffer() {
    let b = valid();
    assert!(matches!(
        check(&b[..size_of::<Params>() - 1]),
        Err(ParamError::Length { .. })
    ));
    assert!(matches!(
        params::check(&b, MAX_RUNCNT, b.len() - 1),
        Err(ParamError::Length { .. })
    ));
    assert!(params::check(&b, MAX_RUNCNT, b.len()).is_ok());
}

#[test]
fn run_count_is_one_to_the_nodes_maximum() {
    let mut b = valid();
    put(&mut b, offset_of!(Params, model_runcnt), 0);
    assert_eq!(check(&b), Err(ParamError::Runs { runs: 0, max: 8 }));
    let b = valid();
    assert_eq!(
        params::check(&b, 1, CAPACITY),
        Err(ParamError::Runs { runs: 2, max: 1 })
    );
    let mut b = valid();
    put(&mut b, offset_of!(Params, model_runcnt), 9);
    assert_eq!(check(&b), Err(ParamError::Runs { runs: 9, max: 8 }));
    // A node set up past the header's maximum still takes no more than it.
    let mut b = valid();
    put(&mut b, offset_of!(Params, model_runcnt), 9);
    assert_eq!(
        params::check(&b, 100, CAPACITY),
        Err(ParamError::Runs { runs: 9, max: 8 })
    );
}

#[test]
fn model_mode_is_one_the_header_names() {
    let mut b = valid();
    put(
        &mut b,
        offset_of!(Params, model_mode),
        model_mode::AIYNR_MODE,
    );
    assert!(check(&b).is_ok());
    put(&mut b, offset_of!(Params, model_mode), 5);
    assert_eq!(check(&b), Err(ParamError::Mode(5)));
}

#[test]
fn table_indices_and_register_fields_are_in_range() {
    let cases: [(usize, usize, u32, &str); 8] = [
        (offset_of!(ModelCfg, sw_aiisp_mode), 0, 2, "sw_aiisp_mode"),
        (
            offset_of!(ModelCfg, sw_aiisp_op_mode),
            0,
            2,
            "sw_aiisp_op_mode",
        ),
        (
            offset_of!(ModelCfg, sw_aiisp_drop_en),
            0,
            2,
            "sw_aiisp_drop_en",
        ),
        (offset_of!(ModelCfg, sw_mi_chn1_sel), 0, 2, "sw_mi_chn1_sel"),
        (offset_of!(ModelCfg, sw_mi_chn3_sel), 0, 2, "sw_mi_chn3_sel"),
        (
            offset_of!(ModelCfg, sw_aiisp_lv_active),
            15,
            4,
            "sw_aiisp_lv_active",
        ),
        (
            offset_of!(ModelCfg, sw_aiisp_lv_mode),
            15,
            64,
            "sw_aiisp_lv_mode",
        ),
        (
            offset_of!(ModelCfg, sw_mi_chn_data_mode),
            0,
            3,
            "sw_mi_chn_data_mode",
        ),
    ];
    for (field, index, bad, name) in cases {
        let mut b = valid();
        put(&mut b, run_field(1, field, index), bad - 1);
        assert!(check(&b).is_ok(), "{name} = {}", bad - 1);
        put(&mut b, run_field(1, field, index), bad);
        assert_eq!(field_error(&b), (1, name, bad));
    }
}

#[test]
fn the_last_levels_mode_indexes_the_channel_table() {
    let mut b = valid();
    // Level 4 of 4 is index 3.
    put(
        &mut b,
        run_field(0, offset_of!(ModelCfg, sw_aiisp_lv_mode), 3),
        4,
    );
    assert!(check(&b).is_ok());
    put(
        &mut b,
        run_field(0, offset_of!(ModelCfg, sw_aiisp_lv_mode), 3),
        5,
    );
    assert_eq!(field_error(&b), (0, "sw_aiisp_lv_mode", 5));
    // The same value on a level past the last is only a register field.
    let mut b = valid();
    put(
        &mut b,
        run_field(0, offset_of!(ModelCfg, sw_aiisp_lv_mode), 4),
        5,
    );
    assert!(check(&b).is_ok());
}

#[test]
fn level_count_is_one_to_sixteen() {
    for bad in [0, LEVELS as u32 + 1] {
        let mut b = valid();
        put(
            &mut b,
            run_field(0, offset_of!(ModelCfg, sw_aiisp_level_num), 0),
            bad,
        );
        assert_eq!(field_error(&b), (0, "sw_aiisp_level_num", bad));
    }
    let mut b = valid();
    put(
        &mut b,
        run_field(0, offset_of!(ModelCfg, sw_aiisp_level_num), 0),
        16,
    );
    assert!(check(&b).is_ok());
}

#[test]
fn channel_modes_are_checked_only_on_enabled_channels() {
    let mut b = valid();
    put(
        &mut b,
        run_field(0, offset_of!(ModelCfg, sw_mi_chn_mode), 0),
        3,
    );
    assert_eq!(field_error(&b), (0, "sw_mi_chn_mode", 3));
    let mut b = valid();
    for channel in 1..CHANNELS {
        put(
            &mut b,
            run_field(0, offset_of!(ModelCfg, sw_mi_chn_mode), channel),
            99,
        );
        put(
            &mut b,
            run_field(0, offset_of!(ModelCfg, sw_mi_chn_data_mode), channel),
            99,
        );
        put(
            &mut b,
            run_field(0, offset_of!(ModelCfg, mi_chn_src), channel),
            99,
        );
    }
    assert!(check(&b).is_ok());
}

#[test]
fn reading_the_last_output_stays_inside_the_layer_table() {
    let last_out = |runs: u32, mode: u32| {
        let mut b = vec![0; size_of::<Params>()];
        put(&mut b, offset_of!(Params, model_runcnt), runs);
        put(&mut b, offset_of!(Params, model_mode), mode);
        for run in 0..runs as usize {
            put(
                &mut b,
                run_field(run, offset_of!(ModelCfg, sw_aiisp_level_num), 0),
                1,
            );
            put(
                &mut b,
                run_field(run, offset_of!(ModelCfg, sw_mi_chn_en), 2),
                1,
            );
            put(
                &mut b,
                run_field(run, offset_of!(ModelCfg, mi_chn_src), 2),
                chn_src::AIISP_LAST_OUT,
            );
        }
        check(&b)
    };
    // Run 0 of seven reads outbuf_size[7]: the last one.
    assert!(last_out(7, model_mode::SINGLE_MODE).is_ok());
    // Run 0 of eight reads outbuf_size[8], and run 0 of seven in COMBO_MODE too.
    assert_eq!(
        last_out(8, model_mode::SINGLE_MODE),
        Err(ParamError::LastOut { run: 0, channel: 2 })
    );
    assert_eq!(
        last_out(7, model_mode::COMBO_MODE),
        Err(ParamError::LastOut { run: 0, channel: 2 })
    );
    assert!(last_out(6, model_mode::COMBO_MODE).is_ok());
}

#[test]
fn weights_lie_inside_the_buffer_given() {
    let len = valid().len() as u32;
    let weights = |offset: u32, size: u32| {
        let mut b = valid();
        put(&mut b, kwt(offset_of!(KwtCfg, kwt_offet), 1), offset);
        put(&mut b, kwt(offset_of!(KwtCfg, kwt_size), 1), size);
        check(&b)
    };
    assert!(weights(len - 8, 8).is_ok());
    assert!(weights(len, 0).is_ok());
    assert_eq!(
        weights(len - 8, 9),
        Err(ParamError::Weights {
            run: 1,
            offset: len - 8,
            size: 9
        })
    );
    // The read is whole words: five bytes at four from the end reach eight.
    assert!(weights(len - 4, 5).is_err());
    assert!(weights(len - 8, 5).is_ok());
    assert!(weights(u32::MAX, 4).is_err());
    assert!(weights(4, u32::MAX).is_err());
    // A run past model_runcnt is not read.
    let mut b = valid();
    put(&mut b, kwt(offset_of!(KwtCfg, kwt_offet), 2), u32::MAX);
    assert!(check(&b).is_ok());
}

// ---- Settings --------------------------------------------------------------------------------

#[test]
fn settings_are_in_range() {
    let s = Settings::new(0, 0, 16 * 1024, 8, 0, 4).expect("valid");
    let para_size = s.info().para_size;
    assert_eq!(para_size, 16 * 1024);
    assert!(Settings::new(3, 0, 1, 1, 0, 2).is_none());
    assert!(Settings::new(0, 3, 1, 1, 0, 2).is_none());
    assert!(Settings::new(0, 0, 0, 1, 0, 2).is_none());
    assert!(Settings::new(0, 0, crate::MAX_PARA_SIZE + 1, 1, 0, 2).is_none());
    assert!(Settings::new(0, 0, 1, 0, 0, 2).is_none());
    assert!(Settings::new(0, 0, 1, 9, 0, 2).is_none());
    assert!(Settings::new(0, 0, 1, 1, 2, 2).is_none());
    assert!(Settings::new(0, 0, 1, 1, 0, 1).is_none());
    assert!(Settings::new(0, 0, 1, 1, 0, 9).is_none());
    assert!(Settings::new(2, 2, crate::MAX_PARA_SIZE, 8, 1, 8).is_some());
}

// ---- The ring --------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Info(ParamInfo),
    Memory(u32),
    Start(u32),
    Write(u32, usize),
    Queue(u32, usize),
    Dequeue,
    Clear,
}

#[derive(Default)]
struct Script {
    calls: Vec<Call>,
    /// What `start` answers.
    buffers: (u32, usize),
    /// Slots `dequeue` gives back, in order.
    done: Vec<u32>,
    /// The next call of this kind fails with this error kind.
    fail_queue: Option<io::ErrorKind>,
    fail_dequeue: bool,
    fail_open: bool,
    /// Bytes last written per slot.
    written: Vec<Vec<u8>>,
    opened: u32,
    record: bool,
}

#[derive(Clone)]
struct Mock(Rc<RefCell<Script>>);

impl Mock {
    fn call(&self, call: Call) {
        let mut s = self.0.borrow_mut();
        if s.record {
            s.calls.push(call);
        }
    }
}

impl Device for Mock {
    fn set_param_info(&mut self, info: &ParamInfo) -> io::Result<()> {
        self.call(Call::Info(*info));
        Ok(())
    }
    fn set_memory_mode(&mut self, mode: u32) -> io::Result<()> {
        self.call(Call::Memory(mode));
        Ok(())
    }
    fn start(&mut self, count: u32) -> io::Result<(u32, usize)> {
        self.call(Call::Start(count));
        Ok(self.0.borrow().buffers)
    }
    fn write(&mut self, slot: u32, bytes: &[u8]) -> io::Result<()> {
        self.call(Call::Write(slot, bytes.len()));
        let mut s = self.0.borrow_mut();
        let dst = &mut s.written[slot as usize];
        dst[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    fn queue(&mut self, slot: u32, used: usize) -> io::Result<()> {
        self.call(Call::Queue(slot, used));
        match self.0.borrow_mut().fail_queue.take() {
            Some(kind) => Err(io::Error::from(kind)),
            None => Ok(()),
        }
    }
    fn dequeue(&mut self) -> io::Result<Option<u32>> {
        self.call(Call::Dequeue);
        let mut s = self.0.borrow_mut();
        if std::mem::take(&mut s.fail_dequeue) {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        if s.done.is_empty() {
            Ok(None)
        } else {
            Ok(Some(s.done.remove(0)))
        }
    }
    fn clear(&mut self) -> io::Result<()> {
        self.call(Call::Clear);
        Ok(())
    }
}

fn settings() -> Settings {
    Settings::new(0, 0, 4096, 8, 1, 3).expect("settings")
}

type Ring = Aiisp<Mock, Box<dyn FnMut() -> io::Result<Mock>>>;

fn ring(count: u32) -> (Ring, Rc<RefCell<Script>>) {
    let script = Rc::new(RefCell::new(Script {
        buffers: (count, CAPACITY),
        written: vec![vec![0; CAPACITY]; 8],
        record: true,
        ..Script::default()
    }));
    let shared = script.clone();
    let open: Box<dyn FnMut() -> io::Result<Mock>> = Box::new(move || {
        let mut s = shared.borrow_mut();
        s.opened += 1;
        if s.fail_open {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        drop(s);
        Ok(Mock(shared.clone()))
    });
    (Aiisp::new(settings(), open), script)
}

#[test]
fn opening_sets_the_node_up_in_the_kernels_order() {
    let (mut r, script) = ring(3);
    assert_eq!(r.state(), State::Closed);
    let b = valid();
    let (slot, checked) = r.submit(0, &b).expect("submit");
    assert_eq!((slot, checked.runs), (0, 2));
    assert_eq!(r.state(), State::Running);
    let s = script.borrow();
    assert_eq!(
        s.calls,
        [
            Call::Info(settings().info()),
            Call::Memory(1),
            Call::Start(3),
            Call::Write(0, b.len()),
            Call::Queue(0, b.len()),
        ]
    );
    assert_eq!(&s.written[0][..b.len()], &b[..]);
}

#[test]
fn a_refused_buffer_never_reaches_the_device() {
    let (mut r, script) = ring(3);
    let mut b = valid();
    put(&mut b, offset_of!(Params, model_mode), 9);
    assert!(matches!(
        r.submit(0, &b),
        Err(RunError::Invalid(ParamError::Mode(9)))
    ));
    let s = script.borrow();
    assert!(
        !s.calls
            .iter()
            .any(|c| matches!(c, Call::Write(..) | Call::Queue(..)))
    );
}

#[test]
fn buffers_are_used_once_until_the_kernel_gives_them_back() {
    let (mut r, script) = ring(2);
    let b = valid();
    assert_eq!(r.submit(0, &b).expect("first").0, 0);
    assert_eq!(r.submit(0, &b).expect("second").0, 1);
    assert!(matches!(r.submit(0, &b), Err(RunError::Busy)));
    script.borrow_mut().done = vec![1];
    assert_eq!(r.reclaim(0).expect("reclaim"), 1);
    assert_eq!(r.submit(0, &b).expect("third").0, 1);
    assert_eq!(r.reclaim(0).expect("nothing"), 0);
}

#[test]
fn a_buffer_the_kernel_did_not_hold_is_a_device_failure() {
    let (mut r, script) = ring(2);
    r.submit(0, &valid()).expect("submit");
    // Slot 1 was never queued, slot 5 does not exist.
    for bad in [1, 5] {
        r.stop();
        r.submit(0, &valid()).expect("submit");
        script.borrow_mut().done = vec![bad];
        assert!(matches!(r.reclaim(0), Err(RunError::Device { .. })));
    }
}

#[test]
fn the_kernel_refusing_keeps_the_node_open_and_the_buffer_free() {
    let (mut r, script) = ring(2);
    script.borrow_mut().fail_queue = Some(io::ErrorKind::InvalidInput);
    assert!(matches!(r.submit(0, &valid()), Err(RunError::Refused(_))));
    assert_eq!(r.state(), State::Running);
    assert_eq!(r.submit(0, &valid()).expect("submit").0, 0);
    assert_eq!(script.borrow().opened, 1);
}

#[test]
fn a_device_failure_closes_and_backs_off_to_the_cap_and_never_gives_up() {
    let (mut r, script) = ring(2);
    script.borrow_mut().fail_open = true;
    let mut now = 0;
    for attempt in 0..10 {
        match r.submit(now, &valid()) {
            Err(RunError::Device { retry_at_ns, .. }) => {
                assert_eq!(retry_at_ns, now + backoff_ns(attempt));
                // Before it is due nothing is touched.
                let opened = script.borrow().opened;
                assert!(matches!(
                    r.submit(retry_at_ns - 1, &valid()),
                    Err(RunError::BackingOff { .. })
                ));
                assert_eq!(script.borrow().opened, opened);
                now = retry_at_ns;
            }
            other => panic!("attempt {attempt}: {other:?}"),
        }
    }
    assert_eq!(backoff_ns(9), 8_000_000_000);
    assert_eq!(
        r.state(),
        State::BackingOff {
            failures: 10,
            retry_at_ns: now
        }
    );
    script.borrow_mut().fail_open = false;
    r.submit(now, &valid()).expect("reopens");
    assert_eq!(r.state(), State::Running);
    // A success clears the backoff.
    script.borrow_mut().fail_dequeue = true;
    match r.reclaim(now) {
        Err(RunError::Device { retry_at_ns, .. }) => assert_eq!(retry_at_ns, now + backoff_ns(0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn start_answering_out_of_range_is_a_device_failure() {
    for buffers in [(1, CAPACITY), (9, CAPACITY), (2, size_of::<Params>() - 1)] {
        let (mut r, script) = ring(2);
        script.borrow_mut().buffers = buffers;
        assert!(matches!(
            r.submit(0, &valid()),
            Err(RunError::Device { .. })
        ));
    }
}

#[test]
fn clear_reaches_the_device() {
    let (mut r, script) = ring(2);
    r.clear(0).expect("clear");
    assert_eq!(script.borrow().calls.last(), Some(&Call::Clear));
}

#[test]
fn checking_submitting_and_reclaiming_allocate_nothing() {
    let (mut r, script) = ring(2);
    let b = valid();
    r.submit(0, &b).expect("open");
    {
        let mut s = script.borrow_mut();
        s.record = false;
        s.done.reserve(8);
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..100 {
            check(&b).expect("check");
            script.borrow_mut().done.push(0);
            assert_eq!(r.reclaim(0).expect("reclaim"), 1);
            r.submit(0, &b).expect("submit");
        }
    });
    assert_eq!(allocated, 0, "steady state");
}
