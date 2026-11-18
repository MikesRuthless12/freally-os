/* SPDX-License-Identifier: Apache-2.0 */
/* Freally Core's link layout: the top 2 GiB of the address space. Every output
 * section starts on a page, so each PT_LOAD segment has one set of permissions
 * (text R-X, rodata R, data and bss RW) and the bootloader and the kernel's own
 * page tables can map them W^X. Every input section is placed here: xtask
 * links with --orphan-handling=error. */
ENTRY(_start)

SECTIONS
{
    . = 0xFFFFFFFF80000000;

    .text : ALIGN(4096) {
        __text_start = .;
        *(.text.entry)
        *(.text .text.*)
        __text_end = .;
    }

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(8);
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;
        . = ALIGN(8);
        __extable_start = .;
        KEEP(*(.extable))
        __extable_end = .;
        __rodata_end = .;
    }

    /* The GOT and relocated read-only data are written at link time only, but
     * they go with the data (RW, NX): never between the script's symbols. */
    .data : ALIGN(4096) {
        __data_start = .;
        *(.got .got.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __kernel_end = .;
    }

    /* Not loaded: the linker's own tables. */
    .symtab 0 : { *(.symtab) }
    .strtab 0 : { *(.strtab) }
    .shstrtab 0 : { *(.shstrtab) }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
        *(.debug*)
    }
}
