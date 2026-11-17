//! A real-valued item must carry exponent 0. A describe that gives a real range, step or warning
//! limit with a nonzero exponent is refused at bind, by the item's name.

use std::sync::Arc;

use lemnos_core::{
    Config, Description, Driver, Item, Manifest, MethodId, Reply, Resources, Result, Status, Unit,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::SimBoard;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// Declares one item built by `item`, and reads nothing.
macro_rules! declaring {
    ($ty:ident, $name:literal, $item:expr) => {
        struct $ty;

        impl Driver for $ty {
            const MANIFEST: Manifest = Manifest {
                name: $name,
                needs: &[],
                provides: &[],
            };

            fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
                Ok(Self)
            }

            fn describe(&self, out: &mut Description) {
                out.add($item);
            }

            fn dispatch(&mut self, _: MethodId, _: &[u8], _: &mut Reply<'_>) -> Status {
                Status::NOT_SUPPORTED
            }
        }
    };
}

declaring!(
    ScaledReal,
    "scaled-real",
    Item::value("volts", READ)
        .unit(Unit::Volt, -9)
        .real_range(0.0, 5.0)
);
declaring!(
    WarnReal,
    "warn-real",
    Item::value("volts", READ)
        .unit(Unit::Volt, -3)
        .real_warn_below(10.8)
);
declaring!(
    BaseReal,
    "base-real",
    Item::value("volts", READ)
        .unit(Unit::Volt, 0)
        .real_range(0.0, 5.0)
        .real_warn_below(1.0)
);
declaring!(
    ScaledInt,
    "scaled-int",
    Item::value("millivolts", READ)
        .unit(Unit::Volt, -3)
        .range(0, 5000)
);

/// Two items with one name: a read and a control both called `duty`.
struct TwoNamed;

impl Driver for TwoNamed {
    const MANIFEST: Manifest = Manifest {
        name: "two-named",
        needs: &[],
        provides: &[],
    };

    fn bind(_: &mut Resources<'_>, _: &Config<'_>) -> Result<Self> {
        Ok(Self)
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("duty", READ).unit(Unit::One, 0));
        out.add(Item::control("duty", READ).unit(Unit::One, 0));
    }

    fn dispatch(&mut self, _: MethodId, _: &[u8], _: &mut Reply<'_>) -> Status {
        Status::NOT_SUPPORTED
    }
}

fn bind_one(driver: &str, entry_of: &[lemnos_host::DriverEntry]) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"{driver}\"\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        entry_of,
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

fn reason_of(host: &Host) -> Option<String> {
    match host.device("a")?.state {
        DeviceState::Backoff { reason, .. } => Some(reason),
        _ => None,
    }
}

#[test]
fn a_real_range_with_a_nonzero_exponent_is_refused_by_name() {
    let host = bind_one("scaled-real", &[entry::<ScaledReal>()]);
    let reason = reason_of(&host).expect("the bind is refused");
    assert!(
        reason.contains("item volts: a real value carries exponent 0"),
        "{reason}"
    );
    assert!(host.device("a").unwrap().state != DeviceState::Running);
}

#[test]
fn a_real_warning_limit_with_a_nonzero_exponent_is_refused_by_name() {
    let host = bind_one("warn-real", &[entry::<WarnReal>()]);
    let reason = reason_of(&host).expect("the bind is refused");
    assert!(
        reason.contains("item volts: a real value carries exponent 0"),
        "{reason}"
    );
}

#[test]
fn a_real_item_at_exponent_zero_binds() {
    let host = bind_one("base-real", &[entry::<BaseReal>()]);
    assert_eq!(reason_of(&host), None);
    assert_eq!(host.device("a").unwrap().state, DeviceState::Running);
}

#[test]
fn an_integer_item_with_a_scale_still_binds() {
    let host = bind_one("scaled-int", &[entry::<ScaledInt>()]);
    assert_eq!(reason_of(&host), None);
}

#[test]
fn two_items_with_one_name_are_refused_by_name() {
    let host = bind_one("two-named", &[entry::<TwoNamed>()]);
    let reason = reason_of(&host).expect("the bind is refused");
    assert!(
        reason.contains("item duty: two items have this name"),
        "{reason}"
    );
}
