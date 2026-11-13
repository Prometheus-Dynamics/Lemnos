//! A device's self-description as bytes, and the owned form a client keeps.

use lemnos_core::{
    ContractInfo, Decode, Description, Encode, ItemKind, Manifest, MethodId, Reader, Result, Shape,
    Status, Writer,
};

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

fn encode_shape(shape: &Shape, out: &mut Writer<'_>) -> Result<()> {
    let code: u8 = match shape {
        Shape::Unit => 0,
        Shape::Bool => 1,
        Shape::U8 => 2,
        Shape::U16 => 3,
        Shape::U32 => 4,
        Shape::U64 => 5,
        Shape::I8 => 6,
        Shape::I16 => 7,
        Shape::I32 => 8,
        Shape::I64 => 9,
        Shape::F32 => 10,
        Shape::F64 => 11,
        Shape::Str => 12,
        Shape::Bytes => 13,
        Shape::Option(inner) => {
            14u8.encode(out)?;
            return encode_shape(inner, out);
        }
        Shape::Array(inner, n) => {
            15u8.encode(out)?;
            n.encode(out)?;
            return encode_shape(inner, out);
        }
        Shape::Record(fields) => {
            16u8.encode(out)?;
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
        0 => OwnedShape::Unit,
        1 => OwnedShape::Bool,
        2 => OwnedShape::U8,
        3 => OwnedShape::U16,
        4 => OwnedShape::U32,
        5 => OwnedShape::U64,
        6 => OwnedShape::I8,
        7 => OwnedShape::I16,
        8 => OwnedShape::I32,
        9 => OwnedShape::I64,
        10 => OwnedShape::F32,
        11 => OwnedShape::F64,
        12 => OwnedShape::Str,
        13 => OwnedShape::Bytes,
        14 => OwnedShape::Option(Box::new(decode_shape(from, depth - 1)?)),
        15 => {
            let n = u32::decode(from)?;
            OwnedShape::Array(Box::new(decode_shape(from, depth - 1)?), n)
        }
        16 => {
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
    contract.tag.as_str().encode(out)?;
    (contract.methods.len() as u16).encode(out)?;
    for method in contract.methods {
        method.id.encode(out)?;
        method.name.encode(out)?;
        (method.params.len() as u16).encode(out)?;
        for param in method.params {
            param.name.encode(out)?;
            encode_shape(&param.shape, out)?;
        }
        encode_shape(&method.returns, out)?;
        method.has_out_buffer.encode(out)?;
    }
    Ok(())
}

const MAX_SHAPE_DEPTH: u32 = 8;

fn kind_code(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Value => 0,
        ItemKind::Control => 1,
        ItemKind::Action => 2,
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
        kind_code(item.kind).encode(&mut out)?;
        item.name.encode(&mut out)?;
        item.unit.symbol().encode(&mut out)?;
        item.exponent.encode(&mut out)?;
        item.min.encode(&mut out)?;
        item.max.encode(&mut out)?;
        contract.encode(&mut out)?;
        method.encode(&mut out)?;
        item.field.encode(&mut out)?;
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
            (index as u8).encode(&mut out)?;
            method.id.encode(&mut out)?;
        }
    }
    Ok(out.len())
}

/// Reads what [`encode_description`] wrote.
pub fn decode_description(bytes: &[u8]) -> Result<DeviceDescription> {
    let from = &mut Reader::new(bytes);
    let mut contracts = Vec::new();
    for _ in 0..u16::decode(from)? {
        let tag = <&str>::decode(from)?.to_string();
        let mut methods = Vec::new();
        for _ in 0..u16::decode(from)? {
            let id = u16::decode(from)?;
            let name = <&str>::decode(from)?.to_string();
            let mut params = Vec::new();
            for _ in 0..u16::decode(from)? {
                let name = <&str>::decode(from)?.to_string();
                let shape = decode_shape(from, MAX_SHAPE_DEPTH)?;
                params.push(OwnedParam { name, shape });
            }
            let returns = decode_shape(from, MAX_SHAPE_DEPTH)?;
            let has_out_buffer = bool::decode(from)?;
            methods.push(OwnedMethod {
                id,
                name,
                params,
                returns,
                has_out_buffer,
                job: false,
            });
        }
        contracts.push(OwnedContract { tag, methods });
    }
    let mut items = Vec::new();
    for _ in 0..u16::decode(from)? {
        let kind = match u8::decode(from)? {
            0 => ItemKind::Value,
            1 => ItemKind::Control,
            2 => ItemKind::Action,
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        items.push(OwnedItem {
            kind,
            name: <&str>::decode(from)?.to_string(),
            unit: <&str>::decode(from)?.to_string(),
            exponent: i8::decode(from)?,
            min: Option::<i64>::decode(from)?,
            max: Option::<i64>::decode(from)?,
            contract: u8::decode(from)?,
            method: u16::decode(from)?,
            field: Option::<&str>::decode(from)?.map(str::to_string),
        });
    }
    // A host from before job methods ends here.
    if from.remaining() > 0 {
        for _ in 0..u16::decode(from)? {
            let contract = u8::decode(from)?;
            let method = u16::decode(from)?;
            let found = contracts
                .get_mut(usize::from(contract))
                .and_then(|c| c.methods.iter_mut().find(|m| m.id == method))
                .ok_or(Status::INVALID_ARGUMENT)?;
            found.job = true;
        }
    }
    Ok(DeviceDescription { contracts, items })
}
