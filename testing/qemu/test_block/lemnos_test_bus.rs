// SPDX-License-Identifier: GPL-2.0
//! A bus of each kind with nothing on it but memory, so the module's transfers can be tested in a
//! guest (the port of lemnos_test_bus.c: the same devices, names, parameters and order):
//! - I2C adapter 40, plain transfers only: address 0x50 behaves as a 256-byte memory with a one-byte
//!   address, and 0x51 is held by a kernel driver.
//! - SPI controller 40, with its data output wired to its input. Chip select 0 is for the module; 1
//!   is left unbound (the harness does not run the kernel's spi-loopback-test: it tests the kernel's
//!   SPI core, not this code, and its timing check fails under host load). The device at 1
//!   ("spi-lemnos-test-twin") can be given another chip select through the parameter spi_twin_cs
//!   while it is unbound, so a check can offer the module two devices at one bus and chip select,
//!   which the SPI core itself never allows.
//! - A two-channel PWM chip that stores what it is told, under the platform device
//!   lemnos-test-parent.
//! - For the block lemnos-test-block: a supply "vdd" that can be made to report a fault and a clock
//!   "core", found by device name.
//! - A thermal zone "lemnos-test" whose temperature is the parameter temp_mc.
//!
//! The inline kernel helpers the C used are composed from their exported parts: i2c_add_driver from
//! i2c_register_driver, spi_alloc_host from __spi_alloc_controller, spi_controller_put from
//! put_device, pwmchip_add from __pwmchip_add, platform_device_register_simple from
//! platform_device_register_full, clk_hw_register_fixed_rate from __clk_hw_register_fixed_rate,
//! device_lock from mutex_lock, and the chip select accessors from the device's field.
//!
//! Copyright (c) 2026 the Lemnos authors.

#[path = "fixture.rs"]
mod fixture;

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering},
};
use fixture::{
    lb, name, param, param_typed, property_end, property_string, this_module, Fixed, Shared,
};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, to_result},
    ffi::{c_char, c_int, c_uint, c_ulong},
    prelude::*,
};

const TB_BUS: i16 = 40;
const TB_MEM_ADDR: u16 = 0x50;
const TB_HELD_ADDR: u16 = 0x51;
/// The SPI mode bits (include/uapi/linux/spi/spi.h, `_BITUL(n)`, which bindgen does not evaluate):
/// SPI_CPHA, SPI_CPOL, SPI_CS_HIGH, SPI_LSB_FIRST and SPI_LOOP.
const TB_SPI_MODES: u32 = 1 | 2 | 4 | 8 | 0x20;

static TB_PARENT: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());

/// The memory at 0x50 and where its next byte is read or written.
struct Mem {
    bytes: [u8; 256],
    pos: u8,
}

/// Written only in `tb_i2c_xfer`, which the I2C core calls with the adapter's bus lock held.
static TB_MEM: Shared<Mem> = Shared::new(Mem {
    bytes: [0; 256],
    pos: 0,
});

unsafe extern "C" fn tb_i2c_xfer(
    _adap: *mut bindings::i2c_adapter,
    msgs: *mut bindings::i2c_msg,
    num: c_int,
) -> c_int {
    // SAFETY: the I2C core holds the adapter's bus lock across this call, so nothing else has the memory.
    let mem = unsafe { &mut *TB_MEM.get() };
    for i in 0..num as usize {
        // SAFETY: the core passes `num` messages at `msgs`.
        let m = unsafe { &*msgs.add(i) };
        let mut n: u16 = 0;
        if m.addr != TB_MEM_ADDR && m.addr != TB_HELD_ADDR {
            return ENXIO.to_errno();
        }
        if u32::from(m.flags) & bindings::I2C_M_RD != 0 {
            while n < m.len {
                // SAFETY: a message's buffer holds `len` bytes, and `n` is below it.
                unsafe { m.buf.add(n as usize).write(mem.bytes[mem.pos as usize]) };
                mem.pos = mem.pos.wrapping_add(1);
                n += 1;
            }
            continue;
        }
        if m.len != 0 {
            // SAFETY: the buffer holds at least one byte: `len` is not zero.
            mem.pos = unsafe { m.buf.read() };
            n += 1;
        }
        while n < m.len {
            // SAFETY: a message's buffer holds `len` bytes, and `n` is below it.
            mem.bytes[mem.pos as usize] = unsafe { m.buf.add(n as usize).read() };
            mem.pos = mem.pos.wrapping_add(1);
            n += 1;
        }
    }
    num
}

