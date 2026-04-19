use lemnos_core::{UartDataBits, UartFlowControl, UartParity, UartStopBits};

pub(crate) fn data_bits_name(data_bits: UartDataBits) -> &'static str {
    match data_bits {
        UartDataBits::Five => "5",
        UartDataBits::Six => "6",
        UartDataBits::Seven => "7",
        UartDataBits::Eight => "8",
    }
}

pub(crate) fn parity_name(parity: UartParity) -> &'static str {
    match parity {
        UartParity::None => "none",
        UartParity::Even => "even",
        UartParity::Odd => "odd",
    }
}

pub(crate) fn stop_bits_name(stop_bits: UartStopBits) -> &'static str {
    match stop_bits {
        UartStopBits::One => "1",
        UartStopBits::Two => "2",
    }
}

pub(crate) fn flow_control_name(flow_control: UartFlowControl) -> &'static str {
    match flow_control {
        UartFlowControl::None => "none",
        UartFlowControl::Software => "software",
        UartFlowControl::Hardware => "hardware",
    }
}
