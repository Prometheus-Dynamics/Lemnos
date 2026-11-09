use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Error, Fields, Ident, ItemStruct, LitStr, Path, Result, Token, Type, bracketed, parse2};

struct Args {
    name: LitStr,
    provides: Vec<Path>,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![,]>()?;
        let key: Ident = input.parse()?;
        if key != "provides" {
            return Err(Error::new(key.span(), "expected `provides = [..]`"));
        }
        input.parse::<Token![=]>()?;
        let content;
        bracketed!(content in input);
        let provides = Punctuated::<Path, Token![,]>::parse_terminated(&content)?;
        Ok(Self {
            name,
            provides: provides.into_iter().collect(),
        })
    }
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

pub fn expand(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let args: Args = parse2(args)?;
    let mut input: ItemStruct = parse2(item)?;
    if !input.generics.params.is_empty() {
        return Err(Error::new(
            input.generics.span(),
            "a driver can't be generic",
        ));
    }
    let Fields::Named(fields) = &mut input.fields else {
        return Err(Error::new(input.span(), "a driver has named fields"));
    };

    let mut need_idents = Vec::new();
    let mut need_types = Vec::new();
    for field in &mut fields.named {
        let Some(position) = field.attrs.iter().position(|a| a.path().is_ident("needs")) else {
            continue;
        };
        let attr = field.attrs.remove(position);
        let optional = match &attr.meta {
            syn::Meta::Path(_) => false,
            _ => {
                let word: Ident = attr.parse_args()?;
                if word != "optional" {
                    return Err(Error::new(word.span(), "expected #[needs(optional)]"));
                }
                true
            }
        };
        if optional != is_option(&field.ty) {
            return Err(Error::new(
                field.ty.span(),
                "#[needs(optional)] and Option<..> go together",
            ));
        }
        need_idents.push(field.ident.clone());
        need_types.push(field.ty.clone());
    }

    let name = input.ident.clone();
    let vis = input.vis.clone();
    let needs_struct = format_ident!("{}Needs", name);
    let driver_name = args.name;
    let need_labels: Vec<String> = need_idents
        .iter()
        .flatten()
        .map(ToString::to_string)
        .collect();
    let provides = &args.provides;
    let indexes = (0..provides.len()).map(|i| i as u8);
    if provides.len() > usize::from(u8::MAX) {
        return Err(Error::new(driver_name.span(), "too many contracts"));
    }

    Ok(quote! {
        #input

        /// What this driver is given when it binds.
        #vis struct #needs_struct {
            #(pub #need_idents: #need_types,)*
        }

        impl ::lemnos_core::Driver for #name {
            const MANIFEST: ::lemnos_core::Manifest = ::lemnos_core::Manifest {
                name: #driver_name,
                needs: &[#(::lemnos_core::Need {
                    name: #need_labels,
                    kind: <#need_types as ::lemnos_core::Resource>::KIND,
                    optional: <#need_types as ::lemnos_core::Resource>::OPTIONAL,
                }),*],
                provides: &[#(<dyn #provides as ::lemnos_core::Contract>::INFO),*],
            };

            fn bind(
                __res: &mut ::lemnos_core::Resources<'_>,
                __cfg: &::lemnos_core::Config<'_>,
            ) -> ::lemnos_core::Result<Self> {
                let __needs = #needs_struct {
                    #(#need_idents: <#need_types as ::lemnos_core::Resource>::take(__res, #need_labels)?,)*
                };
                <Self as ::lemnos_core::Lifecycle>::start(__needs, __cfg)
            }

            fn describe(&self, __out: &mut ::lemnos_core::Description) {
                <Self as ::lemnos_core::Lifecycle>::describe(self, __out)
            }

            fn health(&self) -> ::lemnos_core::Health {
                <Self as ::lemnos_core::Lifecycle>::health(self)
            }

            const TICKS: bool = <Self as ::lemnos_core::Lifecycle>::TICKS;

            fn tick(&mut self, __now_ns: u64) -> ::core::option::Option<u64> {
                <Self as ::lemnos_core::Lifecycle>::tick(self, __now_ns)
            }

            const TRACKS_CALLERS: bool = <Self as ::lemnos_core::Lifecycle>::TRACKS_CALLERS;

            fn caller(&mut self, __caller: ::lemnos_core::CallerId) {
                <Self as ::lemnos_core::Lifecycle>::caller(self, __caller)
            }

            fn caller_gone(&mut self, __caller: ::lemnos_core::CallerId) {
                <Self as ::lemnos_core::Lifecycle>::caller_gone(self, __caller)
            }

            fn dispatch(
                &mut self,
                __method: ::lemnos_core::MethodId,
                __args: &[u8],
                __reply: &mut ::lemnos_core::Reply<'_>,
            ) -> ::lemnos_core::Status {
                match __method.contract {
                    #(#indexes => <Self as #provides>::__serve(self, __method.method, __args, __reply),)*
                    _ => ::lemnos_core::Status::NOT_SUPPORTED,
                }
            }
        }
    })
}
