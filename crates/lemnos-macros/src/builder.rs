use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, FieldsNamed, GenericParam, ItemStruct, Type};

pub(crate) fn expand_builder_for_derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "Lemnos builder generation does not yet support generics",
        ));
    }

    let fields = extract_named_fields_from_data(&input.data)?;
    Ok(expand_builder(
        &input.ident,
        &fields.named,
        BuilderMode::Inherent,
    ))
}

pub(crate) fn expand_builder_for_struct(item: &ItemStruct) -> syn::Result<TokenStream> {
    if !item.generics.params.is_empty() {
        let generic = item
            .generics
            .params
            .iter()
            .find(|param| {
                matches!(
                    param,
                    GenericParam::Type(_) | GenericParam::Const(_) | GenericParam::Lifetime(_)
                )
            })
            .expect("checked generic params are not empty");
        return Err(syn::Error::new_spanned(
            generic,
            "Lemnos builder generation does not yet support generics",
        ));
    }

    let fields = match &item.fields {
        Fields::Named(fields) => fields,
        Fields::Unit => {
            return Ok(TokenStream::new());
        }
        Fields::Unnamed(fields) => {
            return Err(syn::Error::new_spanned(
                fields,
                "Lemnos builder generation only supports named-field structs",
            ));
        }
    };

    Ok(expand_builder(
        &item.ident,
        &fields.named,
        BuilderMode::Inherent,
    ))
}

pub(crate) fn expand_builder_only(input: &DeriveInput) -> syn::Result<TokenStream> {
    expand_builder_for_derive(input)
}

fn extract_named_fields_from_data(data: &Data) -> syn::Result<&FieldsNamed> {
    match data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => Ok(fields),
            Fields::Unit => Err(syn::Error::new_spanned(
                &data.fields,
                "Lemnos builder generation only supports named-field structs",
            )),
            Fields::Unnamed(fields) => Err(syn::Error::new_spanned(
                fields,
                "Lemnos builder generation only supports named-field structs",
            )),
        },
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "Lemnos builder generation only supports structs",
        )),
    }
}

enum BuilderMode {
    Inherent,
}

fn expand_builder(
    struct_ident: &syn::Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
    _mode: BuilderMode,
) -> TokenStream {
    let builder_ident = format_ident!("{struct_ident}Builder");

    let builder_fields = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");
        let builder_ty = builder_field_type(&field.ty);
        quote! {
            #field_ident: #builder_ty
        }
    });

    let builder_defaults = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");
        quote! {
            #field_ident: ::core::default::Default::default()
        }
    });

    let setters = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");
        let setter_ty = setter_arg_type(&field.ty);
        quote! {
            pub fn #field_ident(mut self, value: impl ::core::convert::Into<#setter_ty>) -> Self {
                self.#field_ident = ::core::option::Option::Some(value.into());
                self
            }
        }
    });

    let build_fields = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");
        if is_option_type(&field.ty).is_some() {
            quote! {
                #field_ident: self.#field_ident
            }
        } else {
            let message = format!("missing required field '{}'", field_ident);
            quote! {
                #field_ident: self.#field_ident.ok_or_else(|| ::std::string::String::from(#message))?
            }
        }
    });

    let from_fields = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields always have identifiers");
        if is_option_type(&field.ty).is_some() {
            quote! {
                #field_ident: value.#field_ident
            }
        } else {
            quote! {
                #field_ident: ::core::option::Option::Some(value.#field_ident)
            }
        }
    });

    quote! {
        #[derive(Debug, Clone)]
        pub struct #builder_ident {
            #(pub(crate) #builder_fields,)*
        }

        impl #builder_ident {
            #( #setters )*

            pub fn build(self) -> ::core::result::Result<#struct_ident, ::std::string::String> {
                ::core::result::Result::Ok(#struct_ident {
                    #( #build_fields, )*
                })
            }
        }

        impl ::core::default::Default for #builder_ident {
            fn default() -> Self {
                Self {
                    #( #builder_defaults, )*
                }
            }
        }

        impl #struct_ident {
            pub fn builder() -> #builder_ident {
                #builder_ident::default()
            }

            pub fn into_builder(self) -> #builder_ident {
                self.into()
            }
        }

        impl ::core::convert::From<#struct_ident> for #builder_ident {
            fn from(value: #struct_ident) -> Self {
                Self {
                    #( #from_fields, )*
                }
            }
        }
    }
}

fn builder_field_type(ty: &Type) -> TokenStream {
    if let Some(inner) = is_option_type(ty) {
        quote!(::core::option::Option<#inner>)
    } else {
        quote!(::core::option::Option<#ty>)
    }
}

fn setter_arg_type(ty: &Type) -> TokenStream {
    if let Some(inner) = is_option_type(ty) {
        quote!(#inner)
    } else {
        quote!(#ty)
    }
}

fn is_option_type(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let generic = arguments.args.first()?;
    let syn::GenericArgument::Type(inner) = generic else {
        return None;
    };
    Some(inner)
}
