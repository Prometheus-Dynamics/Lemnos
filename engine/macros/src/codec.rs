//! `#[derive(Encode)]` and `#[derive(Decode)]` for a struct of named fields.
//!
//! A struct is written as a block: a `u32` byte length, then its fields in order. A reader takes
//! the block and reads the fields it knows; bytes left after them are skipped, so a field a newer
//! writer appended is ignored. A field the block ends before takes its `Default`, so a field a
//! writer from before it did not send reads as the default. Every field type therefore implements
//! `Default`. A struct may have one lifetime parameter (for its `&str` and `&[u8]` fields) and no
//! type parameters.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, FieldsNamed, Result, parse2, spanned::Spanned};

fn plain_struct(input: &DeriveInput) -> Result<&FieldsNamed> {
    if input.generics.type_params().next().is_some()
        || input.generics.const_params().next().is_some()
    {
        return Err(syn::Error::new(
            input.generics.span(),
            "a wire type can't have type or const parameters",
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
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let idents: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();
    let types: Vec<_> = fields.named.iter().map(|f| &f.ty).collect();
    let labels: Vec<String> = fields
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
        .collect();
    Ok(quote! {
        impl #impl_generics ::lemnos_core::Encode for #name #ty_generics #where_clause {
            const SHAPE: ::lemnos_core::Shape = ::lemnos_core::Shape::Record(&[
                #(::lemnos_core::Field {
                    name: #labels,
                    shape: <#types as ::lemnos_core::Encode>::SHAPE,
                }),*
            ]);
            fn encode(&self, out: &mut ::lemnos_core::Writer<'_>) -> ::lemnos_core::Result<()> {
                let at = out.begin_block()?;
                #(::lemnos_core::Encode::encode(&self.#idents, out)?;)*
                out.end_block(at)
            }
        }
    })
}

pub fn decode(item: TokenStream) -> Result<TokenStream> {
    let input: DeriveInput = parse2(item)?;
    let fields = plain_struct(&input)?;
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let idents: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();
    let types: Vec<_> = fields.named.iter().map(|f| &f.ty).collect();
    let lifetime = input.generics.lifetimes().next().map_or_else(
        || quote!('_),
        |l| {
            let lt = &l.lifetime;
            quote!(#lt)
        },
    );
    Ok(quote! {
        impl #impl_generics ::lemnos_core::Decode<#lifetime> for #name #ty_generics #where_clause {
            fn decode(from: &mut ::lemnos_core::Reader<#lifetime>) -> ::lemnos_core::Result<Self> {
                #[allow(unused_mut)]
                let mut body = from.block()?;
                #(
                    let #idents: #types = if body.remaining() == 0 {
                        ::core::default::Default::default()
                    } else {
                        ::lemnos_core::Decode::decode(&mut body)?
                    };
                )*
                ::core::result::Result::Ok(Self { #(#idents),* })
            }
        }
    })
}
