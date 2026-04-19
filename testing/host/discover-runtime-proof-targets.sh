#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"

has_command() {
    command -v "$1" >/dev/null 2>&1
}

read_file_trimmed() {
    local path="$1"
    if [[ -f "$path" ]]; then
        tr -d '\r' <"$path" | sed 's/[[:space:]]*$//'
    fi
}

read_link_name() {
    local path="$1"
    if [[ -L "$path" ]]; then
        basename "$(readlink -f "$path")"
    fi
}

print_header() {
    printf '\n== %s ==\n' "$1"
}

print_gpio_candidates() {
    print_header "GPIO"
    local found=0
    shopt -s nullglob
    for chip_path in /sys/class/gpio/gpiochip*; do
        [[ -d "$chip_path" ]] || continue
        local chip_name label base ngpio devnode has_cdev
        chip_name="$(basename "$chip_path")"
        label="$(read_file_trimmed "$chip_path/label")"
        base="$(read_file_trimmed "$chip_path/base")"
        ngpio="$(read_file_trimmed "$chip_path/ngpio")"
        devnode="/dev/$chip_name"
        has_cdev='no'
        [[ -c "$devnode" ]] && has_cdev='yes'

        printf 'candidate: chip=%s base=%s ngpio=%s cdev=%s' \
            "$chip_name" "${base:-unknown}" "${ngpio:-unknown}" "$has_cdev"
        if [[ -n "${label:-}" ]]; then
            printf ' label=%s' "$label"
        fi
        printf '\n'
        printf '  export LEMNOS_TEST_GPIO_CHIP=%s\n' "$chip_name"
        printf '  export LEMNOS_TEST_GPIO_OFFSET=<safe-line-offset>\n'
        printf '  export LEMNOS_TEST_GPIO_EXPECT_LEVEL=<optional 0|1|low|high>\n'
        if has_command gpioinfo; then
            printf '  gpioinfo %s\n' "$chip_name"
        fi
        printf '  testing/host/run-runtime-host-proofs.sh gpio\n'
        if [[ "$has_cdev" == yes ]]; then
            printf '  testing/host/run-runtime-host-proofs.sh gpio-cdev\n'
        fi
        printf '\n'
        found=1
    done
    shopt -u nullglob

    if [[ "$found" -eq 0 ]]; then
        printf 'no GPIO chip candidates found under /sys/class/gpio\n'
    fi
}

