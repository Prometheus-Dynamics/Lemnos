use crate::{CoreError, CoreResult};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt;

const EXTRA_IDENTIFIER_CHARS: &[char] = &['.', '_', '-', ':', '/', '+', '#'];

fn validate_identifier(kind: &'static str, value: &str) -> CoreResult<()> {
    if value.is_empty() {
        return Err(CoreError::EmptyIdentifier { kind });
    }

    let mut chars = value.chars();
    let first = chars.next().ok_or(CoreError::EmptyIdentifier { kind })?;
    if !first.is_ascii_alphanumeric() {
        return Err(CoreError::InvalidIdentifierStart {
            kind,
            value: value.to_string(),
        });
    }

    for ch in chars {
        if ch.is_ascii_alphanumeric() || EXTRA_IDENTIFIER_CHARS.contains(&ch) {
            continue;
        }

        return Err(CoreError::InvalidIdentifierCharacter {
            kind,
            value: value.to_string(),
            invalid: ch,
        });
    }

    Ok(())
}

macro_rules! identifier_type {
    ($name:ident, $kind:literal) => {
        #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
        #[cfg_attr(feature = "serde", serde(try_from = "String", into = "String"))]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> CoreResult<Self> {
                let value = value.into();
                validate_identifier($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.into_string()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl TryFrom<&str> for $name {
            type Error = CoreError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

identifier_type!(DeviceId, "device id");
identifier_type!(LocalDeviceId, "local device id");
identifier_type!(CapabilityId, "capability id");
identifier_type!(InteractionId, "interaction id");
identifier_type!(IssueCode, "issue code");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_device_identifier_shapes() {
        let id = DeviceId::new("usb.1-2:1.0/dev#3").expect("valid identifier");
        assert_eq!(id.as_str(), "usb.1-2:1.0/dev#3");
    }

    #[test]
    fn rejects_empty_identifier() {
        let err = DeviceId::new("").expect_err("empty identifier should fail");
        assert!(matches!(err, CoreError::EmptyIdentifier { .. }));
    }

    #[test]
    fn rejects_invalid_first_character() {
        let err = InteractionId::new("_gpio.read")
            .expect_err("identifier with invalid first character should fail");
        assert!(matches!(err, CoreError::InvalidIdentifierStart { .. }));
    }

    #[test]
    fn rejects_whitespace() {
        let err = IssueCode::new("gpio write").expect_err("whitespace should fail");
        assert!(matches!(
            err,
            CoreError::InvalidIdentifierCharacter { invalid: ' ', .. }
        ));
    }
}
