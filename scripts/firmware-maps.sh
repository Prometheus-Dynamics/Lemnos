#!/bin/bash
# Builds the Raspberry Pi firmware's own files that a bay set carries under overlays/ from the pinned
# Raspberry Pi kernel tree, the way kbuild builds them (scripts/Makefile.dtbs of that tree):
#
#   overlay_map.dtb   the firmware's overlay name map        (cmd_dtc)
#   hat_map.dtb       the firmware's HAT EEPROM overlay map  (cmd_dtc)
#   bcm2712d0.dtbo    the overlay for D0-stepping BCM2712    (cmd_dtco, with -@ -H epapr)
#
# Usage: scripts/firmware-maps.sh KERNEL_TREE OUT_DIR [COMPARE_DIR]
#
# KERNEL_TREE is a git checkout of raspberrypi/linux. It must be at the pinned commit and the sources
# these files are built from must be unmodified there; the tree is only read (the preprocessed text
# goes to OUT_DIR). OUT_DIR is created and must not hold any of the three files. DTC names the dtc to
# run (default: dtc on PATH, its version is printed). With COMPARE_DIR, each file's SHA-256 is compared
# with the file of the same name there and the result printed: information only, it changes nothing.
set -euo pipefail

PIN=53679a5788f7912272b412fd78029bccae6f2b1a # docs/kernel-config.md, the board build's kernel
SOURCES=(
  arch/arm/boot/dts/overlays/overlay_map.dts
  arch/arm/boot/dts/overlays/hat_map.dts
  arch/arm/boot/dts/overlays/bcm2712d0-overlay.dts
  scripts/dtc/include-prefixes
  include/dt-bindings
)

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
  echo "usage: $0 KERNEL_TREE OUT_DIR [COMPARE_DIR]" >&2
  exit 2
fi
tree=$1
out=$2
compare=${3:-}
dtc=${DTC:-dtc}

head=$(git -C "$tree" rev-parse HEAD)
if [ "$head" != "$PIN" ]; then
  echo "$tree is at $head; the pinned commit is $PIN: nothing was built" >&2
  exit 1
fi
if ! git -C "$tree" diff --quiet HEAD -- "${SOURCES[@]}"; then
  echo "$tree has local changes in the sources these files are built from:" >&2
  git -C "$tree" diff --stat HEAD -- "${SOURCES[@]}" >&2
  echo "nothing was built" >&2
  exit 1
fi
for name in overlay_map.dtb hat_map.dtb bcm2712d0.dtbo; do
  if [ -e "$out/$name" ]; then
    echo "$out/$name is already there: nothing was built" >&2
    exit 1
  fi
done
mkdir -p "$out"

overlays=$tree/arch/arm/boot/dts/overlays
include=$tree/scripts/dtc/include-prefixes
# The warning set kbuild passes without KBUILD_EXTRA_WARN.
warn=(-Wno-unique_unit_address -Wno-unit_address_vs_reg -Wno-avoid_unnecessary_addr_size
  -Wno-alias_paths -Wno-interrupt_map -Wno-simple_bus_reg)
overlay_warn=(-Wno-avoid_default_addr_size -Wno-gpios_property -Wno-i2c_bus_reg
  -Wno-interrupt_provider -Wno-interrupts_property -Wno-label_is_string -Wno-pci_device_bus_num
  -Wno-reg_format -Wno-spi_bus_reg)

# preprocess SOURCE TEXT: cpp as kbuild runs it, into TEXT.
preprocess() {
  cpp -nostdinc -I "$include" -undef -D__DTS__ -x assembler-with-cpp -o "$2" "$1"
}

echo "dtc: $("$dtc" --version | head -1)"
for map in overlay_map hat_map; do
  text=$out/.$map.dts.tmp
  preprocess "$overlays/$map.dts" "$text"
  "$dtc" -o "$out/$map.dtb" -b 0 -i "$overlays" -i "$include" "${warn[@]}" "$text"
  rm -f "$text"
done
text=$out/.bcm2712d0.dts.tmp
preprocess "$overlays/bcm2712d0-overlay.dts" "$text"
"$dtc" -@ -H epapr -O dtb -o "$out/bcm2712d0.dtbo" -b 0 -i "$overlays" -i "$include" \
  "${warn[@]}" "${overlay_warn[@]}" "$text"
rm -f "$text"

for name in overlay_map.dtb hat_map.dtb bcm2712d0.dtbo; do
  sha=$(sha256sum "$out/$name" | cut -d' ' -f1)
  line="$name $(stat -c %s "$out/$name") bytes sha256 $sha"
  if [ -n "$compare" ]; then
    if [ -f "$compare/$name" ]; then
      theirs=$(sha256sum "$compare/$name" | cut -d' ' -f1)
      if [ "$theirs" = "$sha" ]; then
        line="$line, the same as $compare/$name"
      else
        line="$line, differs from $compare/$name ($theirs; information only)"
      fi
    else
      line="$line, $compare/$name is not there"
    fi
  fi
  echo "$line"
done
echo "built from $tree at $PIN into $out"
