use core::marker::PhantomData;

use crate::{CallerId, Health, JobId, MethodId, Reading, Result, Shape, Status, Tag};

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
    /// An addition after the base shape: appended after every base method. Set by `#[added]`
    /// (unless `#[breaking]`). Left out of the contract's base hash.
    pub added: bool,
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
    /// The contract's base hash, [`layout::contract_hash`](crate::layout::contract_hash) of
    /// [`INFO`](Self::INFO): the base methods only. A device says it in its description; a typed
    /// client checks it, then checks each of its own methods (added ones too) by their hashes.
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

/// How a long job a driver started on another device stands, from [`Router::job_poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polled {
    /// Still going: `done` of `total`, as its contract's job says (0 when not known).
    Running { done: u64, total: u64 },
    /// Over. `len` bytes of its result were copied to the buffer; they are the result only when
    /// `status` is OK.
    Finished { status: Status, len: usize },
}

/// How the host gets a call to a device in the same process.
///
/// The methods after [`Router::health`] serve a driver that calls another device it uses: a
/// caller other than the host itself, which is named by its [`CallerId`], and the long jobs it
/// starts there. A router that does not serve them answers `NOT_SUPPORTED`, which is the default.
pub trait Router: Sync {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize>;
    fn health(&self, device: u32) -> Health;
    /// [`Router::call`], for `caller`. [`CallerId::LOCAL`] is the same call.
    fn call_as(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if caller == CallerId::LOCAL {
            self.call(device, method, args, reply)
        } else {
            Err(Status::NOT_SUPPORTED)
        }
    }
    /// Starts job method `method` on `device` for `caller`, and returns the host's number for the
    /// job. `caller` owns it: only `caller` polls it or cancels it, and it ends when `caller`'s
    /// device goes.
    fn start_job(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
    ) -> Result<u64> {
        let _ = (caller, device, method, args);
        Err(Status::NOT_SUPPORTED)
    }
    /// How job `job` (from [`Router::start_job`]) stands, for its owner `caller`. A job that
    /// finished is answered once, with its result copied to `result`. `NOT_FOUND` for a job that
    /// is over and was answered, or never was; `NOT_PERMITTED` for one `caller` doesn't own.
    fn job_poll(&self, caller: CallerId, job: u64, result: &mut [u8]) -> Result<Polled> {
        let _ = (caller, job, result);
        Err(Status::NOT_SUPPORTED)
    }
    /// Whether a caller other than `caller` is using `device`: one that has called it and has not
    /// gone (see the host's `caller_gone`). Only the answer is given, never who the callers are.
    /// `NOT_SUPPORTED` in a router that counts no callers.
    fn others_use(&self, caller: CallerId, device: u32) -> Result<bool> {
        let _ = (caller, device);
        Err(Status::NOT_SUPPORTED)
    }
    /// Cancels job `job` for its owner `caller`, as [`Driver::job_cancel`] does.
    fn cancel_job(&self, caller: CallerId, job: u64) -> Result<()> {
        let _ = (caller, job);
        Err(Status::NOT_SUPPORTED)
    }
}

/// A direct call to another device in the same host.
pub struct LocalCaller {
    router: &'static dyn Router,
    device: u32,
    contract: u8,
    /// Who the calls are for. [`CallerId::LOCAL`] unless the host says otherwise (see [`by`](Self::by)).
    caller: CallerId,
    args: [u8; LOCAL_ARGS],
}

impl LocalCaller {
    pub const fn new(router: &'static dyn Router, device: u32, contract: u8) -> Self {
        Self {
            router,
            device,
            contract,
            caller: CallerId::LOCAL,
            args: [0; LOCAL_ARGS],
        }
    }

    /// The same route, for `caller`: the device sees calls from it as from that caller, and jobs
    /// started through it belong to it.
    pub const fn by(mut self, caller: CallerId) -> Self {
        self.caller = caller;
        self
    }

    /// How job `job` (one this route started) stands. See [`Router::job_poll`].
    pub fn poll(&self, job: JobId, result: &mut [u8]) -> Result<Polled> {
        self.router.job_poll(self.caller, job.0, result)
    }

    /// Cancels job `job` (one this route started). See [`Router::cancel_job`].
    pub fn cancel(&self, job: JobId) -> Result<()> {
        self.router.cancel_job(self.caller, job.0)
    }
}

impl core::fmt::Debug for LocalCaller {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LocalCaller")
            .field("device", &self.device)
            .field("contract", &self.contract)
            .field("caller", &self.caller)
            .finish()
    }
}

impl LocalCaller {
    /// Whether a caller other than this one is using the device. See [`Router::others_use`].
    pub fn others_use(&self) -> Result<bool> {
        self.router.others_use(self.caller, self.device)
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
        self.router
            .call_as(self.caller, self.device, id, args, reply)
    }

    fn start_job(&mut self, method: u16, args_len: usize) -> Result<JobId> {
        let args = self.args.get(..args_len).ok_or(Status::BUFFER_TOO_SMALL)?;
        let id = MethodId {
            contract: self.contract,
            method,
        };
        self.router
            .start_job(self.caller, self.device, id, args)
            .map(JobId)
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
