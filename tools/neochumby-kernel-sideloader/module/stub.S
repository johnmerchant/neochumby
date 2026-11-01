/* SPDX-License-Identifier: MIT
 *
 * Position-independent handoff stub, copied into the reserved kernel block.
 * The module has already cleaned/invalidated the caches with the ARM926
 * sequence; this disables the MMU, puts the initrd where the module chose
 * and jumps.  Physical addresses equal virtual ones in the kernel's mapping
 * of RAM here.  Literals 0xdeadbeef (ATAGS physical), 0xfeedface (kernel
 * physical) and 0xcafe000[123] (the initrd's source, destination and length
 * in bytes, a multiple of 4) are replaced first.
 */
	.section .text
	.global sideload_stub_start
	.global sideload_stub_end
	.global sideload_flush_cache
sideload_flush_cache:
	mov	r2, #0
1:	mrc	p15, 0, r15, c7, c14, 3	@ test, clean and invalidate
	bne	1b
	mcr	p15, 0, r2, c7, c6, 0	@ invalidate D cache
	mcr	p15, 0, r2, c7, c10, 4	@ drain write buffer
	mcr	p15, 0, r2, c7, c5, 0	@ invalidate I cache
	mov	pc, lr

sideload_stub_start:
	mrs	r3, cpsr
	orr	r3, r3, #0xc0
	msr	cpsr_c, r3
	mrc	p15, 0, r0, c1, c0, 0
	bic	r0, r0, #0x00000005	@ MMU off, D cache off
	bic	r0, r0, #0x00001000	@ I cache off
	mcr	p15, 0, r0, c1, c0, 0
	nop
	nop
	nop
	nop
	ldr	r4, =0xcafe0001
	ldr	r5, =0xcafe0002
	ldr	r6, =0xcafe0003
1:	ldr	r7, [r4], #4
	str	r7, [r5], #4
	subs	r6, r6, #4
	bgt	1b
	ldr	r0, =0
	ldr	r1, =851
	ldr	r2, =0xdeadbeef
	ldr	pc, =0xfeedface
	.ltorg
sideload_stub_end:
