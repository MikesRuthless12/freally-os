/* SPDX-License-Identifier: Apache-2.0 */
/* Freally Core's link layout: the top 2 GiB of the address space. Every output
 * section starts on a page, so each PT_LOAD segment has one set of permissions
 * (text R-X, rodata R, data and bss RW) and the bootloader and the kernel's own
 * page tables can map them W^X. */
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

    .data : ALIGN(4096) {
        __data_start = .;
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __kernel_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