print_usb_candidates() {
    print_header "USB"
    local found=0
    shopt -s nullglob
    for interface_path in /sys/bus/usb/devices/*:*; do
        [[ -d "$interface_path" ]] || continue
        local interface_name parent_name bus ports interface_number vendor product manufacturer product_name
        interface_name="$(basename "$interface_path")"
        parent_name="${interface_name%%:*}"
        [[ "$parent_name" == usb* ]] && continue

        bus="${parent_name%%-*}"
        ports="${parent_name#*-}"
        [[ "$ports" == 0 ]] && continue
        interface_number="$(read_file_trimmed "$interface_path/bInterfaceNumber")"
        vendor="$(read_file_trimmed "/sys/bus/usb/devices/$parent_name/idVendor")"
        product="$(read_file_trimmed "/sys/bus/usb/devices/$parent_name/idProduct")"
        manufacturer="$(read_file_trimmed "/sys/bus/usb/devices/$parent_name/manufacturer")"
        product_name="$(read_file_trimmed "/sys/bus/usb/devices/$parent_name/product")"

        printf 'candidate: bus=%s ports=%s interface=%s vendor=%s product=%s' \
            "$bus" "$ports" "${interface_number:-unknown}" "${vendor:-unknown}" "${product:-unknown}"
        if [[ -n "${manufacturer:-}" || -n "${product_name:-}" ]]; then
            printf ' name=%s%s' "${manufacturer:+$manufacturer }" "${product_name:-}"
        fi
        printf '\n'
        printf '  export LEMNOS_TEST_USB_BUS=%s\n' "$bus"
        printf '  export LEMNOS_TEST_USB_PORTS=%s\n' "$ports"
        printf '  export LEMNOS_TEST_USB_INTERFACE=%s\n' "$((16#${interface_number:-00}))"
        printf '\n'
        found=1
    done
    shopt -u nullglob

    if [[ "$found" -eq 0 ]]; then
        printf 'no USB interface candidates found under /sys/bus/usb/devices\n'
    fi
}

i2c_bus_supports_transfers() {
    local bus="$1"
    if ! has_command i2cdetect; then
        return 2
    fi

    local capability
    capability="$(i2cdetect -F "$bus" 2>/dev/null | awk '/^I2C[[:space:]]/ {print $2}')"
    [[ "$capability" == yes ]]
}

print_i2c_candidates() {
    print_header "I2C"
    local found=0
    shopt -s nullglob
    for device_path in /sys/bus/i2c/devices/*-*; do
        [[ -d "$device_path" ]] || continue
        local name bus address_hex capability_note driver status_note
        name="$(basename "$device_path")"
        [[ "$name" == i2c-* ]] && continue

        bus="${name%%-*}"
        address_hex="0x${name#*-}"
        driver="$(read_link_name "$device_path/driver")"
        capability_note='adapter capability unknown'
        if i2c_bus_supports_transfers "$bus"; then
            capability_note='adapter supports I2C_RDWR'
        else
            case "$?" in
                1) capability_note='adapter lacks I2C_RDWR; SMBus-style proof may still work for simple register devices' ;;
                2) capability_note='i2cdetect not installed; adapter capability unknown' ;;
            esac
        fi

        if [[ -n "${driver:-}" ]]; then
            status_note="bound to kernel driver '$driver'; not a safe host-proof target unless you intentionally unbind it"
        else
            status_note='not bound to a kernel driver'
        fi

        printf 'candidate: bus=%s address=%s name=%s driver=%s (%s; %s)\n' \
            "$bus" "$address_hex" "$(read_file_trimmed "$device_path/name")" "${driver:-<none>}" "$capability_note" "$status_note"
        printf '  export LEMNOS_TEST_I2C_BUS=%s\n' "$bus"
        printf '  export LEMNOS_TEST_I2C_ADDRESS=%s\n' "$address_hex"
        printf '  export LEMNOS_TEST_I2C_WRITE_HEX=<device-specific-write-bytes>\n'
        printf '  export LEMNOS_TEST_I2C_EXPECT_READ_HEX=<expected-read-bytes>\n'
        printf '\n'
        found=1
    done
    shopt -u nullglob

    if [[ "$found" -eq 0 ]]; then
        printf 'no I2C device candidates found under /sys/bus/i2c/devices\n'
    fi
}

print_spi_candidates() {
    print_header "SPI"
    local found=0
    shopt -s nullglob
    for device_path in /sys/bus/spi/devices/*; do
        [[ -d "$device_path" ]] || continue
        local name bus chip_select modalias
        name="$(basename "$device_path")"
        bus="${name#spi}"
        bus="${bus%%.*}"
        chip_select="${name##*.}"
        modalias="$(read_file_trimmed "$device_path/modalias")"

        printf 'candidate: bus=%s chip_select=%s modalias=%s\n' \
            "$bus" "$chip_select" "${modalias:-unknown}"
        printf '  export LEMNOS_TEST_SPI_BUS=%s\n' "$bus"
        printf '  export LEMNOS_TEST_SPI_CHIP_SELECT=%s\n' "$chip_select"
        printf '  export LEMNOS_TEST_SPI_TRANSFER_HEX=<bytes-to-write>\n'
        printf '  export LEMNOS_TEST_SPI_EXPECT_READ_HEX=<expected-read-bytes>\n'
        printf '\n'
        found=1
    done
    shopt -u nullglob

    if [[ "$found" -eq 0 ]]; then
        printf 'no SPI device candidates found under /sys/bus/spi/devices\n'
    fi
}

printf 'lemnos host runtime proof target discovery\n'
printf 'repo: %s\n' "$repo_root"

print_gpio_candidates
print_usb_candidates
print_i2c_candidates
print_spi_candidates
