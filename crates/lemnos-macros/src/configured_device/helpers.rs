use proc_macro2::TokenStream;
use quote::quote;
use std::collections::BTreeSet;
use syn::{
    Expr, ExprLit, ExprPath, GenericArgument, Ident, Lit, LitStr, PathArguments, Token, Type,
};

pub(crate) fn driver_hint_option(driver: Option<&LitStr>) -> TokenStream {
    match driver {
        Some(driver) => quote!(::core::option::Option::Some(#driver)),
        None => quote!(::core::option::Option::None),
    }
}

pub(crate) fn default_id_prefix(ident: &Ident) -> String {
    let mut kebab = String::new();
    let name = ident.to_string();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                kebab.push('-');
            }
            kebab.push(ch.to_ascii_lowercase());
        } else {
            kebab.push(ch);
        }
    }
    let trimmed = kebab.strip_suffix("-config").unwrap_or(&kebab);
    format!("configured.{trimmed}")
}

pub(crate) fn label_expr_for_field(field_ident: &Ident, ty: &Type) -> TokenStream {
    field_to_label_expr(quote!(self.#field_ident), ty)
}

pub(crate) fn label_expr_from_ref(expr: TokenStream, ty: &Type) -> TokenStream {
    field_to_label_expr(expr, ty)
}

pub(crate) fn field_to_label_expr(expr: TokenStream, ty: &Type) -> TokenStream {
    if is_string_type(ty) {
        quote!(#expr.clone())
    } else if is_bool_type(ty) || is_numeric_type(ty) {
        quote!(#expr.to_string())
    } else {
        quote!(::std::format!("{:?}", &#expr))
    }
}

pub(crate) fn value_expr_for_field(field_ident: &Ident, ty: &Type) -> TokenStream {
    value_expr_from_ref(quote!(self.#field_ident), ty)
}

pub(crate) fn value_expr_from_ref(expr: TokenStream, ty: &Type) -> TokenStream {
    if is_string_type(ty) {
        quote!(::lemnos::core::Value::from(#expr.clone()))
    } else if is_bool_type(ty) || is_numeric_type(ty) {
        quote!(::lemnos::core::Value::from(#expr))
    } else {
        quote!(::lemnos::core::Value::from(::std::format!("{:?}", &#expr)))
    }
}

pub(crate) fn field_is_option(ty: &Type) -> bool {
    option_inner_type(ty).is_some()
}

pub(crate) fn is_string_type(ty: &Type) -> bool {
    type_ident(ty).is_some_and(|ident| ident == "String")
}

pub(crate) fn is_bool_type(ty: &Type) -> bool {
    type_ident(ty).is_some_and(|ident| ident == "bool")
}

pub(crate) fn is_numeric_type(ty: &Type) -> bool {
    matches!(
        type_ident(ty).as_deref(),
        Some("u8")
            | Some("u16")
            | Some("u32")
            | Some("u64")
            | Some("usize")
            | Some("i8")
            | Some("i16")
            | Some("i32")
            | Some("i64")
            | Some("isize")
            | Some("f32")
            | Some("f64")
    )
}

pub(crate) fn type_ident(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

pub(crate) fn option_inner_type(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let first = arguments.args.first()?;
    let GenericArgument::Type(inner) = first else {
        return None;
    };
    Some(inner)
}

pub(crate) fn parse_optional_key(
    input: syn::parse::ParseStream<'_>,
    default: String,
) -> syn::Result<String> {
    if input.is_empty() {
        return Ok(default);
    }

    let _: Token![=] = input.parse()?;
    let value: LitStr = input.parse()?;
    Ok(value.value())
}

pub(crate) fn parse_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<LitStr> {
    let value_stream = meta.value()?;
    value_stream.parse()
}

pub(crate) fn parse_ident_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<Ident> {
    let value_stream = meta.value()?;
    let expr: Expr = value_stream.parse()?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupportedInterface {
    Gpio,
    Pwm,
    I2c,
    Spi,
    Uart,
    Usb,
}

pub(crate) fn validate_interface(value: &Ident) -> syn::Result<SupportedInterface> {
    match value.to_string().as_str() {
        "Gpio" => Ok(SupportedInterface::Gpio),
        "Pwm" => Ok(SupportedInterface::Pwm),
        "I2c" => Ok(SupportedInterface::I2c),
        "Spi" => Ok(SupportedInterface::Spi),
        "Uart" => Ok(SupportedInterface::Uart),
        "Usb" => Ok(SupportedInterface::Usb),
        _ => Err(syn::Error::new_spanned(
            value,
            "unsupported interface; expected one of Gpio, Pwm, I2c, Spi, Uart, Usb",
        )),
    }
}

pub(crate) fn validate_kind(value: &Ident) -> syn::Result<()> {
    match value.to_string().as_str() {
        "Unspecified" | "GpioChip" | "GpioLine" | "PwmChip" | "PwmChannel" | "I2cBus"
        | "I2cDevice" | "SpiBus" | "SpiDevice" | "UartPort" | "UartDevice" | "UsbBus"
        | "UsbDevice" | "UsbInterface" => Ok(()),
        _ => Err(syn::Error::new_spanned(
            value,
            "unsupported device kind for current Lemnos core model",
        )),
    }
}

pub(crate) fn validate_unique_names<'a, I>(items: I, message: &str) -> syn::Result<()>
where
    I: IntoIterator<Item = (&'a Ident, &'a LitStr)>,
{
    let mut seen = BTreeSet::new();
    for (ident, name) in items {
        if !seen.insert(name.value()) {
            return Err(syn::Error::new_spanned(
                ident,
                format!("{message} `{}`", name.value()),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_unique_keys<'a, I>(items: I, message: &str) -> syn::Result<()>
where
    I: IntoIterator<Item = (&'a Ident, &'a str)>,
{
    let mut seen = BTreeSet::new();
    for (ident, key) in items {
        if !seen.insert(key.to_string()) {
            return Err(syn::Error::new_spanned(ident, format!("{message} `{key}`")));
        }
    }
    Ok(())
}
