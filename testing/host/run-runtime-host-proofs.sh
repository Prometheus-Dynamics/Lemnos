#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
target="${1:-all}"

require_env() {
    local name="$1"
    if [[ -z "${!name:-}" ]]; then
        printf 'missing required env var: %s\n' "$name" >&2
        return 1
    fi
}

auto_select_usb_target() {
    local interface_path interface_name parent_name bus ports interface_number
    shopt -s nullglob
    for interface_path in /sys/bus/usb/devices/*:*; do
        [[ -d "$interface_path" ]] || continue
        interface_name="$(basename "$interface_path")"
        parent_name="${interface_name%%:*}"
        [[ "$parent_name" == usb* ]] && continue
        interface_number="$(tr -d '\r' <"$interface_path/bInterfaceNumber" | sed 's/[[:space:]]*$//')"
        bus="${parent_name%%-*}"
        ports="${parent_name#*-}"
        [[ "$ports" == 0 ]] && continue

        export LEMNOS_TEST_USB_BUS="$bus"
        export LEMNOS_TEST_USB_PORTS="$ports"
        export LEMNOS_TEST_USB_INTERFACE="$((16#${interface_number:-00}))"
        shopt -u nullglob
        return 0
    done
    shopt -u nullglob
    return 1
}

read_link_name() {
    local path="$1"
    if [[ -L "$path" ]]; then
        basename "$(readlink -f "$path")"
    fi
}

run_usb_proof() {
    if [[ -z "${LEMNOS_TEST_USB_BUS:-}" || -z "${LEMNOS_TEST_USB_PORTS:-}" || -z "${LEMNOS_TEST_USB_INTERFACE:-}" ]]; then
        if auto_select_usb_target; then
            printf 'auto-selected USB target: bus=%s ports=%s interface=%s\n' \
                "$LEMNOS_TEST_USB_BUS" "$LEMNOS_TEST_USB_PORTS" "$LEMNOS_TEST_USB_INTERFACE"
        else
            printf 'unable to auto-select a USB target; run testing/host/discover-runtime-proof-targets.sh\n' >&2
            return 1
        fi
    fi

    (
        cd "$repo_root"
        cargo test -p lemnos-runtime runtime_refreshes_inventory_and_dispatches_usb_requests_through_host_linux_backend -- --ignored --nocapture
    )
}

run_gpio_proof() {
    require_env LEMNOS_TEST_GPIO_CHIP
    require_env LEMNOS_TEST_GPIO_OFFSET
    (
        cd "$repo_root"
        cargo test -p lemnos-runtime runtime_refreshes_inventory_and_dispatches_gpio_requests_through_host_linux_backend -- --ignored --nocapture
    )
}

run_gpio_cdev_proof() {
    require_env LEMNOS_TEST_GPIO_CHIP
    require_env LEMNOS_TEST_GPIO_OFFSET
    (
        cd "$repo_root"
        cargo test -p lemnos-runtime --features gpio-cdev runtime_refreshes_inventory_and_dispatches_gpio_requests_through_host_linux_backend -- --ignored --nocapture
    )
}

run_i2c_proof() {
    require_env LEMNOS_TEST_I2C_BUS
    require_env LEMNOS_TEST_I2C_ADDRESS
    require_env LEMNOS_TEST_I2C_WRITE_HEX
    require_env LEMNOS_TEST_I2C_EXPECT_READ_HEX

    local sysfs_name driver
    sysfs_name="${LEMNOS_TEST_I2C_BUS}-$(printf '%04x' "$((LEMNOS_TEST_I2C_ADDRESS))")"
    driver="$(read_link_name "/sys/bus/i2c/devices/$sysfs_name/driver")"
    if [[ -n "${driver:-}" ]]; then
        printf "warning: I2C target %s is bound to kernel driver '%s'; Lemnos should report an access conflict unless you unbind it first\n" \
            "$sysfs_name" "$driver" >&2
    fi

    (
        cd "$repo_root"
        cargo test -p lemnos-runtime runtime_refreshes_inventory_and_dispatches_i2c_requests_through_host_linux_backend -- --ignored --nocapture
    )
}

run_spi_proof() {
    require_env LEMNOS_TEST_SPI_BUS
    require_env LEMNOS_TEST_SPI_CHIP_SELECT
    require_env LEMNOS_TEST_SPI_TRANSFER_HEX
    require_env LEMNOS_TEST_SPI_EXPECT_READ_HEX
    (
        cd "$repo_root"
        cargo test -p lemnos-runtime runtime_refreshes_inventory_and_dispatches_spi_requests_through_host_linux_backend -- --ignored --nocapture
    )
}

case "$target" in
    gpio) run_gpio_proof ;;
    gpio-cdev) run_gpio_cdev_proof ;;
    usb) run_usb_proof ;;
    i2c) run_i2c_proof ;;
    spi) run_spi_proof ;;
    all)
        if [[ -n "${LEMNOS_TEST_GPIO_CHIP:-}" ]]; then
            run_gpio_proof
            run_gpio_cdev_proof
        else
            printf 'skipping GPIO host proofs; env vars not set\n'
        fi
        run_usb_proof
        if [[ -n "${LEMNOS_TEST_I2C_BUS:-}" ]]; then
            run_i2c_proof
        else
            printf 'skipping I2C host proof; env vars not set\n'
        fi
        if [[ -n "${LEMNOS_TEST_SPI_BUS:-}" ]]; then
            run_spi_proof
        else
            printf 'skipping SPI host proof; env vars not set\n'
        fi
        ;;
    *)
        printf 'usage: %s [gpio|gpio-cdev|usb|i2c|spi|all]\n' "${BASH_SOURCE[0]}" >&2
        exit 1
        ;;
esac