unsafe extern "C" fn tb_i2c_func(_adap: *mut bindings::i2c_adapter) -> u32 {
    bindings::I2C_FUNC_I2C | bindings::I2C_FUNC_SMBUS_EMUL
}

static TB_I2C_ALGO: Fixed<bindings::i2c_algorithm> = Fixed(bindings::i2c_algorithm {
    __bindgen_anon_1: bindings::i2c_algorithm__bindgen_ty_1 {
        xfer: Some(tb_i2c_xfer),
    },
    functionality: Some(tb_i2c_func),
    // SAFETY: every other operation absent (null).
    ..unsafe { core::mem::zeroed() }
});

static TB_ADAPTER: Shared<bindings::i2c_adapter> = Shared::new(bindings::i2c_adapter {
    owner: this_module(),
    class: 0,
    algo: TB_I2C_ALGO.get(),
    name: name(b"lemnos-test-i2c"),
    nr: TB_BUS as c_int,
    // SAFETY: the rest of a struct i2c_adapter is what a C static leaves zero, for the core to fill in.
    ..unsafe { core::mem::zeroed() }
});

unsafe extern "C" fn tb_held_probe(_client: *mut bindings::i2c_client) -> c_int {
    0
}

static TB_HELD_ID: Fixed<[bindings::i2c_device_id; 2]> = Fixed([
    bindings::i2c_device_id {
        name: name(b"lemnos-test-held"),
        driver_data: 0,
    },
    // SAFETY: the empty entry that ends the table.
    unsafe { core::mem::zeroed() },
]);

static TB_HELD_DRIVER: Shared<bindings::i2c_driver> = Shared::new(bindings::i2c_driver {
    driver: bindings::device_driver {
        name: c"lemnos-test-held".as_ptr().cast(),
        // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
    probe: Some(tb_held_probe),
    id_table: TB_HELD_ID.get().cast(),
    // SAFETY: every other callback absent (null), and the core's own fields zero.
    ..unsafe { core::mem::zeroed() }
});

static TB_HELD_INFO: Fixed<bindings::i2c_board_info> = Fixed(bindings::i2c_board_info {
    type_: name(b"lemnos-test-held"),
    addr: TB_HELD_ADDR,
    // SAFETY: the rest of a struct i2c_board_info is optional pointers and integers, zero by default.
    ..unsafe { core::mem::zeroed() }
});

static TB_HELD: AtomicPtr<bindings::i2c_client> = AtomicPtr::new(ptr::null_mut());

/// SPI: whatever goes out comes back.
unsafe extern "C" fn tb_spi_transfer_one(
    _ctlr: *mut lb::spi_controller,
    spi: *mut lb::spi_device,
    t: *mut lb::spi_transfer,
) -> c_int {
    // SAFETY: the SPI core passes the transfer it is running, live for this call.
    let t = unsafe { &*t };
    // SAFETY: and the device the transfer is for.
    let max_speed_hz = unsafe { (*spi).max_speed_hz };
    let hz = if t.effective_speed_hz != 0 {
        t.effective_speed_hz
    } else {
        max_speed_hz
    };
    // The kernel's loopback test refuses a transfer faster than its clock allows.
    let usecs = 8u64 * u64::from(t.len) * u64::from(bindings::USEC_PER_SEC) / u64::from(hz) + 1;
    // SAFETY: transfer_one runs in the controller's message pump, which may sleep.
    unsafe { bindings::fsleep(usecs as c_ulong) };
    if !t.rx_buf.is_null() {
        if !t.tx_buf.is_null() {
            // SAFETY: both buffers hold `len` bytes; they may be one buffer, which `copy` allows.
            unsafe { ptr::copy(t.tx_buf.cast::<u8>(), t.rx_buf.cast::<u8>(), t.len as usize) };
        } else {
            // SAFETY: the receive buffer holds `len` bytes.
            unsafe { ptr::write_bytes(t.rx_buf.cast::<u8>(), 0, t.len as usize) };
        }
    }
    0
}

unsafe extern "C" fn tb_pwm_apply(
    _chip: *mut bindings::pwm_chip,
    _pwm: *mut bindings::pwm_device,
    _state: *const bindings::pwm_state,
) -> c_int {
    0
}

static TB_PWM_OPS: Fixed<bindings::pwm_ops> = Fixed(bindings::pwm_ops {
    apply: Some(tb_pwm_apply),
    // SAFETY: every other operation absent (null).
    ..unsafe { core::mem::zeroed() }
});

static TB_PWM: AtomicPtr<bindings::pwm_chip> = AtomicPtr::new(ptr::null_mut());

static TB_SPI_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,test-spi"),
    property_end(),
]);

