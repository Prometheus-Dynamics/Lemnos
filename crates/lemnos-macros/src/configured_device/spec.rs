mod types;

use syn::{Attribute, Data, DeriveInput, Field, Fields, Ident, LitStr};

use super::helpers::{
    option_inner_type, parse_ident_value, parse_optional_key, parse_string_value, type_ident,
    validate_interface, validate_kind, validate_unique_keys, validate_unique_names,
};
pub(crate) use types::EndpointInterface;
use types::{
    AnnotatedField, BusInterface, EndpointField, FieldAnnotations, NamedField, SignalField,
    populate_named_field_optionality, supports_configured_bus_endpoints,
};

#[derive(Default)]
pub(crate) struct ConfiguredDeviceSpec {
    pub(crate) interface: Option<Ident>,
    pub(crate) kind: Option<Ident>,
    pub(crate) id: Option<LitStr>,
    pub(crate) summary: Option<LitStr>,
    pub(crate) driver: Option<LitStr>,
    pub(crate) bus_field: Option<AnnotatedField>,
    pub(crate) endpoint_fields: Vec<EndpointField>,
    pub(crate) signal_fields: Vec<SignalField>,
    pub(crate) display_name_field: Option<AnnotatedField>,
    pub(crate) label_fields: Vec<NamedField>,
    pub(crate) property_fields: Vec<NamedField>,
    pub(crate) saw_descriptor_metadata: bool,
}

impl ConfiguredDeviceSpec {
    pub(crate) fn enabled(&self) -> bool {
        self.saw_descriptor_metadata || self.interface.is_some()
    }

    pub(crate) fn parse(input: &DeriveInput) -> syn::Result<Self> {
        let mut spec = Self::default();
        spec.parse_struct_attrs(&input.attrs)?;

        let Data::Struct(data) = &input.data else {
            return Err(syn::Error::new_spanned(
                input,
                "ConfiguredDevice only supports structs",
            ));
        };
        let Fields::Named(fields) = &data.fields else {
            return Err(syn::Error::new_spanned(
                &data.fields,
                "ConfiguredDevice only supports named-field structs",
            ));
        };

        for field in &fields.named {
            spec.parse_field(field)?;
        }

        spec.validate(input)?;
        Ok(spec)
    }

    fn parse_struct_attrs(&mut self, attrs: &[Attribute]) -> syn::Result<()> {
        for attr in attrs.iter().filter(|attr| attr.path().is_ident("lemnos")) {
            attr.parse_nested_meta(|meta| {
                self.saw_descriptor_metadata = true;
                if meta.path.is_ident("interface") {
                    self.interface = Some(parse_ident_value(&meta)?);
                    return Ok(());
                }
                if meta.path.is_ident("kind") {
                    self.kind = Some(parse_ident_value(&meta)?);
                    return Ok(());
                }
                if meta.path.is_ident("id") {
                    self.id = Some(parse_string_value(&meta)?);
                    return Ok(());
                }
                if meta.path.is_ident("summary") {
                    self.summary = Some(parse_string_value(&meta)?);
                    return Ok(());
                }
                if meta.path.is_ident("driver") {
                    self.driver = Some(parse_string_value(&meta)?);
                    return Ok(());
                }

                Err(meta.error(
                    "unsupported configured-device struct argument; supported keys are `interface`, `kind`, `id`, `summary`, and `driver`",
                ))
            })?;
        }
        Ok(())
    }

