//! The bulk endpoint and SD block transfers, as register sequences over [`RegAccess`]. The -sys
//! crate implements the register access with volatile MMIO; the models in [`crate::models`] answer
//! the same registers for the host tests, with the failures the hardware shows.
//!
//! Sources (linux-rpi-7.2 at 53679a578):
//! - Endpoint configuration: `drivers/usb/dwc3/gadget.c` 596-614 (params 0 and 1), 663 (SETEPCONFIG),
//!   946-948 (DALEPENA bit n); `gadget.h` 23-46 (DEPCFG fields); `core.h` 164, 167-171, 584-600.
//! - Transfer start: `gadget.c` 985-990 (param0 = upper 32 bits of the TRB address, param1 = lower).
//! - TRB: `core.h` 890-895 (bpl, bph, size, ctrl), ctrl bits 863-875.
//! - dw_mmc: `dw_mmc.c` 246, 316 (command = opcode in the low bits, flags above), 285-296 (response and
//!   data flags); `dw_mmc.h` 370-408 (interrupt, command and status bits), 278-320 (registers).

use crate::bind::RegAccess;
use crate::regs::{dw_mmc, dwc3};
use crate::wdt::LONGEST_PERIOD_US;
use lemnos_flash_stage::wait::{Clock, WaitError, wait_for};

/// Why a bulk or block transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoFault {
    /// A bounded wait ran out.
    Timeout,
    /// The card or the controller reported a command timeout (RTO).
    CommandTimeout,
    /// The command response had a CRC error (RCRC).
    CommandCrc,
    /// The data phase timed out (DRTO).
    DataTimeout,
    /// The data phase had a CRC error (DCRC).
    DataCrc,
    /// The card held DATA_BUSY past the wait: it answers busy and does not finish.
    CardBusy,
}

impl From<WaitError> for IoFault {
    fn from(_: WaitError) -> Self {
        IoFault::Timeout
    }
}

/// A transfer request block (`struct dwc3_trb`, core.h:890-895).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trb {
    pub bpl: u32,
    pub bph: u32,
    pub size: u32,
    pub ctrl: u32,
}

/// TRB ctrl bits (core.h:863-875): hardware owns the TRB while HWO is set.
pub const TRB_HWO: u32 = 1 << 0;
pub const TRB_LST: u32 = 1 << 1;
pub const TRB_IOC: u32 = 1 << 11;
/// TRBCTL field (bits 9:4) for a normal TRB: `DWC3_TRBCTL_NORMAL` = TRB_CTRL_TRBCTL(1).
pub const TRB_TRBCTL_NORMAL: u32 = 1 << 4;

impl Trb {
    /// One normal TRB for a buffer at `addr` of `len` bytes, owned by the controller, last in the
    /// list, interrupt on completion.
    pub fn normal(addr: u64, len: u32) -> Self {
        Self {
            bpl: addr as u32,
            bph: (addr >> 32) as u32,
            size: len,
            ctrl: TRB_TRBCTL_NORMAL | TRB_LST | TRB_IOC | TRB_HWO,
        }
    }

    /// One control TRB of TRBCTL `kind` (core.h:874-881: setup 2, status 3 or 4, data 5) for a buffer at `addr`.
    pub fn control(addr: u64, len: u32, kind: u32) -> Self {
        Self {
            bpl: addr as u32,
            bph: (addr >> 32) as u32,
            size: len,
            ctrl: (kind << 4) | TRB_LST | TRB_IOC | TRB_HWO,
        }
    }

    /// Whether the controller still owns the TRB (HWO set). It clears HWO when the transfer is done.
    pub fn owned_by_controller(&self) -> bool {
        self.ctrl & TRB_HWO != 0
    }
}

/// Waits until the endpoint's command in flight is done (DEPCMD CMDACT clear), or until `deadline_us`. A command issued
/// inside a transfer phase is bounded by that phase's deadline, so the arm of a transfer counts against the phase.
fn wait_depcmd_until<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    deadline_us: u64,
) -> Result<(), IoFault> {
    let budget = until(clock, deadline_us)?;
    let depcmd = base + dwc3::depcmd(ep);
    wait_for(clock, budget, LONGEST_PERIOD_US, || {
        (regs.read32(depcmd) & dwc3::DEPCMD_CMDACT == 0).then_some(())
    })?;
    Ok(())
}

