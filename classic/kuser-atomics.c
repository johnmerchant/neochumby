/*
 * The __sync_* functions zig's libc calls on the ARM926EJ-S (ARMv5TE),
 * which has no ldrex/strex for the compiler to inline them with: a
 * compare-and-swap is the kernel's, its "kuser" helper __kernel_cmpxchg at
 * 0xffff0fc0, as libgcc's linux-atomic.c does it (and musl's own atomics,
 * and Rust's compiler_builtins, which supplies these for neochumby's Rust
 * binaries). zig's compiler-rt has none of them for ARMv5. The helper is
 * in every kernel since 2.6.12, so the stock 2.6.16 has it.
 *
 * Only what zig's libc needs (its allocator's locks): compare-and-swap and
 * exchange, of 1, 2 and 4 bytes. A byte or halfword is swapped within its
 * aligned word.
 */
#include <stdint.h>

typedef int kernel_cmpxchg_t(int oldval, int newval, volatile int *ptr);
#define kernel_cmpxchg (*(kernel_cmpxchg_t *)0xffff0fc0)

/* Replaces the SIZE bytes at P with what NEW makes of them, if TEST says
 * so, atomically; returns what they were. */
static unsigned update(volatile void *p, unsigned size, int test, unsigned old, unsigned new)
{
    volatile int *w = (volatile int *)((uintptr_t)p & ~(uintptr_t)3);
    unsigned shift = ((uintptr_t)p & 3) * 8;
    unsigned mask = size == 4 ? ~0u : ((1u << (size * 8)) - 1) << shift;
    for (;;) {
        unsigned cur = (unsigned)*w;
        unsigned was = (cur & mask) >> shift;
        if (test && was != old)
            return was;
        unsigned next = (cur & ~mask) | ((new << shift) & mask);
        if (kernel_cmpxchg((int)cur, (int)next, w) == 0)
            return was;
    }
}

/* Under other names in C, which has these as builtins (clang refuses to
 * define them), and their own in the object. */
#define SYNC(n, type)                                                              \
    type sync_cas_##n(volatile void *p, type old, type new)                        \
        __asm__("__sync_val_compare_and_swap_" #n);                                \
    type sync_cas_##n(volatile void *p, type old, type new)                        \
    {                                                                              \
        return (type)update(p, n, 1, old, new);                                    \
    }                                                                              \
    type sync_xchg_##n(volatile void *p, type v)                                   \
        __asm__("__sync_lock_test_and_set_" #n);                                   \
    type sync_xchg_##n(volatile void *p, type v)                                   \
    {                                                                              \
        return (type)update(p, n, 0, 0, v);                                        \
    }

SYNC(1, uint8_t)
SYNC(2, uint16_t)
SYNC(4, uint32_t)
