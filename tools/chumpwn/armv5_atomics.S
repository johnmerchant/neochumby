/* Minimal __sync_* helpers for Zig compiler-rt on ARMv5TE (single core).
 * The BusyBox link references these through Zig's compiler-rt; no LDREX. */
    .syntax unified
    .arm
    .text

    .global __sync_lock_test_and_set_1
    .type __sync_lock_test_and_set_1, %function
__sync_lock_test_and_set_1:
    ldrb    r2, [r0]
    strb    r1, [r0]
    mov     r0, r2
    bx      lr

    .global __sync_lock_test_and_set_2
    .type __sync_lock_test_and_set_2, %function
__sync_lock_test_and_set_2:
    ldrh    r2, [r0]
    strh    r1, [r0]
    mov     r0, r2
    bx      lr

    .global __sync_lock_test_and_set_4
    .type __sync_lock_test_and_set_4, %function
__sync_lock_test_and_set_4:
    swp     r2, r1, [r0]
    mov     r0, r2
    bx      lr

    .global __sync_val_compare_and_swap_1
    .type __sync_val_compare_and_swap_1, %function
__sync_val_compare_and_swap_1:
    ldrb    r3, [r0]
    cmp     r3, r1
    strbeq  r2, [r0]
    mov     r0, r3
    bx      lr

    .global __sync_val_compare_and_swap_2
    .type __sync_val_compare_and_swap_2, %function
__sync_val_compare_and_swap_2:
    ldrh    r3, [r0]
    cmp     r3, r1
    strheq  r2, [r0]
    mov     r0, r3
    bx      lr

    .global __sync_val_compare_and_swap_4
    .type __sync_val_compare_and_swap_4, %function
__sync_val_compare_and_swap_4:
    ldr     r3, [r0]
    cmp     r3, r1
    streq   r2, [r0]
    mov     r0, r3
    bx      lr

    .size __sync_lock_test_and_set_1, .-__sync_lock_test_and_set_1
