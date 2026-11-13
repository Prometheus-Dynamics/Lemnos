//! A device's self-description as bytes, and the owned form a client keeps.
//!
//! The records are declared with the same derive as the frames' values: each one is a block
//! (see `lemnos_core::Encode`), and its shape is folded into the core hash by [`fold_framing`]. The
//! counts before each list and the shape codes are constants, folded the same way. A shape a
//! record carries is written with [`encode_shape`], whose codes are in [`shape_code`].

use lemnos_core::{
    ContractInfo, Decode, Description, Encode, Item, ItemKind, Manifest, MethodId, MethodInfo,
    Reader, Reading, Result, Shape, Status, Tail, UnitState, Writer,
};

use crate::hash;

/// The codes a [`Shape`] is written as. Folded into the core hash.
pub mod shape_code {
    constants! {
        pub const UNIT: u8 = 0;
        pub const BOOL: u8 = 1;
        pub const U8: u8 = 2;
        pub const U16: u8 = 3;
        pub const U32: u8 = 4;
        pub const U64: u8 = 5;
        pub const I8: u8 = 6;
        pub const I16: u8 = 7;
        pub const I32: u8 = 8;
        pub const I64: u8 = 9;
        pub const F32: u8 = 10;
        pub const F64: u8 = 11;
        pub const STR: u8 = 12;
        pub const BYTES: u8 = 13;
        pub const OPTION: u8 = 14;
        pub const ARRAY: u8 = 15;
        pub const RECORD: u8 = 16;
    }
}

/// The kinds of item, as written. Folded into the core hash.
pub mod item_kind {
    constants! {
        pub const VALUE: u8 = 0;
        pub const CONTROL: u8 = 1;
        pub const ACTION: u8 = 2;
    }
}

