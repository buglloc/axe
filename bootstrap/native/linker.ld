ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
    stack PT_GNU_STACK FLAGS(6);
}

SECTIONS
{
    . = 0x400000;
    .text : ALIGN(16)
    {
        KEEP(*(.text.start))
        *(.text*)
    } :text
    .rodata : ALIGN(16)
    {
        *(.rodata*)
    } :text
    .data : ALIGN(16)
    {
        *(.data*)
    } :data
    .bss : ALIGN(16)
    {
        *(.bss*)
        *(COMMON)
    } :data
    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.rela*)
    }
}
