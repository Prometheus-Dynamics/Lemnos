//! The lemnosd side. One blocking connection on a thread of its own, because lemnosd's blocking
//! client is the one that reports device events and connection state.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use lemnos_client::{Client, DeviceEntry, DeviceState, Error};
use lemnos_core::ItemKind;
use lemnos_wire::text::{method_of, shape_name};
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod, OwnedShape};

use crate::backoff::Backoff;
use crate::mirror::{ControlSpec, DeviceSpec, ItemSpec, Status};
use crate::values::{self, Plain, plain_of};
use crate::{LEMNOSD_NAME, Shared, log};

/// How long one wait for a value lasts. The loop checks for stop and for a lost connection
/// between waits.
const WAIT: Duration = Duration::from_millis(200);
/// Asks for the fastest period the service grants for an item (its cost floor). With
/// `only_changes`, the service then sends a reply only when its bytes differ from the last.
const PERIOD_US: u64 = 1;
/// The largest reply lemnos-ctl reads. Items reply with a few bytes.
const REPLY_MAX: usize = 64 * 1024;

/// How to turn one subscription's reply into a value for one item.
#[derive(Debug)]
struct Route {
    device: usize,
    item: usize,
    shape: OwnedShape,
    field: Option<String>,
    exponent: i8,
    /// Whether a reply that did not read has been logged, so a bad device logs once.
    reported: bool,
}

/// Keeps the mirror in step with lemnosd until the shared state is stopped. Each time the
/// connection is lost, the devices are removed from the mirror and the connection is made again.
pub fn run(socket: &Path, shared: &Shared) {
    let mut backoff = Backoff::new();
    while !shared.stopping() {
        let client = Client::connecting(socket, LEMNOSD_NAME);
        match session(&client, shared, &mut backoff) {
            // A device that could not be described is running now: read every device again
            // on a fresh connection. The list the mirror holds stays until the new one
            // replaces it.
            Ok(Ended::Undescribed) => continue,
            Ok(Ended::Stopped) => {}
            Err(error) => log(&format!("lemnosd: {error}; retrying")),
        }
        shared.with(|mirror| {
            if !mirror.is_empty() {
                mirror.set_devices(Vec::new());
            }
        });
        shared.changed();
        if shared.stopping() {
            break;
        }
        std::thread::sleep(backoff.next_delay());
    }
}

/// How a session ended, when the connection did not.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Stopped,
    /// A device that was not described at discovery has started running.
    Undescribed,
}

/// One connection's life: discover, then forward values and events until the connection ends.
fn session(client: &Client, shared: &Shared, backoff: &mut Backoff) -> Result<Ended, Error> {
    let (specs, mut routes, undescribed) = discover(client)?;
    shared.with(|mirror| mirror.set_devices(specs));
    shared.changed();
    backoff.reset();
    let mut reply = vec![0; REPLY_MAX];
    while !shared.stopping() {
        if !client.is_connected() {
            return Err(Error::Disconnected);
        }
        while let Some(event) = client.next_event() {
            if event.state == DeviceState::Running && undescribed.contains(&event.device) {
                return Ok(Ended::Undescribed);
            }
            shared.with(|mirror| {
                mirror.set_status(event.device, event.state, event.health, &event.reason);
            });
            shared.changed();
        }
        let Some(value) = client.next_value(&mut reply, WAIT)? else {
            continue;
        };
        let Some(route) = routes.get_mut(&value.subscription) else {
            continue;
        };
        match values::read(
            &route.shape,
            route.field.as_deref(),
            route.exponent,
            &reply[..value.len],
        ) {
            Ok(reading) => {
                shared.with(|mirror| mirror.set_reading(route.device, route.item, reading));
                shared.changed();
            }
            Err(error) => {
                if !route.reported {
                    route.reported = true;
                    log(&format!("a reply did not read: {error}"));
                }
            }
        }
    }
    Ok(Ended::Stopped)
}

/// Every device lemnosd has, with the value items that can be published and a route for each.
///
/// Only a failure to list the devices, or a lost connection, fails the whole discovery. Any
/// other error on one device or item leaves that item out and logs why, so one bad device does
/// not hold back the rest.
/// The indexes of devices that could not be described come back too: lemnosd describes only a
/// running device, and an event says when one starts running, which is when it is read again.
#[allow(clippy::type_complexity)]
fn discover(client: &Client) -> Result<(Vec<DeviceSpec>, HashMap<u64, Route>, Vec<u32>), Error> {
    let mut specs = Vec::new();
    let mut routes = HashMap::new();
    let mut undescribed = Vec::new();
    for entry in client.list()? {
        let position = specs.len();
        let mut items = Vec::new();
        let mut controls = Vec::new();
        match client.describe(entry.index) {
            Ok(description) => {
                controls = controls_of(&description);
                for item in description
                    .items
                    .iter()
                    .filter(|item| item.kind == ItemKind::Value)
                {
                    let Some((tag, method, plain)) = plan(&description, item) else {
                        log(&format!(
                            "{}: {} has no plain value; not published",
                            entry.name, item.name
                        ));
                        continue;
                    };
                    match subscribe(client, &entry, tag, method) {
                        Ok(subscription) => {
                            routes.insert(
                                subscription,
                                Route {
                                    device: position,
                                    item: items.len(),
                                    shape: method.returns.clone(),
                                    field: item.field.clone(),
                                    exponent: item.exponent,
                                    reported: false,
                                },
                            );
                            items.push(ItemSpec {
                                name: item.name.clone(),
                                unit: item.unit.clone(),
                                plain,
                            });
                        }
                        Err(error) if transient(&error) => return Err(error),
                        Err(error) => log(&format!(
                            "{}: {} not subscribed: {error}",
                            entry.name, item.name
                        )),
                    }
                }
            }
            Err(error) if transient(&error) => return Err(error),
            Err(error) => {
                log(&format!("{}: not described: {error}", entry.name));
                undescribed.push(entry.index);
            }
        }
        specs.push(DeviceSpec {
            index: entry.index,
            name: entry.name,
            driver: entry.driver,
            tags: entry.tags,
            status: Status {
                state: entry.state,
                health: entry.health,
                reason: entry.reason,
            },
            items,
            controls,
        });
    }
    Ok((specs, routes, undescribed))
}

