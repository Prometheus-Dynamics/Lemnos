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
    // A `#[link]` field takes the device the board's `link` names. It is not a need: the board
    // gives it no name to `uses`, so it is not in the manifest.
    let mut link_idents = Vec::new();
    let mut link_types = Vec::new();
    for field in &mut fields.named {
        if let Some(position) = field.attrs.iter().position(|a| a.path().is_ident("link")) {
            if !matches!(field.attrs[position].meta, syn::Meta::Path(_)) {
                return Err(Error::new(field.attrs[position].span(), "expected #[link]"));
            }
            field.attrs.remove(position);
            link_idents.push(field.ident.clone());
            link_types.push(field.ty.clone());
            continue;
        }
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
            #(pub #link_idents: #link_types,)*
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
                    #(#link_idents: <#link_types as ::lemnos_core::Resource>::take_link(__res)?,)*
                };
                <Self as ::lemnos_core::Lifecycle>::start(__needs, __cfg)
            }

            fn bind_in(
                __place: &mut ::core::option::Option<Self>,
                __res: &mut ::lemnos_core::Resources<'_>,
                __cfg: &::lemnos_core::Config<'_>,
            ) -> ::lemnos_core::Result<()> {
                let __needs = #needs_struct {
                    #(#need_idents: <#need_types as ::lemnos_core::Resource>::take(__res, #need_labels)?,)*
                    #(#link_idents: <#link_types as ::lemnos_core::Resource>::take_link(__res)?,)*
                };
                <Self as ::lemnos_core::Lifecycle>::start_in(__place, __needs, __cfg)
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

            const WATCHES_BOARD: bool = <Self as ::lemnos_core::Lifecycle>::WATCHES_BOARD;

            fn board(&mut self, __view: &dyn ::lemnos_core::BoardView) {
                <Self as ::lemnos_core::Lifecycle>::board(self, __view)
            }

            const HOST_OWNED: ::lemnos_core::HostOwned =
                <Self as ::lemnos_core::Lifecycle>::HOST_OWNED;

            const I2C_REGISTER_ADDRESS: ::core::option::Option<::lemnos_core::AddressWidth> =
                <Self as ::lemnos_core::Lifecycle>::I2C_REGISTER_ADDRESS;

            const TRACKS_CALLERS: bool = <Self as ::lemnos_core::Lifecycle>::TRACKS_CALLERS;

            fn caller(&mut self, __caller: ::lemnos_core::CallerId) {
                <Self as ::lemnos_core::Lifecycle>::caller(self, __caller)
            }

            fn caller_gone(&mut self, __caller: ::lemnos_core::CallerId) {
                <Self as ::lemnos_core::Lifecycle>::caller_gone(self, __caller)
            }

            fn endpoints(
                &mut self,
                __caller: ::lemnos_core::CallerId,
            ) -> ::lemnos_core::Result<[::lemnos_core::BufferHandle<'_>; 2]> {
                <Self as ::lemnos_core::Lifecycle>::endpoints(self, __caller)
            }

            fn release_endpoints(
                &mut self,
                __caller: ::lemnos_core::CallerId,
            ) -> ::lemnos_core::Result<()> {
                <Self as ::lemnos_core::Lifecycle>::release_endpoints(self, __caller)
            }

            fn endpoints_ended(&mut self) -> bool {
                <Self as ::lemnos_core::Lifecycle>::endpoints_ended(self)
            }

            const JOBS: bool = <Self as ::lemnos_core::Lifecycle>::JOBS;

            fn job(
                &mut self,
                __job: ::lemnos_core::JobId,
                __result: &mut ::lemnos_core::Reply<'_>,
            ) -> ::lemnos_core::JobState {
                <Self as ::lemnos_core::Lifecycle>::job(self, __job, __result)
            }

            fn job_cancel(&mut self, __job: ::lemnos_core::JobId) {
                <Self as ::lemnos_core::Lifecycle>::job_cancel(self, __job)
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