/// The SPI core names a device with a software node after the node: "spi-lemnos-test".
static TB_SPI_NODE: Fixed<bindings::software_node> = Fixed(bindings::software_node {
    name: c"lemnos-test".as_ptr().cast(),
    parent: ptr::null(),
    properties: TB_SPI_PROPS.get().cast(),
});

/// The device at chip select 1: the same compatible, so the module may be asked to bind it.
static TB_SPI_TWIN_NODE: Fixed<bindings::software_node> = Fixed(bindings::software_node {
    name: c"lemnos-test-twin".as_ptr().cast(),
    parent: ptr::null(),
    properties: TB_SPI_PROPS.get().cast(),
});

/// The parameter temp_mc: the kernel's int operations write it, the zone's get_temp reads it.
static TEMP_MC: AtomicI32 = AtomicI32::new(40000);
param_typed!(
    temp_mc,
    0o644,
    param_ops_int,
    TEMP_MC.as_ptr().cast(),
    "int"
);

unsafe extern "C" fn tb_get_temp(_tz: *mut lb::thermal_zone_device, temp: *mut c_int) -> c_int {
    // SAFETY: the thermal core passes where the temperature goes.
    unsafe { temp.write(TEMP_MC.load(Ordering::Relaxed)) };
    0
}

static TB_TZ_OPS: Fixed<lb::thermal_zone_device_ops> = Fixed(lb::thermal_zone_device_ops {
    get_temp: Some(tb_get_temp),
    // SAFETY: every other operation absent (null).
    ..unsafe { core::mem::zeroed() }
});

static TB_TZ: AtomicPtr<lb::thermal_zone_device> = AtomicPtr::new(ptr::null_mut());

// A supply from 1.0 V to 3.3 V in 100 mV steps.
static TB_SEL: AtomicU32 = AtomicU32::new(0);
static TB_ON: AtomicBool = AtomicBool::new(false);
static TB_RDEV: AtomicPtr<bindings::regulator_dev> = AtomicPtr::new(ptr::null_mut());

unsafe extern "C" fn tb_reg_enable(_rdev: *mut bindings::regulator_dev) -> c_int {
    TB_ON.store(true, Ordering::Relaxed);
    0
}

unsafe extern "C" fn tb_reg_disable(_rdev: *mut bindings::regulator_dev) -> c_int {
    TB_ON.store(false, Ordering::Relaxed);
    0
}

unsafe extern "C" fn tb_reg_is_enabled(_rdev: *mut bindings::regulator_dev) -> c_int {
    c_int::from(TB_ON.load(Ordering::Relaxed))
}

unsafe extern "C" fn tb_reg_get_sel(_rdev: *mut bindings::regulator_dev) -> c_int {
    TB_SEL.load(Ordering::Relaxed) as c_int
}

unsafe extern "C" fn tb_reg_set_sel(_rdev: *mut bindings::regulator_dev, sel: c_uint) -> c_int {
    TB_SEL.store(sel, Ordering::Relaxed);
    0
}

static TB_REG_OPS: Fixed<lb::regulator_ops> = Fixed(lb::regulator_ops {
    enable: Some(tb_reg_enable),
    disable: Some(tb_reg_disable),
    is_enabled: Some(tb_reg_is_enabled),
    list_voltage: Some(lb::regulator_list_voltage_linear),
    map_voltage: Some(lb::regulator_map_voltage_linear),
    get_voltage_sel: Some(tb_reg_get_sel),
    set_voltage_sel: Some(tb_reg_set_sel),
    // SAFETY: every other operation absent (null).
    ..unsafe { core::mem::zeroed() }
});