/// The endpoint type field of DEPCFG (gadget.c:596-597): control is 0, bulk is 2.
pub const EP_TYPE_CONTROL: u32 = 0;
pub const EP_TYPE_BULK: u32 = 2;

/// Starts the control endpoints (DEPSTARTCFG, resource index 0): the first step of the device, before any
/// endpoint is configured (gadget.c:2965-2971, gadget.c:555-570).
///
/// Bring-up: `deadline_us` is the init deadline (see `timing::INIT_BUDGET_US`).
pub fn start_control_endpoints<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    deadline_us: u64,
) -> Result<(), IoFault> {
    regs.write32(base + dwc3::depcmdpar0(0), 0);
    regs.write32(
        base + dwc3::depcmd(0),
        dwc3::DEPCMD_DEPSTARTCFG | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, 0, deadline_us)
}

/// Configures endpoint `ep` of type `ep_type` (control or bulk) with `max_packet` bytes on FIFO `fifo`: the
/// configuration (SETEPCONFIG), then one transfer resource (SETTRANSFRESOURCE), then the endpoint is enabled in
/// DALEPENA. The order is the one mainline keeps (gadget.c:925-948, 520-535); a transfer started
/// before the resource is allocated is not accepted. Each of its two commands is bounded by `deadline_us`: the caller's
/// phase deadline in a session (R4-B1), the init deadline during bring-up.
// The register sequence needs each of these: the endpoint, its type, packet size, FIFO and the deadline.
#[allow(clippy::too_many_arguments)]
pub fn configure_endpoint<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    ep_type: u32,
    max_packet: u16,
    fifo: u8,
    deadline_us: u64,
) -> Result<(), IoFault> {
    // DWC3_DEPCFG_EP_TYPE (gadget.c:596-597, 639; gadget.h:36-38).
    let par0 = (ep_type << 1) | (u32::from(max_packet) << 3) | (u32::from(fifo) << 17);
    // XFER_COMPLETE_EN | XFER_NOT_READY_EN (gadget.c:611-614; gadget.h:24, 26).
    let par1 = (1u32 << 8) | (1u32 << 10);
    regs.write32(base + dwc3::depcmdpar0(ep), par0);
    regs.write32(base + dwc3::depcmdpar1(ep), par1);
    regs.write32(
        base + dwc3::depcmd(ep),
        dwc3::DEPCMD_SETEPCONFIG | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, ep, deadline_us)?;
    // NUM_XFER_RES(1): one transfer resource (gadget.c:520-535).
    regs.write32(base + dwc3::depcmdpar0(ep), 1);
    regs.write32(
        base + dwc3::depcmd(ep),
        dwc3::DEPCMD_SETTRANSFRESOURCE | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, ep, deadline_us)?;
    let enabled = regs.read32(base + dwc3::DALEPENA) | (1 << ep);
    regs.write32(base + dwc3::DALEPENA, enabled);
    Ok(())
}

/// Configures endpoint `ep` as a bulk endpoint (see [`configure_endpoint`]).
pub fn configure_bulk_endpoint<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    max_packet: u16,
    fifo: u8,
    deadline_us: u64,
) -> Result<(), IoFault> {
    configure_endpoint(
        regs,
        clock,
        base,
        ep,
        EP_TYPE_BULK,
        max_packet,
        fifo,
        deadline_us,
    )
}

/// Ends the transfer in flight on endpoint `ep` (DEPCMD ENDTRANSFER), as a bus reset requires: the controller drops
/// the TRB it was given. The resource id is 0, the one DEPSTARTCFG allocated; a bench check.
pub fn end_transfer<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    deadline_us: u64,
) -> Result<(), IoFault> {
    regs.write32(base + dwc3::depcmdpar0(ep), 0);
    regs.write32(
        base + dwc3::depcmd(ep),
        dwc3::DEPCMD_ENDTRANSFER | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, ep, deadline_us)
}

