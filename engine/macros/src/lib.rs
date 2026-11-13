//! `#[contract]`, `#[driver]` and the codec derives. Re-exported by
//! `lemnos-core`; generated code refers to that crate.

mod codec;
mod contract;
mod driver;

use proc_macro::TokenStream;

/// Turns a trait into a contract: its client, its dispatcher and its
/// description. Every method carries a stable `#[method(N)]`.
///
/// ```ignore
/// #[contract("example.thing", version = 1)]
/// pub trait Thing {
///     #[method(1)]
///     fn read(&mut self) -> Result<u32>;
///     #[method(2)]
///     fn dump(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
///     /// Starts a long job: the host carries its progress and result to the caller.
///     #[method(3, job)]
///     fn rebuild(&mut self, from: u32) -> Result<JobId>;
/// }
/// ```
#[proc_macro_attribute]
pub fn contract(args: TokenStream, item: TokenStream) -> TokenStream {
    contract::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Writes a struct's `Driver` implementation from its `#[needs]` fields and
/// the contracts it provides. The author implements `Lifecycle`.
///
/// ```ignore
/// #[driver("example", provides = [Thing])]
/// pub struct Example {
///     #[needs] bus: I2c,
///     #[needs(optional)] other: Option<Dev<dyn Thing>>,
///     count: u32,
/// }
/// ```
#[proc_macro_attribute]
pub fn driver(args: TokenStream, item: TokenStream) -> TokenStream {
    driver::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Encodes a struct's named fields in order.
#[proc_macro_derive(Encode)]
pub fn derive_encode(item: TokenStream) -> TokenStream {
    codec::encode(item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Decodes a struct's named fields in order.
#[proc_macro_derive(Decode)]
pub fn derive_decode(item: TokenStream) -> TokenStream {
    codec::decode(item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
