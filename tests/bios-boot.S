/* Original disposable smoke-test boot sector, GPL-3.0-or-later. */
.code16
.global _start
_start:
    cli
    xor %ax,%ax
    mov %ax,%ds
    mov %ax,%ss
    mov $0x7c00,%sp
    mov $0x3f8,%dx
    mov $message,%si
next:
    lodsb
    test %al,%al
    jz done
    out %al,%dx
    jmp next
done:
    mov $0xf4,%dx
    mov $0x10,%al
    out %al,%dx
    hlt
    jmp done
message:
    .asciz "FORGEUSB_BIOS_BOOT_OK\r\n"
.org 446
.byte 0x80,0,2,0,0x83,0,0,0
.long 1
.long 2047
.zero 48
.word 0xaa55
