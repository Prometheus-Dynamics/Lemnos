//! The real daemon, started as a process with a directory of real driver packages: what it
//! loads runs, what it refuses is said on its error output and to a client that asks, and
//! neither stops it.
//!
//! The packages are built here: the loader's fixture crate with cargo, and the package written
//! in C with the C compiler, once as it is and once with no driver name.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use lemnos_client::{Client, DeviceState, Error, PackageEntry, PackageMode};
use lemnos_core::Status;

const PATIENCE: Duration = Duration::from_secs(30);
const TAG: &str = "example.package/1";
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "t"

[[devices]]
id = "thing"
driver = "package-fixture"
config = { start = 10 }

[[devices]]
id = "pin"
driver = "c-package"
config = { base = 7 }
"#;

fn eventually<T>(mut done: impl FnMut() -> Option<T>, what: &str) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = done() {
            return value;
        }
        assert!(start.elapsed() < PATIENCE, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The loader's fixture package, built with cargo into the workspace's target directory.
fn fixture() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let target = exe.ancestors().nth(3).unwrap().join("package-fixture");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .arg("build")
            .arg("--manifest-path")
            .arg(repository().join("engine/package/tests/fixture/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target);
        for outer in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_TARGET_DIR"] {
            command.env_remove(outer);
        }
        let output = command.output().expect("cargo runs");
        assert!(
            output.status.success(),
            "the fixture package did not build:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let kept = target.join("for-the-daemon.so");
        std::fs::copy(target.join("debug/liblemnos_package_fixture.so"), &kept).unwrap();
        kept
    })
}

/// The package written in C, built with `flags` added.
fn c_package(out: &Path, flags: &[&str]) {
    let output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-shared", "-fPIC", "-I"])
        .arg(repository().join("include"))
        .arg(repository().join("engine/package/tests/c-package/package.c"))
        .args(flags)
        .arg("-o")
        .arg(out)
        .output()
        .expect("the C compiler runs");
    assert!(
        output.status.success(),
        "cc: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sign(path: &Path, key: &SigningKey) {
    let signature = key.sign(&std::fs::read(path).unwrap());
    let mut name = path.as_os_str().to_owned();
    name.push(".sig");
    std::fs::write(name, signature.to_bytes()).unwrap();
}

/// A directory of packages: one that loads, one written in C that loads, one file that is no
/// package, and one whose driver has no name. The first, second and last are signed by `key`.
struct Packages {
    dir: PathBuf,
    keys: PathBuf,
}

impl Packages {
    fn new(name: &str, key: &SigningKey) -> Self {
        let root =
            std::env::temp_dir().join(format!("lemnosd-daemon-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("packages");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fixture(), dir.join("a-fixture.so")).unwrap();
        c_package(&dir.join("b-in-c.so"), &[]);
        std::fs::write(dir.join("c-junk.so"), "not a shared object").unwrap();
        c_package(&dir.join("d-nameless.so"), &["-DNO_NAME"]);
        for signed in ["a-fixture.so", "b-in-c.so", "d-nameless.so"] {
            sign(&dir.join(signed), key);
        }
        let keys = root.join("keys");
        let hex: String = key
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::write(&keys, format!("# the test's key\n{hex}\n")).unwrap();
        std::fs::write(root.join("board.toml"), BOARD).unwrap();
        Self { dir, keys }
    }

    fn root(&self) -> &Path {
        self.dir.parent().unwrap()
    }

    fn path(&self, file: &str) -> String {
        self.dir.join(file).display().to_string()
    }
}

impl Drop for Packages {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root());
    }
}

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(packages: &Packages, args: &[&str]) -> Self {
        let socket = packages.root().join("sock");
        let child = Command::new(env!("CARGO_BIN_EXE_lemnosd"))
            .arg("--sim")
            .arg("--board")
            .arg(packages.root().join("board.toml"))
            .arg("--socket")
            .arg(&socket)
            .args(args)
            .env_remove("LEMNOSD_PACKAGES")
            .env_remove("LEMNOSD_PACKAGE_KEYS")
            .env_remove("LEMNOSD_PACKAGE_MODE")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        eventually(
            || UnixStream::connect(&socket).ok(),
            "the daemon never listened",
        );
        Self { child, socket }
    }

    fn client(&self) -> Client {
        let client = Client::connect(&self.socket, "test").unwrap();
        client.set_timeout(Duration::from_secs(10));
        client
    }

    /// Stops the daemon as an init system does and returns what it wrote to its error output.
    fn stop(mut self) -> String {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = eventually(
            || self.child.try_wait().unwrap(),
            "the daemon ignored the signal",
        );
        assert!(status.success(), "{status}");
        let mut said = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut said)
            .unwrap();
        assert!(!self.socket.exists(), "the socket was left behind");
        said
    }
}

