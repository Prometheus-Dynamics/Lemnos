//! A real compiled package, loaded by path and by scan, open and signed, and run by the host:
//! every refusal, and a package's panics and hangs contained like a built-in driver's.

mod common;

use std::time::Duration;

use common::{
    TWO, count, descriptor_at, fixtures, good_bytes, host, host_on, index, method, new_key,
    scratch, sign, signature_path, trusted, write_changed,
};
use lemnos_core::{Health, ItemKind, Status};
use lemnos_host::{DeviceState, Options};
use lemnos_package::{Loader, Mode, Refusal, TrustedKeys};
use lemnos_sim::Layout;
use lemnos_sim::SimBoard;

fn open_loader() -> Loader {
    Loader::new(Mode::Open)
}

/// The good fixture, loaded on its own.
fn loaded() -> Loader {
    let mut loader = open_loader();
    loader.load(&fixtures().good).unwrap();
    loader
}

fn refusal(loader: &Loader, file: &str) -> Refusal {
    loader
        .refused()
        .iter()
        .find(|refused| refused.path.file_name().unwrap() == file)
        .unwrap_or_else(|| panic!("{file} was not refused"))
        .why
        .clone()
}

#[test]
fn a_package_loaded_by_path_answers_through_the_host() {
    let mut loader = open_loader();
    let package = loader.load(&fixtures().good).unwrap();
    assert_eq!(package.name(), "package-fixture");
    assert_eq!(
        package.manifest().provides[0].tag.as_str(),
        "example.package/1"
    );
    let add = &package.manifest().provides[0].methods[1];
    assert_eq!((add.id, add.name, add.params.len()), (2, "add", 2));
    assert_eq!(package.manifest().needs.len(), 15);
    assert!(package.manifest().needs.iter().all(|need| need.optional));

    let (host, _) = host(TWO, &loader.entries());
    let good = index(&host, "good");
    assert_eq!(host.device("good").unwrap().state, DeviceState::Running);
    assert_eq!(host.device("good").unwrap().health, Health::Ok);
    assert_eq!(
        host.device("good").unwrap().tags[0].as_str(),
        "example.package/1"
    );

    // The arguments go in and the right value comes back.
    let mut args = Vec::new();
    args.extend_from_slice(&40u32.to_le_bytes());
    args.extend_from_slice(&2u32.to_le_bytes());
    let mut reply = [0; 4];
    assert_eq!(host.call(good, method(2), &args, &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 42);
    // The driver's own error comes back as it is.
    let mut overflow = Vec::new();
    overflow.extend_from_slice(&u32::MAX.to_le_bytes());
    overflow.extend_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        host.call(good, method(2), &overflow, &mut reply),
        Err(Status::INVALID_ARGUMENT)
    );
    // It keeps its state between calls, and each device has its own.
    assert_eq!(count(&host, good), Ok(1));
    assert_eq!(count(&host, good), Ok(2));
    assert_eq!(count(&host, index(&host, "bad")), Ok(1));
    // A reply that does not fit is refused, not cut short.
    assert_eq!(
        host.call(good, method(1), &[], &mut [0; 2]),
        Err(Status::BUFFER_TOO_SMALL)
    );

    // It describes itself, and the host ticks it: once at the start and after each method.
    let description = host.describe(good).unwrap();
    let item = description.find("count").unwrap();
    assert_eq!(
        (item.kind, item.method, item.max),
        (ItemKind::Value, method(1), Some(i64::from(u32::MAX)))
    );
    assert_eq!(description.find("crash").unwrap().kind, ItemKind::Action);
    host.run_ticks();
    assert_eq!(host.call(good, method(9), &[], &mut reply), Ok(4));
    assert!(u32::from_le_bytes(reply) >= 5, "{reply:?}");
}

