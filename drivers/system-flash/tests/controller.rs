//! The target named by its storage controller (`controller`), on the reference board's own block listing
//! (`lemnos_slot_store::disk::fixture::emmc_board`, the board's sysfs): the target is the eMMC's user area, never
//! its hardware boot areas, found again at each look, so a disk that arrives after the start is taken. The disk's
//! node is a temp file in a stand-in `/dev` (`kind = "file"`); nothing here touches a real device.

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_contract_system_flash::{FORMAT_XZ, Flash};
use lemnos_core::{CallerId, Config, ConfigValue, JobId, JobState, Lifecycle, Reply, Status};
use lemnos_driver_system_flash::SystemFlash;
use lemnos_driver_system_flash::manifest::DISK;
use lemnos_slot_store::disk::fixture::{Sys, emmc_board};

const BOARD: &str = "flash-controller-test";
const EMMC: &str = "/soc@107c000000/mmc@fff000";
const SMALL_SHA: &str = "f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58";
const SMALL: u64 = 16 * 1024;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

/// A stand-in `/dev` with a node file for the user area and each boot area, and a mount table with nothing on them.
struct Dev(PathBuf);

impl Dev {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("lemnos-flash-ctl-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (node, bytes) in [
            ("mmcblk0", SMALL + 4096),
            ("mmcblk0boot0", 4096),
            ("mmcblk0boot1", 4096),
        ] {
            fs::write(dir.join(node), vec![0xa5; bytes as usize]).unwrap();
        }
        fs::write(dir.join("mountinfo"), "1 0 0:1 / / rw - rootfs rootfs rw\n").unwrap();
        Self(dir)
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn start(sys: &Sys, dev: &Dev) -> SystemFlash {
    let sysfs = sys.path().to_str().unwrap().to_string();
    let devdir = dev.0.to_str().unwrap().to_string();
    let mountinfo = dev.0.join("mountinfo").to_str().unwrap().to_string();
    let view = dev.0.join("security");
    fs::write(&view, "off\ngeneration 1\n").unwrap();
    lemnos_slot_store::security::set_view_for_thread(Some(view));
    let entries = [
        ("controller", ConfigValue::Str(EMMC)),
        ("sysfs", ConfigValue::Str(&sysfs)),
        ("dev", ConfigValue::Str(&devdir)),
        ("kind", ConfigValue::Str("file")),
        ("allow_first_flash", ConfigValue::Bool(true)),
        ("mountinfo", ConfigValue::Str(&mountinfo)),
    ];
    let cfg = Config::new(&entries).on_board(BOARD);
    <SystemFlash as Lifecycle>::check_config(&cfg).unwrap();
    let mut driver = <SystemFlash as Lifecycle>::start(
        lemnos_driver_system_flash::SystemFlashNeeds { indicator: None },
        &cfg,
    )
    .unwrap();
    driver.caller(CallerId(1));
    driver
}

fn manifest() -> Vec<u8> {
    format!(
        r#"{{"schema":1,"board":"{BOARD}","format":"{DISK}","image":{{"size":{SMALL},"sha256":"{SMALL_SHA}"}}}}"#
    )
    .into_bytes()
}

fn info(driver: &mut SystemFlash) -> String {
    let mut out = vec![0u8; 4096];
    let n = driver.info(&mut out).unwrap();
    String::from_utf8(out[..n].to_vec()).unwrap()
}

/// The job's end, ticking the driver while its writer finishes.
fn finished(driver: &mut SystemFlash, job: JobId) -> (Status, Vec<u8>) {
    loop {
        let mut out = [0u8; 64];
        let (state, n) = {
            let mut reply = Reply::new(&mut out);
            let state = driver.job(job, &mut reply);
            (state, reply.len())
        };
        match state {
            JobState::Finished(status) => return (status, out[..n].to_vec()),
            JobState::Running { .. } => {
                lemnos_core::Lifecycle::tick(driver, 0);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

fn write_small(driver: &mut SystemFlash) -> (Status, Vec<u8>) {
    let bytes = fixture("crc64.img.xz");
    driver
        .manifest(&manifest(), &[])
        .expect("the manifest is accepted");
    let job = driver.write(bytes.len() as u64, FORMAT_XZ).unwrap();
    let mut at = 0;
    for chunk in bytes.chunks(4096) {
        // `BUSY` is the writer behind: the piece is offered again, as the bridge does.
        while driver.data(at, chunk) == Err(Status::BUSY) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        at += chunk.len() as u64;
    }
    finished(driver, job)
}

#[test]
fn on_the_board_s_emmc_the_controller_s_target_is_the_user_area_and_the_boot_areas_are_untouched() {
    let sys = Sys::new("flash-emmc");
    emmc_board(&sys);
    let dev = Dev::new("emmc");
    let mut driver = start(&sys, &dev);
    let (status, reply) = write_small(&mut driver);
    let want = format!("raw_bytes={SMALL} outcome=1").into_bytes();
    assert_eq!((status, reply), (Status::OK, want), "{}", info(&mut driver));
    let user = fs::read(dev.0.join("mmcblk0")).unwrap();
    assert_eq!(&user[..SMALL as usize], fixture("small.img").as_slice());
    assert!(
        user[SMALL as usize..].iter().all(|&b| b == 0xa5),
        "bytes past the image are left as they were"
    );
    for area in ["mmcblk0boot0", "mmcblk0boot1"] {
        assert!(
            fs::read(dev.0.join(area))
                .unwrap()
                .iter()
                .all(|&b| b == 0xa5),
            "{area} is never written"
        );
    }
}

#[test]
fn a_disk_that_is_not_there_yet_is_named_and_taken_once_it_arrives() {
    let sys = Sys::new("flash-late");
    let dev = Dev::new("late");
    let mut driver = start(&sys, &dev);
    let text = info(&mut driver);
    assert!(
        text.contains(&format!("no whole disk sits under {EMMC}")),
        "the reason names the controller: {text}"
    );
    driver
        .manifest(&manifest(), &[])
        .expect("a manifest needs no target");
    assert!(
        driver.write(SMALL, FORMAT_XZ).is_err(),
        "no write before the disk is there"
    );
    // The eMMC registers its disk after the start: the next look finds it.
    emmc_board(&sys);
    let (status, _) = write_small(&mut driver);
    assert_eq!(status, Status::OK, "{}", info(&mut driver));
    assert_eq!(
        &fs::read(dev.0.join("mmcblk0")).unwrap()[..SMALL as usize],
        fixture("small.img").as_slice()
    );
}

#[test]
fn the_target_is_named_once_by_path_or_controller_and_a_controller_names_a_disk() {
    fn check(entries: &[(&'static str, ConfigValue<'static>)]) -> Option<String> {
        <SystemFlash as Lifecycle>::check_config(&Config::new(entries))
            .err()
            .map(|e| e.key.to_string())
    }
    assert_eq!(check(&[]), Some("path".into()));
    assert_eq!(
        check(&[
            ("path", ConfigValue::Str("/dev/mmcblk0")),
            ("controller", ConfigValue::Str(EMMC))
        ]),
        Some("controller".into())
    );
    assert_eq!(
        check(&[("controller", ConfigValue::Str("mmc@fff000"))]),
        Some("controller".into())
    );
    assert_eq!(
        check(&[
            ("controller", ConfigValue::Str(EMMC)),
            ("kind", ConfigValue::Str("mtd"))
        ]),
        Some("controller".into())
    );
    assert_eq!(
        check(&[
            ("path", ConfigValue::Str("/dev/mmcblk0")),
            ("dev", ConfigValue::Str("/dev"))
        ]),
        Some("dev".into())
    );
    assert_eq!(check(&[("controller", ConfigValue::Str(EMMC))]), None);
}
