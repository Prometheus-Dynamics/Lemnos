/*
 * Link script for the 472 loader image (ours): a static PIE, linked with --no-dynamic-linker.
 * Built by the bench builder and by scripts/check-pic.sh for aarch64-unknown-none; not used by cargo.
 *
 * BASE is the image's link base (default 0; the check links the same objects at other bases). The flat
 * image the boot ROM loads starts at BASE: its first byte is the 472 prologue (lemnos_loader472_entry),
 * which the .text.lemnos_entry section puts first (KEEP). No program headers are loaded.
 *
 * The prologue (stage471.S) arms the watchdog, then applies the R_AARCH64_RELATIVE entries between
 * __rela_start and __rela_end (slot = runtime base + addend), then branches to the Rust body. The table
 * is the last loaded section; the dynamic-linking sections a PIE carries are discarded.
 */
PROVIDE(BASE = 0);
ENTRY(lemnos_loader472_entry)

PHDRS
{
    image PT_LOAD FLAGS(7);
}

SECTIONS
{
    . = BASE;
    __image_base = .;
    .text : { KEEP(*(.text.lemnos_entry)) *(.text .text.*) } :image
    .rodata : { *(.rodata .rodata.*) } :image
    .data.rel.ro : { *(.data.rel.ro .data.rel.ro.*) } :image
    .data : { *(.data .data.*) } :image
    .rela.dyn : {
        __rela_start = .;
        *(.rela.dyn)
        __rela_end = .;
    } :image
    .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } :image
    /DISCARD/ : {
        *(.interp) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash) *(.dynamic)
        *(.eh_frame*) *(.comment)
    }
}
