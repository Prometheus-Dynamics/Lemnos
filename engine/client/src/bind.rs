//! The bind check: what a typed client compares with a device before it calls anything. The
//! blocking client, the async client and the polled session all run this same check.
//!
//! The rule, for a contract `C` the client was built with:
//! 1. the device provides the contract's tag;
//! 2. the device's base hash for it equals the client's base hash (`C::HASH`): the base methods
//!    are laid out the same;
//! 3. for every method the client has, base or added, the device has a method with the same
//!    number and name and the same method hash; then, for every method, the device has every
//!    added field the client's method carries (by the field's own hash). The methods are all
//!    checked before any field, so a missing method is the one named.
//!
//! Extra methods and fields on the device are ignored. Nothing is called on a refused contract.

use lemnos_core::{Contract, layout};

use crate::DeviceDescription;
use crate::error::Error;

pub(crate) fn verify<C: Contract + ?Sized>(description: &DeviceDescription) -> Result<(), Error> {
    let contract = C::TAG.as_str();
    let Some(theirs) = description
        .contracts
        .iter()
        .find(|found| found.tag == contract)
    else {
        return Err(Error::ContractMissing {
            contract: contract.to_string(),
        });
    };
    if theirs.hash != C::HASH {
        return Err(Error::ContractMismatch {
            contract: contract.to_string(),
            ours: C::HASH,
            theirs: theirs.hash,
        });
    }
    // Every method first, so a method the device lacks is named before any field of another.
    for method in C::INFO.methods {
        let Some(found) = theirs
            .methods
            .iter()
            .find(|found| found.id == method.id && found.name == method.name)
        else {
            return Err(Error::MethodMissing {
                contract: contract.to_string(),
                method: method.id,
                name: method.name.to_string(),
            });
        };
        if found.hash != layout::method_hash(method) {
            return Err(Error::MethodShape {
                contract: contract.to_string(),
                method: method.id,
                name: method.name.to_string(),
            });
        }
    }
    for method in C::INFO.methods {
        // Present and of the same shape: the first pass checked it.
        let Some(found) = theirs
            .methods
            .iter()
            .find(|found| found.id == method.id && found.name == method.name)
        else {
            continue;
        };
        let mut missing = None;
        layout::added_fields(method, &mut |field, hash| {
            if missing.is_none() && !found.added_fields.contains(&hash) {
                missing = Some(field);
            }
        });
        if let Some(field) = missing {
            return Err(Error::FieldMissing {
                contract: contract.to_string(),
                method: method.id,
                name: method.name.to_string(),
                field: field.to_string(),
            });
        }
    }
    Ok(())
}
