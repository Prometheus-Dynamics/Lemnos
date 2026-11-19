/* The stage image's link map. It is linked at the load address the boot ROM is given (the vendor
 * tool's default, kburn_k230.h:31, inside the range k230_flash.cpp:109 accepts) and nowhere else, so
 * no relocation is needed. The entry is first in the image: the ROM starts it at the load address. */
ENTRY(lemnos_image_entry)

/* Three segments, named so the file headers are not placed below the image: the stack's reserve is a NOLOAD
 * segment below the load address, so without these the linker would put the ELF headers there and objcopy would
 * carry them into the binary. The file is the code segment and the read-only data: it starts at the load address. */
PHDRS
{
    stack PT_LOAD;
    code PT_LOAD;
    data PT_LOAD;
}

SECTIONS
{
    . = 0x80360000;

    .text : {
        KEEP(*(.text.lemnos_image_entry))
        *(.text .text.*)
    } :code

    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    } :code

    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } :data

    .bss (NOLOAD) : {
        /* NOLOAD: the ROM copies only the file, so the entry zeroes this range before any static is read. */
        . = ALIGN(16);
        lemnos_bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(16);
        lemnos_bss_end = .;
    } :data

    /* The stack and its guard band sit BELOW the image's code, at the load address minus the reserve, so that
     * nothing of the image lies below the stack. A stack that grows down out of its reserve meets the guard band
     * first, then the RAM below the image, which the image never uses. It does not reach code, read-only data,
     * statics, or the job area above 0x80380000. The entry paints both (lemnos_image_entry).
     * Reserve: 16 KiB. The deepest mode's frames, summed down the image's direct-call graph, are 3.5 KiB (the card
     * write piece; the report 1 KiB, the probe 0.7 KiB), but the indirect calls are not counted, so the measured
     * high-water mark is the figure (printed as `stack: <used> of 16384 bytes`, docs/k230-flash-extent.md).
     * The guard band: 256 bytes of canary, checked before every card write and at every end. */
    .guard 0x8035BF00 (NOLOAD) : {
        lemnos_guard_bottom = .;
        . += 0x100;
    } :stack

    .stack 0x8035C000 (NOLOAD) : {
        lemnos_stack_bottom = .;
        . += 0x4000;
        lemnos_stack_top = .;
    } :stack

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
