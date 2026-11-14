//! A `uses` or `config` entry nothing consumes, and a `uses` that names a device the board does
//! not have, are refused by name, by `lemnos-ctl validate` and by the daemon's start. The daemon
//! starts without the refused device and every other device runs.

use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_host::board::parse;
use lemnos_host::{Options, settings_refusals};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                    [security]\nrecord = \"/nonexistent/lemnos-uses-test/record\"\n";

/// A device that runs on the simulated board, for the refused one to sit beside.
const LEASE: &str = "[[devices]]\nid = \"lease\"\ndriver = \"gpu-lease\"\n";

fn validate(text: &str) -> Result<String, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-uses-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    let mut out = Vec::new();
    let args = [
        "--socket".to_string(),
        dir.join("nobody").display().to_string(),
        "validate".to_string(),
        path.display().to_string(),
    ];
    let result = lemnos_ctl::run(&args, &mut out).map(|()| String::from_utf8(out).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
    result
}

/// The rows of `lemnos-ctl list` against a daemon started on `text`.
fn list(text: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let socket = std::env::temp_dir().join(format!(
        "lemnos-uses-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: lemnosd::builtin::drivers(),
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .expect("the daemon starts");
    let args = [
        "--socket".to_string(),
        socket.display().to_string(),
        "list".to_string(),
    ];
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).unwrap();
    drop(service);
    String::from_utf8(out).unwrap()
}

fn row<'a>(list: &'a str, id: &str) -> &'a str {
    list.lines()
        .find(|row| row.starts_with(&format!("{id} ")))
        .unwrap_or_else(|| panic!("no row for {id}: {list}"))
}

/// Validate refuses with `reason` (ending the message), the daemon starts, `d` is failed with the
/// same reason, and `lease` runs.
fn refused(body: &str, reason: &str) {
    let text = format!("{HEAD}{LEASE}{body}");
    let error = validate(&text).unwrap_err();
    assert!(error.ends_with(reason), "validate: {error}");
    let list = list(&text);
    let d = row(&list, "d");
    assert!(d.contains("retrying  failed"), "{d}");
    assert!(d.contains(reason), "{d}");
    assert!(row(&list, "lease").contains("running"), "{list}");
}

#[test]
fn an_unconsumed_uses_key_is_refused_by_name_and_lists_what_the_driver_takes() {
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"system-security\"\nuses = { zzz = \"lease\" }\n",
        "device \"d\": `uses` key \"zzz\" is not a resource this driver takes (it takes: \"slots\", \"flash\", \"identity\")",
    );
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"gpu-lease\"\nuses = { zzz = \"lease\" }\n",
        "device \"d\": `uses` key \"zzz\" is not a resource this driver takes (it takes none)",
    );
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"ov9782\"\nuses = { bus = \"i2c-1\", zzz = \"lease\" }\n",
        "device \"d\": `uses` key \"zzz\" is not a resource this driver takes (it takes: \"bus\", \"values\")",
    );
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:udc0\", zzz = \"lease\" }\n\
         [devices.config]\nvendor = 4617\nproduct = 1\n",
        "device \"d\": `uses` key \"zzz\" is not a resource this driver takes (it takes: \"function\", \"data\")",
    );
}

#[test]
fn a_gadget_identity_under_uses_keeps_its_specific_text() {
    let body = "[[devices]]\nid = \"d\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:udc0\", identity = \"lease\" }\n\
                [devices.config]\nvendor = 4617\nproduct = 1\n";
    let error = validate(&format!("{HEAD}{LEASE}{body}")).unwrap_err();
    assert!(
        error.ends_with(
            "device \"d\": `identity` is a setting, not a resource: put it in the device's `config`"
        ),
        "{error}"
    );
}

#[test]
fn a_uses_naming_a_device_the_board_does_not_have_is_refused_by_name() {
    // `flash` is an optional need of the security switch: naming a device that is not there is
    // refused all the same.
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"system-security\"\nuses = { flash = \"flash\" }\n",
        "device \"d\": `uses` key \"flash\" names device \"flash\", which the board does not have",
    );
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"system-security\"\nuses = { slots = \"nowhere\" }\n",
        "device \"d\": `uses` key \"slots\" names device \"nowhere\", which the board does not have",
    );
}

#[test]
fn a_uses_naming_a_device_of_the_wrong_contract_is_refused_by_name() {
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"system-security\"\nuses = { flash = \"lease\" }\n",
        "device \"d\": `uses` key \"flash\" names device \"lease\", which does not serve system.security-precondition/1",
    );
}

#[test]
fn an_unconsumed_config_key_is_refused_by_name_on_the_security_switch() {
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"system-security\"\n[devices.config]\nzzz = 1\n",
        "device \"d\": setting \"zzz\" is not a setting this driver reads",
    );
    refused(
        "[[devices]]\nid = \"d\"\ndriver = \"gpu-lease\"\n[devices.config]\nzzz = 1\n",
        "device \"d\": setting \"zzz\" is not a setting this driver reads",
    );
}

/// Every built-in driver, with no opt-out: a `uses` key it does not take, and a `config` key it does
/// not read, are refused by name.
#[test]
fn every_builtin_driver_refuses_an_unconsumed_uses_key_and_config_key_by_name() {
    let drivers = lemnosd::builtin::drivers();
    assert!(drivers.len() > 40, "the table is {} drivers", drivers.len());
    for entry in &drivers {
        let manifest = entry.manifest();
        let name = manifest.name;
        let text = format!(
            "{HEAD}{LEASE}[[devices]]\nid = \"d\"\ndriver = \"{name}\"\nuses = {{ zzz_unused = \"lease\" }}\n"
        );
        let refusals = settings_refusals(&parse(&text).unwrap(), &drivers);
        let reason = refusals
            .iter()
            .find(|(id, _)| id == "d")
            .unwrap_or_else(|| panic!("{name}: a `uses` key nothing takes was accepted"))
            .1
            .clone();
        let takes = if manifest.needs.is_empty() {
            "it takes none".to_string()
        } else {
            let names: Vec<String> = manifest
                .needs
                .iter()
                .map(|need| format!("{:?}", need.name))
                .collect();
            format!("it takes: {}", names.join(", "))
        };
        assert_eq!(
            reason,
            format!(
                "device \"d\": `uses` key \"zzz_unused\" is not a resource this driver takes ({takes})"
            ),
            "{name}"
        );

        let text = format!(
            "{HEAD}{LEASE}[[devices]]\nid = \"d\"\ndriver = \"{name}\"\n[devices.config]\nzzz_unused = 1\n"
        );
        let refusals = settings_refusals(&parse(&text).unwrap(), &drivers);
        let reason = refusals
            .iter()
            .find(|(id, _)| id == "d")
            .unwrap_or_else(|| panic!("{name}: a `config` key nothing reads was accepted"))
            .1
            .clone();
        assert!(
            reason.starts_with(
                "device \"d\": setting \"zzz_unused\" is not a setting this driver reads"
            ),
            "{name}: {reason}"
        );
    }
}
