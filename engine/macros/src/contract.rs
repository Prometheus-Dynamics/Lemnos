use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, FnArg, GenericArgument, Ident, ItemTrait, LitInt, LitStr, Pat, PathArguments,
    ReceiverKind, Result, ReturnType, Token, TraitItem, TraitItemFn, Type, parse2,
};

struct Args {
    name: LitStr,
    version: LitInt,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![,]>()?;
        let key: Ident = input.parse()?;
        if key != "version" {
            return Err(Error::new(key.span(), "expected `version = N`"));
        }
        input.parse::<Token![=]>()?;
        Ok(Self {
            name,
            version: input.parse()?,
        })
    }
}

struct Param {
    ident: Ident,
    ty: Type,
}

struct Method {
    id: u16,
    job: bool,
    ident: Ident,
    params: Vec<Param>,
    out: Option<Ident>,
    returns: Type,
}

/// `#[method(N)]`, or `#[method(N, job)]` for a method that starts a long job.
struct MethodArgs {
    id: LitInt,
    job: bool,
}

impl Parse for MethodArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let id = input.parse()?;
        let mut job = false;
        if input.parse::<Option<Token![,]>>()?.is_some() {
            let word: Ident = input.parse()?;
            if word != "job" {
                return Err(Error::new(
                    word.span(),
                    "expected #[method(N)] or #[method(N, job)]",
                ));
            }
            job = true;
        }
        Ok(Self { id, job })
    }
}

fn remove_method_id(method: &mut TraitItemFn) -> Result<(u16, bool)> {
    let position = method
        .attrs
        .iter()
        .position(|attr| attr.path().is_ident("method"))
        .ok_or_else(|| {
            Error::new(
                method.sig.ident.span(),
                "every contract method declares a stable number: #[method(N)]",
            )
        })?;
    let attr = method.attrs.remove(position);
    let args = attr.parse_args::<MethodArgs>()?;
    Ok((args.id.base10_parse()?, args.job))
}

fn is_job_id(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "JobId"))
}

fn ok_type(output: &ReturnType) -> Result<Type> {
    let error = || Error::new(output.span(), "a contract method returns Result<T>");
    let ReturnType::Type(_, ty) = output else {
        return Err(error());
    };
    let Type::Path(path) = &**ty else {
        return Err(error());
    };
    let last = path.path.segments.last().ok_or_else(error)?;
    if last.ident != "Result" {
        return Err(error());
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(error());
    };
    match args.args.first() {
        Some(GenericArgument::Type(inner)) => Ok(inner.clone()),
        _ => Err(error()),
    }
}

fn is_mut_bytes(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    let Type::Slice(slice) = &*reference.elem else {
        return false;
    };
    reference.mutability.is_some() && matches!(&*slice.elem, Type::Path(p) if p.path.is_ident("u8"))
}

fn is_usize(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("usize"))
}

fn parse_method(method: &mut TraitItemFn) -> Result<Method> {
    if method.default.is_some() {
        return Err(Error::new(
            method.sig.ident.span(),
            "a contract method has no default body",
        ));
    }
    if !method.sig.generics.params.is_empty() {
        return Err(Error::new(
            method.sig.generics.span(),
            "a contract method can't be generic",
        ));
    }
    let (id, job) = remove_method_id(method)?;
    let returns = ok_type(&method.sig.output)?;
    if job && !is_job_id(&returns) {
        return Err(Error::new(
            method.sig.output.span(),
            "a job method returns Result<JobId>",
        ));
    }
    let mut inputs = method.sig.inputs.iter_mut();
    match inputs.next() {
        Some(FnArg::Receiver(r)) if matches!(r.kind, ReceiverKind::Reference(_, _, Some(_))) => {}
        _ => {
            return Err(Error::new(
                method.sig.ident.span(),
                "a contract method takes &mut self",
            ));
        }
    }
    let mut params = Vec::new();
    let mut out = None;
    for input in inputs {
        let FnArg::Typed(arg) = input else {
            return Err(Error::new(input.span(), "unexpected receiver"));
        };
        let Pat::Ident(pat) = &*arg.pat else {
            return Err(Error::new(arg.pat.span(), "parameters must be plain names"));
        };
        let ident = pat.ident.clone();
        if let Some(position) = arg.attrs.iter().position(|a| a.path().is_ident("out")) {
            arg.attrs.remove(position);
            if out.is_some() {
                return Err(Error::new(ident.span(), "only one #[out] buffer"));
            }
            if !is_mut_bytes(&arg.ty) {
                return Err(Error::new(arg.ty.span(), "#[out] takes &mut [u8]"));
            }
            if !is_usize(&returns) {
                return Err(Error::new(
                    method.sig.output.span(),
                    "a method with an #[out] buffer returns Result<usize>",
                ));
            }
            out = Some(ident);
        } else {
            if out.is_some() {
                return Err(Error::new(ident.span(), "#[out] comes last"));
            }
            params.push(Param {
                ident,
                ty: (*arg.ty).clone(),
            });
        }
    }
    if job && out.is_some() {
        return Err(Error::new(
            method.sig.ident.span(),
            "a job method has no #[out] buffer: its result comes when it finishes",
        ));
    }
    Ok(Method {
        id,
        job,
        ident: method.sig.ident.clone(),
        params,
        out,
        returns,
    })
}