/// A [`Shape`] a client owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedShape {
    Unit,
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Str,
    Bytes,
    Option(Box<OwnedShape>),
    Array(Box<OwnedShape>, u32),
    Record(Vec<(String, OwnedShape)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedParam {
    pub name: String,
    pub shape: OwnedShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedMethod {
    pub id: u16,
    pub name: String,
    pub params: Vec<OwnedParam>,
    pub returns: OwnedShape,
    pub has_out_buffer: bool,
    /// The method starts a long job (CallJob), and isn't called.
    pub job: bool,
    /// The method's hash ([`lemnos_core::layout::method_hash`]): its base shape, without its added
    /// fields. 0 from a device from before method hashes.
    pub hash: u64,
    /// The method is an addition after its contract's base shape.
    pub added: bool,
    /// The hash of each added field its parameters and result carry
    /// ([`lemnos_core::layout::added_fields`]).
    pub added_fields: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedContract {
    pub tag: String,
    /// What the contract is for, such as `sensor`. `other` when it names none; empty from a host
    /// that sends no kinds.
    pub kind: String,
    /// The contract's base hash ([`lemnos_core::layout::contract_hash`]): equal on both sides when
    /// the base methods are laid out the same. Additions are not in it.
    pub hash: u64,
    pub methods: Vec<OwnedMethod>,
}

impl OwnedContract {
    /// The contract's additions level: how many of its methods are added.
    pub fn level(&self) -> usize {
        self.methods.iter().filter(|method| method.added).count()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnedItem {
    pub kind: ItemKind,
    pub name: String,
    pub unit: String,
    /// Whether the unit is known, dimensionless or not said. From a host before the state, it is
    /// unknown when `unit` is empty and known otherwise.
    pub unit_state: UnitState,
    pub exponent: i8,
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// The range and the step as real numbers in the unit, for a float value (see
    /// [`lemnos_core::Item::real_range`]). `None` when the device gave none, or from a host before
    /// them.
    pub real_min: Option<f64>,
    pub real_max: Option<f64>,
    pub real_step: Option<f64>,
    pub contract: u8,
    pub method: u16,
    pub field: Option<String>,
    /// How the value reads. `None` from a host that sends no readings, or for a reading this
    /// build does not know.
    pub reading: Option<Reading>,
    /// The smallest change the item can take, in raw units.
    pub step: Option<i64>,
    /// Raw values beyond these are warnings.
    pub warn_low: Option<i64>,
    pub warn_high: Option<i64>,
    /// The vector this item is one part of, when it is one.
    pub group: Option<String>,
}

/// Everything a tool with no code for a device needs to use it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceDescription {
    /// The device's name for people: its board `label`, or its id when the board names none.
    /// Empty from a host that sends no identity.
    pub label: String,
    /// The grouping the device's contracts give it: the first contract kind that is not
    /// `other`, else `other`. Empty from a host that sends no identity.
    pub kind: String,
    /// The bus or block the device sits on, as the board names it. Empty when it names none.
    pub bus: String,
    /// The 7-bit I2C address the driver named, if it named one.
    pub address: Option<u8>,
    /// Where the driver says the device is, in words, for what an address cannot say (a sysfs
    /// directory, a chip select, a port path). Empty when the driver names none.
    pub location: String,
    /// The device's state, as [`crate::state`] numbers it, and why it is in that state (empty
    /// when it is running). `None` from a host that sends no identity.
    pub state: Option<u8>,
    pub reason: String,
    pub contracts: Vec<OwnedContract>,
    pub items: Vec<OwnedItem>,
}

/// What the host knows about a device and tells a reader of its description: the parts of a
/// device's identity that come from the board file and the host's state, not from the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Identity<'a> {
    /// The board's `label`, or the device id when the board names none.
    pub label: &'a str,
    /// The bus or block the board names for the device, empty when none.
    pub bus: &'a str,
    /// A [`crate::state`] number.
    pub state: u8,
    /// Why the device is not running, or its last failure; empty when there is none.
    pub reason: &'a str,
}

/// A contract's header: its tag, its hash and how many methods follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescContract<'a> {
    tag: &'a str,
    hash: u64,
    methods: u16,
}

/// A method: its number, name, how many parameters follow, and whether it fills a buffer. Its
/// hash and whether it is an addition are appended: a host from before them reads 0 and false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescMethod<'a> {
    id: u16,
    name: &'a str,
    params: u16,
    has_out_buffer: bool,
    hash: u64,
    added: bool,
}

/// A parameter's name. Its shape follows, written with [`encode_shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescParam<'a> {
    name: &'a str,
}

/// One item of the description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescItem<'a> {
    kind: u8,
    name: &'a str,
    unit: &'a str,
    exponent: i8,
    min: Option<i64>,
    max: Option<i64>,
    contract: u8,
    method: u16,
    field: Option<&'a str>,
}

/// One method that starts a long job, appended after the items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescJob {
    contract: u8,
    method: u16,
}

/// The identity, appended after the job list as a [`Tail`]: a host from before it sends none, and
/// the fields are the device's label, bus, state, reason, address and kind. It is an addition, so
/// it is left out of the core hash (see `docs/wire.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Encode, Decode)]
struct DescIdentity<'a> {
    label: &'a str,
    bus: &'a str,
    state: Option<u8>,
    reason: &'a str,
    address: Option<u8>,
    kind: &'a str,
    /// Appended after the fields above (the identity's struct block grew by it): a reader from
    /// before it skips it.
    location: &'a str,
}

/// One contract's kind, appended after the identity: one per contract, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Encode, Decode)]
struct DescContractKind<'a> {
    kind: &'a str,
}

/// One item's extra fields, appended after the contract kinds: one per item, in order.
#[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
struct DescItemExtra<'a> {
    reading: u8,
    step: Option<i64>,
    warn_low: Option<i64>,
    warn_high: Option<i64>,
    group: Option<&'a str>,
    /// Appended after the fields above: its absence (a host from before it) reads as `None`, and
    /// the unit's state is then decided by the unit string.
    unit_state: Option<u8>,
    /// Appended after the unit state: the range and step as real numbers in the unit (a host from
    /// before them reads none, and the integer range stands).
    real_min: Option<f64>,
    real_max: Option<f64>,
    real_step: Option<f64>,
}

