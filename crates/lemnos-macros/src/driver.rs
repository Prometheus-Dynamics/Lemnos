use crate::builder;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::{
    Expr, ExprLit, ExprPath, Ident, ItemStruct, Lit, LitInt, LitStr, Result, Token, parenthesized,
};

pub(crate) fn expand(attr: proc_macro::TokenStream, item: proc_macro::TokenStream) -> TokenStream {
    let args = syn::parse::<DriverArgs>(attr).and_then(|args| args.validate());
    let item_struct = syn::parse::<ItemStruct>(item);

    match (args, item_struct) {
        (Ok(args), Ok(item_struct)) => {
            expand_driver(args, item_struct).unwrap_or_else(|error| error.to_compile_error())
        }
        (Err(error), _) => error.to_compile_error(),
        (_, Err(error)) => error.to_compile_error(),
    }
}

fn expand_driver(args: DriverArgs, item_struct: ItemStruct) -> syn::Result<TokenStream> {
    let ident = &item_struct.ident;
    let driver_id = args
        .id
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| ident.to_string());
    let summary = args
        .summary
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| ident.to_string());
    let description_const = match args.description.as_ref() {
        Some(description) => quote!(::core::option::Option::Some(#description)),
        None => quote!(::core::option::Option::None),
    };
    let interface_variant = args
        .interface
        .expect("validated interface is present")
        .value;
    let priority_variant = args
        .priority
        .map(|priority| priority.value)
        .unwrap_or(DriverPriority::Generic);
    let version = args.version.unwrap_or((0, 1, 0));
    let version_major = version.0;
    let version_minor = version.1;
    let version_patch = version.2;
    let tags = args.tags.iter().map(LitStr::value).collect::<Vec<_>>();

    let kind_const = if let Some(kind_variant) = args.kind.map(|kind| kind.value) {
        let kind_variant = kind_variant.ident(ident.span());
        quote! {
            ::core::option::Option::Some(::lemnos::core::DeviceKind::#kind_variant)
        }
    } else {
        quote! {
            ::core::option::Option::None
        }
    };

    let builder_tokens = builder::expand_builder_for_struct(&item_struct)?;

    Ok(quote! {
        #item_struct

        impl #ident {
            pub const DRIVER_ID: &'static str = #driver_id;
            pub const DRIVER_SUMMARY: &'static str = #summary;
            pub const DRIVER_DESCRIPTION: ::core::option::Option<&'static str> = #description_const;
            pub const DRIVER_INTERFACE: ::lemnos::core::InterfaceKind =
                ::lemnos::core::InterfaceKind::#interface_variant;
            pub const DRIVER_VERSION: ::lemnos::driver::DriverVersion =
                ::lemnos::driver::DriverVersion::new(#version_major, #version_minor, #version_patch);
            pub const DRIVER_PRIORITY: ::lemnos::driver::DriverPriority =
                ::lemnos::driver::DriverPriority::#priority_variant;
            pub const DRIVER_KIND: ::core::option::Option<::lemnos::core::DeviceKind> =
                #kind_const;
            pub const DRIVER_TAGS: &'static [&'static str] = &[#(#tags),*];

            pub fn driver_manifest_ref() -> &'static ::lemnos::driver::DriverManifest {
                static DRIVER_MANIFEST: ::std::sync::OnceLock<::lemnos::driver::DriverManifest> =
                    ::std::sync::OnceLock::new();

                ::lemnos::driver::cached_manifest(&DRIVER_MANIFEST, || {
                    let manifest = ::lemnos::driver::DriverManifest::new(
                        Self::DRIVER_ID,
                        Self::DRIVER_SUMMARY,
                        vec![Self::DRIVER_INTERFACE],
                    )
                    .with_version(Self::DRIVER_VERSION)
                    .with_priority(Self::DRIVER_PRIORITY);

                    let manifest = match Self::DRIVER_KIND {
                        ::core::option::Option::Some(kind) => manifest.with_kind(kind),
                        ::core::option::Option::None => manifest,
                    };

                    let manifest = match Self::DRIVER_DESCRIPTION {
                        ::core::option::Option::Some(description) => manifest.with_description(description),
                        ::core::option::Option::None => manifest,
                    };

                    Self::DRIVER_TAGS
                        .iter()
                        .fold(manifest, |manifest, tag| manifest.with_tag(*tag))
                })
            }

            pub fn driver_manifest_base() -> ::lemnos::driver::DriverManifest {
                Self::driver_manifest_ref().clone()
            }
        }

        #builder_tokens
    })
}

struct DriverArgs {
    id: Option<LitStr>,
    summary: Option<LitStr>,
    description: Option<LitStr>,
    interface: Option<ParsedDriverInterface>,
    kind: Option<ParsedDriverKind>,
    priority: Option<ParsedDriverPriority>,
    version: Option<(u16, u16, u16)>,
    tags: Vec<LitStr>,
}

impl DriverArgs {
    fn validate(self) -> syn::Result<Self> {
        if self.interface.is_none() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "missing required driver argument `interface = ...`",
            ));
        }

        if let (Some(interface), Some(kind)) = (&self.interface, &self.kind) {
            validate_kind_matches_interface(kind, interface)?;
        }

        Ok(self)
    }
}

