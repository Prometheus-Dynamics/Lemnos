//! A package written in C against `include/lemnos/package.h`, built with the C compiler and
//! loaded for real: the header is the boundary. The same file, built to misbehave, gives the
//! refusals that need a package nothing in Rust would produce, and files the dynamic loader
//! itself refuses, files too large and files that can't be read give the rest.

mod common;

use common::{c_package, host_on, index, method, new_key, scratch, sign, signature_path, trusted};
use lemnos_core::Status;
use lemnos_host::{DeviceState, Options};
use lemnos_package::{Loader, MAX_PACKAGE_BYTES, Mode, Refusal};
use lemnos_sim::SimBoard;

#[test]
fn a_package_written_in_c_binds_takes_a_line_and_answers() {
    let dir = scratch("c-package");
    let file = c_package(&dir, "c.so", &[]);
    let mut loader = Loader::new(Mode::Open);
    let package = loader.load(&file).unwrap();
    assert_eq!(package.name(), "c-package");
    let manifest = package.manifest();
    assert_eq!(manifest.provides[0].tag.as_str(), "example.c/1");
    let methods = manifest.provides[0].methods;
    assert_eq!((methods[0].id, methods[0].name), (1, "level"));
    assert_eq!(
        (methods[1].name, methods[1].params[0].name),
        ("set", "value")
    );
    assert_eq!(manifest.needs[0].name, "line");

    let sim = SimBoard::new();
    let line = sim.add_line("gpio7");
    let board = sim.clone();
    let devices = r#"
[[devices]]
id = "thing"
driver = "c-package"
uses = { line = "gpio7" }
config = { base = 40 }
"#;
    let (host, _) = host_on(sim, devices, &loader.entries(), Options::default());
    let thing = index(&host, "thing");
    assert_eq!(host.device("thing").unwrap().state, DeviceState::Running);
    assert_eq!(
        host.device("thing").unwrap().tags[0].as_str(),
        "example.c/1"
    );
    // Its bind configured the line through the host's table: an output, low.
    assert_eq!(line.level(), Some(false));

    let mut reply = [0; 4];
    assert_eq!(host.call(thing, method(1), &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 40);
    assert_eq!(host.call(thing, method(2), &[1], &mut reply), Ok(0));
    assert_eq!(line.level(), Some(true));
    assert_eq!(host.call(thing, method(1), &[], &mut reply), Ok(4));
    assert_eq!(u32::from_le_bytes(reply), 41);
    // Its own statuses come back as they are, and the line's errors through it.
    assert_eq!(
        host.call(thing, method(9), &[], &mut reply),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        host.call(thing, method(1), &[], &mut [0; 2]),
        Err(Status::BUFFER_TOO_SMALL)
    );
    line.fail(Some(lemnos_core::ErrorKind::Busy));
    assert_eq!(
        host.call(thing, method(1), &[], &mut reply),
        Err(Status::BUSY)
    );

    // Its drop releases what it took.
    assert!(board.is_claimed("gpio7"));
    host.shutdown();
    assert!(!board.is_claimed("gpio7"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn what_a_package_registers_is_checked_before_it_is_used() {
    let dir = scratch("c-refused");
    let mut loader = Loader::new(Mode::Open);
    let mut refused = |name: &str, flag: &str| {
        loader
            .load(&c_package(&dir, name, &[flag]))
            .map(|package| package.name())
            .unwrap_err()
    };
    assert_eq!(
        refused("fails.so", "-DFAIL_REGISTRATION"),
        Refusal::RegistrationFailed(Status::NO_MEMORY)
    );
    assert_eq!(
        refused("none.so", "-DNO_DRIVER"),
        Refusal::BadManifest("it did not register exactly one driver")
    );
    assert_eq!(
        refused("nameless.so", "-DNO_NAME"),
        Refusal::BadManifest("the driver has no name")
    );
    assert_eq!(
        refused("tag.so", "-DBAD_TAG"),
        Refusal::BadManifest("a contract tag is not name/major")
    );
    assert_eq!(
        refused("need.so", "-DBAD_NEED"),
        Refusal::BadManifest("a need of a kind this host does not know")
    );
    // None of them took the name, and none left a driver behind.
    assert!(loader.entries().is_empty());
    assert_eq!(
        loader
            .load(&c_package(&dir, "good.so", &[]))
            .map(|p| p.name()),
        Ok("c-package")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_package_the_dynamic_loader_cannot_load_is_refused_in_its_words() {
    let dir = scratch("c-unloadable");
    // A library the package needs, there when it is linked and gone when it is loaded.
    let source = dir.join("gone.c");
    std::fs::write(&source, "int lemnos_test_gone(void) { return 1; }\n").unwrap();
    let library = dir.join("libgone.so");
    let built = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&library)
        .arg(&source)
        .status()
        .unwrap();
    assert!(built.success());
    let link = format!("-L{}", dir.display());
    let file = c_package(
        &dir,
        "needs-gone.so",
        &[&link, "-Wl,--no-as-needed", "-lgone"],
    );
    std::fs::remove_file(&library).unwrap();

    let mut loader = Loader::new(Mode::Open);
    // The file itself is a whole package for this machine: every check before the loader
    // passes, and the loader is the one that refuses.
    let Err(Refusal::LoadFailed(why)) = loader.load(&file).map(|package| package.name()) else {
        panic!("the loader took a package whose library is missing");
    };
    assert!(why.contains("libgone.so"), "{why}");
    assert!(loader.entries().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_too_large_or_unreadable_are_refused_without_being_looked_at() {
    let dir = scratch("unreadable");
    let mut loader = Loader::new(Mode::Open);
    let mut refused =
        |path: &std::path::Path| loader.load(path).map(|package| package.name()).unwrap_err();

    // One byte over the limit. The file has no blocks: only its length is large.
    let large = dir.join("large.so");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(MAX_PACKAGE_BYTES + 1)
        .unwrap();
    assert_eq!(refused(&large), Refusal::TooLarge);
    // At the limit it is read, and is then simply not a package.
    std::fs::File::create(&large)
        .unwrap()
        .set_len(MAX_PACKAGE_BYTES)
        .unwrap();
    assert_eq!(refused(&large), Refusal::NotAPackage("not a shared object"));
    std::fs::remove_file(&large).unwrap();

    // Not there, and not a file.
    let Refusal::Unreadable(why) = refused(&dir.join("missing.so")) else {
        panic!("a missing file was read");
    };
    assert!(why.contains("No such file"), "{why}");
    let folder = dir.join("folder.so");
    std::fs::create_dir(&folder).unwrap();
    assert!(matches!(refused(&folder), Refusal::Unreadable(_)));

    // Signed mode: a signature that is there and can't be read is not "unsigned".
    let key = new_key();
    let file = c_package(&dir, "signed.so", &[]);
    std::fs::create_dir(signature_path(&file)).unwrap();
    let mut signed = Loader::new(Mode::Signed(trusted(&[&key])));
    assert!(matches!(
        signed.load(&file).map(|package| package.name()),
        Err(Refusal::Unreadable(_))
    ));
    std::fs::remove_dir(signature_path(&file)).unwrap();
    sign(&file, &key);
    assert_eq!(signed.load(&file).map(|p| p.name()), Ok("c-package"));

    // In a scan, each is recorded with its reason and the rest still load.
    let scanned = scratch("unreadable-scan");
    std::fs::File::create(scanned.join("a-large.so"))
        .unwrap()
        .set_len(MAX_PACKAGE_BYTES + 1)
        .unwrap();
    std::os::unix::fs::symlink(scanned.join("nowhere"), scanned.join("b-dangling.so")).unwrap();
    std::fs::copy(&file, scanned.join("c-good.so")).unwrap();
    let mut loader = Loader::new(Mode::Open);
    loader.scan(&scanned).unwrap();
    assert_eq!(loader.packages().len(), 1);
    assert_eq!(loader.refused().len(), 1, "{:?}", loader.refused());
    assert_eq!(loader.refused()[0].why, Refusal::TooLarge);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(scanned);
}

#[test]
fn a_c_package_gives_its_items_limits_readings_and_kinds() {
    use lemnos_core::{Item, ItemKind, MethodId, Reading, Unit};
    let dir = scratch("c-package-extras");
    let file = c_package(&dir, "extras.so", &["-DWITH_EXTRAS"]);
    let mut loader = Loader::new(Mode::Open);
    let package = loader.load(&file).unwrap();
    // The manifest: the contract's kind, and the method's reading, with no driver bound.
    let contract = &package.manifest().provides[0];
    assert_eq!(contract.kind, "sensor");
    assert_eq!(contract.methods[0].reading, Some(Reading::Switch));
    assert_eq!(contract.methods[1].reading, None);

    let sim = SimBoard::new();
    let line = sim.add_line("gpio7");
    let devices = r#"
[[devices]]
id = "thing"
driver = "c-package"
uses = { line = "gpio7" }
config = { base = 40 }
"#;
    let (host, _) = host_on(sim, devices, &loader.entries(), Options::default());
    let description = host.describe(index(&host, "thing")).unwrap();
    let level = description
        .find("level")
        .expect("the item the package describes");
    // What the built-in driver would describe, field for field.
    let built_in = Item {
        kind: ItemKind::Value,
        name: "level",
        unit: Unit::One,
        exponent: 0,
        min: Some(0),
        max: Some(100),
        step: Some(5),
        warn_low: Some(10),
        warn_high: Some(90),
        reading: Some(Reading::Range),
        group: Some("grp"),
        method: MethodId {
            contract: 0,
            method: 1,
        },
        field: None,
    };
    assert_eq!(*level, built_in);
    let _ = line;
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_package_built_before_the_extras_loads_and_its_items_read_unknown() {
    use lemnos_core::{MethodId, Reading};
    let dir = scratch("c-package-base");
    let file = common::c_package_base(&dir, "base.so", &["-DWITH_EXTRAS"]);
    let mut loader = Loader::new(Mode::Open);
    let package = loader.load(&file).unwrap();
    let contract = &package.manifest().provides[0];
    assert_eq!(contract.kind, "other");
    assert_eq!(contract.methods[0].reading, None::<Reading>);

    let sim = SimBoard::new();
    let _line = sim.add_line("gpio7");
    let devices = r#"
[[devices]]
id = "thing"
driver = "c-package"
uses = { line = "gpio7" }
config = { base = 40 }
"#;
    let (host, _) = host_on(sim, devices, &loader.entries(), Options::default());
    let description = host.describe(index(&host, "thing")).unwrap();
    let level = description.find("level").unwrap();
    // Its own record gives the limits; the reading, step, warning limits and group are unknown.
    assert_eq!((level.min, level.max), (Some(0), Some(100)));
    assert_eq!(
        (
            level.reading,
            level.step,
            level.warn_low,
            level.warn_high,
            level.group
        ),
        (None, None, None, None, None)
    );
    assert_eq!(
        level.method,
        MethodId {
            contract: 0,
            method: 1
        }
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_package_with_a_boundary_this_host_does_not_know_is_refused() {
    let dir = scratch("c-package-future");
    let file = c_package(&dir, "future.so", &["-DFUTURE_LAYOUT"]);
    let mut loader = Loader::new(Mode::Open);
    assert!(matches!(loader.load(&file), Err(Refusal::LayoutMismatch)));
    let _ = std::fs::remove_dir_all(dir);
}
