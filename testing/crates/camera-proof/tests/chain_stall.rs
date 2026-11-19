//! A consumer that pauses: the simulated sensor's starts with no job queued are dropped (no
//! buffer), and the frames already queued are late when the consumer comes back. Run in process
//! on the simulated clock, with the same set-up (`setup::Chain`) and job posts (`setup::post_front`)
//! the chain proof uses. Every number is exact: the sensor's starts are the simulator's, and a
//! frame is late when the sim clock is more than a second past its start, by the proof's own check
//! (`lemnos_sim_clock::age_bad`).

use std::sync::Arc;
use std::time::Duration;

use lemnos_camera_proof::setup::{Chain, Tuning, post_front};
use lemnos_camera_sim::{Model, SimBuffers, chain_board, drivers, new_world, provider, scene};
use lemnos_client::Client;
use lemnos_contract_camera_isp::CameraIsp;
use lemnos_host::Options;
use lemnosd::{Config, Service};

const REFERENCE_BOARD: &str = include_str!("../../../boards/reference.toml");
/// The proof's age bound: a frame more than this past its start is late.
const MAX_AGE_NS: u64 = 1_000_000_000;

/// Queues the next job: its identity counts up, its output buffer is the identity's slot. The job
/// is in the block when this returns, so the sensor's next start can take it: the driver passes
/// a posted job to the block on its tick, and the count of queued jobs is what says it has.
fn post(chain: &mut Chain, identity: &mut u64) {
    *identity += 1;
    post_front(&mut chain.front.cfg, *identity, (*identity % 4) as u32).unwrap();
    let want = lemnos_camera_sim::pisp::jobs_queued() + 1;
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while lemnos_camera_sim::pisp::jobs_queued() < want {
        assert!(
            std::time::Instant::now() < until,
            "the driver did not pass job {} to the block within 30 s",
            *identity
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Takes and releases the statistics frame of a job, as the chain proof does: the driver needs
/// the statistics buffer back before it takes the next job.
fn drain_stats(chain: &mut Chain, identity: u64) {
    let st = chain
        .front
        .stats
        .next_frame(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(st.identity(), identity, "a statistics frame names its job");
    chain.front.stats.release(st).unwrap();
}

#[test]
fn a_paused_consumer_loses_the_starts_it_queued_nothing_for_and_its_queued_frames_are_late() {
    assert!(lemnos_camera_sim::pisp::install());
    lemnos_camera_sim::pisp::set_paced(true);
    *lemnos_camera_sim::WORLD.lock().unwrap() = Some(new_world(true, Model::B1));
    let board = chain_board(REFERENCE_BOARD).unwrap();
    let dir = std::env::temp_dir().join(format!("chain-stall-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("lemnosd.sock");
    let service = Service::start_with_buffers(
        Config {
            log: None,
            socket: socket.clone(),
            name: board.board.id.clone(),
            board,
            drivers: drivers(),
            backend: Box::new(provider()),
            options: Options::default(),
        },
        Arc::new(SimBuffers::probe()),
    )
    .unwrap();
    std::fs::write(dir.join("reference.raw10"), scene::reference_raw10()).unwrap();
    let client = Client::connect(&socket, "chain-stall").unwrap();
    client.set_timeout(Duration::from_secs(30));
    let list = client.list().unwrap();
    let mut chain = Chain::open(
        &client,
        &list,
        ["cam0-sensor", "cam0-receiver", "cam0-fe", "cam0-isp"],
        Tuning {
            frame_length: Some(3664),
            pattern: Some(1),
        },
    )
    .unwrap();
    chain.start().unwrap();
    // The sensor's mode select reaches the simulated bus on a host tick after its start, so the
    // steps begin once the sensor streams: a wait on the sensor's state, not on time.
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while !lemnos_camera_sim::world(|w| w.streaming) {
        assert!(
            std::time::Instant::now() < until,
            "the sensor did not start streaming within 30 s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let period = lemnos_camera_sim::period_ns();
    let wait = Duration::from_secs(30);
    let mut identity = 0u64;

    // Steady: one job queued before each start, and its frame read and released before the next.
    // Every start makes its frame; nothing is dropped and nothing is late.
    for _ in 0..5 {
        post(&mut chain, &mut identity);
        lemnos_camera_sim::sensor_step();
        let frame = chain.front.out.next_frame(wait).unwrap().unwrap();
        assert_eq!(frame.identity(), identity);
        assert!(!lemnos_sim_clock::age_bad(
            frame.timestamp_ns(),
            lemnos_camera_sim::time::now(),
            MAX_AGE_NS
        ));
        chain.front.out.release(frame).unwrap();
        drain_stats(&mut chain, identity);
    }
    let steady = chain.fe.counters().unwrap();
    assert_eq!((steady.jobs, steady.dropped, steady.errors), (5, 0, 0));

    // The stall: three jobs queued, then the consumer pauses for enough starts that the queued
    // frames are more than a second old by the time it returns. The first three starts take the
    // jobs; every later start finds no job queued, and is dropped (no buffer).
    let queued = 3u64;
    let frames_late_at_least = MAX_AGE_NS / period;
    let stalled_starts = frames_late_at_least + 2;
    for _ in 0..queued {
        post(&mut chain, &mut identity);
    }
    let first_identity = identity - queued + 1;
    let first_start = lemnos_camera_sim::time::peek_start(period);
    for _ in 0..stalled_starts {
        lemnos_camera_sim::sensor_step();
    }
    // The driver folds the block's counts in on its tick: wait until they show the stall's exact
    // numbers (a count that never gets there fails the wait, it is not accepted as it stands).
    let want_jobs = steady.jobs + queued;
    let want_dropped = steady.dropped + (stalled_starts - queued);
    let until = std::time::Instant::now() + Duration::from_secs(30);
    let after = loop {
        let c = chain.fe.counters().unwrap();
        if (c.jobs, c.dropped) == (want_jobs, want_dropped) || std::time::Instant::now() >= until {
            break c;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let dropped = after.dropped - steady.dropped;
    let jobs = after.jobs - steady.jobs;
    assert_eq!(jobs, queued, "the queued jobs were not all started");
    assert_eq!(
        dropped,
        stalled_starts - queued,
        "every start with no job queued is dropped (no buffer)"
    );
    assert_eq!(after.errors, 0);
    // Delivered and dropped account for every start the sensor made during the stall.
    assert_eq!(jobs + dropped, stalled_starts);

    // Back: the queued frames come out in order, each stamped with its own start. The sim clock
    // stands at the last start, so the first queued frame is the oldest, and it is late; a frame
    // is late when more than a second has passed since its start.
    let now = lemnos_camera_sim::time::now();
    let mut late = 0u64;
    for k in 0..queued {
        let frame = chain.front.out.next_frame(wait).unwrap().unwrap();
        assert_eq!(frame.identity(), first_identity + k);
        assert_eq!(
            frame.timestamp_ns(),
            first_start + k * period,
            "each queued frame is stamped with its own sensor start"
        );
        late += u64::from(lemnos_sim_clock::age_bad(
            frame.timestamp_ns(),
            now,
            MAX_AGE_NS,
        ));
        chain.front.out.release(frame).unwrap();
        drain_stats(&mut chain, first_identity + k);
    }
    // Frame k (from 0) started (stalled_starts - 1 - k) periods before the last start.
    let predicted_late = (0..queued)
        .filter(|k| (stalled_starts - 1 - k) * period > MAX_AGE_NS)
        .count() as u64;
    assert_eq!(late, predicted_late, "late frames after the stall");
    assert!(
        late >= 1 && late < queued,
        "the stall must make some queued frames late and not others (late {late} of {queued})"
    );
    if std::env::var_os("CHAIN_SHOW").is_some() {
        use std::io::Write;
        let _ = writeln!(
            std::io::stdout(),
            "stall: starts {stalled_starts}, jobs {jobs}, dropped {dropped}, late {late} of {queued}, period {period} ns"
        );
    }
    service.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