#[test]
fn a_scan_loads_what_it_can_and_says_why_it_refused_the_rest() {
    let dir = scratch("scan");
    let good = good_bytes();
    let at = descriptor_at(&good);
    std::fs::write(dir.join("a-good.so"), &good).unwrap();
    // Same driver name as the one before it.
    std::fs::write(dir.join("b-again.so"), &good).unwrap();
    std::fs::write(dir.join("c-text.so"), "not a shared object at all").unwrap();
    std::fs::write(dir.join("d-cut.so"), &good[..good.len() / 2]).unwrap();
    std::fs::write(dir.join("e-header-only.so"), &good[..32]).unwrap();
    write_changed(&dir.join("f-magic.so"), |bytes| bytes[at] ^= 0xff);
    write_changed(&dir.join("g-major.so"), |bytes| bytes[at + 12] += 1);
    write_changed(&dir.join("h-minor.so"), |bytes| bytes[at + 14] += 1);
    write_changed(&dir.join("i-layout.so"), |bytes| bytes[at + 16] ^= 0xff);
    // A shared object that exports something else: the entry's name, one letter off.
    write_changed(&dir.join("j-no-entry.so"), |bytes| {
        let name = b"lemnos_package\0";
        let found = bytes.windows(name.len()).position(|w| w == name).unwrap();
        bytes[found] = b'L';
    });
    std::fs::copy(&fixtures().panics_registering, dir.join("k-panics.so")).unwrap();
    // Built for a machine this is not: the header's machine field, changed.
    write_changed(&dir.join("l-machine.so"), |bytes| bytes[18] ^= 0x55);
    // Not looked at: the name does not end in `.so`.
    std::fs::write(dir.join("notes.txt"), "nothing").unwrap();
    std::fs::create_dir(dir.join("inner.so")).unwrap();

    let mut loader = open_loader();
    loader.scan(&dir).unwrap();

    assert_eq!(loader.packages().len(), 1);
    assert_eq!(loader.packages()[0].path(), dir.join("a-good.so"));
    assert_eq!(loader.refused().len(), 11, "{:#?}", loader.refused());
    assert_eq!(
        refusal(&loader, "b-again.so"),
        Refusal::DuplicateName("package-fixture".into())
    );
    assert_eq!(
        refusal(&loader, "c-text.so"),
        Refusal::NotAPackage("not a shared object")
    );
    assert_eq!(refusal(&loader, "d-cut.so"), Refusal::Truncated);
    assert_eq!(refusal(&loader, "e-header-only.so"), Refusal::Truncated);
    assert_eq!(
        refusal(&loader, "f-magic.so"),
        Refusal::NotAPackage("its entry lacks the package magic")
    );
    assert_eq!(
        refusal(&loader, "g-major.so"),
        Refusal::AbiMismatch {
            package: (3, 0),
            host: (2, 0)
        }
    );
    assert_eq!(
        refusal(&loader, "h-minor.so"),
        Refusal::AbiMismatch {
            package: (2, 1),
            host: (2, 0)
        }
    );
    assert_eq!(refusal(&loader, "i-layout.so"), Refusal::LayoutMismatch);
    assert_eq!(
        refusal(&loader, "j-no-entry.so"),
        Refusal::NotAPackage("it has no package entry")
    );
    assert_eq!(
        refusal(&loader, "k-panics.so"),
        Refusal::RegistrationPanicked
    );
    assert_eq!(
        refusal(&loader, "l-machine.so"),
        Refusal::NotAPackage("built for another kind of machine")
    );
    // Each reason reads differently.
    let mut texts: Vec<String> = loader.refused().iter().map(|r| r.why.to_string()).collect();
    texts.sort();
    texts.dedup();
    assert_eq!(texts.len(), 10, "{texts:#?}");

    // The one that loaded works, whatever was beside it.
    let (host, _) = host(TWO, &loader.entries());
    assert_eq!(count(&host, index(&host, "good")), Ok(1));

    // A directory that is not there is the scan's own error, and loads nothing.
    let mut loader = open_loader();
    assert!(loader.scan(&dir.join("missing")).is_err());
    assert!(loader.packages().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_built_in_name_wins_over_a_package() {
    let mut loader = open_loader();
    loader.reserve(["package-fixture"]);
    assert_eq!(
        loader.load(&fixtures().good).unwrap_err(),
        Refusal::DuplicateName("package-fixture".into())
    );
    assert!(loader.entries().is_empty());
}

#[test]
fn signed_mode_runs_only_what_a_trusted_key_signed() {
    let dir = scratch("signed");
    let (key, stranger) = (new_key(), new_key());
    let good = good_bytes();

    let signed = dir.join("a-signed.so");
    std::fs::write(&signed, &good).unwrap();
    sign(&signed, &key);

    std::fs::write(dir.join("b-unsigned.so"), &good).unwrap();

    // Signed, then one byte changed: the last one, which no format check looks at.
    let changed = dir.join("c-changed.so");
    std::fs::write(&changed, &good).unwrap();
    sign(&changed, &key);
    write_changed(&changed, |bytes| *bytes.last_mut().unwrap() ^= 1);

    let other = dir.join("d-stranger.so");
    std::fs::write(&other, &good).unwrap();
    sign(&other, &stranger);

    let short = dir.join("e-short-signature.so");
    std::fs::write(&short, &good).unwrap();
    std::fs::write(signature_path(&short), [0; 12]).unwrap();

    // Properly signed, and not a package: the signature says who, not what.
    let cut = dir.join("f-signed-cut.so");
    std::fs::write(&cut, &good[..good.len() / 2]).unwrap();
    sign(&cut, &key);

    let mut loader = Loader::new(Mode::Signed(trusted(&[&key])));
    loader.scan(&dir).unwrap();
    assert_eq!(loader.packages().len(), 1, "{:#?}", loader.refused());
    assert_eq!(loader.packages()[0].path(), signed);
    assert_eq!(refusal(&loader, "b-unsigned.so"), Refusal::Unsigned);
    assert_eq!(refusal(&loader, "c-changed.so"), Refusal::BadSignature);
    assert_eq!(refusal(&loader, "d-stranger.so"), Refusal::BadSignature);
    assert_eq!(
        refusal(&loader, "e-short-signature.so"),
        Refusal::BadSignature
    );
    assert_eq!(refusal(&loader, "f-signed-cut.so"), Refusal::Truncated);

    let (host, _) = host(TWO, &loader.entries());
    assert_eq!(count(&host, index(&host, "good")), Ok(1));

    // By path, the same rules.
    let mut by_path = Loader::new(Mode::Signed(trusted(&[&stranger, &key])));
    assert_eq!(by_path.load(&changed).unwrap_err(), Refusal::BadSignature);
    assert_eq!(
        by_path.load(&dir.join("b-unsigned.so")).unwrap_err(),
        Refusal::Unsigned
    );
    assert_eq!(by_path.load(&signed).unwrap().name(), "package-fixture");

    // With no trusted key, nothing runs, however well it is signed.
    let mut nobody = Loader::new(Mode::Signed(TrustedKeys::default()));
    assert_eq!(nobody.load(&signed).unwrap_err(), Refusal::BadSignature);

    // Open mode is the same loader with the other switch: it runs the unsigned one.
    let mut open = open_loader();
    assert!(open.load(&dir.join("b-unsigned.so")).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn trusted_keys_come_from_a_key_file() {
    let key = new_key();
    let hex: String = key
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let keys = TrustedKeys::parse(&format!("# the build machine\n\n{hex}\n")).unwrap();
    assert_eq!(keys.len(), 1);
    assert!(TrustedKeys::parse("# nobody\n").unwrap().is_empty());
    assert!(TrustedKeys::parse("abcd\n").is_err());
    assert!(TrustedKeys::parse(&format!("{}\n", "zz".repeat(32))).is_err());
    assert!(TrustedKeys::from_file(std::path::Path::new("/nonexistent/keys")).is_err());
}

#[test]
fn a_package_that_panics_in_bind_is_contained_and_retried_with_the_normal_backoff() {
    let loader = loaded();
    let devices = r#"
[[devices]]
id = "bad"
driver = "package-fixture"
config = { panic_binds = 2 }

[[devices]]
id = "good"
driver = "package-fixture"
"#;
    let (host, clock) = host(devices, &loader.entries());
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));
    let second = Duration::from_secs(1);

    // The first bind panicked inside the package. Its neighbour never noticed.
    let DeviceState::Backoff {
        attempts: 1,
        next_retry_ns,
        reason,
    } = host.device("bad").unwrap().state
    else {
        panic!("{:?}", host.device("bad").unwrap().state);
    };
    assert_eq!(next_retry_ns, second.as_nanos() as u64);
    assert_eq!(reason, format!("bind failed: {:?}", Status::PANIC));
    assert_eq!(host.device("good").unwrap().state, DeviceState::Running);
    assert_eq!(count(&host, good), Ok(1));
    assert_eq!(count(&host, bad), Err(Status::NO_DEVICE));

    // The retry comes after one second and panics again: the next wait is twice as long.
    clock.advance(second);
    host.tick();
    assert!(matches!(
        host.device("bad").unwrap().state,
        DeviceState::Backoff { attempts: 2, next_retry_ns, .. } if next_retry_ns == 3 * second.as_nanos() as u64
    ));
    clock.advance(second);
    host.tick();
    assert!(matches!(
        host.device("bad").unwrap().state,
        DeviceState::Backoff { attempts: 2, .. }
    ));

    // The third bind works, and the device is as good as new.
    clock.advance(second);
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(count(&host, bad), Ok(1));
    assert_eq!(count(&host, good), Ok(2));
}

#[test]
fn a_panic_in_a_package_is_contained_and_retried() {
    let loader = loaded();
    let (host, clock) = host(TWO, &loader.entries());
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));
    assert_eq!(count(&host, bad), Ok(1));

    assert_eq!(
        host.call(bad, method(3), &[], &mut [0; 8]),
        Err(Status::PANIC)
    );
    assert_eq!(count(&host, good), Ok(1));
    let DeviceState::Backoff {
        attempts: 1,
        reason,
        ..
    } = host.device("bad").unwrap().state
    else {
        panic!("{:?}", host.device("bad").unwrap().state);
    };
    assert_eq!(reason, "the driver panicked");
    assert_eq!(count(&host, bad), Err(Status::NO_DEVICE));

    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    // A new driver: the old one's state went with it.
    assert_eq!(count(&host, bad), Ok(1));
}

