//! The lemnosd side. One blocking connection on a thread of its own, because lemnosd's blocking
//! client is the one that reports device events and connection state.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use std::time::Instant;

use lemnos_client::{Client, DeviceEntry, DeviceState, Error, Handle};
use lemnos_core::ItemKind;
use lemnos_wire::text::{method_of, shape_name};
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod, OwnedShape};

use crate::backoff::Backoff;
use crate::hello;
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
/// How long a `BUSY` description is asked again: `BUSY_TRIES` times, `BUSY_WAIT` apart.
const BUSY_WAIT: Duration = Duration::from_millis(2);
const BUSY_TRIES: u32 = 500;
/// How often the identity device's facts are read again. Its slot files are read on each read,
/// so a change of the running slot reaches Orion within this time.
const FACTS_EVERY: Duration = Duration::from_secs(2);

/// How to turn one subscription's reply into a value for one item.
#[derive(Debug)]
struct Route {
    /// lemnosd's index for the device: the mirror finds the device by it.
    device: u32,
    item: usize,
    shape: OwnedShape,
    field: Option<String>,
    exponent: i8,
    /// Whether a reply that did not read has been logged, so a bad device logs once.
    reported: bool,
}

/// The identity device's `facts` method, asked for on a timer: what the board's `hello.*`
/// facts are now.
struct IdentityRead {
    device: String,
    handle: Handle,
    method: u16,
    next: Instant,
    /// Whether a failure to read has been logged, so a device that stays absent logs once.
    reported: bool,
}

impl IdentityRead {
    /// Opens the identity device's contract. `None` when the call is not possible now.
    fn open(client: &Client, device: &str, method: u16) -> Result<Option<Self>, Error> {
        match client.open_tag(device, hello::IDENTITY_TAG) {
            Ok(handle) => Ok(Some(Self {
                device: device.to_owned(),
                handle,
                method,
                next: Instant::now(),
                reported: false,
            })),
            Err(error) if transient(&error) => Err(error),
            Err(_) => Ok(None),
        }
    }

