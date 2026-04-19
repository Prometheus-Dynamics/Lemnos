use proc_macro2::TokenStream;
use quote::quote;
use std::collections::BTreeMap;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, ItemEnum, Result, Token, Type, Variant};

pub(crate) fn expand(attr: proc_macro::TokenStream, item: proc_macro::TokenStream) -> TokenStream {
    let spec = syn::parse::<EnumValueSpec>(attr);
    let item_enum = syn::parse::<ItemEnum>(item);

    match (spec, item_enum) {
        (Ok(spec), Ok(item_enum)) => {
            expand_enum_values(spec, item_enum).unwrap_or_else(|error| error.to_compile_error())
        }
        (Err(error), _) => error.to_compile_error(),
        (_, Err(error)) => error.to_compile_error(),
    }
}

fn expand_enum_values(spec: EnumValueSpec, mut item_enum: ItemEnum) -> syn::Result<TokenStream> {
    let ident = &item_enum.ident;
    let variant_values = item_enum
        .variants
        .iter()
        .map(|variant| parse_variant_values(variant, &spec))
        .collect::<syn::Result<Vec<_>>>()?;

    for variant in &mut item_enum.variants {
        variant.attrs.retain(|attr| !attr.path().is_ident("lemnos"));
    }

    let methods = spec.methods.iter().map(|method| {
        let method_ident = &method.name;
        let return_ty = &method.ty;
        let arms = item_enum
            .variants
            .iter()
            .zip(variant_values.iter())
            .map(|(variant, values)| {
                let variant_ident = &variant.ident;
                let expr = values
                    .get(&method.name.to_string())
                    .expect("validated variant method value exists");
                quote!(Self::#variant_ident => #expr)
            });

        quote! {
            pub const fn #method_ident(self) -> #return_ty {
                match self {
                    #( #arms, )*
                }
            }
        }
    });

    Ok(quote! {
        #item_enum

        impl #ident {
            #( #methods )*
        }
    })
}

fn parse_variant_values(
    variant: &Variant,
    spec: &EnumValueSpec,
) -> syn::Result<BTreeMap<String, Expr>> {
    let mut values = BTreeMap::new();

    for attr in variant
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("lemnos"))
    {
        attr.parse_nested_meta(|meta| {
            let key = meta.path.require_ident()?.to_string();
            if !spec.contains(&key) {
                return Err(meta.error(format!("unsupported enum value key `{key}` for this enum")));
            }

            let value = meta.value()?;
            let expr: Expr = value.parse()?;
            values.insert(key, expr);
            Ok(())
        })?;
    }

    for method in &spec.methods {
        if !values.contains_key(&method.name.to_string()) {
            return Err(syn::Error::new_spanned(
                variant,
                format!(
                    "missing required `#[lemnos({} = ...)]` value on variant `{}`",
                    method.name, variant.ident
                ),
            ));
        }
    }

    Ok(values)
}

struct EnumValueSpec {
    methods: Vec<MethodSpec>,
}

impl EnumValueSpec {
    fn contains(&self, name: &str) -> bool {
        self.methods.iter().any(|method| method.name == name)
    }
}

impl Parse for EnumValueSpec {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut methods = Vec::new();

        while !input.is_empty() {
            let name: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            let ty: Type = input.parse()?;
            methods.push(MethodSpec { name, ty });

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        if methods.is_empty() {
            return Err(input.error("expected at least one enum value method"));
        }

        Ok(Self { methods })
    }
}

struct MethodSpec {
    name: Ident,
    ty: Type,
}
