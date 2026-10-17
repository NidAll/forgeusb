/* Original disposable x86_64 UEFI application, GPL-3.0-or-later.
 * It deliberately contains no Microsoft or third-party boot code. Reaching the
 * marker proves firmware loaded this PE application, never Windows Setup.
 */
static inline void outb(unsigned short port, unsigned char value) {
    __asm__ volatile("outb %0, %1" : : "a"(value), "Nd"(port));
}

static inline unsigned char inb(unsigned short port) {
    unsigned char value;
    __asm__ volatile("inb %1, %0" : "=a"(value) : "Nd"(port));
    return value;
}

unsigned long long efi_main(void *image, void *system_table) {
    (void)image;
    (void)system_table;
    const char *message = "FORGEUSB_UEFI_BOOT_OK\r\n";
    outb(0x3f9, 0);       /* Disable UART interrupts. */
    outb(0x3fb, 0x80);    /* Set baud divisor to 115200. */
    outb(0x3f8, 1);
    outb(0x3f9, 0);
    outb(0x3fb, 3);       /* Eight data bits, one stop bit. */
    while (*message) {
        /* Bound the wait even if a different VM lacks a UART. */
        unsigned int attempts = 100000;
        while (!(inb(0x3fd) & 0x20) && --attempts) {}
        outb(0x3f8, (unsigned char)*message++);
    }
    /* QEMU's explicitly configured test-only isa-debug-exit returns 33. */
    outb(0xf4, 0x10);
    for (;;) __asm__ volatile("hlt");
}
