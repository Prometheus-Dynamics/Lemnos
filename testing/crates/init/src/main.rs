//! The QEMU guest's first process for the modules run. It mounts the filesystems, loads the test
//! modules from the image, binds the test devices through sysfs, runs every check by its absolute
//! path with its arguments, prints one summary line and powers off. Static musl, so the image needs
//! nothing else.
//!
//! Port of `testing/qemu/init.c` (CZ-GUEST-3). Every step runs in the C file's order, every check
//! keeps its text, and the lines run.sh greps for are printed as before: `lemnos-qemu: start`, the
//! `ok`/`FAIL` lines, and `lemnos-qemu: passed|FAILED`. Modules load with `finit_module(2)` from the
//! file, and unload with `delete_module(2)`; sysfs and configfs are written as plain files.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, chown};
use std::process::{Command, ExitCode};

use lemnos_linux_sys::guest;

const EBUSY: i32 = libc::EBUSY;
const EAGAIN: i32 = libc::EAGAIN;
const EEXIST: i32 = libc::EEXIST;
const EIO: i32 = libc::EIO;
const EROFS: i32 = libc::EROFS;
const ENODEV: i32 = libc::ENODEV;

/// `O_NONBLOCK`, as `delete_module(2)` takes it.
const NONBLOCK: u32 = libc::O_NONBLOCK as u32;

/// The group the daemon runs as in the gadget check (`DAEMON_ID` in testing/crates/gadget-check).
const DAEMON_GID: u32 = 999;

const BLOCK: &str = "lemnos-test-block";
const OTHER: &str = "lemnos-test-other";
const SPI: &str = "spi-lemnos-test";
const PISP: &str = "lemnos-test-pisp";
const GSDMA: &str = "lemnos-test-gsdma";
const VE: &str = "lemnos-test-ve";
const VIP: &str = "lemnos-test-vip";
const VIP_SYS: &str = "/sys/bus/platform/devices/lemnos-test-vip";
const ISP: &str = "lemnos-test-isp";
const BIND: &str = "/sys/bus/platform/drivers/lemnos/bind";
const UNBIND: &str = "/sys/bus/platform/drivers/lemnos/unbind";
const CSI2: &str = "lemnos-test-csi2";
const CSI2_DRIVER: &str = "lemnos-rp1-csi2";
const CSI2_BIND: &str = "/sys/bus/platform/drivers/lemnos-rp1-csi2/bind";
const CSI2_UNBIND: &str = "/sys/bus/platform/drivers/lemnos-rp1-csi2/unbind";
/// The NAND simulator's chip: see `testing/qemu/init.c`'s NANDSIM for the geometry and faults.
const NANDSIM: &str = "first_id_byte=0x98 second_id_byte=0xda third_id_byte=0x90 fourth_id_byte=0x15 \
     parts=64,64,64,8,4 badblocks=5,9,70,104,131,148,178 weakblocks=203:0 bitflips=1";
const SIM: &str = "/sys/kernel/config/gpio-sim/lemnos";

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The errno of a failed call, or `EIO` for an error that carries none.
fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

/// `strerror(errno)`: the text of an errno, without the " (os error N)" std appends.
fn strerror(errno: i32) -> String {
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// Whether `path` exists. For the root process this answers as `access(path, R_OK)` and
/// `access(path, F_OK)` do.
fn accessible(path: &str) -> bool {
    fs::metadata(path).is_ok()
}

/// Counts the checks and prints each one's line, as the C file's `check()` does.
#[derive(Default)]
struct Checks {
    failures: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        say(&format!("{} {what}", if ok { "ok  " } else { "FAIL " }));
        self.failures += u32::from(!ok);
    }
}

/// Loads the module at `path` with `params`. 0, or -1 after printing why it failed.
fn load(path: &str, params: &str) -> i32 {
    let result = File::open(path).and_then(|file| guest::finit_module(file.as_fd(), params));
    match result {
        Ok(()) => 0,
        Err(e) => {
            let e = errno_of(&e);
            say(&format!("load {path}: {}", strerror(e)));
            -1
        }
    }
}

/// Unloads the module `name`. 0, or -1 after printing why it failed.
fn unload(name: &str) -> i32 {
    match guest::delete_module(name, NONBLOCK) {
        Ok(()) => 0,
        Err(e) => {
            let e = errno_of(&e);
            say(&format!("unload {name}: {}", strerror(e)));
            -1
        }
    }
}

