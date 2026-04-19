mod codegen;
mod helpers;
mod spec;

use crate::builder;
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let builder_tokens = builder::expand_builder_for_derive(input)?;
    let spec = spec::ConfiguredDeviceSpec::parse(input)?;

    if !spec.enabled() {
        return Ok(builder_tokens);
    }

    let descriptor_tokens = codegen::expand_configured_device(input, &spec)?;
    Ok(quote! {
        #builder_tokens
        #descriptor_tokens
    })
}
