//! Layout hashes. A hash is FNV-1a over a fixed walk of a type's shape, computed at compile time,
//! so a change to any field, name, number or kind moves it. Nothing here is a hand-kept number:
//! the walks read the same [`Shape`]s the encoders and decoders are generated from.
//!
//! Each contract has a **base shape**: its tag, its base methods, and for each base method its
//! base parameters and result. An addition (a method or a field marked `#[added]`, and not
//! `#[breaking]`) is left out of the base, and has its own hash: a method's [`method_hash`], a
//! field's hash from [`added_fields`]. Two sides that agree on the base can differ in additions,
//! and each side's additions are then checked on their own at bind.

use crate::{ContractInfo, Field, MethodInfo, Shape};

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

/// How many fields of `fields` are in the base shape.
pub const fn base_fields(fields: &[Field]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < fields.len() {
        if !fields[i].added {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Folds a [`Shape`] into `hash`: its kind, and for a record each base field's name and shape, in
/// order. Added fields are left out.
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
            let mut hash = word(word(hash, 16), base_fields(fields) as u64);
            let mut i = 0;
            while i < fields.len() {
                if !fields[i].added {
                    hash = shape(text(hash, fields[i].name), &fields[i].shape);
                }
                i += 1;
            }
            hash
        }
    }
}

/// Folds one method into `hash`: its number, name, parameters (names and base shapes), base
/// result shape, and whether it fills a buffer or starts a job.
pub const fn method(hash: u64, info: &MethodInfo) -> u64 {
    let mut hash = text(word(hash, info.id as u64), info.name);
    hash = word(hash, info.params.len() as u64);
    let mut p = 0;
    while p < info.params.len() {
        hash = shape(text(hash, info.params[p].name), &info.params[p].shape);
        p += 1;
    }
    hash = shape(hash, &info.returns);
    word(word(hash, info.has_out_buffer as u64), info.job as u64)
}

/// The hash of one method: what a client and a device must agree on for a call to mean the same
/// thing. A method's added fields are not in it (each has its own hash, see [`added_fields`]), so
/// a method that gained a field keeps its hash. A device says each method's hash in its
/// description, and a typed client compares each of its own methods with it.
pub const fn method_hash(info: &MethodInfo) -> u64 {
    method(OFFSET, info)
}

/// Folds a contract's tag and its base methods into `hash`: each base method's
/// [`method_hash`]. Added methods are left out.
pub const fn contract(hash: u64, info: &ContractInfo) -> u64 {
    let mut hash = text(hash, info.tag.as_str());
    let mut base = 0;
    let mut m = 0;
    while m < info.methods.len() {
        if !info.methods[m].added {
            base += 1;
        }
        m += 1;
    }
    hash = word(hash, base);
    let mut m = 0;
    while m < info.methods.len() {
        if !info.methods[m].added {
            hash = word(hash, method_hash(&info.methods[m]));
        }
        m += 1;
    }
    hash
}

/// The base hash of one contract: what a client and a device must agree on for the contract to
/// mean the same thing. It moves when a base method is added, renamed, renumbered or changed, and
/// when a base field of an argument or result struct changes. An added method or field does not
/// move it.
pub const fn contract_hash(info: &ContractInfo) -> u64 {
    contract(OFFSET, info)
}

/// The additions level of a contract: how many of its methods are added. Diagnostics only; a
/// bind checks each method, so the level is not compared.
pub const fn level(info: &ContractInfo) -> usize {
    let mut count = 0;
    let mut m = 0;
    while m < info.methods.len() {
        if info.methods[m].added {
            count += 1;
        }
        m += 1;
    }
    count
}

/// The hash of one added field: its name and shape, under the name of the path it sits on (the
/// parameter or result it belongs to, and each record above it), so two fields of the same name
/// on different paths differ.
pub fn added_field_hash(path: u64, field: &Field) -> u64 {
    shape(text(path, field.name), &field.shape)
}