fn state(client: &Client, device: &str) -> (DeviceState, Vec<String>, String) {
    let entry = client
        .list()
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == device)
        .unwrap();
    (entry.state, entry.tags, entry.reason)
}

fn loaded(path: String, driver: &str) -> PackageEntry {
    PackageEntry {
        path,
        driver: Some(driver.to_string()),
        reason: String::new(),
    }
}

fn refused(path: String, reason: &str) -> PackageEntry {
    PackageEntry {
        path,
        driver: None,
        reason: reason.to_string(),
    }
}

/// The fixture's `add`, over the socket.
fn add(client: &Client, a: u32, b: u32) -> Result<u32, Error> {
    let mut handle = client.open_tag("thing", TAG)?;
    let mut args = a.to_le_bytes().to_vec();
    args.extend(b.to_le_bytes());
    let mut reply = [0; 4];
    client.call(&mut handle, 2, &args, &mut reply)?;
    Ok(u32::from_le_bytes(reply))
}

#[test]
fn the_daemon_runs_the_packages_it_loads_and_says_what_it_refused() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let packages = Packages::new("open", &key);
    let dir = packages.dir.display().to_string();
    let daemon = Daemon::start(&packages, &["--packages", &dir, "--package-mode", "open"]);
    let client = daemon.client();

    // What it tells a client that asks: every file, loaded first, each refusal with its reason.
    let list = client.packages().unwrap();
    assert_eq!(list.mode, PackageMode::Open);
    assert_eq!(list.note, "");
    assert_eq!(
        list.packages,
        [
            loaded(packages.path("a-fixture.so"), "package-fixture"),
            loaded(packages.path("b-in-c.so"), "c-package"),
            refused(
                packages.path("c-junk.so"),
                "not a driver package: not a shared object"
            ),
            refused(
                packages.path("d-nameless.so"),
                "bad manifest: the driver has no name"
            ),
        ]
    );

    // Both loaded drivers run: the one built from Rust and the one written in C.
    let (running, tags, _) = state(&client, "thing");
    assert_eq!(
        (running, tags),
        (DeviceState::Running, vec![TAG.to_string()])
    );
    assert_eq!(add(&client, 40, 2), Ok(42));
    let mut count = client.open_tag("thing", TAG).unwrap();
    let mut reply = [0; 4];
    client.call(&mut count, 1, &[], &mut reply).unwrap();
    assert_eq!(u32::from_le_bytes(reply), 11, "its settings arrived");
    let (running, tags, _) = state(&client, "pin");
    assert_eq!(
        (running, tags),
        (DeviceState::Running, vec!["example.c/1".to_string()])
    );
    // The simulated board the daemon starts with has no line for it, so it runs without one.
    let mut pin = client.open_tag("pin", "example.c/1").unwrap();
    client.call(&mut pin, 1, &[], &mut reply).unwrap();
    assert_eq!(u32::from_le_bytes(reply), 7, "its setting arrived");
    assert!(matches!(
        client.call(&mut pin, 2, &[1], &mut reply),
        Err(Error::Refused {
            status: Status::NO_DEVICE,
            ..
        })
    ));

    // A panic inside a package is the device's failure, not the daemon's: the call is refused,
    // the neighbour answers, and the device is bound again by itself.
    let mut thing = client.open_tag("thing", TAG).unwrap();
    assert!(matches!(
        client.call(&mut thing, 3, &[], &mut reply),
        Err(Error::Refused {
            status: Status::PANIC,
            ..
        })
    ));
    client.call(&mut pin, 1, &[], &mut reply).unwrap();
    let (down, _, reason) = state(&client, "thing");
    assert_eq!(
        (down, reason.as_str()),
        (DeviceState::BackingOff, "the driver panicked")
    );
    eventually(
        || (state(&client, "thing").0 == DeviceState::Running).then_some(()),
        "the device was never bound again",
    );
    assert_eq!(add(&client, 1, 2), Ok(3));

    drop(client);
    let said = daemon.stop();
    for line in [
        format!(
            "lemnosd: package {}: driver \"package-fixture\"",
            packages.path("a-fixture.so")
        ),
        format!(
            "lemnosd: package {}: driver \"c-package\"",
            packages.path("b-in-c.so")
        ),
        format!(
            "lemnosd: package {} refused: not a driver package: not a shared object",
            packages.path("c-junk.so")
        ),
        format!(
            "lemnosd: package {} refused: bad manifest: the driver has no name",
            packages.path("d-nameless.so")
        ),
    ] {
        assert!(said.lines().any(|said| said == line), "{line}\nin:\n{said}");
    }
}

