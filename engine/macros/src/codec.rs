use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, FieldsNamed, Result, parse2, spanned::Spanned};

fn plain_struct(input: &DeriveInput) -> Result<&FieldsNamed> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "a wire type can't be generic",
        ));
    }
    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => Ok(named),
            other => Err(syn::Error::new(
                other.span(),
                "a wire type has named fields",
            )),
        },
        _ => Err(syn::Error::new(
            input.ident.span(),
            "a wire type is a struct",
        )),
    }
}

pub fn encode(item: TokenStream) -> Result<TokenStream> {
    let input: DeriveInput = parse2(item)?;
    let fields = plain_struct(&input)?;
    let name = &input.ident;
    let idents: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();
    let types: Vec<_> = fields.named.iter().map(|f| &f.ty).collect();
    let labels: Vec<String> = fields
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
        .collect();
    Ok(quote! {
        impl ::lemnos_core::Encode for #name {
            const SHAPE: ::lemnos_core::Shape = ::lemnos_core::Shape::Record(&[
                #(::lemnos_core::Field {
                    name: #labels,
                    shape: <#types as ::lemnos_core::Encode>::SHAPE,
                }),*
            ]);
            fn encode(&self, out: &mut ::lemnos_core::Writer<'_>) -> ::lemnos_core::Result<()> {
                #(::lemnos_core::Encode::encode(&self.#idents, out)?;)*
                ::core::result::Result::Ok(())
            }
        }
    })
}

pub fn decode(item: TokenStream) -> Result<TokenStream> {
    let input: DeriveInput = parse2(item)?;
    let fields = plain_struct(&input)?;
    let name = &input.ident;
    let idents: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();
    Ok(quote! {
        impl ::lemnos_core::Decode<'_> for #name {
            fn decode(from: &mut ::lemnos_core::Reader<'_>) -> ::lemnos_core::Result<Self> {
                ::core::result::Result::Ok(Self {
                    #(#idents: ::lemnos_core::Decode::decode(from)?,)*
                })
            }
        }
    })
}