impl Parse for DriverArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut args = Self {
            id: None,
            summary: None,
            description: None,
            interface: None,
            kind: None,
            priority: None,
            version: None,
            tags: Vec::new(),
        };

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            if key == "tags" {
                args.tags = parse_string_list(input)?;
                if input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }

            input.parse::<Token![=]>()?;

            match DriverArgKey::from_ident(&key) {
                Ok(DriverArgKey::Id) => {
                    let value: LitStr = input.parse()?;
                    args.id = Some(value);
                }
                Ok(DriverArgKey::Summary) => {
                    let value: LitStr = input.parse()?;
                    args.summary = Some(value);
                }
                Ok(DriverArgKey::Description) => {
                    let value: LitStr = input.parse()?;
                    args.description = Some(value);
                }
                Ok(DriverArgKey::Interface) => {
                    args.interface = Some(parse_interface(input)?);
                }
                Ok(DriverArgKey::Kind) => {
                    args.kind = Some(parse_kind(input)?);
                }
                Ok(DriverArgKey::Priority) => {
                    args.priority = Some(parse_priority(input)?);
                }
                Ok(DriverArgKey::Version) => {
                    args.version = Some(parse_version_tuple(input)?);
                }
                Err(()) => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unsupported driver argument `{}`; supported keys are `id`, `summary`, `description`, `interface`, `kind`, `priority`, `version`, and `tags`",
                            key
                        ),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(args)
    }
}

#[derive(Clone, Copy)]
enum DriverArgKey {
    Id,
    Summary,
    Description,
    Interface,
    Kind,
    Priority,
    Version,
}