#[test]
fn the_daemon_in_signed_mode_runs_only_what_its_keys_signed() {
    let key = SigningKey::from_bytes(&[9; 32]);
    let packages = Packages::new("signed", &key);
    let dir = packages.dir.display().to_string();
    let keys = packages.keys.display().to_string();
    // One byte added to the C package after it was signed.
    let changed = packages.dir.join("b-in-c.so");
    let mut bytes = std::fs::read(&changed).unwrap();
    bytes.push(0);
    std::fs::write(&changed, bytes).unwrap();

    // Signed is the default mode.
    let daemon = Daemon::start(&packages, &["--packages", &dir, "--package-keys", &keys]);
    let client = daemon.client();
    let list = client.packages().unwrap();
    assert_eq!(list.mode, PackageMode::Signed);
    assert_eq!(
        list.packages,
        [
            loaded(packages.path("a-fixture.so"), "package-fixture"),
            refused(
                packages.path("b-in-c.so"),
                "bad signature: not signed by a trusted key, or changed since"
            ),
            refused(
                packages.path("c-junk.so"),
                "unsigned: signed mode, and it has no signature file"
            ),
            // Signed by the right key, and still not a usable driver.
            refused(
                packages.path("d-nameless.so"),
                "bad manifest: the driver has no name"
            ),
        ]
    );
    assert_eq!(add(&client, 40, 2), Ok(42));
    // The device whose package was refused waits, as for any driver that is not there.
    let (waiting, _, reason) = state(&client, "pin");
    assert_eq!(waiting, DeviceState::BackingOff);
    assert!(reason.contains("no driver named"), "{reason}");
    drop(client);
    daemon.stop();

    // With no keys named, nothing is looked at, and the daemon says why and runs on.
    let daemon = Daemon::start(&packages, &["--packages", &dir]);
    let client = daemon.client();
    let list = client.packages().unwrap();
    assert_eq!(list.mode, PackageMode::Signed);
    assert!(list.packages.is_empty());
    assert_eq!(
        list.note,
        format!("{dir} not used: signed mode needs a key file")
    );
    assert_eq!(state(&client, "thing").0, DeviceState::BackingOff);
    drop(client);
    let said = daemon.stop();
    assert!(
        said.contains(&format!(
            "lemnosd: packages: {dir} not used: signed mode needs a key file"
        )),
        "{said}"
    );

    // And with no directory, packages are off.
    let daemon = Daemon::start(&packages, &[]);
    let client = daemon.client();
    let list = client.packages().unwrap();
    assert_eq!((list.mode, list.packages.len()), (PackageMode::Off, 0));
    drop(client);
    assert_eq!(
        daemon
            .stop()
            .lines()
            .filter(|l| l.contains("package"))
            .count(),
        0
    );
}