    fn due(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Reads the facts now. `None` when they cannot be read: then none of the identity facts is
    /// published. A lost connection is an error, as it is for values.
    fn read(
        &mut self,
        client: &Client,
        reply: &mut [u8],
    ) -> Result<Option<Vec<(String, String)>>, Error> {
        self.next = Instant::now() + FACTS_EVERY;
        // An out-buffer method takes its capacity first, as the typed client sends it.
        let args = hello::FACTS_CAPACITY.to_le_bytes();
        match client.call(&mut self.handle, self.method, &args, reply) {
            Ok(len) => {
                self.reported = false;
                Ok(std::str::from_utf8(&reply[..len]).ok().map(hello::parse))
            }
            Err(error) if transient(&error) => Err(error),
            Err(error) => {
                if !self.reported {
                    self.reported = true;
                    log(&format!(
                        "{}: facts not read ({error}); its facts are not published until they are",
                        self.device
                    ));
                }
                Ok(None)
            }
        }
    }
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
                mirror.set_devices(Vec::new(), None);
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
    let (specs, mut routes, undescribed, identity_device) = discover(client)?;
    let mut reply = vec![0; REPLY_MAX];
    // The identity device's facts are read before the list is published, so the first list
    // Orion sees already carries them.
    let mut identity = match identity_device {
        Some((device, method)) => IdentityRead::open(client, &device, method)?,
        None => None,
    };
    let facts = match identity.as_mut() {
        Some(read) => read.read(client, &mut reply)?,
        None => None,
    };
    shared.with(|mirror| mirror.set_devices(specs, facts));
    shared.changed();
    backoff.reset();
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
        if let Some(read) = identity.as_mut()
            && read.due()
        {
            let facts = read.read(client, &mut reply)?;
            shared.with(|mirror| mirror.set_identity(facts));
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
                let stamp = value.stamp_ns;
                shared.with(|mirror| mirror.set_reading(route.device, route.item, reading, stamp));
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
/// The identity device, when one is described, comes back with the name it is called and the
/// method number of its `facts`.
#[allow(clippy::type_complexity)]
fn discover(
    client: &Client,
) -> Result<
    (
        Vec<DeviceSpec>,
        HashMap<u64, Route>,
        Vec<u32>,
        Option<(String, u16)>,
    ),
    Error,
> {
    let mut specs = Vec::new();
    let mut routes = HashMap::new();
    let mut undescribed = Vec::new();
    let mut identity = None;
    for entry in client.list()? {
        let mut items = Vec::new();
        let mut controls = Vec::new();
        match describe(client, entry.index) {
            Ok(description) => {
                controls = controls_of(&description);
                if identity.is_none() && entry.tags.iter().any(|t| t == hello::IDENTITY_TAG) {
                    identity = facts_method(&description).map(|id| (entry.name.clone(), id));
                }
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
                                    device: entry.index,
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
    Ok((specs, routes, undescribed, identity))
}

/// A device's description. lemnosd answers `BUSY` while a call or a value read holds the device,
/// which is a moment, not a failure; the answer is asked again (see [`describe_when_free`]).
pub fn describe(client: &Client, device: u32) -> Result<DeviceDescription, Error> {
    describe_when_free(|| client.describe(device))
}

/// Asks for a description again while the device answers `BUSY`, for a bounded time. Any other
/// answer, or a `BUSY` that lasts, is returned as it is.
pub fn describe_when_free(
    mut ask: impl FnMut() -> Result<DeviceDescription, Error>,
) -> Result<DeviceDescription, Error> {
    let mut tries = 0;
    loop {
        match ask() {
            Err(Error::Refused { status, .. })
                if status == lemnos_core::Status::BUSY && tries < BUSY_TRIES =>
            {
                tries += 1;
                std::thread::sleep(BUSY_WAIT);
            }
            answer => return answer,
        }
    }
}

/// The method number of the identity device's `facts`, from its description.
fn facts_method(description: &DeviceDescription) -> Option<u16> {
    let item = description
        .items
        .iter()
        .find(|item| item.kind == ItemKind::Value && item.name == hello::FACTS_METHOD)?;
    method_of(description, item)
        .ok()
        .map(|(_, method)| method.id)
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
    use std::cell::Cell;

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
            reading: None,
            step: None,
            warn_low: None,
            warn_high: None,
            group: None,
        }
    }

    fn description() -> DeviceDescription {
        DeviceDescription {
            contracts: vec![OwnedContract {
                tag: "example.thing/1".to_owned(),
                kind: "other".to_owned(),
                hash: 0,
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
            ..DeviceDescription::default()
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

    #[test]
    fn a_busy_description_is_asked_again_and_any_other_answer_is_not() {
        let busy = || Error::Refused {
            status: lemnos_core::Status::BUSY,
            text: "the device isn't running".to_owned(),
        };
        let asked = Cell::new(0);
        let answer = describe_when_free(|| {
            asked.set(asked.get() + 1);
            if asked.get() < 3 {
                Err(busy())
            } else {
                Ok(description())
            }
        });
        assert_eq!(answer, Ok(description()));
        assert_eq!(asked.get(), 3, "two BUSY answers, then the description");

        let asked = Cell::new(0);
        let answer = describe_when_free(|| {
            asked.set(asked.get() + 1);
            Err(Error::Refused {
                status: lemnos_core::Status::NO_DEVICE,
                text: String::new(),
            })
        });
        assert!(matches!(
            answer,
            Err(Error::Refused { status, .. }) if status == lemnos_core::Status::NO_DEVICE
        ));
        assert_eq!(
            asked.get(),
            1,
            "a refusal that is not BUSY is not asked again"
        );

        let asked = Cell::new(0);
        let answer = describe_when_free(|| {
            asked.set(asked.get() + 1);
            Err(busy())
        });
        assert!(answer.is_err());
        assert_eq!(
            asked.get(),
            BUSY_TRIES + 1,
            "a BUSY that lasts is given up at the bound"
        );
    }
}
