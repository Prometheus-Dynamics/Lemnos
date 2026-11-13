//! A stream's kind is the driver's declaration. A board file that has a `kind` key is refused by
//! name, whatever its value; a driver that declares no kind for a stream it serves is refused too.

use std::sync::Arc;

use lemnos_core::{
    BusKind, Config, Driver, Manifest, Need, NeedKind, Resources, Result as CoreResult, StreamKind,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::SimBoard;

/// Serves `frames` as a job-driven stream.
struct JobSource;

/// Serves `frames` as a free-running stream.
struct FreeSource;

/// Serves `frames` but declares no kind, as a packaged driver's table does today.
struct PackagedSource;

macro_rules! source {
    ($ty:ident, $name:literal, $stream:expr) => {
        impl Driver for $ty {
            const MANIFEST: Manifest = Manifest {
                name: $name,
                needs: &[Need {
                    name: "frames",
                    kind: NeedKind::Bus(BusKind::Stream),
                    optional: true,
                    stream: $stream,
                }],
                provides: &[],
            };

            fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> CoreResult<Self> {
                Ok(Self)
            }

            fn describe(&self, _: &mut lemnos_core::Description) {}

            fn dispatch(
                &mut self,
                _: lemnos_core::MethodId,
                _: &[u8],
                _: &mut lemnos_core::Reply<'_>,
            ) -> lemnos_core::Status {
                lemnos_core::Status::NOT_SUPPORTED
            }
        }
    };
}

source!(JobSource, "job-source", Some(StreamKind::JobDriven));
source!(FreeSource, "free-source", Some(StreamKind::FreeRunning));
source!(PackagedSource, "packaged-source", None);

fn entry_for(driver: &str) -> lemnos_host::DriverEntry {
    match driver {
        "job-source" => entry::<JobSource>(),
        _ => entry::<FreeSource>(),
    }
}

/// A board with one device of `driver`, whose `frames` stream states `key` when given.
fn board(driver: &str, key: Option<&str>) -> String {
    let kind = key.map(|k| format!("kind = \"{k}\"\n")).unwrap_or_default();
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"{driver}\"\n\
         [devices.streams.frames]\nbuffers = 2\nbuffer_bytes = 64\n{kind}"
    )
}

fn start(text: &str, drivers: &[lemnos_host::DriverEntry]) -> std::result::Result<Host, String> {
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(text).map_err(|e| e.to_string())?,
        drivers,
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .map_err(|e| e.to_string())
}

#[test]
fn a_board_that_states_a_kind_is_refused_by_name_whatever_the_value() {
    for (driver, key) in [
        ("job-source", "job"),
        ("job-source", "free-running"),
        ("free-source", "free-running"),
        ("free-source", "job"),
    ] {
        let error = start(&board(driver, Some(key)), &[entry_for(driver)])
            .err()
            .expect("refused");
        assert!(
            error.contains(&format!(
                "stream `frames` of driver `{driver}` has a `kind` key in the board file"
            )),
            "{key} on {driver}: {error}"
        );
        assert!(error.contains("the driver declares"), "{error}");
    }
}

#[test]
fn a_board_that_states_no_kind_takes_the_drivers() {
    assert!(start(&board("job-source", None), &[entry::<JobSource>()]).is_ok());
    assert!(start(&board("free-source", None), &[entry::<FreeSource>()]).is_ok());
}

/// A stream a driver package declares no kind for: its device is refused by name, with the reason,
/// and the board starts.
#[test]
fn a_packaged_stream_with_no_declared_kind_is_refused_by_name() {
    let host = start(
        &board("packaged-source", None),
        &[entry::<PackagedSource>()],
    )
    .expect("the board starts");
    let DeviceState::Backoff { reason, .. } = host.device("a").unwrap().state else {
        panic!("the device should be refused");
    };
    assert!(
        reason.starts_with("device \"a\": ")
            && reason.contains("stream `frames` of driver `packaged-source` declares no kind"),
        "{reason}"
    );
}
