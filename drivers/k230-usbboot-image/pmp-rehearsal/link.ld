/* The qemu virt RAM at 0x80000000: the rehearsal's own layout. The stack is 16 KiB at the top of the image, as in
 * the card image's reserve; `stack_top` is the initial stack pointer. */
ENTRY(_start)
MEMORY { RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 16M }
SECTIONS
{
    .text : { KEEP(*(.text.start)) *(.text .text.*) } > RAM
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) } > RAM
    .data : { *(.data .data.*) *(.sdata .sdata.*) } > RAM
    .bss (NOLOAD) : { *(.bss .bss.*) *(.sbss .sbss.*) } > RAM
    .stack (NOLOAD) : { . = ALIGN(16); . += 0x4000; stack_top = .; } > RAM
}