/// An unload the test expects to be refused: true when it was refused with EAGAIN. Nothing is
/// printed.
fn unload_refused_busy(name: &str) -> bool {
    matches!(guest::delete_module(name, NONBLOCK), Err(e) if errno_of(&e) == EAGAIN)
}

/// Writes `text` to the sysfs (or configfs) file at `path`. 0, or the errno of the failed open or
/// write: an ENODEV write is returned without a line, the others print one.
fn put(path: &str, text: &str) -> i32 {
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(e) => {
            let e = errno_of(&e);
            say(&format!("open {path}: {}", strerror(e)));
            return e;
        }
    };
    let e = (&file)
        .write(text.as_bytes())
        .err()
        .map_or(0, |err| errno_of(&err));
    drop(file);
    if e != 0 && e != ENODEV {
        say(&format!("write {path}: {}", strerror(e)));
    }
    e
}

/// Binds the platform device `dev` to the Lemnos driver through its override. 0, or -1 when the
/// override was refused, or the bind's errno.
fn bind_block(dev: &str) -> i32 {
    if put(
        &format!("/sys/bus/platform/devices/{dev}/driver_override"),
        "lemnos",
    ) != 0
    {
        return -1;
    }
    put(BIND, dev)
}

/// Binds the fake receiver to its driver through its override. Same results as `bind_block`.
fn bind_csi2() -> i32 {
    if put(
        &format!("/sys/bus/platform/devices/{CSI2}/driver_override"),
        CSI2_DRIVER,
    ) != 0
    {
        return -1;
    }
    put(CSI2_BIND, CSI2)
}

/// A device keeps its override after an unbind, and would bind on the next load. 0 or the errno.
fn forget_override(dev: &str) -> i32 {
    put(
        &format!("/sys/bus/platform/devices/{dev}/driver_override"),
        "\n",
    )
}

/// Runs `argv[0]` with the rest of `argv` and waits for it. 0 when it exits 0. Otherwise its status
/// (128 when a signal ended it), or 127 when it could not be started, which is what the C child's
/// `_exit(127)` gave.
fn run(argv: &[&str]) -> i32 {
    match Command::new(argv[0]).args(&argv[1..]).status() {
        Ok(status) => status.code().unwrap_or(128),
        Err(_) => 127,
    }
}

/// The DMA heap the checks allocate from: the first of the three the kernel may name that exists,
/// else the CMA one.
fn cma_heap() -> &'static str {
    const HEAPS: [&str; 3] = [
        "/dev/dma_heap/default_cma_region",
        "/dev/dma_heap/linux,cma",
        "/dev/dma_heap/reserved",
    ];
    HEAPS
        .into_iter()
        .find(|heap| accessible(heap))
        .unwrap_or(HEAPS[1])
}

/// The heap's name, as an argument the module check takes (the heap path without its directory).
fn cma_heap_name() -> &'static str {
    let heap = cma_heap();
    heap.strip_prefix("/dev/dma_heap/").unwrap_or(heap)
}

/// Gives a node the daemon opens by name to the daemon's group, the way the board's generated udev
/// fragment (`lemnos-ctl tree --udev`) does, before the gadget check reads it. Prints its line only
/// when the node exists.
fn give_to_daemon_group(c: &mut Checks, path: &str) {
    if !accessible(path) {
        return;
    }
    let given = chown(path, Some(0), Some(DAEMON_GID)).is_ok()
        && fs::set_permissions(path, Permissions::from_mode(0o660)).is_ok();
    c.check(given, "node given to the daemon group");
}

fn give_module_nodes_to_daemon_group(c: &mut Checks) {
    let Ok(dev) = fs::read_dir("/dev") else {
        return;
    };
    for entry in dev.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != "lemnos" && !name.starts_with("lemnos-") {
            continue;
        }
        give_to_daemon_group(c, &format!("/dev/{name}"));
    }
}

