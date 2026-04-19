use proc_macro::TokenStream;
use syn::{DeriveInput, Item, parse_macro_input};

mod builder;
mod configured_device;
mod driver;
mod enum_values;

#[proc_macro_attribute]
pub fn driver(attr: TokenStream, item: TokenStream) -> TokenStream {
    driver::expand(attr, item).into()
}

#[proc_macro_attribute]
pub fn interaction(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as Item);
    quote::quote!(#item).into()
}

#[proc_macro_attribute]
pub fn enum_values(attr: TokenStream, item: TokenStream) -> TokenStream {
    enum_values::expand(attr, item).into()
}

#[proc_macro_derive(ConfiguredDevice, attributes(lemnos))]
pub fn derive_configured_device(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    configured_device::expand(&input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

#[proc_macro_derive(LemnosResource, attributes(lemnos))]
pub fn derive_lemnos_resource(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    builder::expand_builder_only(&input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

#[proc_macro_derive(LemnosDriver, attributes(lemnos))]
pub fn derive_lemnos_driver(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    builder::expand_builder_only(&input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}
