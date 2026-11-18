/* SPDX-License-Identifier: Apache-2.0 */
/* Freally Core's link layout: the top 2 GiB of the address space. Every output
 * section starts on a page, so each PT_LOAD segment has one set of permissions
 * (text R-X, rodata R, data and bss RW) and the bootloader can map them W^X. */
ENTRY(_start)

SECTIONS
{
    . = 0xFFFFFFFF80000000;

    .text : ALIGN(4096) {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
        . = ALIGN(8);
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment)
        *(.note*)
    }
}
