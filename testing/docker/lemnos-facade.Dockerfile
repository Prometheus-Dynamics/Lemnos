FROM rust:1.86-bookworm AS builder
WORKDIR /workspace

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libudev-dev \
        libusb-1.0-0-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release -p lemnos --examples --features mock,builtin-drivers,tokio,macros

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libudev1 \
        libusb-1.0-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/lemnos
COPY --from=builder /workspace/target/release/examples/mock_gpio /usr/local/bin/mock_gpio
COPY --from=builder /workspace/target/release/examples/mock_gpio_async /usr/local/bin/mock_gpio_async
COPY --from=builder /workspace/target/release/examples/mock_power_sensor_driver /usr/local/bin/mock_power_sensor_driver
COPY --from=builder /workspace/target/release/examples/mock_ina226_driver /usr/local/bin/mock_ina226_driver
COPY --from=builder /workspace/target/release/examples/mock_bmm150_driver /usr/local/bin/mock_bmm150_driver
COPY --from=builder /workspace/target/release/examples/mock_bmi088_driver /usr/local/bin/mock_bmi088_driver
COPY --from=builder /workspace/target/release/examples/mock_usb_hotplug /usr/local/bin/mock_usb_hotplug
