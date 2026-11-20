use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A root under the temp dir, removed on drop.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "lemnos-root-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, rel: &str, text: &str, mode: u32) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn link(&self, rel: &str, target: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&path);
        symlink(target, path).unwrap();
    }

    fn remove(&self, rel: &str) {
        fs::remove_file(self.0.join(rel)).unwrap();
    }

    /// The daemon, the user and the group: what both init forms need.
    fn base(&self) {
        self.write(DAEMON, "", 0o755);
        self.write(
            "etc/passwd",
            "root:x:0:0::/root:/bin/sh\nlemnos:x:990:990::/:/sbin/nologin\n",
            0o644,
        );
        self.write("etc/group", "root:x:0:\nlemnos:x:990:\n", 0o644);
    }

    /// A systemd root as `systemctl enable lemnosd` leaves it.
    fn systemd(&self) {
        self.base();
        self.write(&format!("usr/lib/systemd/system/{UNIT}"), "[Unit]\n", 0o644);
        self.link(
            &format!("etc/systemd/system/multi-user.target.wants/{UNIT}"),
            &format!("/usr/lib/systemd/system/{UNIT}"),
        );
    }

    fn check(&self) -> Result<&'static str, String> {
        check(&self.0)
    }
}

#[test]
fn an_enabled_unit_with_its_user_and_group_starts() {
    let root = Root::new();
    root.systemd();
    assert_eq!(root.check(), Ok("systemd unit enabled"));
}

#[test]
fn the_busybox_init_script_starts() {
    let root = Root::new();
    root.base();
    root.write(INIT_SCRIPT, "#!/bin/sh\n", 0o755);
    assert_eq!(root.check(), Ok("init script"));
}

/// A root built with no user and nothing enabled. Every missing piece is named in one line.
#[test]
fn a_root_with_no_user_and_nothing_enabled_is_refused_naming_each() {
    let root = Root::new();
    root.write(DAEMON, "", 0o755);
    root.write(&format!("usr/lib/systemd/system/{UNIT}"), "[Unit]\n", 0o644);
    root.write("etc/passwd", "root:x:0:0::/root:/bin/sh\n", 0o644);
    root.write("etc/group", "root:x:0:\n", 0o644);
    let why = root.check().unwrap_err();
    assert!(why.contains(&format!("{UNIT} is not enabled")), "{why}");
    assert!(why.contains("no user lemnos in /etc/passwd"), "{why}");
    assert!(why.contains("no group lemnos in /etc/group"), "{why}");
}

#[test]
fn each_missing_piece_refuses() {
    for (what, undo, expect) in [
        ("daemon", DAEMON, "no /usr/bin/lemnosd"),
        ("passwd", "etc/passwd", "no user lemnos"),
        ("group", "etc/group", "no group lemnos"),
    ] {
        let root = Root::new();
        root.systemd();
        root.remove(undo);
        let why = root.check().unwrap_err();
        assert!(why.contains(expect), "{what}: {why}");
    }
    let root = Root::new();
    root.systemd();
    root.remove(&format!(
        "etc/systemd/system/multi-user.target.wants/{UNIT}"
    ));
    assert!(root.check().unwrap_err().contains("is not enabled"));
    let root = Root::new();
    root.base();
    assert!(
        root.check()
            .unwrap_err()
            .contains(&format!("no {UNIT} and no /{INIT_SCRIPT}"))
    );
}

#[test]
fn a_daemon_that_is_not_executable_refuses() {
    let root = Root::new();
    root.systemd();
    root.write(DAEMON, "", 0o644);
    assert!(
        root.check()
            .unwrap_err()
            .contains("is not an executable file")
    );
}

#[test]
fn a_masked_unit_refuses_even_when_enabled() {
    let root = Root::new();
    root.systemd();
    root.link(&format!("etc/systemd/system/{UNIT}"), "/dev/null");
    assert!(root.check().unwrap_err().contains("is masked"));
}

/// A user whose name only starts with `lemnos` is another user.
#[test]
fn a_name_must_match_whole() {
    let root = Root::new();
    root.systemd();
    root.write("etc/passwd", "lemnosd:x:990:990::/:/sbin/nologin\n", 0o644);
    assert!(root.check().unwrap_err().contains("no user lemnos"));
}

/// Links in the root name paths inside it: `/usr/...` is the root's, not the initramfs's. A merged
/// `/usr` with an absolute `etc -> /usr/etc` link still resolves inside the root, and `..` stops at it.
#[test]
fn links_resolve_inside_the_root() {
    let root = Root::new();
    root.systemd();
    fs::create_dir_all(root.0.join("usr/etc")).unwrap();
    fs::rename(root.0.join("etc/passwd"), root.0.join("usr/etc/passwd")).unwrap();
    root.link("etc/passwd", "/usr/etc/passwd");
    root.link("usr/bin/lemnosd", "../../../../../usr/libexec/lemnosd");
    root.write("usr/libexec/lemnosd", "", 0o755);
    assert_eq!(root.check(), Ok("systemd unit enabled"));
    assert_eq!(
        resolve(&root.0, "etc/passwd"),
        Some(root.0.join("usr/etc/passwd"))
    );
}

#[test]
fn a_link_loop_is_missing() {
    let root = Root::new();
    root.base();
    root.link("usr/bin/lemnosd", "/usr/bin/lemnosd");
    assert!(root.check().unwrap_err().contains("no /usr/bin/lemnosd"));
}

/// The names checked are the ones the units pass: a change to one must change the other.
#[test]
fn the_names_are_the_units_own() {
    let args = format!(
        "{} --board /etc/lemnos/board.toml --user {USER} --group {GROUP}",
        "/usr/bin/lemnosd"
    );
    let unit = include_str!("../../../../packaging/systemd/lemnosd.service");
    assert!(
        unit.contains(&format!("ExecStart={args}")),
        "lemnosd.service ExecStart"
    );
    assert!(unit.contains("WantedBy=multi-user.target"));
    let script = include_str!("../../../../packaging/init/S50lemnosd");
    assert!(
        script.contains(&format!("--user {USER} --group {GROUP}")),
        "S50lemnosd"
    );
    assert!(script.contains(&format!("DAEMON=/{DAEMON}")));
    assert!(INIT_SCRIPT.ends_with("S50lemnosd"));
}
