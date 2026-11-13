use lemnos_contract_raw_bus::RawBus;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn RawBus as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "raw-bus/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("i2c_transfer", 1),
            ("spi_transfer", 2),
            ("line_get", 3),
            ("line_set", 4),
            ("pwm_set", 5),
            ("uart_write", 6),
            ("uart_read", 7),
        ]
    );
}

#[test]
fn the_shapes_carry_bytes_in_the_call_and_fill_a_reply_buffer() {
    let info = <dyn RawBus as Contract>::INFO;
    let i2c = info.methods[0];
    assert_eq!(i2c.params[0].shape, Shape::U16);
    assert_eq!(i2c.params[1].name, "write");
    assert_eq!(i2c.params[1].shape, Shape::Bytes);
    assert_eq!(i2c.params[2].shape, Shape::U32);
    assert!(i2c.has_out_buffer);
    let spi = info.methods[1];
    assert_eq!(spi.params[0].shape, Shape::Bytes);
    assert!(spi.has_out_buffer);
    assert_eq!(info.methods[2].returns, Shape::Bool);
    assert_eq!(info.methods[4].params[0].shape, Shape::U64);
    let uart_write = info.methods[5];
    assert_eq!(uart_write.params[0].shape, Shape::Bytes);
    assert_eq!(uart_write.returns, Shape::U32);
    let uart_read = info.methods[6];
    assert_eq!(uart_read.params[0].shape, Shape::U32);
    assert_eq!(uart_read.params[1].name, "timeout_ms");
    assert!(uart_read.has_out_buffer);
}