/// Calls `found` with the name and hash of every added field a method's parameters and result
/// carry, at any depth. A method's fields are checked this way at bind: a client that uses an
/// added field needs a device whose method has it.
pub fn added_fields(info: &MethodInfo, found: &mut dyn FnMut(&'static str, u64)) {
    for param in info.params {
        walk_added(text(OFFSET, param.name), &param.shape, found);
    }
    walk_added(text(OFFSET, "returns"), &info.returns, found);
}

fn walk_added(path: u64, value: &Shape, found: &mut dyn FnMut(&'static str, u64)) {
    match value {
        Shape::Option(inner) | Shape::Array(inner, _) => walk_added(path, inner, found),
        Shape::Record(fields) => {
            for field in *fields {
                if field.added {
                    found(field.name, added_field_hash(path, field));
                } else {
                    walk_added(text(path, field.name), &field.shape, found);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MethodInfo, Param, Tag};

    const FIELDS: &[Field] = &[
        Field {
            name: "a",
            shape: Shape::U32,
            added: false,
        },
        Field {
            name: "b",
            shape: Shape::Bool,
            added: false,
        },
    ];
    const EXTRA: &[Field] = &[
        Field {
            name: "a",
            shape: Shape::U32,
            added: false,
        },
        Field {
            name: "b",
            shape: Shape::Bool,
            added: false,
        },
        Field {
            name: "c",
            shape: Shape::U8,
            added: true,
        },
    ];
    const EXTRA_BREAKING: &[Field] = &[
        Field {
            name: "a",
            shape: Shape::U32,
            added: false,
        },
        Field {
            name: "b",
            shape: Shape::Bool,
            added: false,
        },
        Field {
            name: "c",
            shape: Shape::U8,
            added: false,
        },
    ];
    const RETYPED: &[Field] = &[
        Field {
            name: "a",
            shape: Shape::U64,
            added: false,
        },
        Field {
            name: "b",
            shape: Shape::Bool,
            added: false,
        },
    ];
    const RENAMED: &[Field] = &[
        Field {
            name: "a",
            shape: Shape::U32,
            added: false,
        },
        Field {
            name: "z",
            shape: Shape::Bool,
            added: false,
        },
    ];

    /// A contract with one base method taking one argument struct of `$fields`.
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
                    added: false,
                }],
            }
        };
    }

    const BASE_METHOD: MethodInfo = MethodInfo {
        id: 1,
        name: "run",
        params: &[],
        returns: Shape::Unit,
        has_out_buffer: false,
        job: false,
        reading: None,
        added: false,
    };

    /// A second method, appended: an addition or a base method.
    const SECOND_ADDED: MethodInfo = MethodInfo {
        id: 2,
        name: "more",
        params: &[],
        returns: Shape::U8,
        has_out_buffer: false,
        job: false,
        reading: None,
        added: true,
    };

    const SECOND_BASE: MethodInfo = MethodInfo {
        added: false,
        ..SECOND_ADDED
    };

    const ONLY_BASE: ContractInfo = ContractInfo {
        tag: Tag::new("t/1"),
        kind: "other",
        methods: &[BASE_METHOD],
    };

    const WITH_ADDED: ContractInfo = ContractInfo {
        tag: Tag::new("t/1"),
        kind: "other",
        methods: &[BASE_METHOD, SECOND_ADDED],
    };

    const WITH_BASE: ContractInfo = ContractInfo {
        tag: Tag::new("t/1"),
        kind: "other",
        methods: &[BASE_METHOD, SECOND_BASE],
    };

    #[test]
    fn a_field_added_to_an_argument_struct_moves_the_base_when_it_is_base() {
        const BEFORE: u64 = contract_hash(&with!(FIELDS));
        const AFTER: u64 = contract_hash(&with!(EXTRA_BREAKING));
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

    #[test]
    fn an_added_field_does_not_move_the_base_or_the_method_hash() {
        let base = contract_hash(&with!(FIELDS));
        assert_eq!(base, contract_hash(&with!(EXTRA)));
        assert_eq!(
            method_hash(&with!(FIELDS).methods[0]),
            method_hash(&with!(EXTRA).methods[0])
        );
    }

    #[test]
    fn an_added_field_is_its_own_hash_on_its_path() {
        use core::cell::Cell;
        let count = Cell::new(0);
        let name = Cell::new("");
        let hash = Cell::new(0);
        added_fields(&with!(EXTRA).methods[0], &mut |found, value| {
            count.set(count.get() + 1);
            name.set(found);
            hash.set(value);
        });
        assert_eq!(count.get(), 1);
        assert_eq!(name.get(), "c");
        assert_ne!(hash.get(), 0);
        let none = Cell::new(0);
        added_fields(&with!(FIELDS).methods[0], &mut |_, _| {
            none.set(none.get() + 1)
        });
        assert_eq!(none.get(), 0);
    }

    #[test]
    fn a_breaking_added_field_moves_the_base() {
        assert_ne!(
            contract_hash(&with!(FIELDS)),
            contract_hash(&with!(EXTRA_BREAKING))
        );
    }

    #[test]
    fn an_added_method_does_not_move_the_base() {
        // A contract with a method appended and marked added has the base of one without it.
        assert_eq!(contract_hash(&WITH_ADDED), contract_hash(&ONLY_BASE));
    }

    #[test]
    fn a_base_method_moves_the_base() {
        assert_ne!(contract_hash(&WITH_BASE), contract_hash(&ONLY_BASE));
    }

    #[test]
    fn the_level_counts_the_added_methods() {
        assert_eq!(level(&WITH_BASE), 0);
        assert_eq!(level(&WITH_ADDED), 1);
        assert_eq!(level(&ONLY_BASE), 0);
    }

    #[test]
    fn a_method_hash_is_its_own() {
        assert_ne!(method_hash(&BASE_METHOD), method_hash(&SECOND_ADDED));
    }
}
