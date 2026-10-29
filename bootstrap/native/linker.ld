ENTRY(_start)

PHDRS
{
    text PT_LOAD FILEHDR PHDRS FLAGS(5);
    data PT_LOAD FLAGS(6);
    stack PT_GNU_STACK FLAGS(6);
}

SECTIONS
{
    /* Map the ELF and program headers with the code so the file needs no
       page-alignment padding before .text. */
    . = 0x400000 + SIZEOF_HEADERS;
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
