/* The stage image's link map. It is linked at the load address the boot ROM is given (the vendor
 * tool's default, kburn_k230.h:31, inside the range k230_flash.cpp:109 accepts) and nowhere else, so
 * no relocation is needed. The entry is first in the image: the ROM starts it at the load address. */
ENTRY(lemnos_image_entry)

SECTIONS
{
    . = 0x80360000;

    .text : {
        KEEP(*(.text.lemnos_image_entry))
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss (NOLOAD) : {
        /* NOLOAD: the ROM copies only the file, so the entry zeroes this range before any static is read. */
        . = ALIGN(16);
        lemnos_bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(16);
        /* 16 KiB, was 64 KiB. The deepest mode's frames, summed down the image's direct-call graph, are 3.5 KiB (the
         * card write piece; the report 1 KiB, the probe 0.7 KiB). The reserve is about four times that. The cut
         * is what lets the image fit below the job address 0x80380000 with the flash extent's checks in it.
         * The image paints the reserve and checks a canary, and prints `stack: <used> of 16384 bytes` at each end: a
         * figure above 8192 means raise the reserve and cut elsewhere (docs/k230-flash-extent.md). */
        lemnos_stack_bottom = .;
        . += 0x4000;
        . = ALIGN(16);
        lemnos_stack_top = .;
        lemnos_bss_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
