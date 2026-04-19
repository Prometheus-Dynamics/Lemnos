use lemnos_core::{SpiBitOrder, SpiMode};

pub(crate) fn mode_name(mode: SpiMode) -> &'static str {
    match mode {
        SpiMode::Mode0 => "mode0",
        SpiMode::Mode1 => "mode1",
        SpiMode::Mode2 => "mode2",
        SpiMode::Mode3 => "mode3",
    }
}

pub(crate) fn bit_order_name(bit_order: SpiBitOrder) -> &'static str {
    match bit_order {
        SpiBitOrder::MsbFirst => "msb-first",
        SpiBitOrder::LsbFirst => "lsb-first",
    }
}