/// Stalls endpoint `ep` (DEPCMD SETSTALL; ep0.c:224 restarts the control endpoint after it). The command is bounded by
/// `deadline_us`, the caller's phase deadline.
pub fn set_stall<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    deadline_us: u64,
) -> Result<(), IoFault> {
    regs.write32(
        base + dwc3::depcmd(ep),
        dwc3::DEPCMD_SETSTALL | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, ep, deadline_us)
}

/// Starts a transfer on endpoint `ep` with the TRB at `trb_addr` (gadget.c:985-990: param0 is the
/// upper 32 bits, param1 the lower). Waits for the command to be accepted.
pub fn start_transfer<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    ep: usize,
    trb_addr: u64,
    deadline_us: u64,
) -> Result<(), IoFault> {
    regs.write32(base + dwc3::depcmdpar0(ep), (trb_addr >> 32) as u32);
    regs.write32(base + dwc3::depcmdpar1(ep), trb_addr as u32);
    regs.write32(
        base + dwc3::depcmd(ep),
        dwc3::DEPCMD_STARTTRANSFER | dwc3::DEPCMD_CMDACT,
    );
    wait_depcmd_until(regs, clock, base, ep, deadline_us)
}

/// The SD card block size.
pub const BLOCK: usize = 512;
const WORDS: usize = BLOCK / 4;
/// CMD17 (READ_SINGLE_BLOCK) and CMD24 (WRITE_BLOCK): the opcodes in the command register's low bits
/// (dw_mmc.c:246, 316).
pub const CMD_READ_SINGLE: u32 = 17;
pub const CMD_WRITE_BLOCK: u32 = 24;

/// The time left until `deadline_us`: a bounded wait takes no more than this, and none is left once the deadline has
/// passed (a timeout, so the call ends).
pub fn until<C: Clock>(clock: &mut C, deadline_us: u64) -> Result<u64, IoFault> {
    let now = clock.now_us();
    if now >= deadline_us {
        Err(IoFault::Timeout)
    } else {
        Ok(deadline_us - now)
    }
}

fn wait_not_busy<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    deadline_us: u64,
) -> Result<(), IoFault> {
    let budget = until(clock, deadline_us)?;
    wait_for(clock, budget, LONGEST_PERIOD_US, || {
        (regs.read32(base + dw_mmc::STATUS) & dw_mmc::STATUS_DATA_BUSY == 0).then_some(())
    })
    .map_err(|_| IoFault::CardBusy)
}

/// Waits for `done` (a status interrupt bit) or an error bit, and maps the error bits to faults.
fn wait_int<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    done: u32,
    errors: &[(u32, IoFault)],
    deadline_us: u64,
) -> Result<(), IoFault> {
    let rintsts = base + dw_mmc::RINTSTS;
    let budget = until(clock, deadline_us)?;
    let status = wait_for(clock, budget, LONGEST_PERIOD_US, || {
        let raw = regs.read32(rintsts);
        if let Some(&(_, fault)) = errors.iter().find(|(bit, _)| raw & bit != 0) {
            return Some(Err(fault));
        }
        (raw & done != 0).then_some(Ok(()))
    })?;
    regs.write32(rintsts, 0xFFFF_FFFF);
    status
}

/// Reads one block at `lba` through the SD host's FIFO (PIO).
pub fn sd_read_block<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    lba: u32,
    deadline_us: u64,
) -> Result<[u8; BLOCK], IoFault> {
    wait_not_busy(regs, clock, base, deadline_us)?;
    regs.write32(base + dw_mmc::BYTCNT, BLOCK as u32);
    regs.write32(base + dw_mmc::BLKSIZ, BLOCK as u32);
    regs.write32(base + dw_mmc::CMDARG, lba);
    regs.write32(
        base + dw_mmc::CMD,
        CMD_READ_SINGLE
            | dw_mmc::CMD_START
            | dw_mmc::CMD_RESP_EXP
            | dw_mmc::CMD_RESP_CRC
            | dw_mmc::CMD_DAT_EXP,
    );
    wait_int(
        regs,
        clock,
        base,
        dw_mmc::INT_CMD_DONE,
        &[
            (dw_mmc::INT_RTO, IoFault::CommandTimeout),
            (dw_mmc::INT_RCRC, IoFault::CommandCrc),
        ],
        deadline_us,
    )?;
    wait_int(
        regs,
        clock,
        base,
        dw_mmc::INT_DATA_OVER,
        &[
            (dw_mmc::INT_DRTO, IoFault::DataTimeout),
            (dw_mmc::INT_DCRC, IoFault::DataCrc),
        ],
        deadline_us,
    )?;
    let mut out = [0u8; BLOCK];
    for chunk in out.as_chunks_mut::<4>().0.iter_mut().take(WORDS) {
        *chunk = regs.read32(base + dw_mmc::DATA).to_le_bytes();
    }
    wait_not_busy(regs, clock, base, deadline_us)?;
    Ok(out)
}

