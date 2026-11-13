//! A PIO strip allocates nothing in steady state: 1000 shows of a 16-LED RGBW frame, counted by the
//! measure crate's test allocator.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lemnos_contract_pixels::Pixels;
use lemnos_core::{
    Config, ConfigValue, Driver, Held, LocalCaller, Pio, Provider, Resources, Spi, Status, Tag,
};
use lemnos_driver_ws2812::Ws2812;
use lemnos_hal::ErrorKind;
use lemnos_hal::pio::{PioJob, PioMachine, PioOperation};
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A machine that counts what it is given and keeps nothing.
struct Quiet(Arc<AtomicUsize>);

impl PioMachine for Quiet {
    fn load(&mut self, _job: &PioJob<'_>) -> Result<(), ErrorKind> {
        Ok(())
    }

    fn transaction(&mut self, operations: &mut [PioOperation<'_>]) -> Result<(), ErrorKind> {
        self.0.fetch_add(operations.len(), Ordering::SeqCst);
        Ok(())
    }
}

struct Board(Option<Pio>);

impl Provider for Board {
    fn provided(&self, _need: &str) -> bool {
        true
    }
    fn device(&mut self, _need: &str, _tag: Tag) -> lemnos_core::Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }
    fn spi(&mut self, _need: &str) -> lemnos_core::Result<Spi> {
        Err(Status::NOT_FOUND)
    }
    fn pio(&mut self, _need: &str) -> lemnos_core::Result<Pio> {
        self.0.take().ok_or(Status::NOT_FOUND)
    }
}

#[test]
fn a_thousand_shows_allocate_nothing() {
    let ops = Arc::new(AtomicUsize::new(0));
    let machine: Pio = Held::boxed(Box::new(Quiet(Arc::clone(&ops))));
    let mut board = Board(Some(machine));
    let mut res = Resources::new(&mut board, None);
    let entries = [
        ("count", ConfigValue::Int(16)),
        ("wire", ConfigValue::Str("rgbw")),
    ];
    let mut driver = Ws2812::bind(&mut res, &Config::new(&entries)).unwrap();
    for led in 0..16 {
        driver.set(led, &[led as u8, 0x20, 0x30, 0x40]).unwrap();
    }
    Pixels::show(&mut driver).unwrap();
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            Pixels::show(&mut driver).unwrap();
        }
    });
    assert_eq!(count, 0, "show allocated");
    assert_eq!(
        ops.load(Ordering::SeqCst),
        2 * 1002,
        "bind, one show, then 1000 shows: two operations each"
    );
}