fn server_arm(method: &Method) -> TokenStream {
    let id = method.id;
    let name = &method.ident;
    let args: Vec<_> = method.params.iter().map(|p| &p.ident).collect();
    let types: Vec<_> = method.params.iter().map(|p| &p.ty).collect();
    let body = if method.out.is_some() {
        quote! {
            let __capacity = <u32 as ::lemnos_core::Decode>::decode(&mut __from)? as usize;
            let __n = self.#name(#(#args,)* __reply.out(__capacity)?)?;
            __reply.advance(__n)
        }
    } else {
        quote! {
            let __value = self.#name(#(#args),*)?;
            ::lemnos_core::Encode::encode(&__value, __reply)
        }
    };
    quote! {
        #id => (|| -> ::lemnos_core::Result<()> {
            let mut __from = ::lemnos_core::Reader::new(__args);
            #(let #args: #types = ::lemnos_core::Decode::decode(&mut __from)?;)*
            #body
        })(),
    }
}

fn client_method(method: &Method, sig: &syn::Signature) -> TokenStream {
    let id = method.id;
    let args: Vec<_> = method.params.iter().map(|p| &p.ident).collect();
    let encode_capacity = method.out.as_ref().map(|out| {
        quote! {
            ::lemnos_core::Encode::encode(&(#out.len() as u32), &mut __writer)?;
        }
    });
    let finish = match &method.out {
        _ if method.job => quote! {
            self.caller.start_job(#id, __len)
        },
        Some(out) => quote! {
            self.caller.call(#id, __len, #out)
        },
        None => quote! {
            let mut __reply = [0u8; ::lemnos_core::REPLY_INLINE];
            let __n = self.caller.call(#id, __len, &mut __reply)?;
            let mut __from = ::lemnos_core::Reader::new(&__reply[..__n]);
            ::lemnos_core::Decode::decode(&mut __from)
        },
    };
    quote! {
        #sig {
            let __len = {
                let mut __writer = ::lemnos_core::Writer::new(
                    ::lemnos_core::Caller::args(&mut self.caller),
                );
                #(::lemnos_core::Encode::encode(&#args, &mut __writer)?;)*
                #encode_capacity
                __writer.len()
            };
            use ::lemnos_core::Caller as _;
            #finish
        }
    }
}

fn method_info(method: &Method) -> TokenStream {
    let id = method.id;
    let name = method.ident.to_string();
    let labels: Vec<String> = method.params.iter().map(|p| p.ident.to_string()).collect();
    let types: Vec<_> = method.params.iter().map(|p| &p.ty).collect();
    let returned = &method.returns;
    let has_out = method.out.is_some();
    let job = method.job;
    // The reply to an #[out] method is the bytes themselves.
    let returns = if has_out {
        quote! { ::lemnos_core::Shape::Bytes }
    } else {
        quote! { <#returned as ::lemnos_core::Encode>::SHAPE }
    };
    quote! {
        ::lemnos_core::MethodInfo {
            id: #id,
            name: #name,
            params: &[#(::lemnos_core::Param {
                name: #labels,
                shape: <#types as ::lemnos_core::Encode>::SHAPE,
            }),*],
            returns: #returns,
            has_out_buffer: #has_out,
            job: #job,
        }
    }
}

pub fn expand(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let args: Args = parse2(args)?;
    let mut input: ItemTrait = parse2(item)?;
    if !input.generics.params.is_empty() {
        return Err(Error::new(
            input.generics.span(),
            "a contract can't be generic",
        ));
    }
    let mut methods = Vec::new();
    let mut signatures = Vec::new();
    let mut seen = BTreeSet::new();
    for item in &mut input.items {
        let TraitItem::Fn(function) = item else {
            return Err(Error::new(item.span(), "a contract only has methods"));
        };
        let method = parse_method(function)?;
        if !seen.insert(method.id) {
            return Err(Error::new(
                method.ident.span(),
                format!("method number {} is used twice", method.id),
            ));
        }
        signatures.push(function.sig.clone());
        methods.push(method);
    }

    let version: u32 = args.version.base10_parse()?;
    let tag = format!("{}/{}", args.name.value(), version);
    let name = input.ident.clone();
    let serve = format_ident!("__serve");
    let arms = methods.iter().map(server_arm);
    let clients = methods
        .iter()
        .zip(&signatures)
        .map(|(method, sig)| client_method(method, sig));
    let infos = methods.iter().map(method_info);

    input.items.push(syn::parse_quote! {
        #[doc(hidden)]
        fn #serve(
            &mut self,
            __method: u16,
            __args: &[u8],
            __reply: &mut ::lemnos_core::Reply<'_>,
        ) -> ::lemnos_core::Status {
            let __result: ::lemnos_core::Result<()> = match __method {
                #(#arms)*
                _ => ::core::result::Result::Err(::lemnos_core::Status::NOT_SUPPORTED),
            };
            match __result {
                ::core::result::Result::Ok(()) => ::lemnos_core::Status::OK,
                ::core::result::Result::Err(__status) => __status,
            }
        }
    });

    Ok(quote! {
        #input

        impl ::lemnos_core::Contract for dyn #name {
            const TAG: ::lemnos_core::Tag = ::lemnos_core::Tag::new(#tag);
            const INFO: ::lemnos_core::ContractInfo = ::lemnos_core::ContractInfo {
                tag: ::lemnos_core::Tag::new(#tag),
                methods: &[#(#infos),*],
            };
            const HASH: u64 = ::lemnos_core::layout::contract_hash(&Self::INFO);
        }

        impl<__T: ::lemnos_core::Caller> #name for ::lemnos_core::Bound<dyn #name, __T> {
            #(#clients)*
        }
    })
}