static TB_REG_DESC: Fixed<lb::regulator_desc> = Fixed(lb::regulator_desc {
    name: c"lemnos-test-vdd".as_ptr().cast(),
    ops: TB_REG_OPS.get(),
    type_: lb::regulator_type_REGULATOR_VOLTAGE,
    owner: this_module(),
    n_voltages: 24,
    min_uV: 1000000,
    uV_step: 100000,
    // SAFETY: the rest of a struct regulator_desc is optional: pointers, integers and flags, zero by default.
    ..unsafe { core::mem::zeroed() }
});

static TB_SUPPLY: Shared<lb::regulator_consumer_supply> =
    Shared::new(lb::regulator_consumer_supply {
        dev_name: c"lemnos-test-block".as_ptr().cast(),
        supply: c"vdd".as_ptr().cast(),
    });

static TB_REG_INIT: Shared<lb::regulator_init_data> = Shared::new(lb::regulator_init_data {
    constraints: lb::regulation_constraints {
        min_uV: 1000000,
        max_uV: 3300000,
        valid_ops_mask: lb::REGULATOR_CHANGE_VOLTAGE | lb::REGULATOR_CHANGE_STATUS,
        // SAFETY: the rest of the constraints are optional: integers, flags and a name pointer, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
    num_consumer_supplies: 1,
    consumer_supplies: TB_SUPPLY.get(),
    // SAFETY: the rest of a struct regulator_init_data is optional pointers, zero by default.
    ..unsafe { core::mem::zeroed() }
});

/// Writing anything here makes the supply report an over-current.
unsafe extern "C" fn tb_fault_set(
    _val: *const c_char,
    _kp: *const bindings::kernel_param,
) -> c_int {
    let rdev = TB_RDEV.load(Ordering::Relaxed);
    if rdev.is_null() {
        return ENODEV.to_errno();
    }
    // SAFETY: the supply is registered until the module's exit, which no parameter write outlives.
    unsafe {
        lb::regulator_notifier_call_chain(
            rdev,
            lb::REGULATOR_EVENT_OVER_CURRENT as c_ulong,
            ptr::null_mut(),
        )
    };
    0
}

static TB_FAULT_OPS: Fixed<bindings::kernel_param_ops> = Fixed(bindings::kernel_param_ops {
    flags: 0,
    set: Some(tb_fault_set),
    get: None,
    free: None,
});
param!(fault, 0o200, TB_FAULT_OPS.get(), ptr::null_mut());

static TB_CLK: AtomicPtr<lb::clk_hw> = AtomicPtr::new(ptr::null_mut());
static TB_CLK_LOOKUP: AtomicPtr<lb::clk_lookup> = AtomicPtr::new(ptr::null_mut());

/// The parent's device, for what is registered under it.
fn tb_parent_dev() -> *mut bindings::device {
    // SAFETY: called only after the parent is registered; the address of its device is taken, not read.
    unsafe { &raw mut (*TB_PARENT.load(Ordering::Relaxed)).dev }
}

fn tb_power_setup() -> Result {
    let cfg = lb::regulator_config {
        dev: tb_parent_dev(),
        init_data: TB_REG_INIT.get(),
        // SAFETY: the rest of a struct regulator_config is optional pointers, zero by default.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the parent is registered, and the description and the configuration live for the call (the
    // description for ever).
    let rdev = from_err_ptr(unsafe {
        lb::regulator_register(tb_parent_dev(), TB_REG_DESC.get(), &raw const cfg)
    })?;
    TB_RDEV.store(rdev, Ordering::Relaxed);
    // SAFETY: clk_hw_register_fixed_rate, as its macro calls this: no node, no parent, 25 MHz, not devm.
    let clk = from_err_ptr(unsafe {
        lb::__clk_hw_register_fixed_rate(
            tb_parent_dev(),
            ptr::null_mut(),
            c"lemnos-test-clk".as_ptr().cast(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            25000000,
            0,
            0,
            false,
        )
    })?;
    TB_CLK.store(clk, Ordering::Relaxed);
    // SAFETY: the clock is registered; the names are NUL-terminated, and the device name holds no format.
    let lookup = unsafe {
        lb::clkdev_hw_create(
            clk,
            c"core".as_ptr().cast(),
            c"lemnos-test-block".as_ptr().cast(),
        )
    };
    if lookup.is_null() {
        return Err(ENOMEM);
    }
    TB_CLK_LOOKUP.store(lookup, Ordering::Relaxed);
    // SAFETY: the operations are a static, and the zone has no data and no parameters.
    let tz = from_err_ptr(unsafe {
        lb::thermal_tripless_zone_device_register(
            c"lemnos-test".as_ptr().cast(),
            ptr::null_mut(),
            TB_TZ_OPS.get(),
            ptr::null(),
        )
    })?;
    TB_TZ.store(tz, Ordering::Relaxed);
    // SAFETY: the zone was registered just above.
    to_result(unsafe { lb::thermal_zone_device_enable(tz) })
}

fn tb_power_teardown() {
    let tz = TB_TZ.load(Ordering::Relaxed);
    if !tz.is_null() {
        // SAFETY: the zone was registered by tb_power_setup, and this is its one unregistration.
        unsafe { lb::thermal_zone_device_unregister(tz) };
    }
    let lookup = TB_CLK_LOOKUP.load(Ordering::Relaxed);
    if !lookup.is_null() {
        // SAFETY: the lookup was made by tb_power_setup, and this is its one drop.
        unsafe { lb::clkdev_drop(lookup) };
    }
    let clk = TB_CLK.load(Ordering::Relaxed);
    if !clk.is_null() {
        // SAFETY: the clock was registered by tb_power_setup, and this is its one unregistration.
        unsafe { lb::clk_hw_unregister_fixed_rate(clk) };
    }
    let rdev = TB_RDEV.load(Ordering::Relaxed);
    if !rdev.is_null() {
        // SAFETY: the supply was registered by tb_power_setup, and this is its one unregistration.
        unsafe { lb::regulator_unregister(rdev) };
    }
}

static TB_CTLR: AtomicPtr<lb::spi_controller> = AtomicPtr::new(ptr::null_mut());
static TB_SPI: [AtomicPtr<lb::spi_device>; 2] = [
    AtomicPtr::new(ptr::null_mut()),
    AtomicPtr::new(ptr::null_mut()),
];

/// spi_twin_cs: the chip select the second device says it is at. Only the device's own field changes
/// (what a driver's probe reads); the controller is not told, and nothing transfers on the device.
/// Refused while a driver is bound to it.
unsafe extern "C" fn tb_twin_cs_set(
    val: *const c_char,
    _kp: *const bindings::kernel_param,
) -> c_int {
    let mut cs: u8 = 0;
    // SAFETY: the kernel passes the parameter's text, NUL-terminated.
    let mut ret = unsafe { bindings::kstrtou8(val, 0, &raw mut cs) };
    if ret != 0 {
        return ret;
    }
    let twin = TB_SPI[1].load(Ordering::Relaxed);
    if twin.is_null() {
        return ENODEV.to_errno();
    }
    // SAFETY: the controller is registered for as long as the twin exists.
    if u16::from(cs) >= unsafe { (*TB_CTLR.load(Ordering::Relaxed)).num_chipselect } {
        return EINVAL.to_errno();
    }
    // SAFETY: the twin is registered until the module's exit; the address of its device's lock is taken.
    let lock = unsafe { &raw mut (*twin).dev.mutex };
    // SAFETY: device_lock: the device's own mutex.
    unsafe { bindings::mutex_lock(lock) };
    // SAFETY: the device's driver is read under its lock.
    if !unsafe { (*twin).dev.driver }.is_null() {
        ret = EBUSY.to_errno();
    } else {
        // SAFETY: spi_set_chipselect(twin, 0, cs): the field a driver's probe reads, under the device's lock.
        unsafe { (*twin).chip_select[0] = cs };
    }
    // SAFETY: device_unlock: taken just above.
    unsafe { bindings::mutex_unlock(lock) };
    ret
}

unsafe extern "C" fn tb_twin_cs_get(buf: *mut c_char, _kp: *const bindings::kernel_param) -> c_int {
    let twin = TB_SPI[1].load(Ordering::Relaxed);
    // SAFETY: spi_get_chipselect(twin, 0): the twin is registered until the module's exit.
    let cs: c_uint = if twin.is_null() {
        1
    } else {
        c_uint::from(unsafe { (*twin).chip_select[0] })
    };
    // SAFETY: the kernel passes a page for the parameter's text.
    unsafe { bindings::sysfs_emit(buf, c"%u\n".as_ptr().cast(), cs) }
}

static TB_TWIN_CS_OPS: Fixed<bindings::kernel_param_ops> = Fixed(bindings::kernel_param_ops {
    flags: 0,
    set: Some(tb_twin_cs_set),
    get: Some(tb_twin_cs_get),
    free: None,
});
param!(spi_twin_cs, 0o644, TB_TWIN_CS_OPS.get(), ptr::null_mut());

fn tb_spi_teardown() {
    for spi in &TB_SPI {
        let spi = spi.load(Ordering::Relaxed);
        if !spi.is_null() {
            // SAFETY: the device was made by tb_spi_setup, and this is its one unregistration.
            unsafe { lb::spi_unregister_device(spi) };
        }
    }
    tb_power_teardown();
    let pwm = TB_PWM.load(Ordering::Relaxed);
    if !pwm.is_null() {
        // SAFETY: the chip was added by tb_spi_setup.
        unsafe { bindings::pwmchip_remove(pwm) };
        // SAFETY: and this gives back the reference pwmchip_alloc took.
        unsafe { bindings::pwmchip_put(pwm) };
    }
    let ctlr = TB_CTLR.load(Ordering::Relaxed);
    if !ctlr.is_null() {
        // SAFETY: the controller was registered by tb_spi_setup, and this is its one unregistration.
        unsafe { lb::spi_unregister_controller(ctlr) };
    }
    let parent = TB_PARENT.load(Ordering::Relaxed);
    if !parent.is_null() {
        // SAFETY: the parent was registered by tb_spi_setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(parent) };
    }
    // SAFETY: unregistering a node that was never registered does nothing, as in the C.
    unsafe { bindings::software_node_unregister(TB_SPI_TWIN_NODE.get()) };
    // SAFETY: as above.
    unsafe { bindings::software_node_unregister(TB_SPI_NODE.get()) };
}

fn tb_spi_setup() -> Result {
    let mut info = lb::spi_board_info {
        modalias: name(b"lemnos-test-spi"),
        max_speed_hz: 10000000,
        bus_num: TB_BUS as u16,
        chip_select: 0,
        swnode: TB_SPI_NODE.get(),
        // SAFETY: the rest of a struct spi_board_info is optional pointers and integers, zero by default.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the node is a static.
    to_result(unsafe { bindings::software_node_register(TB_SPI_NODE.get()) })?;
    // SAFETY: as above.
    to_result(unsafe { bindings::software_node_register(TB_SPI_TWIN_NODE.get()) })?;
    let parent_info = bindings::platform_device_info {
        name: c"lemnos-test-parent".as_ptr().cast(),
        id: -1,
        // SAFETY: no parent, no resources, no data and no properties: zero.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: platform_device_register_simple("lemnos-test-parent", -1, NULL, 0), as its inline calls this.
    let parent =
        from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const parent_info) })?;
    TB_PARENT.store(parent, Ordering::Relaxed);
    // SAFETY: spi_alloc_host(&parent->dev, 0): a host controller with no private data.
    let ctlr = unsafe { lb::__spi_alloc_controller(tb_parent_dev(), 0, false) };
    if ctlr.is_null() {
        return Err(ENOMEM);
    }
    TB_CTLR.store(ctlr, Ordering::Relaxed);
    {
        // SAFETY: the controller is allocated and not yet registered: nothing else has it.
        let c = unsafe { &mut *ctlr };
        c.bus_num = TB_BUS;
        c.num_chipselect = 2;
        c.mode_bits = TB_SPI_MODES;
        // SPI_BPW_RANGE_MASK(1, 32): every word size.
        c.bits_per_word_mask = u32::MAX;
        c.max_speed_hz = 10000000;
        c.transfer_one = Some(tb_spi_transfer_one);
    }
    // SAFETY: the controller is filled in above.
    if let Err(e) = to_result(unsafe { lb::spi_register_controller(ctlr) }) {
        // SAFETY: spi_controller_put: gives back the allocation's reference, which frees it.
        unsafe { bindings::put_device(&raw mut (*ctlr).dev) };
        TB_CTLR.store(ptr::null_mut(), Ordering::Relaxed);
        return Err(e);
    }
    // SAFETY: the parent is registered; two channels, no private data.
    let pwm = from_err_ptr(unsafe { bindings::pwmchip_alloc(tb_parent_dev(), 2, 0) })?;
    // SAFETY: the chip is allocated and not yet added: nothing else has it.
    unsafe { (*pwm).ops = TB_PWM_OPS.get() };
    // SAFETY: pwmchip_add(pwm), as its macro calls this with THIS_MODULE.
    if let Err(e) = to_result(unsafe { bindings::__pwmchip_add(pwm, this_module()) }) {
        // SAFETY: gives back the reference pwmchip_alloc took.
        unsafe { bindings::pwmchip_put(pwm) };
        return Err(e);
    }
    TB_PWM.store(pwm, Ordering::Relaxed);
    tb_power_setup()?;
    // SAFETY: the controller is registered, and the board info lives for the call.
    let first = unsafe { lb::spi_new_device(ctlr, &raw mut info) };
    TB_SPI[0].store(first, Ordering::Relaxed);
    if first.is_null() {
        return Err(ENODEV);
    }
    info.modalias = name(b"spi-loopback-test");
    info.chip_select = 1;
    info.swnode = TB_SPI_TWIN_NODE.get();
    // SAFETY: as above.
    let twin = unsafe { lb::spi_new_device(ctlr, &raw mut info) };
    TB_SPI[1].store(twin, Ordering::Relaxed);
    if twin.is_null() {
        return Err(ENODEV);
    }
    Ok(())
}

struct TestBus;

impl kernel::Module for TestBus {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // SAFETY: the adapter is a static, registered once here and deleted once at exit.
        to_result(unsafe { bindings::i2c_add_numbered_adapter(TB_ADAPTER.get()) })?;
        // SAFETY: i2c_add_driver, as its macro calls this with THIS_MODULE; the driver is a static.
        if let Err(e) =
            to_result(unsafe { bindings::i2c_register_driver(this_module(), TB_HELD_DRIVER.get()) })
        {
            // SAFETY: undoes the adapter, added above.
            unsafe { bindings::i2c_del_adapter(TB_ADAPTER.get()) };
            return Err(e);
        }
        // SAFETY: the adapter is registered, and the board info is a static.
        match from_err_ptr(unsafe {
            bindings::i2c_new_client_device(TB_ADAPTER.get(), TB_HELD_INFO.get())
        }) {
            Ok(held) => TB_HELD.store(held, Ordering::Relaxed),
            Err(e) => {
                // SAFETY: undoes the driver, added above.
                unsafe { bindings::i2c_del_driver(TB_HELD_DRIVER.get()) };
                // SAFETY: undoes the adapter, added above.
                unsafe { bindings::i2c_del_adapter(TB_ADAPTER.get()) };
                return Err(e);
            }
        }
        if let Err(e) = tb_spi_setup() {
            tb_spi_teardown();
            // SAFETY: undoes the client, made above.
            unsafe { bindings::i2c_unregister_device(TB_HELD.load(Ordering::Relaxed)) };
            // SAFETY: undoes the driver, added above.
            unsafe { bindings::i2c_del_driver(TB_HELD_DRIVER.get()) };
            // SAFETY: undoes the adapter, added above.
            unsafe { bindings::i2c_del_adapter(TB_ADAPTER.get()) };
            return Err(e);
        }
        Ok(TestBus)
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        tb_spi_teardown();
        // SAFETY: called once, at unload, after a successful init; undoes the client.
        unsafe { bindings::i2c_unregister_device(TB_HELD.load(Ordering::Relaxed)) };
        // SAFETY: as above; undoes the driver.
        unsafe { bindings::i2c_del_driver(TB_HELD_DRIVER.get()) };
        // SAFETY: as above; undoes the adapter.
        unsafe { bindings::i2c_del_adapter(TB_ADAPTER.get()) };
    }
}

module! {
    type: TestBus,
    name: "lemnos_test_bus",
    authors: ["Lemnos"],
    description: "I2C and SPI buses with nothing on them, for testing the Lemnos module",
    license: "GPL",
    imports_ns: ["PWM"],
}