    fn parse_field(&mut self, field: &Field) -> syn::Result<()> {
        let Some(field_ident) = field.ident.clone() else {
            return Ok(());
        };

        let mut annotations = FieldAnnotations::default();
        for attr in field
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("lemnos"))
        {
            attr.parse_nested_meta(|meta| {
                self.saw_descriptor_metadata = true;
                if meta.path.is_ident("display_name") {
                    annotations.display_name = true;
                    return Ok(());
                }
                if meta.path.is_ident("label") {
                    annotations
                        .labels
                        .push(parse_optional_key(meta.input, field_ident.to_string())?);
                    return Ok(());
                }
                if meta.path.is_ident("property") {
                    annotations
                        .properties
                        .push(parse_optional_key(meta.input, field_ident.to_string())?);
                    return Ok(());
                }
                if meta.path.is_ident("bus") {
                    let mut interface = None;
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("i2c") {
                            interface = Some(BusInterface::I2c);
                            return Ok(());
                        }
                        if inner.path.is_ident("spi") {
                            interface = Some(BusInterface::Spi);
                            return Ok(());
                        }
                        Err(inner.error("unsupported bus annotation; expected `bus(i2c)` or `bus(spi)`"))
                    })?;
                    let Some(interface) = interface else {
                        return Err(meta.error("missing bus interface; expected `bus(i2c)` or `bus(spi)`"));
                    };
                    if self.bus_field.is_some() {
                        return Err(syn::Error::new_spanned(
                            &field_ident,
                            "only one configured bus field is allowed",
                        ));
                    }
                    annotations.bus_interface = Some(interface);
                    return Ok(());
                }
                if meta.path.is_ident("endpoint") {
                    let mut name = None;
                    let mut interface = None;
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("i2c") {
                            interface = Some(EndpointInterface::I2c);
                            return Ok(());
                        }
                        if inner.path.is_ident("spi") {
                            interface = Some(EndpointInterface::Spi);
                            return Ok(());
                        }
                        if inner.path.is_ident("name") {
                            name = Some(parse_string_value(&inner)?);
                            return Ok(());
                        }
                        Err(inner.error(
                            "unsupported endpoint annotation; expected `endpoint(i2c, name = \"...\")` or `endpoint(spi, name = \"...\")`",
                        ))
                    })?;
                    if interface.is_none() {
                        return Err(meta.error(
                            "missing endpoint interface; expected `endpoint(i2c, name = \"...\")` or `endpoint(spi, name = \"...\")`",
                        ));
                    }
                    annotations.endpoint = Some((
                        interface.expect("checked endpoint interface"),
                        name.ok_or_else(|| {
                            meta.error("missing required endpoint name; expected `name = \"...\"`")
                        })?,
                    ));
                    return Ok(());
                }
                if meta.path.is_ident("signal") {
                    let mut name = None;
                    let mut saw_gpio = false;
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("gpio") {
                            saw_gpio = true;
                            return Ok(());
                        }
                        if inner.path.is_ident("name") {
                            name = Some(parse_string_value(&inner)?);
                            return Ok(());
                        }
                        Err(inner.error(
                            "unsupported signal annotation; expected `signal(gpio, name = \"...\")`",
                        ))
                    })?;
                    if !saw_gpio {
                        return Err(meta.error(
                            "missing signal interface; expected `signal(gpio, name = \"...\")`",
                        ));
                    }
                    annotations.signal_gpio = Some(name.ok_or_else(|| {
                        meta.error("missing required signal name; expected `name = \"...\"`")
                    })?);
                    return Ok(());
                }

                Err(meta.error(
                    "unsupported configured-device field annotation; supported forms are `bus(i2c)`, `bus(spi)`, `endpoint(i2c, name = \"...\")`, `endpoint(spi, name = \"...\")`, `signal(gpio, name = \"...\")`, `display_name`, `label`, and `property`",
                ))
            })?;
        }

        if let Some(interface) = annotations.bus_interface {
            self.bus_field = Some(AnnotatedField::new(
                field_ident.clone(),
                field.ty.clone(),
                Some(interface),
            ));
        }

        if let Some((interface, name)) = annotations.endpoint {
            self.endpoint_fields.push(EndpointField::new(
                field_ident.clone(),
                field.ty.clone(),
                name,
                interface,
            ));
        }

        if let Some(name) = annotations.signal_gpio {
            self.signal_fields.push(SignalField::new(
                field_ident.clone(),
                field.ty.clone(),
                name,
            ));
        }

        if annotations.display_name {
            if self.display_name_field.is_some() {
                return Err(syn::Error::new_spanned(
                    &field_ident,
                    "only one `display_name` field is allowed",
                ));
            }
            self.display_name_field = Some(AnnotatedField::new(
                field_ident.clone(),
                field.ty.clone(),
                None,
            ));
        }

        for key in annotations.labels {
            self.label_fields
                .push(NamedField::new(field_ident.clone(), field.ty.clone(), key));
        }

        for key in annotations.properties {
            self.property_fields
                .push(NamedField::new(field_ident.clone(), field.ty.clone(), key));
        }

        Ok(())
    }

    fn validate(&mut self, input: &DeriveInput) -> syn::Result<()> {
        if !self.enabled() {
            return Ok(());
        }

        let interface = self.interface.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(
                input,
                "configured descriptor generation requires `#[lemnos(interface = ...)]`",
            )
        })?;

        let configured_interface = validate_interface(interface)?;

        if let Some(kind) = &self.kind {
            validate_kind(kind)?;
            if kind == "Unspecified" {
                return Err(syn::Error::new_spanned(
                    kind,
                    "omit `kind` to use `DeviceKind::Unspecified(interface)`",
                ));
            }
        }

        if (!self.endpoint_fields.is_empty() || self.bus_field.is_some())
            && !supports_configured_bus_endpoints(configured_interface)
        {
            return Err(syn::Error::new_spanned(
                interface,
                "configured bus endpoints currently require `interface = I2c` or `interface = Spi`",
            ));
        }

        if !self.endpoint_fields.is_empty() && self.bus_field.is_none() {
            return Err(syn::Error::new_spanned(
                input,
                "configured bus endpoints require a matching `#[lemnos(bus(...))]` field",
            ));
        }

        if let Some(bus_field) = &self.bus_field {
            if type_ident(&bus_field.ty).as_deref() != Some("u32") {
                return Err(syn::Error::new_spanned(
                    &bus_field.ident,
                    bus_field
                        .interface
                        .expect("validated bus field interface")
                        .type_error(),
                ));
            }

            if !bus_field
                .interface
                .expect("validated bus field interface")
                .matches_configured_interface(configured_interface)
            {
                return Err(syn::Error::new_spanned(
                    &bus_field.ident,
                    "configured bus annotation must match the configured device interface",
                ));
            }
        }

        for endpoint in &self.endpoint_fields {
            if type_ident(&endpoint.ty).as_deref() != Some("u16") {
                return Err(syn::Error::new_spanned(
                    &endpoint.ident,
                    endpoint.interface.type_error(),
                ));
            }
            if !endpoint
                .interface
                .matches_configured_interface(configured_interface)
            {
                return Err(syn::Error::new_spanned(
                    &endpoint.ident,
                    "configured endpoint annotation must match the configured device interface",
                ));
            }
        }

        for signal in &mut self.signal_fields {
            let (optional, inner_ty) = match option_inner_type(&signal.ty) {
                Some(inner) => (true, inner.clone()),
                None => (false, signal.ty.clone()),
            };
            if type_ident(&inner_ty).as_deref() != Some("ConfiguredGpioSignal") {
                return Err(syn::Error::new_spanned(
                    &signal.ident,
                    "`signal(gpio, ..)` fields must have type `ConfiguredGpioSignal` or `Option<ConfiguredGpioSignal>`",
                ));
            }
            signal.optional = optional;
        }

        populate_named_field_optionality(&mut self.label_fields);
        populate_named_field_optionality(&mut self.property_fields);

        validate_unique_names(
            self.endpoint_fields
                .iter()
                .map(|field| (&field.ident, &field.name)),
            "duplicate configured endpoint name",
        )?;
        validate_unique_names(
            self.signal_fields
                .iter()
                .map(|field| (&field.ident, &field.name)),
            "duplicate configured signal name",
        )?;
        validate_unique_keys(
            self.label_fields
                .iter()
                .map(|field| (&field.ident, field.key.as_str())),
            "duplicate configured label key",
        )?;
        validate_unique_keys(
            self.property_fields
                .iter()
                .map(|field| (&field.ident, field.key.as_str())),
            "duplicate configured property key",
        )?;

        Ok(())
    }
}
