use core::marker::PhantomData;

use crate::{Health, MethodId, Reading, Result, Shape, Status, Tag};

/// One parameter of a contract method.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub shape: Shape,
}

/// One method of a contract, for tools that have no code for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MethodInfo {
    /// Declared in the contract. Never changes.
    pub id: u16,
    pub name: &'static str,
    pub params: &'static [Param],
    pub returns: Shape,
    /// The method also fills a caller-supplied byte buffer.
    pub has_out_buffer: bool,
    /// The method starts a long job and returns its [`JobId`](crate::JobId): it is started with
    /// the host's job form, not called.
    pub job: bool,
    /// How the value this method reads or sets shows, when the contract says. Not part of the
    /// method's layout: see [`Description`](crate::Description) for how a device's items use it.
    pub reading: Option<Reading>,
}

/// A contract as data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContractInfo {
    pub tag: Tag,
    /// What the contract is for, for grouping devices: `"sensor"`, `"actuator"`, `"camera"` and
    /// so on, or `"other"`. Declared once in the contract crate; not part of the layout.
    pub kind: &'static str,
    pub methods: &'static [MethodInfo],
}

/// Implemented on `dyn YourContract` by `#[contract]`.
pub trait Contract {
    const TAG: Tag;
    const INFO: ContractInfo;
    /// The contract's encoding hash, [`layout::contract_hash`](crate::layout::contract_hash) of
    /// [`INFO`](Self::INFO). A device says its hash in its description; a typed client checks it.
    const HASH: u64;
}

/// Carries a contract's calls to wherever the device is.
pub trait Caller {
    /// Where the generated client encodes arguments.
    fn args(&mut self) -> &mut [u8];
    /// Sends `args()[..args_len]` as `method` and fills `reply`.
    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize>;
    /// Starts `args()[..args_len]` as job method `method` and returns the job's number. Only
    /// a client of the service runs jobs: one driver can't start another's.
    fn start_job(&mut self, _method: u16, _args_len: usize) -> Result<crate::JobId> {
        Err(Status::NOT_SUPPORTED)
    }
}

/// A contract's client, over any way of reaching the device.
#[derive(Debug)]
pub struct Bound<C: ?Sized, T> {
    /// Used by generated code.
    pub caller: T,
    contract: PhantomData<fn() -> *const C>,
}

impl<C: ?Sized, T> Bound<C, T> {
    pub const fn new(caller: T) -> Self {
        Self {
            caller,
            contract: PhantomData,
        }
    }
}

/// Bytes of arguments one driver can pass another. Calls within a host are
/// small; anything bigger belongs in shared memory.
pub const LOCAL_ARGS: usize = 64;

/// Stack space a generated client gives a reply with no output buffer.
pub const REPLY_INLINE: usize = 256;

/// How the host gets a call to a device in the same process.
pub trait Router: Sync {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize>;
    fn health(&self, device: u32) -> Health;
}

/// A direct call to another device in the same host.
pub struct LocalCaller {
    router: &'static dyn Router,
    device: u32,
    contract: u8,
    args: [u8; LOCAL_ARGS],
}

impl LocalCaller {
    pub const fn new(router: &'static dyn Router, device: u32, contract: u8) -> Self {
        Self {
            router,
            device,
            contract,
            args: [0; LOCAL_ARGS],
        }
    }
}

impl core::fmt::Debug for LocalCaller {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LocalCaller")
            .field("device", &self.device)
            .field("contract", &self.contract)
            .finish()
    }
}

impl Caller for LocalCaller {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let args = self.args.get(..args_len).ok_or(Status::BUFFER_TOO_SMALL)?;
        let id = MethodId {
            contract: self.contract,
            method,
        };
        self.router.call(self.device, id, args, reply)
    }
}

/// Another device this driver depends on. Implements the contract `C`.
pub type Dev<C> = Bound<C, LocalCaller>;

impl<C: ?Sized> Dev<C> {
    /// How the other device is doing right now.
    pub fn health(&self) -> Health {
        self.caller.router.health(self.caller.device)
    }
}
