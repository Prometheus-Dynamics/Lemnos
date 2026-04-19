use syn::{Ident, LitStr, Type};

use super::super::helpers::SupportedInterface;
use super::super::helpers::option_inner_type;

#[derive(Default)]
pub(super) struct FieldAnnotations {
    pub(super) bus_interface: Option<BusInterface>,
    pub(super) endpoint: Option<(EndpointInterface, LitStr)>,
    pub(super) signal_gpio: Option<LitStr>,
    pub(super) display_name: bool,
    pub(super) labels: Vec<String>,
    pub(super) properties: Vec<String>,
}

pub(super) fn supports_configured_bus_endpoints(interface: SupportedInterface) -> bool {
    matches!(interface, SupportedInterface::I2c | SupportedInterface::Spi)
}

pub(super) fn populate_named_field_optionality(fields: &mut [NamedField]) {
    for field in fields {
        let inner_ty = option_inner_type(&field.ty).cloned();
        field.optional = inner_ty.is_some();
        field.inner_ty = inner_ty;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BusInterface {
    I2c,
    Spi,
}

impl BusInterface {
    pub(super) fn type_error(self) -> &'static str {
        match self {
            Self::I2c => "`bus(i2c)` fields must have type `u32`",
            Self::Spi => "`bus(spi)` fields must have type `u32`",
        }
    }

    pub(super) fn matches_configured_interface(
        self,
        configured_interface: SupportedInterface,
    ) -> bool {
        matches!(
            (self, configured_interface),
            (Self::I2c, SupportedInterface::I2c) | (Self::Spi, SupportedInterface::Spi)
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointInterface {
    I2c,
    Spi,
}

impl EndpointInterface {
    pub(super) fn type_error(self) -> &'static str {
        match self {
            Self::I2c => "`endpoint(i2c, ..)` fields must have type `u16`",
            Self::Spi => "`endpoint(spi, ..)` fields must have type `u16`",
        }
    }

    pub(super) fn matches_configured_interface(
        self,
        configured_interface: SupportedInterface,
    ) -> bool {
        matches!(
            (self, configured_interface),
            (Self::I2c, SupportedInterface::I2c) | (Self::Spi, SupportedInterface::Spi)
        )
    }
}

#[derive(Clone)]
pub(crate) struct AnnotatedField {
    pub(crate) ident: Ident,
    pub(crate) ty: Type,
    pub(crate) interface: Option<BusInterface>,
}

impl AnnotatedField {
    pub(super) fn new(ident: Ident, ty: Type, interface: Option<BusInterface>) -> Self {
        Self {
            ident,
            ty,
            interface,
        }
    }
}

#[derive(Clone)]
pub(crate) struct EndpointField {
    pub(crate) ident: Ident,
    pub(crate) ty: Type,
    pub(crate) name: LitStr,
    pub(crate) interface: EndpointInterface,
}

impl EndpointField {
    pub(super) fn new(ident: Ident, ty: Type, name: LitStr, interface: EndpointInterface) -> Self {
        Self {
            ident,
            ty,
            name,
            interface,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SignalField {
    pub(crate) ident: Ident,
    pub(crate) ty: Type,
    pub(crate) name: LitStr,
    pub(crate) optional: bool,
}

impl SignalField {
    pub(super) fn new(ident: Ident, ty: Type, name: LitStr) -> Self {
        Self {
            ident,
            ty,
            name,
            optional: false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct NamedField {
    pub(crate) ident: Ident,
    pub(crate) ty: Type,
    pub(crate) key: String,
    pub(crate) optional: bool,
    pub(crate) inner_ty: Option<Type>,
}

impl NamedField {
    pub(super) fn new(ident: Ident, ty: Type, key: String) -> Self {
        Self {
            ident,
            ty,
            key,
            optional: false,
            inner_ty: None,
        }
    }
}