/// Writes one block at `lba` through the FIFO (PIO), then waits for the card to stop being busy.
pub fn sd_write_block<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    lba: u32,
    data: &[u8; BLOCK],
    deadline_us: u64,
) -> Result<(), IoFault> {
    wait_not_busy(regs, clock, base, deadline_us)?;
    regs.write32(base + dw_mmc::BYTCNT, BLOCK as u32);
    regs.write32(base + dw_mmc::BLKSIZ, BLOCK as u32);
    regs.write32(base + dw_mmc::CMDARG, lba);
    regs.write32(
        base + dw_mmc::CMD,
        CMD_WRITE_BLOCK
            | dw_mmc::CMD_START
            | dw_mmc::CMD_RESP_EXP
            | dw_mmc::CMD_RESP_CRC
            | dw_mmc::CMD_DAT_EXP
            | dw_mmc::CMD_DAT_WR,
    );
    wait_int(
        regs,
        clock,
        base,
        dw_mmc::INT_CMD_DONE,
        &[
            (dw_mmc::INT_RTO, IoFault::CommandTimeout),
            (dw_mmc::INT_RCRC, IoFault::CommandCrc),
        ],
        deadline_us,
    )?;
    for chunk in data.as_chunks::<4>().0 {
        regs.write32(base + dw_mmc::DATA, u32::from_le_bytes(*chunk));
    }
    wait_int(
        regs,
        clock,
        base,
        dw_mmc::INT_DATA_OVER,
        &[
            (dw_mmc::INT_DRTO, IoFault::DataTimeout),
            (dw_mmc::INT_DCRC, IoFault::DataCrc),
        ],
        deadline_us,
    )?;
    wait_not_busy(regs, clock, base, deadline_us)
}

/// One SD command: writes the argument and the command (opcode, the response and CRC flags, and any
/// `extra` bits such as the clock update), waits for it to finish, and returns the four response
/// registers. A clock update (opcode 0, no response) is finished when the start bit clears.
// The command's opcode, argument, flags, extra bits and the deadline: each is a register field of one command.
#[allow(clippy::too_many_arguments)]
pub fn sd_command_response<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
    opcode: u32,
    arg: u32,
    flags: u32,
    extra: u32,
    deadline_us: u64,
) -> Result<[u32; 4], IoFault> {
    regs.write32(base + dw_mmc::CMDARG, arg);
    regs.write32(
        base + dw_mmc::CMD,
        opcode | dw_mmc::CMD_START | flags | extra,
    );
    if extra & dw_mmc::CMD_UPD_CLK != 0 && opcode == 0 && flags == 0 {
        let cmd = base + dw_mmc::CMD;
        let budget = until(clock, deadline_us)?;
        wait_for(clock, budget, LONGEST_PERIOD_US, || {
            (regs.read32(cmd) & dw_mmc::CMD_START == 0).then_some(())
        })?;
        return Ok([0; 4]);
    }
    wait_int(
        regs,
        clock,
        base,
        dw_mmc::INT_CMD_DONE,
        &[
            (dw_mmc::INT_RTO, IoFault::CommandTimeout),
            (dw_mmc::INT_RCRC, IoFault::CommandCrc),
        ],
        deadline_us,
    )?;
    let resp = base + dw_mmc::RESP0;
    Ok([
        regs.read32(resp),
        regs.read32(resp + 4),
        regs.read32(resp + 8),
        regs.read32(resp + 12),
    ])
}
