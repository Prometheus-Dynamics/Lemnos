//! A device's self-description as bytes, and the owned form a client keeps.
//!
//! The records are declared with the same derive as the frames' values: each one is a block
//! (see `lemnos_core::Encode`), and its shape is folded into the core hash by [`fold_framing`]. The
//! counts before each list and the shape codes are constants, folded the same way. A shape a
//! record carries is written with [`encode_shape`], whose codes are in [`shape_code`].

use lemnos_core::{
    ContractInfo, Decode, Description, Encode, ItemKind, Manifest, MethodId, Reader, Result, Shape,
    Status, Writer,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedContract {
    pub tag: String,
    /// The contract's encoding hash ([`lemnos_core::layout::contract_hash`]): equal on both sides
    /// when every argument and result type is laid out the same.
    pub hash: u64,
    pub methods: Vec<OwnedMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedItem {
    pub kind: ItemKind,
    pub name: String,
    pub unit: String,
    pub exponent: i8,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub contract: u8,
    pub method: u16,
    pub field: Option<String>,
}

/// Everything a tool with no code for a device needs to use it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceDescription {
    pub contracts: Vec<OwnedContract>,
    pub items: Vec<OwnedItem>,
}

/// A contract's header: its tag, its hash and how many methods follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescContract<'a> {
    tag: &'a str,
    hash: u64,
    methods: u16,
}

/// A method: its number, name, how many parameters follow, and whether it fills a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct DescMethod<'a> {
    id: u16,
    name: &'a str,
    params: u16,
    has_out_buffer: bool,
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

/// Writes a device's manifest and description. Returns the length.
pub fn encode_description(
    manifest: &Manifest,
    description: &Description,
    buf: &mut [u8],
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
    Ok(out.len())
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
            });
        }
        contracts.push(OwnedContract {
            tag: header.tag.to_string(),
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
            exponent: item.exponent,
            min: item.min,
            max: item.max,
            contract: item.contract,
            method: item.method,
            field: item.field.map(str::to_string),
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
    Ok(DeviceDescription { contracts, items })
}
