/* For configure's link tests only, never in a binary: zig's runtime for
 * ARMv5 calls these (when optimised, as build.rs has it), and only the Rust
 * binaries that link ncurses provide them (compiler-builtins). configure
 * never runs what it links. */
	.text
	.globl	__sync_val_compare_and_swap_1
	.globl	__sync_val_compare_and_swap_4
	.globl	__sync_lock_test_and_set_1
__sync_val_compare_and_swap_1:
__sync_val_compare_and_swap_4:
__sync_lock_test_and_set_1:
	bx	lr