#[test]
fn a_hang_in_a_package_returns_on_time_and_leaves_its_neighbour_running() {
    let loader = loaded();
    let quick = Options {
        call_deadline: Duration::from_millis(100),
        ..Options::default()
    };
    let (host, clock) = host_on(SimBoard::new(), TWO, &loader.entries(), quick);
    let (bad, good) = (index(&host, "bad"), index(&host, "good"));

    // The call gives up at its deadline, long before the driver comes back.
    let started = std::time::Instant::now();
    assert_eq!(
        host.call(bad, method(4), &1000u32.to_le_bytes(), &mut [0; 8]),
        Err(Status::TIMED_OUT)
    );
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(count(&host, good), Ok(1));
    assert!(matches!(
        host.device("bad").unwrap().health,
        Health::Failed(_)
    ));
    assert!(matches!(
        host.device("bad").unwrap().state,
        DeviceState::Backoff { attempts: 1, .. }
    ));
    assert_eq!(count(&host, bad), Err(Status::NO_DEVICE));

    // Once the stuck call has come back, the retry binds a new driver.
    std::thread::sleep(Duration::from_millis(1200));
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(count(&host, bad), Ok(1));
}

#[test]
fn a_package_that_says_it_is_gone_is_bound_again() {
    let loader = loaded();
    let (host, clock) = host(TWO, &loader.entries());
    let bad = index(&host, "bad");
    assert_eq!(count(&host, bad), Ok(1));

    // The call's own answer stands, and the driver goes.
    assert_eq!(host.call(bad, method(8), &[], &mut [0; 8]), Ok(0));
    let DeviceState::Backoff { reason, .. } = host.device("bad").unwrap().state else {
        panic!("{:?}", host.device("bad").unwrap().state);
    };
    assert_eq!(reason, "asked to leave");

    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(host.device("bad").unwrap().state, DeviceState::Running);
    assert_eq!(count(&host, bad), Ok(1));
}

