//! Driver packages in the service: off by default, signed unless asked otherwise, and never in
//! the way of the built-in drivers. The loader's own tests, in `engine/package`, load a real
//! package; these cover what the service adds.

use std::path::PathBuf;

use lemnosd::builtin;
use lemnosd::packages::{PackageConfig, add_packages};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnosd-packages-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn packages_are_off_unless_a_directory_is_configured() {
    let mut drivers = builtin::drivers();
    let built_in = drivers.len();
    let config = PackageConfig::default();
    assert_eq!((config.dir.as_ref(), config.open), (None, false));
    assert!(add_packages(&mut drivers, &config).is_empty());
    assert_eq!(drivers.len(), built_in);

    // Keys and a mode with no directory are still off.
    let config = PackageConfig {
        dir: None,
        keys: Some("/nonexistent/keys".into()),
        open: true,
    };
    assert!(add_packages(&mut drivers, &config).is_empty());
    assert_eq!(drivers.len(), built_in);
}

#[test]
fn signed_mode_without_usable_keys_loads_nothing_and_says_so() {
    let dir = scratch("keys");
    std::fs::write(dir.join("driver.so"), "anything").unwrap();
    let mut drivers = builtin::drivers();
    let built_in = drivers.len();

    let no_keys = PackageConfig {
        dir: Some(dir.clone()),
        keys: None,
        open: false,
    };
    let said = add_packages(&mut drivers, &no_keys);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("signed mode needs a key file"), "{said:?}");

    let bad_keys = PackageConfig {
        keys: Some(dir.join("missing-keys")),
        ..no_keys.clone()
    };
    let said = add_packages(&mut drivers, &bad_keys);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("not used"), "{said:?}");

    std::fs::write(dir.join("keys"), "not a key\n").unwrap();
    let malformed = PackageConfig {
        keys: Some(dir.join("keys")),
        ..no_keys
    };
    let said = add_packages(&mut drivers, &malformed);
    assert!(said[0].contains("not 64 hex digits"), "{said:?}");
    assert_eq!(drivers.len(), built_in);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_is_no_package_is_refused_and_the_built_in_drivers_stay() {
    let dir = scratch("refused");
    std::fs::write(dir.join("driver.so"), "anything").unwrap();
    // A well-formed key: nothing here is signed by it.
    std::fs::write(dir.join("keys"), format!("58{}\n", "66".repeat(31))).unwrap();
    let mut drivers = builtin::drivers();
    let built_in = drivers.len();

    let open = PackageConfig {
        dir: Some(dir.clone()),
        keys: None,
        open: true,
    };
    let said = add_packages(&mut drivers, &open);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].ends_with("driver.so refused: not a driver package: not a shared object"));

    // Signed mode refuses it sooner: it has no signature.
    let signed = PackageConfig {
        keys: Some(dir.join("keys")),
        open: false,
        ..open.clone()
    };
    let said = add_packages(&mut drivers, &signed);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("refused: unsigned"), "{said:?}");

    // A directory that is not there is said once, and is nobody's failure.
    let missing = PackageConfig {
        dir: Some(dir.join("missing")),
        ..open
    };
    let said = add_packages(&mut drivers, &missing);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(drivers.len(), built_in);
    let _ = std::fs::remove_dir_all(dir);
}