/// The contents of a one-line file, read as the C file reads it: at most `size - 1` bytes, cut at a
/// newline or NUL. `None` when it cannot be read or is empty.
fn get_text(path: &str, size: usize) -> Option<String> {
    let mut buf = vec![0u8; size - 1];
    let n = File::open(path)
        .and_then(|mut file| file.read(&mut buf))
        .ok()?;
    if n == 0 {
        return None;
    }
    let end = buf[..n]
        .iter()
        .position(|&b| b == b'\n' || b == 0)
        .unwrap_or(n);
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// The simulated chip's number and sysfs directory, as the checks take them. Empty until the chip
/// is up.
#[derive(Default)]
struct GpioSim {
    chip: String,
    dir: String,
}

/// `snprintf(out, room, ...)`: the text, cut to `room - 1` bytes.
fn fit(text: &str, room: usize) -> String {
    text.chars().take(room - 1).collect()
}

fn mkdir(path: &str) -> io::Result<()> {
    DirBuilder::new().mode(0o755).create(path)
}

/// An eight-line simulated chip, through configfs. `sim` gets its chip and directory on success.
fn gpio_sim_up(sim: &mut GpioSim) -> bool {
    if let Err(e) = mkdir("/sys/kernel/config")
        && !matches!(errno_of(&e), EEXIST | EROFS)
    {
        return false;
    }
    if let Err(e) = guest::mount("configfs", "/sys/kernel/config", "configfs")
        && errno_of(&e) != EBUSY
    {
        return false;
    }
    if accessible("/gpio-sim.ko") && load("/gpio-sim.ko", "") != 0 {
        return false;
    }
    if mkdir(SIM).is_err()
        || mkdir(&format!("{SIM}/bank0")).is_err()
        || put(&format!("{SIM}/bank0/num_lines"), "8") != 0
        || put(&format!("{SIM}/live"), "1") != 0
    {
        return false;
    }
    let Some(dev) = get_text(&format!("{SIM}/dev_name"), 64) else {
        return false;
    };
    let Some(chip) = get_text(&format!("{SIM}/bank0/chip_name"), 32) else {
        return false;
    };
    // The chip's number: the name past "gpiochip". The C file keeps 16 bytes for it.
    let number = chip.get("gpiochip".len()..).unwrap_or("");
    sim.chip = fit(number, 16);
    sim.dir = fit(&format!("/sys/devices/platform/{dev}/{chip}"), 128);
    true
}

fn gpio_sim_down() {
    let _ = put(&format!("{SIM}/live"), "0");
    let _ = fs::remove_dir(format!("{SIM}/bank0"));
    let _ = fs::remove_dir(SIM);
    if accessible("/gpio-sim.ko") {
        unload("gpio_sim");
    }
}

/// Binds the SPI test device to `driver`. True when both writes went through.
fn spi_bind(driver: &str) -> bool {
    put(
        "/sys/bus/spi/devices/spi-lemnos-test/driver_override",
        driver,
    ) == 0
        && put(&format!("/sys/bus/spi/drivers/{driver}/bind"), SPI) == 0
}

/// Unbinds the SPI test device from `driver` and clears its override. True when both writes went
/// through.
fn spi_unbind(driver: &str) -> bool {
    put(&format!("/sys/bus/spi/drivers/{driver}/unbind"), SPI) == 0
        && put("/sys/bus/spi/devices/spi-lemnos-test/driver_override", "\n") == 0
}

/// `route` is how I2C, GPIO and PWM should go; `spi_route` is how SPI should.
fn backend_check(sim: &GpioSim, route: &str, spi_route: &str) -> bool {
    run(&[
        "/lemnos-module-check",
        route,
        spi_route,
        sim.chip.as_str(),
        sim.dir.as_str(),
    ]) == 0
}

fn main() -> ExitCode {
    let _ = guest::mount("devtmpfs", "/dev", "devtmpfs");
    let _ = guest::mount("sysfs", "/sys", "sysfs");
    let _ = guest::mount("proc", "/proc", "proc");
    let _ = guest::mount("debugfs", "/sys/kernel/debug", "debugfs");
    // /work holds the images the checks write (nand-check's 7.5 MB UBI image). The root is a tmpfs
    // that the initramfs fills (about 188 MB of check binaries in a 191 MB root), so it has no room
    // for them: give /work its own tmpfs, sized for the largest image. (From I2b2-6, carried here.)
    let _ = std::fs::create_dir("/work");
    match guest::mount_data("tmpfs", "/work", "tmpfs", "size=64M") {
        Err(e) if e.raw_os_error() != Some(libc::EBUSY) => {
            say(&format!("lemnos-qemu: /work tmpfs not mounted: {e}"));
        }
        _ => {}
    }
    let _ = guest::dup_console_stdio();
    say("lemnos-qemu: start");

    let mut c = Checks::default();
    let mut sim = GpioSim::default();

    c.check(
        load("/rp1_pio_mock.ko", "") == 0,
        "stand-in PIO driver loaded",
    );
    c.check(
        load("/lemnos_test_block.ko", "csi2_phys=0x20000000") == 0,
        "test block module loaded",
    );
    c.check(
        load("/lemnos.ko", "owned=lemnos,test-block,lemnos,test-spi") == 0,
        "module loaded",
    );
    c.check(accessible("/dev/lemnos"), "control node exists");
    // The Rust half's skeleton (kernel/rust, stage 0 of the port): it loads and unloads beside the C
    // module, under its own names, and the kernel reports nothing wrong while it does.
    c.check(load("/lemnos_rs.ko", "") == 0, "Rust skeleton loaded");
    c.check(unload("lemnos_rs") == 0, "Rust skeleton unloaded");
    c.check(
        bind_block(OTHER) == ENODEV,
        "a block not listed is refused (ENODEV)",
    );
    c.check(
        bind_csi2() == ENODEV,
        "the receiver not in owned= is refused (ENODEV)",
    );
    c.check(forget_override(CSI2) == 0, "receiver override cleared");
    c.check(bind_block(BLOCK) == 0, "block bound");
    c.check(
        run(&["/lemnos_check", "/dev/lemnos", BLOCK, cma_heap()]) == 0,
        "lemnos_check",
    );
    c.check(
        run(&[
            "/lemnos_stream_check",
            "/dev/lemnos",
            BLOCK,
            UNBIND,
            cma_heap(),
            "no-source",
        ]) == 0,
        "lemnos_stream_check, no test source",
    );
    c.check(put(UNBIND, BLOCK) == 0, "unbound");
    c.check(
        forget_override(BLOCK) == 0 && forget_override(OTHER) == 0,
        "overrides cleared",
    );
    c.check(unload("lemnos") == 0, "module unloaded");

    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi unconfined=lemnos,test-block test_irq_inject=1",
        ) == 0,
        "module loaded with the test block unconfined and the interrupt test path",
    );
    c.check(bind_block(BLOCK) == 0, "block bound");
    c.check(
        run(&["/lemnos_irq_check", "/dev/lemnos", BLOCK, "2000"]) == 0,
        "lemnos_irq_check",
    );
    c.check(
        run(&["/lemnos_unbind_check", "/dev/lemnos", BLOCK, UNBIND]) == 0,
        "lemnos_unbind_check",
    );
    c.check(
        run(&["/lemnos_claim_check", "/dev/lemnos"]) == 0,
        "lemnos_claim_check",
    );

    c.check(
        load("/i2c-stub.ko", "chip_addr=0x30") == 0,
        "i2c-stub loaded",
    );
    c.check(load("/lemnos_test_bus.ko", "") == 0, "test buses loaded");
    c.check(spi_bind("lemnos"), "SPI device bound");
    c.check(
        run(&["/lemnos_bus_check", "/dev/lemnos"]) == 0,
        "lemnos_bus_check",
    );
    c.check(
        put("/sys/bus/spi/drivers/lemnos/unbind", SPI) == 0,
        "SPI device unbound",
    );
    c.check(
        put("/sys/bus/spi/devices/spi-lemnos-test/driver_override", "\n") == 0,
        "override cleared",
    );

    c.check(gpio_sim_up(&mut sim), "simulated GPIO chip up");
    c.check(
        run(&[
            "/lemnos_gpio_check",
            "/dev/lemnos",
            sim.chip.as_str(),
            sim.dir.as_str(),
            "lemnos-test-parent",
        ]) == 0,
        "lemnos_gpio_check",
    );

    c.check(
        run(&["/lemnos_power_check", "/dev/lemnos", BLOCK, BIND, UNBIND]) == 0,
        "lemnos_power_check",
    );

    c.check(
        run(&["/lemnos_pio_check", "/dev/lemnos"]) == 0,
        "lemnos_pio_check",
    );

    // One binary, the real backend: the module where it serves a bus, stock elsewhere.
    c.check(
        bind_block(BLOCK) == 0,
        "block bound for the backend's DMA memory",
    );
    c.check(spi_bind("lemnos"), "SPI device bound to the module");
    c.check(
        backend_check(&sim, "module", "module"),
        "backend: everything through the module",
    );
    c.check(spi_unbind("lemnos"), "SPI device unbound from the module");
    if accessible("/spidev.ko") {
        c.check(load("/spidev.ko", "") == 0, "spidev loaded");
    }
    c.check(spi_bind("spidev"), "SPI device bound to spidev");
    c.check(
        backend_check(&sim, "module", "stock"),
        "backend: SPI alone stays stock",
    );
    // Waits for every buffer the backend allocated: it returns only if none is left.
    c.check(
        put(UNBIND, BLOCK) == 0,
        "block unbound after the backend's DMA memory",
    );
    c.check(
        forget_override(BLOCK) == 0 && forget_override(OTHER) == 0,
        "overrides cleared",
    );
    c.check(
        unload("lemnos") == 0,
        "module unloaded with nothing left held",
    );
    c.check(
        backend_check(&sim, "stock", "stock"),
        "backend: no module, everything stock",
    );
    c.check(spi_unbind("spidev"), "SPI device unbound from spidev");
    if accessible("/spidev.ko") {
        c.check(unload("spidev") == 0, "spidev unloaded");
    }
    gpio_sim_down();
    c.check(unload("lemnos_test_bus") == 0, "test buses unloaded");
    c.check(unload("i2c_stub") == 0, "i2c-stub unloaded");

    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi test_stream_source=1",
        ) == 0,
        "module loaded with the test stream source",
    );
    c.check(bind_block(BLOCK) == 0, "block bound");
    c.check(
        run(&["/lemnos-shm-ring-check", "/dev/lemnos", BLOCK, cma_heap()]) == 0,
        "lemnos-shm-ring-check",
    );
    c.check(
        run(&["/lemnos-shm-holds-check", "/dev/lemnos", BLOCK, cma_heap()]) == 0,
        "lemnos-shm-holds-check",
    );
    c.check(
        run(&[
            "/lemnos-shm-holds-check",
            "/dev/lemnos",
            BLOCK,
            cma_heap(),
            "cutoff",
        ]) == 0,
        "lemnos-shm-holds-check cutoff",
    );
    c.check(
        run(&[
            "/lemnos-shm-holds-check",
            "/dev/lemnos",
            BLOCK,
            cma_heap(),
            "release",
        ]) == 0,
        "lemnos-shm-holds-check release",
    );
    c.check(
        run(&[
            "/lemnos-shm-holds-check",
            "/dev/lemnos",
            BLOCK,
            cma_heap(),
            "layout",
        ]) == 0,
        "lemnos-shm-holds-check layout",
    );
    c.check(
        run(&["/lemnos-shm-bell-check", "/dev/lemnos", BLOCK, cma_heap()]) == 0,
        "lemnos-shm-bell-check",
    );
    c.check(
        run(&[
            "/lemnos_stream_check",
            "/dev/lemnos",
            BLOCK,
            UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_stream_check",
    );
    c.check(forget_override(BLOCK) == 0, "override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded with every stream released",
    );

    c.check(
        load("/lemnos.ko", "owned=lemnos,test-block,lemnos,test-csi2") == 0,
        "module loaded owning the receiver",
    );
    c.check(bind_block(BLOCK) == 0, "test block bound");
    c.check(bind_csi2() == 0, "receiver bound");
    c.check(
        run(&[
            "/lemnos_rp1_csi2_check",
            "/dev/lemnos",
            BLOCK,
            CSI2,
            CSI2_UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_rp1_csi2_check",
    );
    c.check(put(UNBIND, BLOCK) == 0, "test block unbound");
    c.check(
        forget_override(BLOCK) == 0 && forget_override(CSI2) == 0,
        "overrides cleared",
    );
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the receiver checks",
    );

    // The frame path end to end: the receiver's frames through the module's stream, the rp1-csi2
    // driver, the host's endpoint and the shm ring, read by a client through lemnosd.
    c.check(
        load("/lemnos.ko", "owned=lemnos,test-block,lemnos,test-csi2") == 0,
        "module loaded for the composition check",
    );
    c.check(
        bind_block(BLOCK) == 0,
        "test block bound for the composition check",
    );
    c.check(bind_csi2() == 0, "receiver bound for the composition check");
    c.check(
        run(&["/lemnos-stream-compose-check", "/lemnosd", CSI2, cma_heap()]) == 0,
        "lemnos-stream-compose-check",
    );
    c.check(
        put(CSI2_UNBIND, CSI2) == 0,
        "receiver unbound after the composition check",
    );
    c.check(
        put(UNBIND, BLOCK) == 0,
        "test block unbound after the composition check",
    );
    c.check(
        forget_override(BLOCK) == 0 && forget_override(CSI2) == 0,
        "composition overrides cleared",
    );
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the composition check",
    );

    c.check(
        load("/lemnos.ko", "owned=lemnos,test-block,lemnos,test-csi2") == 0,
        "module loaded for the stuck-DMA check",
    );
    c.check(bind_csi2() == 0, "receiver bound again");
    c.check(
        run(&[
            "/lemnos_rp1_csi2_check",
            "wedge",
            CSI2,
            CSI2_UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_rp1_csi2_check wedge",
    );
    c.check(forget_override(CSI2) == 0, "override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded with a wedged stream pinned",
    );

    c.check(
        load("/lemnos.ko", "owned=lemnos,test-block,lemnos,test-csi2") == 0,
        "module loaded for the stuck-DMA check on own buffers",
    );
    c.check(bind_csi2() == 0, "receiver bound again");
    c.check(
        run(&[
            "/lemnos_rp1_csi2_check",
            "wedge-own",
            CSI2,
            CSI2_UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_rp1_csi2_check wedge-own",
    );
    c.check(forget_override(CSI2) == 0, "override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded with a wedged stream's own buffers pinned",
    );

    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-csi2 test_pisp_fe=1",
        ) == 0,
        "module loaded with the front end test paths",
    );
    c.check(bind_block(BLOCK) == 0, "test block bound");
    c.check(bind_csi2() == 0, "receiver with front end bound");
    c.check(
        run(&[
            "/lemnos_pisp_fe_check",
            "/dev/lemnos",
            CSI2,
            CSI2_UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_pisp_fe_check",
    );
    c.check(put(UNBIND, BLOCK) == 0, "test block unbound");
    c.check(
        forget_override(BLOCK) == 0 && forget_override(CSI2) == 0,
        "overrides cleared",
    );
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the front end checks",
    );

    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-csi2 test_pisp_fe=1",
        ) == 0,
        "module loaded for the front end's stuck-DMA check",
    );
    c.check(bind_csi2() == 0, "receiver bound again");
    c.check(
        run(&[
            "/lemnos_pisp_fe_check",
            "wedge",
            CSI2,
            CSI2_UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_pisp_fe_check wedge",
    );
    c.check(forget_override(CSI2) == 0, "override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded with a wedged front end stream pinned",
    );
    c.check(
        unload("lemnos_test_block") == 0,
        "test block module unloaded",
    );

    c.check(
        load("/lemnos_test_block.ko", "") == 0,
        "test block module loaded for the PiSP back end",
    );

    // The graphics DMA engine on its fake engine.
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-gsdma gsdma=lemnos,test-gsdma test_gsdma_fake_regs=1",
        ) == 0,
        "module loaded with the graphics DMA engine",
    );
    c.check(bind_block(GSDMA) == 0, "graphics DMA engine bound");
    c.check(
        run(&[
            "/lemnos_gsdma_check",
            "/dev/lemnos",
            GSDMA,
            cma_heap(),
            "run",
        ]) == 0,
        "lemnos_gsdma_check run",
    );
    // The driver's own Linux device, through the same node and fake engine.
    c.check(
        run(&["/lemnos-module-check", "gsdma", GSDMA, cma_heap_name()]) == 0,
        "lemnos-module-check gsdma",
    );
    c.check(put(UNBIND, GSDMA) == 0, "graphics DMA engine unbound");
    c.check(forget_override(GSDMA) == 0, "graphics DMA override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the graphics DMA checks",
    );
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-gsdma gsdma=lemnos,test-gsdma test_gsdma_fake_regs=1 test_gsdma_reach=0x100000",
        ) == 0,
        "module loaded with the graphics DMA engine's reach lowered",
    );
    c.check(bind_block(GSDMA) == 0, "graphics DMA engine bound");
    c.check(
        run(&[
            "/lemnos_gsdma_check",
            "/dev/lemnos",
            GSDMA,
            cma_heap(),
            "reach",
        ]) == 0,
        "lemnos_gsdma_check reach",
    );
    c.check(put(UNBIND, GSDMA) == 0, "graphics DMA engine unbound");
    c.check(forget_override(GSDMA) == 0, "graphics DMA override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the reach check",
    );

    // The VIP9000 on its fake block: a chip pointed at its tables by register.
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-vip vip9000=lemnos,test-vip test_vip_fake_regs=1 test_vip_timeout_ms=300",
        ) == 0,
        "module loaded with the VIP9000",
    );
    c.check(bind_block(VIP) == 0, "VIP9000 bound");
    c.check(
        run(&["/lemnos_vip_check", "/dev/lemnos", VIP, cma_heap(), "run"]) == 0,
        "lemnos_vip_check run",
    );
    // The driver's claim over its Linux session, through the same node and fake block.
    c.check(
        run(&["/lemnos-module-check", "vip", VIP, cma_heap_name()]) == 0,
        "lemnos-module-check vip",
    );
    c.check(put(UNBIND, VIP) == 0, "VIP9000 unbound");
    // One pointed at them by a command.
    c.check(
        put(&format!("{VIP_SYS}/cid"), "0x10000016") == 0,
        "fake VIP9000 reports a command-set-up chip",
    );
    c.check(bind_block(VIP) == 0, "VIP9000 bound as that chip");
    c.check(
        run(&[
            "/lemnos_vip_check",
            "/dev/lemnos",
            VIP,
            cma_heap(),
            "command",
        ]) == 0,
        "lemnos_vip_check command",
    );
    c.check(put(UNBIND, VIP) == 0, "VIP9000 unbound");
    // Chips whose MMU isn't known, or that have none: refused at bind.
    c.check(
        put(&format!("{VIP_SYS}/cid"), "0x12345678") == 0,
        "fake VIP9000 reports an unknown chip",
    );
    bind_block(VIP);
    c.check(
        !accessible(&format!("{VIP_SYS}/driver")),
        "an unknown chip is refused",
    );
    c.check(
        put(&format!("{VIP_SYS}/cid"), "0xee") == 0,
        "fake VIP9000 reports a chip without an MMU",
    );
    bind_block(VIP);
    c.check(
        !accessible(&format!("{VIP_SYS}/driver")),
        "a chip without an MMU is refused",
    );
    c.check(
        put(&format!("{VIP_SYS}/cid"), "0x1000003b") == 0,
        "fake VIP9000 reports the first chip again",
    );
    c.check(forget_override(VIP) == 0, "VIP9000 override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the VIP9000 checks",
    );

    // The PiSP back end on its fake block.
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-pisp-be pisp_be=lemnos,test-pisp-be test_pb_fake_regs=1",
        ) == 0,
        "module loaded with the PiSP back end",
    );
    c.check(bind_block(PISP) == 0, "PiSP block bound");
    c.check(
        run(&[
            "/lemnos_pisp_be_check",
            "/dev/lemnos",
            PISP,
            BIND,
            UNBIND,
            cma_heap(),
            "refuse",
        ]) == 0,
        "lemnos_pisp_be_check refuse",
    );
    c.check(
        run(&[
            "/lemnos_pisp_be_check",
            "/dev/lemnos",
            PISP,
            BIND,
            UNBIND,
            cma_heap(),
            "lifetime",
        ]) == 0,
        "lemnos_pisp_be_check lifetime",
    );
    c.check(
        run(&[
            "/lemnos_pisp_be_version_check",
            "/dev/lemnos",
            PISP,
            "0x02252700",
        ]) == 0,
        "PiSP back end version read back through the control node",
    );
    c.check(put(UNBIND, PISP) == 0, "PiSP block unbound");
    c.check(forget_override(PISP) == 0, "PiSP override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded with the back end idle",
    );

    // A block whose version register is a foreign version is refused at probe: it does not bind.
    c.check(
        unload("lemnos_test_block") == 0,
        "test block module unloaded before the foreign version",
    );
    c.check(
        load("/lemnos_test_block.ko", "pisp_version=0x01146666") == 0,
        "test block module loaded with a foreign PiSP back end version",
    );
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-pisp-be pisp_be=lemnos,test-pisp-be test_pb_fake_regs=1",
        ) == 0,
        "module loaded with the PiSP back end on the foreign version",
    );
    c.check(
        bind_block(PISP) != 0,
        "a foreign PiSP back end version does not bind",
    );
    c.check(
        forget_override(PISP) == 0,
        "PiSP override cleared after the refusal",
    );
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the foreign version",
    );
    c.check(
        unload("lemnos_test_block") == 0,
        "test block module unloaded after the foreign version",
    );
    c.check(
        load("/lemnos_test_block.ko", "") == 0,
        "test block module reloaded with the family's version",
    );

    // Register jobs on a fake video engine and a fake ISP. The first check unbinds its block.
    c.check(
        load("/lemnos_test_regjob.ko", "") == 0,
        "fake register-job blocks loaded",
    );
    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-ve,lemnos,test-isp a733_ve=lemnos,test-ve k230_isp=lemnos,test-isp test_rj_fake_regs=1",
        ) == 0,
        "module loaded with register jobs",
    );
    c.check(bind_block(VE) == 0, "fake video engine bound");
    c.check(bind_block(ISP) == 0, "fake ISP bound");
    c.check(
        run(&[
            "/lemnos_regjob_check",
            "/dev/lemnos",
            VE,
            UNBIND,
            cma_heap(),
        ]) == 0,
        "lemnos_regjob_check",
    );
    c.check(
        run(&["/lemnos_regjob_isp_check", "/dev/lemnos", ISP, cma_heap()]) == 0,
        "lemnos_regjob_isp_check",
    );
    c.check(put(UNBIND, ISP) == 0, "fake ISP unbound");
    c.check(
        forget_override(VE) == 0,
        "fake video engine override cleared",
    );
    c.check(forget_override(ISP) == 0, "fake ISP override cleared");
    c.check(
        unload("lemnos") == 0,
        "module unloaded after the register jobs",
    );
    c.check(
        unload("lemnos_test_regjob") == 0,
        "fake register-job blocks unloaded",
    );

    c.check(
        load(
            "/lemnos.ko",
            "owned=lemnos,test-block,lemnos,test-spi,lemnos,test-pisp-be,lemnos,test-gsdma,lemnos,test-vip pisp_be=lemnos,test-pisp-be test_pb_fake_regs=1 gsdma=lemnos,test-gsdma test_gsdma_fake_regs=1 vip9000=lemnos,test-vip test_vip_fake_regs=1 test_vip_timeout_ms=300",
        ) == 0,
        "module loaded for the wedge checks",
    );
    c.check(bind_block(GSDMA) == 0, "graphics DMA engine bound");
    c.check(
        run(&[
            "/lemnos_gsdma_check",
            "/dev/lemnos",
            GSDMA,
            cma_heap(),
            "wedge",
        ]) == 0,
        "lemnos_gsdma_check wedge",
    );
    c.check(bind_block(VIP) == 0, "VIP9000 bound");
    c.check(
        run(&["/lemnos_vip_check", "/dev/lemnos", VIP, cma_heap(), "wedge"]) == 0,
        "lemnos_vip_check wedge",
    );
    c.check(bind_block(PISP) == 0, "PiSP block bound");
    // A system-heap dma-buf lent to the block. It rebinds the block itself when it is done, so the
    // wedge checks below start from a fresh block.
    c.check(
        run(&[
            "/lemnos_lend_check",
            "/dev/lemnos",
            PISP,
            BIND,
            UNBIND,
            cma_heap(),
            "/dev/dma_heap/system",
        ]) == 0,
        "lemnos_lend_check",
    );
    c.check(
        run(&[
            "/lemnos_pisp_be_check",
            "/dev/lemnos",
            PISP,
            BIND,
            UNBIND,
            cma_heap(),
            "wedge",
        ]) == 0,
        "lemnos_pisp_be_check wedge",
    );
    c.check(
        unload_refused_busy("lemnos"),
        "a wedged block keeps the module loaded",
    );
    // The test block's devices go with it; a wedged block's memory stays pinned regardless.
    c.check(put(UNBIND, PISP) == 0, "wedged PiSP block unbound");
    c.check(
        put(UNBIND, GSDMA) == 0,
        "wedged graphics DMA engine unbound",
    );
    c.check(put(UNBIND, VIP) == 0, "wedged VIP9000 unbound");
    // The stand-in PIO driver stays: the wedged module above depends on it.

    // The USB gadget end to end: the daemon's root phase, FunctionFS on dummy_hcd, and a host.
    let _ = mkdir("/sys/kernel/config");
    if let Err(e) = guest::mount("configfs", "/sys/kernel/config", "configfs")
        && errno_of(&e) != EBUSY
    {
        c.check(false, "configfs mounted");
    }
    let _ = mkdir("/dev/usb-ffs");
    give_module_nodes_to_daemon_group(&mut c);
    give_to_daemon_group(&mut c, cma_heap());
    c.check(run(&["/lemnos-gadget-check"]) == 0, "lemnos_gadget_check");

    // The first flash on raw NAND: the driver's writer on the kernel's own NAND simulator, with its
    // bad blocks, a weak erase block and bit flips injected by the kernel, under the kernel's MTD and
    // UBI.
    c.check(load("/nandsim.ko", NANDSIM) == 0, "NAND simulator loaded");
    c.check(run(&["/lemnos-nand-check"]) == 0, "lemnos-nand-check");
    c.check(unload("nandsim") == 0, "NAND simulator unloaded");

    // The USB link end to end, last because it leaves processes running (the daemon, the bridge,
    // the comm core) that only the power-off ends: the real lemnosd and lemnos-orion on the dummy
    // controller, a comm core, and a host session over the emulated bus.
    c.check(
        run(&["/lemnos-orion-link-check"]) == 0,
        "lemnos_orion_link_check",
    );

    say(&format!(
        "lemnos-qemu: {} ({} failed)",
        if c.failures > 0 { "FAILED" } else { "passed" },
        c.failures
    ));
    let _ = guest::power_off();
    ExitCode::SUCCESS
}