#[test]
fn a_package_reaches_another_device_and_a_bus_through_the_host() {
    let loader = loaded();
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c0");
    bus.attach(0x48, Layout::Bytes);
    bus.set_bytes(0x48, 0x10, &[0xa5]);
    let devices = r#"
[[devices]]
id = "user"
driver = "package-fixture"
uses = { peer = "chip" }

[[devices]]
id = "chip"
driver = "package-fixture"
uses = { bus = "i2c0" }
config = { start = 6 }
"#;
    let (host, _) = host_on(sim, devices, &loader.entries(), Options::default());
    let (user, chip) = (index(&host, "user"), index(&host, "chip"));
    assert_eq!(host.device("user").unwrap().state, DeviceState::Running);
    assert_eq!(host.device("chip").unwrap().state, DeviceState::Running);

    // One package's driver calls another device, which is a package's driver too.
    let mut reply = [0; 4];
    assert_eq!(host.call(user, method(6), &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 7);
    assert_eq!(count(&host, chip), Ok(8));
    // A device with no peer says so.
    assert_eq!(
        host.call(chip, method(6), &[], &mut reply),
        Err(Status::NOT_FOUND)
    );

    // A register read goes out through the host's bus and the byte comes back.
    let before = bus.transactions();
    let mut byte = [0; 1];
    assert_eq!(host.call(chip, method(7), &[0x48, 0x10], &mut byte), Ok(1));
    assert_eq!(byte, [0xa5]);
    assert_eq!(bus.transactions(), before + 1);
    // A chip that is not there fails as it does for a built-in driver.
    assert_eq!(
        host.call(chip, method(7), &[0x21, 0x10], &mut byte),
        Err(Status::IO)
    );
    // A device with no bus says so.
    assert_eq!(
        host.call(user, method(7), &[0x48, 0x10], &mut byte),
        Err(Status::NOT_FOUND)
    );

    // The user's health follows the device it uses, which it can do without. The driver sees
    // the same through its handle.
    assert_eq!(host.device("user").unwrap().health, Health::Ok);
    assert_eq!(host.call(user, method(11), &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 0);
    host.off("chip").unwrap();
    assert_eq!(host.call(user, method(11), &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 2, "the device it uses is off");
    assert!(matches!(
        host.device("user").unwrap().health,
        Health::Degraded(_)
    ));
    assert_eq!(
        host.call(user, method(6), &[], &mut reply),
        Err(Status::NO_DEVICE)
    );
}
