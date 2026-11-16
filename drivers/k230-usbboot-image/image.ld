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
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(16);
        lemnos_stack_bottom = .;
        . += 0x4000;
        . = ALIGN(16);
        lemnos_stack_top = .;
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
