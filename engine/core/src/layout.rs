//! Layout hashes. A hash is FNV-1a over a fixed walk of a type's shape, computed at compile time,
//! so a change to any field, name, number or kind moves it. Nothing here is a hand-kept number:
//! the walks read the same [`Shape`]s the encoders and decoders are generated from.

use crate::{ContractInfo, Shape};

/// The FNV-1a offset basis: where every walk starts.
pub const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Folds one byte into `hash`.
pub const fn byte(hash: u64, value: u8) -> u64 {
    (hash ^ value as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

/// Folds a `u64`, little-endian, into `hash`.
pub const fn word(mut hash: u64, value: u64) -> u64 {
    let mut i = 0;
    while i < 8 {
        hash = byte(hash, (value >> (8 * i)) as u8);
        i += 1;
    }
    hash
}

/// Folds a string into `hash`, its length first so that no two walks run together.
pub const fn text(hash: u64, value: &str) -> u64 {
    let bytes = value.as_bytes();
    let mut hash = word(hash, bytes.len() as u64);
    let mut i = 0;
    while i < bytes.len() {
        hash = byte(hash, bytes[i]);
        i += 1;
    }
    hash
}

/// Folds a [`Shape`] into `hash`: its kind, and for a record each field's name and shape, in
/// order.
pub const fn shape(hash: u64, value: &Shape) -> u64 {
    match value {
        Shape::Unit => word(hash, 0),
        Shape::Bool => word(hash, 1),
        Shape::U8 => word(hash, 2),
        Shape::U16 => word(hash, 3),
        Shape::U32 => word(hash, 4),
        Shape::U64 => word(hash, 5),
        Shape::I8 => word(hash, 6),
        Shape::I16 => word(hash, 7),
        Shape::I32 => word(hash, 8),
        Shape::I64 => word(hash, 9),
        Shape::F32 => word(hash, 10),
        Shape::F64 => word(hash, 11),
        Shape::Str => word(hash, 12),
        Shape::Bytes => word(hash, 13),
        Shape::Option(inner) => shape(word(hash, 14), inner),
        Shape::Array(inner, count) => shape(word(word(hash, 15), *count as u64), inner),
        Shape::Record(fields) => {
            let mut hash = word(word(hash, 16), fields.len() as u64);
            let mut i = 0;
            while i < fields.len() {
                hash = shape(text(hash, fields[i].name), &fields[i].shape);
                i += 1;
            }
            hash
        }
    }
}

/// Folds a contract's tag and every method into `hash`: its number, name, parameters (names and
/// shapes), result shape, and whether it fills a buffer or starts a job.
pub const fn contract(hash: u64, info: &ContractInfo) -> u64 {
    let mut hash = text(hash, info.tag.as_str());
    hash = word(hash, info.methods.len() as u64);
    let mut m = 0;
    while m < info.methods.len() {
        let method = &info.methods[m];
        hash = text(word(hash, method.id as u64), method.name);
        hash = word(hash, method.params.len() as u64);
        let mut p = 0;
        while p < method.params.len() {
            hash = shape(text(hash, method.params[p].name), &method.params[p].shape);
            p += 1;
        }
        hash = shape(hash, &method.returns);
        hash = word(word(hash, method.has_out_buffer as u64), method.job as u64);
        m += 1;
    }
    hash
}

/// The encoding hash of one contract: what a client and a device must agree on for a call to
/// mean the same thing. It moves when any argument or result struct gains, loses or renames a
/// field, and when a method is added, renamed or renumbered.
pub const fn contract_hash(info: &ContractInfo) -> u64 {
    contract(OFFSET, info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MethodInfo, Param, Tag};

    const FIELDS: &[crate::Field] = &[
        crate::Field {
            name: "a",
            shape: Shape::U32,
        },
        crate::Field {
            name: "b",
            shape: Shape::Bool,
        },
    ];
    const EXTRA: &[crate::Field] = &[
        crate::Field {
            name: "a",
            shape: Shape::U32,
        },
        crate::Field {
            name: "b",
            shape: Shape::Bool,
        },
        crate::Field {
            name: "c",
            shape: Shape::U8,
        },
    ];
    const RETYPED: &[crate::Field] = &[
        crate::Field {
            name: "a",
            shape: Shape::U64,
        },
        crate::Field {
            name: "b",
            shape: Shape::Bool,
        },
    ];
    const RENAMED: &[crate::Field] = &[
        crate::Field {
            name: "a",
            shape: Shape::U32,
        },
        crate::Field {
            name: "z",
            shape: Shape::Bool,
        },
    ];

    /// A contract with one method taking one argument struct of `$fields`, as a constant so the
    /// tables it points at live for the whole test.
    macro_rules! with {
        ($fields:expr) => {
            ContractInfo {
                tag: Tag::new("t/1"),
                kind: "other",
                methods: &[MethodInfo {
                    id: 1,
                    name: "run",
                    params: &[Param {
                        name: "args",
                        shape: Shape::Record($fields),
                    }],
                    returns: Shape::Unit,
                    has_out_buffer: false,
                    job: false,
                    reading: None,
                }],
            }
        };
    }

    #[test]
    fn a_field_added_to_an_argument_struct_moves_the_contract_hash() {
        const BEFORE: u64 = contract_hash(&with!(FIELDS));
        const AFTER: u64 = contract_hash(&with!(EXTRA));
        assert_ne!(BEFORE, AFTER);
    }

    #[test]
    fn a_field_renamed_or_retyped_moves_it_too() {
        let base = contract_hash(&with!(FIELDS));
        assert_ne!(base, contract_hash(&with!(RETYPED)));
        assert_ne!(base, contract_hash(&with!(RENAMED)));
    }

    #[test]
    fn the_same_contract_hashes_the_same() {
        assert_eq!(contract_hash(&with!(FIELDS)), contract_hash(&with!(FIELDS)));
    }
}