/// The contract tag and method that read a value item, and what it publishes as, when the
/// item can be published: a reader takes no arguments, has no out-buffer, starts no job, and
/// replies with a number or a flag.
fn plan<'a>(
    description: &'a DeviceDescription,
    item: &OwnedItem,
) -> Option<(&'a str, &'a OwnedMethod, Plain)> {
    let contract = description.contracts.get(usize::from(item.contract))?;
    let method = contract.methods.iter().find(|m| m.id == item.method)?;
    if !method.params.is_empty() || method.has_out_buffer || method.job {
        return None;
    }
    let plain = plain_of(&method.returns, item.field.as_deref())?;
    Some((contract.tag.as_str(), method, plain))
}

/// The items a caller drives: controls and actions, each with what it takes. An item whose
/// method the device does not have is left out.
fn controls_of(description: &DeviceDescription) -> Vec<ControlSpec> {
    description
        .items
        .iter()
        .filter(|item| item.kind != ItemKind::Value)
        .filter_map(|item| {
            let (_, method) = method_of(description, item).ok()?;
            let takes = method
                .params
                .iter()
                .map(|param| shape_name(&param.shape))
                .collect::<Vec<_>>()
                .join(" ");
            Some(ControlSpec {
                name: item.name.clone(),
                takes,
            })
        })
        .collect()
}

fn subscribe(
    client: &Client,
    entry: &DeviceEntry,
    tag: &str,
    method: &OwnedMethod,
) -> Result<u64, Error> {
    let mut handle = client.open_tag(&entry.name, tag)?;
    let subscription = client.subscribe(&mut handle, method.id, PERIOD_US, true)?;
    Ok(subscription.id)
}

/// Errors that mean the connection is gone or busy, rather than the item being unusable.
const fn transient(error: &Error) -> bool {
    matches!(
        error,
        Error::Disconnected | Error::TimedOut | Error::Stale | Error::Full
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_wire::{OwnedContract, OwnedParam};

    fn method(id: u16, params: usize, returns: OwnedShape, out: bool) -> OwnedMethod {
        OwnedMethod {
            id,
            name: format!("m{id}"),
            params: (0..params)
                .map(|i| OwnedParam {
                    name: format!("p{i}"),
                    shape: OwnedShape::U32,
                })
                .collect(),
            returns,
            has_out_buffer: out,
            job: false,
        }
    }

    fn item(kind: ItemKind, name: &str, method: u16, field: Option<&str>) -> OwnedItem {
        OwnedItem {
            kind,
            name: name.to_owned(),
            unit: String::new(),
            exponent: 0,
            min: None,
            max: None,
            contract: 0,
            method,
            field: field.map(str::to_owned),
        }
    }

    fn description() -> DeviceDescription {
        DeviceDescription {
            contracts: vec![OwnedContract {
                tag: "example.thing/1".to_owned(),
                methods: vec![
                    method(1, 0, OwnedShape::U32, false),
                    method(2, 1, OwnedShape::U32, false),
                    method(3, 0, OwnedShape::U8, true),
                    method(4, 0, OwnedShape::Str, false),
                    method(
                        5,
                        0,
                        OwnedShape::Record(vec![("x".to_owned(), OwnedShape::F32)]),
                        false,
                    ),
                ],
            }],
            items: vec![
                item(ItemKind::Value, "count", 1, None),
                item(ItemKind::Control, "target", 2, None),
                item(ItemKind::Value, "dump", 3, None),
                item(ItemKind::Value, "label", 4, None),
                item(ItemKind::Value, "x", 5, Some("x")),
                item(ItemKind::Value, "whole", 5, None),
            ],
        }
    }

    #[test]
    fn only_plain_readers_are_planned_and_a_field_is_checked() {
        let description = description();
        let planned = |name: &str| {
            let item = description.items.iter().find(|i| i.name == name).unwrap();
            plan(&description, item).map(|(tag, method, _)| (tag.to_owned(), method.id))
        };
        assert_eq!(planned("count"), Some(("example.thing/1".to_owned(), 1)));
        assert_eq!(planned("x"), Some(("example.thing/1".to_owned(), 5)));
        assert_eq!(
            planned("target"),
            None,
            "a method with an argument reads nothing"
        );
        assert_eq!(planned("dump"), None, "an out-buffer reply is raw bytes");
        assert_eq!(planned("label"), None, "a string is not a number or a flag");
        assert_eq!(
            planned("whole"),
            None,
            "a record with no field has no plain value"
        );
    }

    #[test]
    fn controls_are_listed_with_what_they_take() {
        let controls = controls_of(&description());
        assert_eq!(
            controls,
            [ControlSpec {
                name: "target".to_owned(),
                takes: "u32".to_owned(),
            }]
        );
    }

    #[test]
    fn only_a_refused_link_or_a_timeout_is_transient() {
        assert!(transient(&Error::Disconnected));
        assert!(transient(&Error::TimedOut));
        assert!(!transient(&Error::Refused {
            status: lemnos_core::Status::NOT_SUPPORTED,
            text: String::new(),
        }));
    }
}