impl DriverArgKey {
    fn from_ident(ident: &Ident) -> std::result::Result<Self, ()> {
        match ident.to_string().as_str() {
            "id" => Ok(Self::Id),
            "summary" => Ok(Self::Summary),
            "description" => Ok(Self::Description),
            "interface" => Ok(Self::Interface),
            "kind" => Ok(Self::Kind),
            "priority" => Ok(Self::Priority),
            "version" => Ok(Self::Version),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DriverInterface {
    Gpio,
    Pwm,
    I2c,
    Spi,
    Uart,
    Usb,
}

impl DriverInterface {
    fn ident(self, span: proc_macro2::Span) -> Ident {
        match self {
            Self::Gpio => Ident::new("Gpio", span),
            Self::Pwm => Ident::new("Pwm", span),
            Self::I2c => Ident::new("I2c", span),
            Self::Spi => Ident::new("Spi", span),
            Self::Uart => Ident::new("Uart", span),
            Self::Usb => Ident::new("Usb", span),
        }
    }
}

impl ToTokens for DriverInterface {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.ident(proc_macro2::Span::call_site()).to_tokens(tokens);
    }
}

#[derive(Clone)]
struct ParsedDriverInterface {
    ident: Ident,
    value: DriverInterface,
}

#[derive(Clone, Copy)]
enum DriverKind {
    GpioChip,
    GpioLine,
    PwmChip,
    PwmChannel,
    I2cBus,
    I2cDevice,
    SpiBus,
    SpiDevice,
    UartPort,
    UartDevice,
    UsbBus,
    UsbDevice,
    UsbInterface,
}

impl DriverKind {
    fn ident(self, span: proc_macro2::Span) -> Ident {
        match self {
            Self::GpioChip => Ident::new("GpioChip", span),
            Self::GpioLine => Ident::new("GpioLine", span),
            Self::PwmChip => Ident::new("PwmChip", span),
            Self::PwmChannel => Ident::new("PwmChannel", span),
            Self::I2cBus => Ident::new("I2cBus", span),
            Self::I2cDevice => Ident::new("I2cDevice", span),
            Self::SpiBus => Ident::new("SpiBus", span),
            Self::SpiDevice => Ident::new("SpiDevice", span),
            Self::UartPort => Ident::new("UartPort", span),
            Self::UartDevice => Ident::new("UartDevice", span),
            Self::UsbBus => Ident::new("UsbBus", span),
            Self::UsbDevice => Ident::new("UsbDevice", span),
            Self::UsbInterface => Ident::new("UsbInterface", span),
        }
    }

    fn interface(self) -> DriverInterface {
        match self {
            Self::GpioChip | Self::GpioLine => DriverInterface::Gpio,
            Self::PwmChip | Self::PwmChannel => DriverInterface::Pwm,
            Self::I2cBus | Self::I2cDevice => DriverInterface::I2c,
            Self::SpiBus | Self::SpiDevice => DriverInterface::Spi,
            Self::UartPort | Self::UartDevice => DriverInterface::Uart,
            Self::UsbBus | Self::UsbDevice | Self::UsbInterface => DriverInterface::Usb,
        }
    }
}

#[derive(Clone, Copy)]
enum DriverPriority {
    Fallback,
    Generic,
    Preferred,
    Exact,
}

impl ToTokens for DriverPriority {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let ident = match self {
            Self::Fallback => Ident::new("Fallback", proc_macro2::Span::call_site()),
            Self::Generic => Ident::new("Generic", proc_macro2::Span::call_site()),
            Self::Preferred => Ident::new("Preferred", proc_macro2::Span::call_site()),
            Self::Exact => Ident::new("Exact", proc_macro2::Span::call_site()),
        };
        ident.to_tokens(tokens);
    }
}

#[derive(Clone)]
struct ParsedDriverKind {
    ident: Ident,
    value: DriverKind,
}

#[derive(Clone)]
struct ParsedDriverPriority {
    value: DriverPriority,
}

fn parse_version_tuple(input: ParseStream<'_>) -> Result<(u16, u16, u16)> {
    let content;
    parenthesized!(content in input);
    let major = parse_u16_literal(&content)?;
    content.parse::<Token![,]>()?;
    let minor = parse_u16_literal(&content)?;
    content.parse::<Token![,]>()?;
    let patch = parse_u16_literal(&content)?;
    if !content.is_empty() {
        return Err(syn::Error::new(
            content.span(),
            "expected version tuple `(major, minor, patch)`",
        ));
    }
    Ok((major, minor, patch))
}

fn parse_string_list(input: ParseStream<'_>) -> Result<Vec<LitStr>> {
    let content;
    parenthesized!(content in input);
    let mut values = Vec::new();
    while !content.is_empty() {
        values.push(content.parse()?);
        if content.peek(Token![,]) {
            content.parse::<Token![,]>()?;
        }
    }
    Ok(values)
}

fn parse_u16_literal(input: ParseStream<'_>) -> Result<u16> {
    let value: LitInt = input.parse()?;
    value.base10_parse()
}

fn parse_ident_expr(input: ParseStream<'_>) -> Result<Ident> {
    let expr: Expr = input.parse()?;
    match expr {
        Expr::Path(ExprPath { path, .. }) => path
            .segments
            .last()
            .map(|segment| segment.ident.clone())
            .ok_or_else(|| syn::Error::new_spanned(path, "expected identifier path")),
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Err(syn::Error::new_spanned(
            value,
            "expected typed identifier, not string literal",
        )),
        other => Err(syn::Error::new_spanned(
            other,
            "expected typed identifier path",
        )),
    }
}

fn parse_interface(input: ParseStream<'_>) -> Result<ParsedDriverInterface> {
    let value = parse_ident_expr(input)?;
    Ok(ParsedDriverInterface {
        ident: value.clone(),
        value: validate_interface(&value)?,
    })
}

fn validate_interface(value: &Ident) -> syn::Result<DriverInterface> {
    match value.to_string().as_str() {
        "Gpio" => Ok(DriverInterface::Gpio),
        "Pwm" => Ok(DriverInterface::Pwm),
        "I2c" => Ok(DriverInterface::I2c),
        "Spi" => Ok(DriverInterface::Spi),
        "Uart" => Ok(DriverInterface::Uart),
        "Usb" => Ok(DriverInterface::Usb),
        _ => Err(syn::Error::new_spanned(
            value,
            "unsupported interface; expected one of Gpio, Pwm, I2c, Spi, Uart, Usb",
        )),
    }
}

fn parse_kind(input: ParseStream<'_>) -> Result<ParsedDriverKind> {
    let value = parse_ident_expr(input)?;
    Ok(ParsedDriverKind {
        ident: value.clone(),
        value: validate_kind(&value)?,
    })
}

fn validate_kind(value: &Ident) -> syn::Result<DriverKind> {
    match value.to_string().as_str() {
        "GpioChip" => Ok(DriverKind::GpioChip),
        "GpioLine" => Ok(DriverKind::GpioLine),
        "PwmChip" => Ok(DriverKind::PwmChip),
        "PwmChannel" => Ok(DriverKind::PwmChannel),
        "I2cBus" => Ok(DriverKind::I2cBus),
        "I2cDevice" => Ok(DriverKind::I2cDevice),
        "SpiBus" => Ok(DriverKind::SpiBus),
        "SpiDevice" => Ok(DriverKind::SpiDevice),
        "UartPort" => Ok(DriverKind::UartPort),
        "UartDevice" => Ok(DriverKind::UartDevice),
        "UsbBus" => Ok(DriverKind::UsbBus),
        "UsbDevice" => Ok(DriverKind::UsbDevice),
        "UsbInterface" => Ok(DriverKind::UsbInterface),
        "Unspecified" => Err(syn::Error::new_spanned(
            value,
            "`#[driver(kind = Unspecified)]` is not supported; omit `kind` instead",
        )),
        _ => Err(syn::Error::new_spanned(
            value,
            "unsupported device kind for current Lemnos core model",
        )),
    }
}

fn parse_priority(input: ParseStream<'_>) -> Result<ParsedDriverPriority> {
    let value = parse_ident_expr(input)?;
    Ok(ParsedDriverPriority {
        value: validate_priority(&value)?,
    })
}

fn validate_priority(value: &Ident) -> syn::Result<DriverPriority> {
    match value.to_string().as_str() {
        "Fallback" => Ok(DriverPriority::Fallback),
        "Generic" => Ok(DriverPriority::Generic),
        "Preferred" => Ok(DriverPriority::Preferred),
        "Exact" => Ok(DriverPriority::Exact),
        _ => Err(syn::Error::new_spanned(
            value,
            "unsupported driver priority; expected one of Fallback, Generic, Preferred, Exact",
        )),
    }
}

fn validate_kind_matches_interface(
    kind: &ParsedDriverKind,
    interface: &ParsedDriverInterface,
) -> syn::Result<()> {
    if kind.value.interface() != interface.value {
        return Err(syn::Error::new_spanned(
            &kind.ident,
            format!(
                "driver kind `{}` does not match interface `{}`",
                kind.ident, interface.ident
            ),
        ));
    }

    Ok(())
}
