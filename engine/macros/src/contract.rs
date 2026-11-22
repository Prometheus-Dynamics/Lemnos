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
    kind: Option<LitStr>,
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
        let version = input.parse()?;
        // `kind = "sensor"`, after the version, when the contract has a grouping.
        let mut kind = None;
        if input.parse::<Option<Token![,]>>()?.is_some() && !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "kind" {
                return Err(Error::new(key.span(), "expected `kind = \"...\"`"));
            }
            input.parse::<Token![=]>()?;
            kind = Some(input.parse()?);
        }
        Ok(Self {
            name,
            version,
            kind,
        })
    }
}

/// The kinds a contract can declare. A kind groups devices for a tool with no code for them.
const KINDS: &[&str] = &[
    "sensor", "actuator", "camera", "power", "system", "light", "bus", "compute", "other",
];

/// The readings a method can declare, by the names [`Reading`](lemnos_core::Reading) is written with.
const READINGS: &[(&str, &str)] = &[
    ("number", "Number"),
    ("range", "Range"),
    ("switch", "Switch"),
    ("text", "Text"),
    ("colour_array", "ColourArray"),
    ("three_axes", "ThreeAxes"),
    ("events", "Events"),
];

struct Param {
    ident: Ident,
    ty: Type,
}

struct Method {
    id: u16,
    job: bool,
    /// A job's stall bound, in seconds: required for a job, zero for a method that is not one.
    stall_secs: u32,
    /// A job's result form, or an #[out] buffer's: `true` for text (UTF-8, whole), `false` for
    /// bytes. Required for both.
    result_text: bool,
    reading: Option<Ident>,
    /// An addition after the base shape: `#[added]`, unless `#[breaking]`.
    added: bool,

    ident: Ident,
    params: Vec<Param>,
    out: Option<Ident>,
    returns: Type,
}

/// The largest stall bound a job can declare, in seconds: the package ABI carries it in 24 bits.
const MAX_STALL_SECS: u32 = 0x00FF_FFFF;

/// `#[method(N)]`, `#[method(N, job, stall = SECS, result = text|bytes)]` for a method that starts a
/// long job, the longest it may go without progress and the form of its result, and `#[method(N, reading = switch)]` for the reading its
/// value shows as.
struct MethodArgs {
    id: LitInt,
    job: bool,
    stall_secs: Option<LitInt>,
    result: Option<Ident>,
    reading: Option<Ident>,
}

impl Parse for MethodArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let id: LitInt = input.parse()?;
        let mut job = false;
        let mut stall_secs = None;
        let mut result = None;
        let mut reading = None;
        while input.parse::<Option<Token![,]>>()?.is_some() {
            let word: Ident = input.parse()?;
            if word == "job" {
                job = true;
            } else if word == "stall" {
                input.parse::<Token![=]>()?;
                stall_secs = Some(input.parse::<LitInt>()?);
            } else if word == "result" {
                input.parse::<Token![=]>()?;
                let form: Ident = input.parse()?;
                if form != "text" && form != "bytes" {
                    return Err(Error::new(
                        form.span(),
                        "a job's result is `text` (UTF-8, read whole) or `bytes`",
                    ));
                }
                result = Some(form);
            } else if word == "reading" {
                input.parse::<Token![=]>()?;
                reading = Some(input.parse::<Ident>()?);
            } else {
                return Err(Error::new(
                    word.span(),
                    "expected #[method(N)], #[method(N, job, stall = SECS, result = text|bytes)] or #[method(N, reading = ...)]",
                ));
            }
        }
        if job && stall_secs.is_none() {
            return Err(Error::new(
                id.span(),
                "a job method declares how long it may go without progress: #[method(N, job, stall = SECS)]",
            ));
        }
        if job && result.is_none() {
            return Err(Error::new(
                id.span(),
                "a job method declares the form of its result: #[method(N, job, stall = SECS, result = text|bytes)]",
            ));
        }
        if !job && result.is_some() {
            return Err(Error::new(
                id.span(),
                "only a job method has a result form: add `job` to #[method(N, ...)]",
            ));
        }
        if !job && stall_secs.is_some() {
            return Err(Error::new(
                id.span(),
                "only a job method has a stall bound: add `job` to #[method(N, ...)]",
            ));
        }
        Ok(Self {
            id,
            job,
            stall_secs,
            result,
            reading,
        })
    }
}

/// A method's number, whether it is a job, its stall bound, whether its result is text, and its reading.
type MethodHead = (u16, bool, u32, bool, Option<Ident>);

