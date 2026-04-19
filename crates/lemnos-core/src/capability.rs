use crate::{CapabilityId, CoreResult, ValueMap};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapabilityAccess {
    pub read: bool,
    pub write: bool,
    pub configure: bool,
    pub stream: bool,
}

impl CapabilityAccess {
    pub const READ: Self = Self {
        read: true,
        write: false,
        configure: false,
        stream: false,
    };
    pub const WRITE: Self = Self {
        read: false,
        write: true,
        configure: false,
        stream: false,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        configure: false,
        stream: false,
    };
    pub const CONFIGURE: Self = Self {
        read: false,
        write: false,
        configure: true,
        stream: false,
    };
    pub const READ_WRITE_CONFIGURE: Self = Self {
        read: true,
        write: true,
        configure: true,
        stream: false,
    };
    pub const STREAM: Self = Self {
        read: false,
        write: false,
        configure: false,
        stream: true,
    };
    pub const FULL: Self = Self {
        read: true,
        write: true,
        configure: true,
        stream: true,
    };

    pub const fn supports_read(self) -> bool {
        self.read
    }

    pub const fn supports_write(self) -> bool {
        self.write
    }

    pub const fn supports_configure(self) -> bool {
        self.configure
    }

    pub const fn supports_stream(self) -> bool {
        self.stream
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub summary: Option<String>,
    pub access: CapabilityAccess,
    pub properties: ValueMap,
}

impl CapabilityDescriptor {
    pub fn new(id: impl Into<String>, access: CapabilityAccess) -> CoreResult<Self> {
        Ok(Self {
            id: CapabilityId::new(id)?,
            summary: None,
            access,
            properties: ValueMap::new(),
        })
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<crate::Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_access_helpers_work() {
        assert!(CapabilityAccess::READ_WRITE.supports_read());
        assert!(CapabilityAccess::READ_WRITE.supports_write());
        assert!(!CapabilityAccess::READ_WRITE.supports_stream());
    }

    #[test]
    fn capability_descriptor_collects_properties() {
        let capability = CapabilityDescriptor::new("gpio.write", CapabilityAccess::WRITE)
            .expect("capability")
            .with_summary("Digital output")
            .with_property("initial_value", true);

        assert_eq!(capability.id.as_str(), "gpio.write");
        assert_eq!(
            capability.properties.get("initial_value"),
            Some(&crate::Value::from(true))
        );
    }
}
