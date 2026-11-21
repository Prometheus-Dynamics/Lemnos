//! The pad rule, as a test: in every mode the stage has, no write reaches the iomux or pmu iomux ranges.
//! The report's register traffic (the UART banner, the pad dump, the UART's own setup) runs through a
//! recording bus behind the pad guard. The test asserts two things: nothing was written into a pad range,
//! and the guard saw no attempt to (an attempt would be refused, and counted).

use lemnos_k230_usbboot_stage::pads::{self, PAD_COUNT, PadGuard, is_pad_address};
use lemnos_k230_usbboot_stage::uart;
use lemnos_k230_usbboot_stage::wdt::RegAccess;

/// Records every write and answers reads with a plausible UART and pad state.
#[derive(Default)]
struct Bus {
    writes: Vec<usize>,
}

impl RegAccess for Bus {
    fn read32(&mut self, addr: usize) -> u32 {
        // The UART reports 8N1 (a line the ROM left configured) and an empty transmitter; pads read as zero.
        if (uart::BASE..uart::BASE + 0x100).contains(&addr) {
            return match addr - uart::BASE {
                0x0c => 0x03,
                // The transmitter is empty (TEMT, bit 6), THRE clear, as the board reads it: the write waits on TEMT.
                0x14 => 0x60,
                _ => 0,
            };
        }
        0
    }

    fn write32(&mut self, addr: usize, _value: u32) {
        self.writes.push(addr);
    }
}

/// The modes the report stage runs in, each one's register traffic: a banner, the dump, and a banner that
/// reports the gate. A mode that wrote to a pad would show here.
fn run_mode(mode: &str, guard: &mut PadGuard<Bus>) {
    uart::configure_if_unset(guard);
    uart::write_line(guard, mode).unwrap();
    let dump = pads::dump(guard);
    assert_eq!(dump.len(), PAD_COUNT);
    let gate = pads::sd_pads_on_sd_function(&dump);
    uart::write_line(guard, &format!("sd gate {gate:?}")).unwrap();
}

#[test]
fn no_mode_writes_into_the_iomux_or_pmu_iomux_ranges() {
    for mode in ["report", "stage", "probe"] {
        let mut guard = PadGuard::new(Bus::default());
        run_mode(mode, &mut guard);
        assert_eq!(
            guard.refused(),
            0,
            "{mode}: an attempt to write a pad register was made"
        );
        assert!(
            !guard_inner_writes_into_pads(&guard),
            "{mode}: a write reached a pad register"
        );
    }
}

#[test]
fn the_guard_is_the_last_line_even_for_a_caller_that_tries() {
    // A deliberately wrong caller: it tries to set the SD function on IO26. The guard refuses it, and the
    // refusal is counted, so the test above would catch it.
    let mut guard = PadGuard::new(Bus::default());
    guard.write32(pads::pad_register(26), 1 << 11);
    assert_eq!(guard.refused(), 1);
    assert!(!guard_inner_writes_into_pads(&guard));
}

fn guard_inner_writes_into_pads(guard: &PadGuard<Bus>) -> bool {
    // The recorded writes are behind the guard; reach them through its inner bus.
    let inner = guard.inner_for_test();
    inner.writes.iter().any(|&a| is_pad_address(a))
}
