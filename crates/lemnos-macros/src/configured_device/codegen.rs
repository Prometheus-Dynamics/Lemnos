use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Ident, LitStr};

use super::helpers::{
    SupportedInterface, default_id_prefix, driver_hint_option, field_is_option,
    field_to_label_expr, label_expr_for_field, label_expr_from_ref, validate_interface,
    value_expr_for_field, value_expr_from_ref,
};
use super::spec::{ConfiguredDeviceSpec, EndpointInterface};

pub(super) fn expand_configured_device(
    input: &DeriveInput,
    spec: &ConfiguredDeviceSpec,
) -> syn::Result<TokenStream> {
    let ident = &input.ident;
    let interface = spec
        .interface
        .as_ref()
        .expect("configured device validation ensures interface exists");
    let interface_variant = interface;
    let id_prefix = spec
        .id
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| default_id_prefix(ident));
    let summary = spec
        .summary
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| format!("Configured {ident}"));
    let kind_expr = if let Some(kind) = &spec.kind {
        quote!(::lemnos::core::DeviceKind::#kind)
    } else {
        quote!(::lemnos::core::DeviceKind::Unspecified(
            Self::CONFIGURED_DEVICE_INTERFACE
        ))
    };
    let bus_ident = spec.bus_field.as_ref().map(|field| &field.ident);
    let i2c_endpoint_list: Vec<_> = spec
        .endpoint_fields
        .iter()
        .filter(|endpoint| endpoint.interface == EndpointInterface::I2c)
        .collect();
    let spi_endpoint_list: Vec<_> = spec
        .endpoint_fields
        .iter()
        .filter(|endpoint| endpoint.interface == EndpointInterface::Spi)
        .collect();
    let i2c_endpoint_pushes = i2c_endpoint_list.iter().map(|endpoint| {
        let field_ident = &endpoint.ident;
        let name = &endpoint.name;
        let bus_ident = bus_ident.expect("i2c endpoints require validated bus field");
        quote! {
            endpoints.push(::lemnos::core::ConfiguredI2cEndpoint::new(
                #name,
                self.#bus_ident,
                self.#field_ident,
            ));
        }
    });
    let spi_endpoint_pushes = spi_endpoint_list.iter().map(|endpoint| {
        let field_ident = &endpoint.ident;
        let name = &endpoint.name;
        let bus_ident = bus_ident.expect("spi endpoints require validated bus field");
        quote! {
            endpoints.push(::lemnos::core::ConfiguredSpiEndpoint::new(
                #name,
                self.#bus_ident,
                self.#field_ident,
            ));
        }
    });
    let endpoint_id_suffixes = spec.endpoint_fields.iter().map(|endpoint| {
        let field_ident = &endpoint.ident;
        let name = &endpoint.name;
        quote! {
            id.push_str(&::std::format!(".{}0x{:02x}", #name, self.#field_ident));
        }
    });
    let configured_interfaces = configured_interface_tokens(spec);
    let driver_hint_tokens = driver_hint_option(spec.driver.as_ref());
    let child_descriptor_pushes = spec.endpoint_fields.iter().map(|endpoint| {
        let field_ident = &endpoint.ident;
        let name = &endpoint.name;
        let bus_ident = bus_ident.expect("configured endpoints require validated bus field");
        match endpoint.interface {
            EndpointInterface::I2c => quote! {
                descriptors.push(
                    ::lemnos::core::ConfiguredI2cEndpoint::new(
                        #name,
                        self.#bus_ident,
                        self.#field_ident,
                    )
                    .descriptor_for_owner(&owner, #driver_hint_tokens)?,
                );
            },
            EndpointInterface::Spi => quote! {
                descriptors.push(
                    ::lemnos::core::ConfiguredSpiEndpoint::new(
                        #name,
                        self.#bus_ident,
                        self.#field_ident,
                    )
                    .descriptor_for_owner(&owner, #driver_hint_tokens)?,
                );
            },
        }
    });
    let signal_pushes = spec.signal_fields.iter().map(|signal| {
        let field_ident = &signal.ident;
        let name = &signal.name;
        if signal.optional {
            quote! {
                if let ::core::option::Option::Some(signal) = &self.#field_ident {
                    signals.push(::lemnos::core::ConfiguredGpioSignalBinding::new(
                        #name,
                        signal.clone(),
                    ));
                }
            }
        } else {
            quote! {
                signals.push(::lemnos::core::ConfiguredGpioSignalBinding::new(
                    #name,
                    self.#field_ident.clone(),
                ));
            }
        }
    });
    let display_name_stmt = if let Some(field_ident) = &spec.display_name_field {
        if field_is_option(&field_ident.ty) {
            let ident = &field_ident.ident;
            Some(quote! {
                if let ::core::option::Option::Some(value) = &self.#ident {
                    builder = builder.display_name(value.clone());
                }
            })
        } else {
            let ident = &field_ident.ident;
            let expr = field_to_label_expr(quote!(self.#ident), &field_ident.ty);
            Some(quote! {
                builder = builder.display_name(#expr);
            })
        }
    } else {
        None
    };
    let label_stmts = spec.label_fields.iter().map(|label| {
        let field_ident = &label.ident;
        let key = &label.key;
        if label.optional {
            let inner_ty = label
                .inner_ty
                .as_ref()
                .expect("optional label captures inner type");
            let expr = label_expr_from_ref(quote!(value), inner_ty);
            quote! {
                if let ::core::option::Option::Some(value) = &self.#field_ident {
                    builder = builder.label(#key, #expr);
                }
            }
        } else {
            let expr = label_expr_for_field(field_ident, &label.ty);
            quote! {
                builder = builder.label(#key, #expr);
            }
        }
    });
    let property_stmts = spec.property_fields.iter().map(|property| {
        let field_ident = &property.ident;
        let key = &property.key;
        if property.optional {
            let inner_ty = property
                .inner_ty
                .as_ref()
                .expect("optional property captures inner type");
            let value_expr = value_expr_from_ref(quote!(value), inner_ty);
            quote! {
                if let ::core::option::Option::Some(value) = &self.#field_ident {
                    builder = builder.property(#key, #value_expr);
                }
            }
        } else {
            let value_expr = value_expr_for_field(field_ident, &property.ty);
            quote! {
                builder = builder.property(#key, #value_expr);
            }
        }
    });
    let driver_stmt = spec.driver.as_ref().map(|driver| {
        quote! {
            builder = builder.driver_hint(#driver);
        }
    });
    let configured_bus_stmt = bus_ident.map(|bus_ident| {
        quote! {
            id.push_str(&::std::format!(".bus{}", self.#bus_ident));
        }
    });
    let configured_bus_property_stmt = bus_ident.map(|bus_ident| {
        quote! {
            builder = builder.property("configured_bus", u64::from(self.#bus_ident));
        }
    });

    Ok(quote! {
        impl #ident {
            pub const CONFIGURED_DEVICE_INTERFACE: ::lemnos::core::InterfaceKind =
                ::lemnos::core::InterfaceKind::#interface_variant;
            pub const CONFIGURED_DEVICE_INTERFACES: &'static [::lemnos::core::InterfaceKind] =
                #configured_interfaces;
            pub const CONFIGURED_DEVICE_ID_PREFIX: &'static str = #id_prefix;
            pub const CONFIGURED_DEVICE_SUMMARY: &'static str = #summary;

            pub fn configured_device_id(&self) -> ::std::string::String {
                let mut id = ::std::string::String::from(Self::CONFIGURED_DEVICE_ID_PREFIX);
                #configured_bus_stmt
                #( #endpoint_id_suffixes )*
                id
            }

            pub fn logical_device_id(&self) -> ::lemnos::core::CoreResult<::lemnos::core::DeviceId> {
                ::lemnos::core::DeviceId::new(self.configured_device_id())
            }

            pub fn configured_i2c_endpoints(&self) -> ::std::vec::Vec<::lemnos::core::ConfiguredI2cEndpoint> {
                let mut endpoints = ::std::vec::Vec::new();
                #( #i2c_endpoint_pushes )*
                endpoints
            }

            pub fn configured_spi_endpoints(&self) -> ::std::vec::Vec<::lemnos::core::ConfiguredSpiEndpoint> {
                let mut endpoints = ::std::vec::Vec::new();
                #( #spi_endpoint_pushes )*
                endpoints
            }

            pub fn configured_gpio_signals(&self) -> ::std::vec::Vec<::lemnos::core::ConfiguredGpioSignalBinding> {
                let mut signals = ::std::vec::Vec::new();
                #( #signal_pushes )*
                signals
            }

            pub fn configured_device_descriptor(
                &self,
            ) -> ::lemnos::core::CoreResult<::lemnos::core::DeviceDescriptor> {
                let device_id = self.configured_device_id();
                let mut builder = ::lemnos::core::DeviceDescriptor::builder(
                    device_id,
                    Self::CONFIGURED_DEVICE_INTERFACE,
                )?
                .kind(#kind_expr)
                .summary(Self::CONFIGURED_DEVICE_SUMMARY)
                .property("configured_device", true)
                .property("configured_interface", Self::CONFIGURED_DEVICE_INTERFACE.to_string());

                #configured_bus_property_stmt

                #display_name_stmt
                #driver_stmt
                #( #label_stmts )*
                #( #property_stmts )*

                let owner_id = ::lemnos::core::DeviceId::new(self.configured_device_id())?;
                for endpoint in self.configured_i2c_endpoints() {
                    let target = ::lemnos::core::DeviceId::new(endpoint.descriptor_id_for_owner(&owner_id))?;
                    builder = builder.link(
                        ::lemnos::core::DeviceLink::new(target, ::lemnos::core::DeviceRelation::Channel)
                            .with_attribute("name", endpoint.name.clone())
                            .with_attribute("interface", "i2c")
                            .with_attribute("bus", u64::from(endpoint.bus))
                            .with_attribute("address", u64::from(endpoint.address)),
                    );
                }

                for signal in self.configured_gpio_signals() {
                    let target = signal.link_target_id(&owner_id)?;
                    let mut link = ::lemnos::core::DeviceLink::new(
                        target,
                        ::lemnos::core::DeviceRelation::Dependency,
                    )
                    .with_attribute("name", signal.name.clone())
                    .with_attribute("interface", "gpio")
                    .with_attribute("required", signal.signal.required)
                    .with_attribute("active_low", signal.signal.active_low);

                    if let ::core::option::Option::Some(edge) = signal.signal.edge {
                        link = link.with_attribute("edge", ::std::string::ToString::to_string(&::std::format!("{:?}", edge)).to_lowercase());
                    }

                    builder = builder.link(link);
                }

                builder.build()
            }

            pub fn configured_child_descriptors(
                &self,
            ) -> ::lemnos::core::CoreResult<::std::vec::Vec<::lemnos::core::DeviceDescriptor>> {
                let owner = self.configured_device_descriptor()?;
                let mut descriptors = ::std::vec::Vec::new();

                #( #child_descriptor_pushes )*

                for signal in self.configured_gpio_signals() {
                    if let ::core::option::Option::Some(descriptor) = signal.descriptor_for_owner(&owner)? {
                        descriptors.push(descriptor);
                    }
                }

                ::core::result::Result::Ok(descriptors)
            }

            pub fn configured_descriptors(
                &self,
            ) -> ::lemnos::core::CoreResult<::std::vec::Vec<::lemnos::core::DeviceDescriptor>> {
                let owner = self.configured_device_descriptor()?;
                let mut descriptors = ::std::vec::Vec::with_capacity(1);
                descriptors.push(owner);
                descriptors.extend(self.configured_child_descriptors()?);
                ::core::result::Result::Ok(descriptors)
            }

            pub fn configured_probe(
                name: &'static str,
                configs: ::std::vec::Vec<Self>,
            ) -> ::lemnos::discovery::ConfiguredDeviceProbe<Self> {
                ::lemnos::discovery::ConfiguredDeviceProbe::from_configs(name, configs)
            }
        }

        impl ::lemnos::core::ConfiguredDeviceModel for #ident {
            fn configured_interfaces() -> &'static [::lemnos::core::InterfaceKind]
            where
                Self: Sized,
            {
                Self::CONFIGURED_DEVICE_INTERFACES
            }

            fn configured_descriptors(
                &self,
            ) -> ::lemnos::core::CoreResult<::std::vec::Vec<::lemnos::core::DeviceDescriptor>> {
                #ident::configured_descriptors(self)
            }
        }
    })
}

fn configured_interface_tokens(spec: &ConfiguredDeviceSpec) -> TokenStream {
    let configured_interface = validate_interface(
        spec.interface
            .as_ref()
            .expect("configured device validation ensures interface exists"),
    )
    .expect("configured device validation ensures interface is supported");
    let mut interfaces = vec![
        spec.interface
            .as_ref()
            .expect("configured device validation ensures interface exists")
            .clone(),
    ];
    if !spec.signal_fields.is_empty() && configured_interface != SupportedInterface::Gpio {
        interfaces.push(Ident::new("Gpio", proc_macro2::Span::call_site()));
    }
    let variants = interfaces
        .iter()
        .map(|interface| quote!(::lemnos::core::InterfaceKind::#interface));
    quote!(&[#(#variants),*])
}