fn remove_method_id(method: &mut TraitItemFn) -> Result<MethodHead> {
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
    let stall_secs = match &args.stall_secs {
        None => 0,
        Some(secs) => {
            let value: u32 = secs.base10_parse()?;
            if value == 0 || value > MAX_STALL_SECS {
                return Err(Error::new(
                    secs.span(),
                    format!("a stall bound is from 1 to {MAX_STALL_SECS} seconds"),
                ));
            }
            value
        }
    };
    let result_text = args.result.is_some_and(|form| form == "text");
    Ok((
        args.id.base10_parse()?,
        args.job,
        stall_secs,
        result_text,
        args.reading,
    ))
}

/// Removes `#[name]` from `attrs` and says whether it was there.
pub(crate) fn take_flag(attrs: &mut Vec<syn::Attribute>, name: &str) -> bool {
    let before = attrs.len();
    attrs.retain(|attr| !attr.path().is_ident(name));
    attrs.len() != before
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
    let (id, job, stall_secs, result_text, reading) = remove_method_id(method)?;
    let marked_added = take_flag(&mut method.attrs, "added");
    let breaking = take_flag(&mut method.attrs, "breaking");
    let added = marked_added && !breaking;

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
    let mut out_text = false;
    for input in inputs {
        let FnArg::Typed(arg) = input else {
            return Err(Error::new(input.span(), "unexpected receiver"));
        };
        let Pat::Ident(pat) = &*arg.pat else {
            return Err(Error::new(arg.pat.span(), "parameters must be plain names"));
        };
        let ident = pat.ident.clone();
        if let Some(position) = arg.attrs.iter().position(|a| a.path().is_ident("out")) {
            let attr = arg.attrs.remove(position);
            let form: Ident = attr.parse_args().map_err(|_| {
                Error::new(
                    attr.span(),
                    "an #[out] buffer declares the form of what it holds: #[out(text)] or #[out(bytes)]",
                )
            })?;
            if form != "text" && form != "bytes" {
                return Err(Error::new(
                    form.span(),
                    "an #[out] buffer holds `text` (UTF-8) or `bytes`",
                ));
            }
            out_text = form == "text";
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
    let reading = match reading {
        None => None,
        Some(word) => {
            let name = word.to_string();
            let (_, variant) = READINGS
                .iter()
                .find(|(written, _)| *written == name)
                .ok_or_else(|| {
                    Error::new(
                        word.span(),
                        "expected a reading: number, range, switch, text, colour_array, three_axes or events",
                    )
                })?;
            Some(format_ident!("{}", variant))
        }
    };
    Ok(Method {
        id,
        job,
        stall_secs,
        result_text: result_text || out_text,
        reading,
        added,

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
    let stall_secs = method.stall_secs;
    let reading = match &method.reading {
        Some(variant) => quote! { ::core::option::Option::Some(::lemnos_core::Reading::#variant) },
        None => quote! { ::core::option::Option::None },
    };
    let added = method.added;

    // An #[out] method's reply is its buffer, and a job's is its result: both are read whole, in
    // the form the contract declares, text (`Str`) or bytes. A job's start returns a JobId, which
    // `job` implies.
    let returns = if job || has_out {
        if method.result_text {
            quote! { ::lemnos_core::Shape::Str }
        } else {
            quote! { ::lemnos_core::Shape::Bytes }
        }
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
            stall_secs: #stall_secs,
            reading: #reading,
            added: #added,

        }
    }
}

pub fn expand(args: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let args: Args = parse2(args)?;
    let kind = match &args.kind {
        None => "other".to_string(),
        Some(kind) => {
            let value = kind.value();
            if !KINDS.contains(&value.as_str()) {
                return Err(Error::new(
                    kind.span(),
                    format!("a contract's kind is one of {}", KINDS.join(", ")),
                ));
            }
            value
        }
    };
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
    let mut last_added: Option<Ident> = None;
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
        // Additions go last: a base method after an added one would sit inside the additions.
        if method.added {
            last_added.get_or_insert_with(|| method.ident.clone());
        } else if let Some(added) = &last_added {
            return Err(Error::new(
                method.ident.span(),
                format!(
                    "a base method can't come after the added method `{added}`: additions go last"
                ),
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
                kind: #kind,
                methods: &[#(#infos),*],
            };
            const HASH: u64 = ::lemnos_core::layout::contract_hash(&Self::INFO);
        }

        impl<__T: ::lemnos_core::Caller> #name for ::lemnos_core::Bound<dyn #name, __T> {
            #(#clients)*
        }
    })
}
