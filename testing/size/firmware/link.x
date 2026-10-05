MEMORY { FLASH : ORIGIN = 0x08000000, LENGTH = 1M
         RAM   : ORIGIN = 0x20000000, LENGTH = 128K }
ENTRY(_start)
SECTIONS {
  .text   : { KEEP(*(.text._start)) *(.text .text.*) } > FLASH
  .rodata : { *(.rodata .rodata.*) } > FLASH
  .data   : { *(.data .data.*) } > RAM AT > FLASH
  .bss    : { *(.bss .bss.*) } > RAM
  /DISCARD/ : { *(.ARM.exidx .ARM.exidx.*) *(.eh_frame*) }
}