/// The item's extras as a host from before the unit state and the real ranges wrote them: the same
/// fields, without the appended ones. Only [`Layout::BeforeUnitState`] writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode)]
struct DescItemExtraBefore<'a> {
    reading: u8,
    step: Option<i64>,
    warn_low: Option<i64>,
    warn_high: Option<i64>,
    group: Option<&'a str>,
}

/// One added field of a method, appended after the trailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescAdded {
    contract: u8,
    method: u16,
    hash: u64,
}

/// Folds the description's framing into `hash`: its record names and shapes, its shape codes and
/// its item kinds. It is part of the core hash, so a change to any of them moves it.
pub(crate) const fn fold_framing(mut hash: u64) -> u64 {
    hash = shape_code::fold_constants(hash);
    hash = item_kind::fold_constants(hash);
    hash = hash::text(hash, "contract");
    hash = lemnos_core::layout::shape(hash, &<DescContract<'static> as Encode>::SHAPE);
    hash = hash::text(hash, "method");
    hash = lemnos_core::layout::shape(hash, &<DescMethod<'static> as Encode>::SHAPE);
    hash = hash::text(hash, "param");
    hash = lemnos_core::layout::shape(hash, &<DescParam<'static> as Encode>::SHAPE);
    hash = hash::text(hash, "item");
    hash = lemnos_core::layout::shape(hash, &<DescItem<'static> as Encode>::SHAPE);
    hash = hash::text(hash, "job");
    hash = lemnos_core::layout::shape(hash, &<DescJob as Encode>::SHAPE);
    hash = hash::text(hash, "added");
    hash = lemnos_core::layout::shape(hash, &<DescAdded as Encode>::SHAPE);
    hash
}

fn encode_shape(shape: &Shape, out: &mut Writer<'_>) -> Result<()> {
    let code = match shape {
        Shape::Unit => shape_code::UNIT,
        Shape::Bool => shape_code::BOOL,
        Shape::U8 => shape_code::U8,
        Shape::U16 => shape_code::U16,
        Shape::U32 => shape_code::U32,
        Shape::U64 => shape_code::U64,
        Shape::I8 => shape_code::I8,
        Shape::I16 => shape_code::I16,
        Shape::I32 => shape_code::I32,
        Shape::I64 => shape_code::I64,
        Shape::F32 => shape_code::F32,
        Shape::F64 => shape_code::F64,
        Shape::Str => shape_code::STR,
        Shape::Bytes => shape_code::BYTES,
        Shape::Option(inner) => {
            shape_code::OPTION.encode(out)?;
            return encode_shape(inner, out);
        }
        Shape::Array(inner, n) => {
            shape_code::ARRAY.encode(out)?;
            n.encode(out)?;
            return encode_shape(inner, out);
        }
        Shape::Record(fields) => {
            shape_code::RECORD.encode(out)?;
            (fields.len() as u16).encode(out)?;
            for field in *fields {
                field.name.encode(out)?;
                encode_shape(&field.shape, out)?;
            }
            return Ok(());
        }
    };
    code.encode(out)
}

fn decode_shape(from: &mut Reader<'_>, depth: u32) -> Result<OwnedShape> {
    if depth == 0 {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(match u8::decode(from)? {
        shape_code::UNIT => OwnedShape::Unit,
        shape_code::BOOL => OwnedShape::Bool,
        shape_code::U8 => OwnedShape::U8,
        shape_code::U16 => OwnedShape::U16,
        shape_code::U32 => OwnedShape::U32,
        shape_code::U64 => OwnedShape::U64,
        shape_code::I8 => OwnedShape::I8,
        shape_code::I16 => OwnedShape::I16,
        shape_code::I32 => OwnedShape::I32,
        shape_code::I64 => OwnedShape::I64,
        shape_code::F32 => OwnedShape::F32,
        shape_code::F64 => OwnedShape::F64,
        shape_code::STR => OwnedShape::Str,
        shape_code::BYTES => OwnedShape::Bytes,
        shape_code::OPTION => OwnedShape::Option(Box::new(decode_shape(from, depth - 1)?)),
        shape_code::ARRAY => {
            let n = u32::decode(from)?;
            OwnedShape::Array(Box::new(decode_shape(from, depth - 1)?), n)
        }
        shape_code::RECORD => {
            let count = u16::decode(from)?;
            let mut fields = Vec::new();
            for _ in 0..count {
                let name = <&str>::decode(from)?.to_string();
                fields.push((name, decode_shape(from, depth - 1)?));
            }
            OwnedShape::Record(fields)
        }
        _ => return Err(Status::INVALID_ARGUMENT),
    })
}

/// The kind a device gets from its contracts: the first one that is not `other`.
#[must_use]
pub fn device_kind(manifest: &Manifest) -> &'static str {
    manifest
        .provides
        .iter()
        .map(|contract| contract.kind)
        .find(|kind| *kind != "other")
        .unwrap_or("other")
}

/// The reading an item shows as: the item's own, else its method's (the contract's), else one
/// its shape suggests: a control is a range, a switch a switch, a string text.
#[must_use]
pub fn effective_reading(manifest: &Manifest, item: &Item) -> Reading {
    if let Some(reading) = item.reading {
        return reading;
    }
    let MethodId { contract, method } = item.method;
    let info = manifest
        .provides
        .get(usize::from(contract))
        .and_then(|c| c.methods.iter().find(|m| m.id == method));
    if let Some(reading) = info.and_then(|m| m.reading) {
        return reading;
    }
    if item.kind == ItemKind::Control {
        return Reading::Range;
    }
    shape_reading(info, item.field)
}

fn shape_reading(info: Option<&MethodInfo>, field: Option<&str>) -> Reading {
    let Some(info) = info else {
        return Reading::Number;
    };
    let shape = match (&info.returns, field) {
        (Shape::Record(fields), Some(field)) => fields
            .iter()
            .find(|f| f.name == field)
            .map_or(&info.returns, |f| &f.shape),
        (shape, _) => shape,
    };
    match shape {
        Shape::Bool => Reading::Switch,
        Shape::Str => Reading::Text,
        _ => Reading::Number,
    }
}

fn encode_contract(contract: &ContractInfo, out: &mut Writer<'_>) -> Result<()> {
    DescContract {
        tag: contract.tag.as_str(),
        hash: lemnos_core::layout::contract_hash(contract),
        methods: contract.methods.len() as u16,
    }
    .encode(out)?;
    for method in contract.methods {
        DescMethod {
            id: method.id,
            name: method.name,
            params: method.params.len() as u16,
            has_out_buffer: method.has_out_buffer,
            hash: lemnos_core::layout::method_hash(method),
            added: method.added,
        }
        .encode(out)?;
        for param in method.params {
            DescParam { name: param.name }.encode(out)?;
            encode_shape(&param.shape, out)?;
        }
        encode_shape(&method.returns, out)?;
    }
    Ok(())
}

const MAX_SHAPE_DEPTH: u32 = 8;

fn kind_code(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Value => item_kind::VALUE,
        ItemKind::Control => item_kind::CONTROL,
        ItemKind::Action => item_kind::ACTION,
    }
}

/// Writes a device's manifest, description and identity. Returns the length.
pub fn encode_description(
    manifest: &Manifest,
    description: &Description,
    identity: &Identity<'_>,
    buf: &mut [u8],
) -> Result<usize> {
    encode_as(manifest, description, identity, buf, Layout::Current)
}

/// Which layout of the item trailer to write. `BeforeUnitState` is what a host from before the
/// unit state and the real-valued ranges sends: the items' extras end at the group. Only the
/// tests write it, to check that a current reader still decodes an old description.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    Current,
    BeforeUnitState,
}

/// [`encode_description`] with the item trailer in the given layout.
pub(crate) fn encode_as(
    manifest: &Manifest,
    description: &Description,
    identity: &Identity<'_>,
    buf: &mut [u8],
    layout: Layout,
) -> Result<usize> {
    let mut out = Writer::new(buf);
    (manifest.provides.len() as u16).encode(&mut out)?;
    for contract in manifest.provides {
        encode_contract(contract, &mut out)?;
    }
    (description.items().count() as u16).encode(&mut out)?;
    for item in description.items() {
        let MethodId { contract, method } = item.method;
        DescItem {
            kind: kind_code(item.kind),
            name: item.name,
            unit: item.unit.symbol(),
            exponent: item.exponent,
            min: item.min,
            max: item.max,
            contract,
            method,
            field: item.field,
        }
        .encode(&mut out)?;
    }
    // Appended after the description's first form: which methods start a long job. A reader
    // from before it stops at the items.
    let jobs = manifest
        .provides
        .iter()
        .flat_map(|contract| contract.methods.iter())
        .filter(|method| method.job)
        .count();
    (jobs as u16).encode(&mut out)?;
    for (index, contract) in manifest.provides.iter().enumerate() {
        for method in contract.methods.iter().filter(|method| method.job) {
            DescJob {
                contract: index as u8,
                method: method.id,
            }
            .encode(&mut out)?;
        }
    }
    encode_trailer(manifest, description, identity, &mut out, layout)?;
    // Appended after the trailer: the hash of every added field of every method. A host from before
    // it stops at the trailer.
    let mut added = 0usize;
    for contract in manifest.provides {
        for method in contract.methods {
            lemnos_core::layout::added_fields(method, &mut |_, _| added += 1);
        }
    }
    (added as u16).encode(&mut out)?;
    for (index, contract) in manifest.provides.iter().enumerate() {
        for method in contract.methods {
            let mut result = Ok(());
            lemnos_core::layout::added_fields(method, &mut |_, hash| {
                if result.is_ok() {
                    result = DescAdded {
                        contract: index as u8,
                        method: method.id,
                        hash,
                    }
                    .encode(&mut out);
                }
            });
            result?;
        }
    }
    Ok(out.len())
}

/// The identity and the per-contract and per-item extras, appended after the job list. A reader
/// from before them stops at the job list.
pub(crate) fn encode_trailer(
    manifest: &Manifest,
    description: &Description,
    identity: &Identity<'_>,
    out: &mut Writer<'_>,
    layout: Layout,
) -> Result<()> {
    DescIdentity {
        label: identity.label,
        bus: identity.bus,
        state: Some(identity.state),
        reason: identity.reason,
        address: description.address(),
        kind: device_kind(manifest),
        location: description.location(),
    }
    .encode(out)?;
    (manifest.provides.len() as u16).encode(out)?;
    for contract in manifest.provides {
        DescContractKind {
            kind: contract.kind,
        }
        .encode(out)?;
    }
    (description.items().count() as u16).encode(out)?;
    for item in description.items() {
        let reading = effective_reading(manifest, item).code();
        if layout == Layout::BeforeUnitState {
            DescItemExtraBefore {
                reading,
                step: item.step,
                warn_low: item.warn_low,
                warn_high: item.warn_high,
                group: item.group,
            }
            .encode(out)?;
            continue;
        }
        DescItemExtra {
            reading,
            step: item.step,
            warn_low: item.warn_low,
            warn_high: item.warn_high,
            group: item.group,
            unit_state: Some(item.unit_state.code()),
            real_min: item.real_min,
            real_max: item.real_max,
            real_step: item.real_step,
        }
        .encode(out)?;
    }
    Ok(())
}

/// Reads what [`encode_description`] wrote.
pub fn decode_description(bytes: &[u8]) -> Result<DeviceDescription> {
    let from = &mut Reader::new(bytes);
    let mut contracts = Vec::new();
    for _ in 0..u16::decode(from)? {
        let header = DescContract::decode(from)?;
        let mut methods = Vec::new();
        for _ in 0..header.methods {
            let method = DescMethod::decode(from)?;
            let mut params = Vec::new();
            for _ in 0..method.params {
                let name = DescParam::decode(from)?.name.to_string();
                let shape = decode_shape(from, MAX_SHAPE_DEPTH)?;
                params.push(OwnedParam { name, shape });
            }
            let returns = decode_shape(from, MAX_SHAPE_DEPTH)?;
            methods.push(OwnedMethod {
                id: method.id,
                name: method.name.to_string(),
                params,
                returns,
                has_out_buffer: method.has_out_buffer,
                job: false,
                hash: method.hash,
                added: method.added,
                added_fields: Vec::new(),
            });
        }
        contracts.push(OwnedContract {
            tag: header.tag.to_string(),
            kind: String::new(),
            hash: header.hash,
            methods,
        });
    }
    let mut items = Vec::new();
    for _ in 0..u16::decode(from)? {
        let item = DescItem::decode(from)?;
        let kind = match item.kind {
            item_kind::VALUE => ItemKind::Value,
            item_kind::CONTROL => ItemKind::Control,
            item_kind::ACTION => ItemKind::Action,
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        items.push(OwnedItem {
            kind,
            name: item.name.to_string(),
            unit: item.unit.to_string(),
            unit_state: UnitState::from_unit(item.unit),
            exponent: item.exponent,
            min: item.min,
            max: item.max,
            real_min: None,
            real_max: None,
            real_step: None,
            contract: item.contract,
            method: item.method,
            field: item.field.map(str::to_string),
            reading: None,
            step: None,
            warn_low: None,
            warn_high: None,
            group: None,
        });
    }
    // A host from before job methods ends here.
    if from.remaining() > 0 {
        for _ in 0..u16::decode(from)? {
            let job = DescJob::decode(from)?;
            let found = contracts
                .get_mut(usize::from(job.contract))
                .and_then(|c| c.methods.iter_mut().find(|m| m.id == job.method))
                .ok_or(Status::INVALID_ARGUMENT)?;
            found.job = true;
        }
    }
    let mut out = DeviceDescription {
        contracts,
        items,
        ..DeviceDescription::default()
    };
    // A host from before the identity ends here: its fields read as unknown.
    if from.remaining() > 0 {
        let identity = Tail::<DescIdentity>::decode(from)?.0;
        out.label = identity.label.to_string();
        out.bus = identity.bus.to_string();
        out.state = identity.state;
        out.reason = identity.reason.to_string();
        out.address = identity.address;
        out.kind = identity.kind.to_string();
        out.location = identity.location.to_string();
        for index in 0..usize::from(u16::decode(from)?) {
            let kind = DescContractKind::decode(from)?.kind.to_string();
            if let Some(contract) = out.contracts.get_mut(index) {
                contract.kind = kind;
            }
        }
        let items = usize::from(u16::decode(from)?);
        for index in 0..items {
            let extra = DescItemExtra::decode(from)?;
            if let Some(item) = out.items.get_mut(index) {
                item.reading = Reading::from_code(extra.reading);
                item.step = extra.step;
                item.warn_low = extra.warn_low;
                item.warn_high = extra.warn_high;
                item.group = extra.group.map(str::to_string);
                if let Some(state) = extra.unit_state.and_then(UnitState::from_code) {
                    item.unit_state = state;
                }
                item.real_min = extra.real_min;
                item.real_max = extra.real_max;
                item.real_step = extra.real_step;
            }
        }
    }
    // A host from before added fields ends here.
    if from.remaining() > 0 {
        for _ in 0..u16::decode(from)? {
            let added = DescAdded::decode(from)?;
            let found = out
                .contracts
                .get_mut(usize::from(added.contract))
                .and_then(|c| c.methods.iter_mut().find(|m| m.id == added.method))
                .ok_or(Status::INVALID_ARGUMENT)?;
            found.added_fields.push(added.hash);
        }
    }
    Ok(out)
}
