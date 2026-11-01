// SPDX-License-Identifier: MIT
//
// Reserve a contiguous kernel block and hand off to a 5.9 zImage without
// CVE-2009-2698.  Loaded on the stock 2.6.16, this module owns the pages;
// root writes the images through /proc and writes /proc/sideload-go to jump.

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/gfp.h>
#include <linux/proc_fs.h>
#include <linux/fs.h>
#include <asm/uaccess.h>
#include <linux/string.h>
#include <asm/io.h>
#include <asm/system.h>

#define KERNEL_ORDER 10
#define INITRD_ORDER 10
#define BLOCKS 4
#define KERNEL_SIZE (1UL << (KERNEL_ORDER + PAGE_SHIFT))
#define INITRD_SIZE (1UL << (INITRD_ORDER + PAGE_SHIFT))
#define ATAGS_PHYS 0xc3f00000UL
#define STUB_PHYS 0xc3f80000UL
#define ATAGS_MAGIC 0x54410001
#define CMDLINE "console=ttymxc0,115200 rdinit=/init neochumby.sideloaded=1"

extern char sideload_stub_start[], sideload_stub_end[];
extern void sideload_flush_cache(void);

static unsigned long kva, iva, guard;
static unsigned long blocks[BLOCKS];
static unsigned long kphys, iphys;
static unsigned long koff, ioff;
static struct proc_dir_entry *reserve_entry, *kernel_entry, *initrd_entry, *go_entry;

static int reserve_read(char *page, char **start, off_t off, int count, int *eof, void *data)
{
	return sprintf(page, "%lx %lx\n", kphys, iphys);
}

static int kernel_write(struct file *file, const char __user *buffer, unsigned long count, void *data)
{
	if (koff + count > KERNEL_SIZE)
		return -ENOSPC;
	if (copy_from_user((void *)__va(kphys + koff), buffer, count))
		return -EFAULT;
	koff += count;
	return count;
}

static int initrd_write(struct file *file, const char __user *buffer, unsigned long count, void *data)
{
	if (ioff + count > INITRD_SIZE)
		return -ENOSPC;
	if (copy_from_user((void *)__va(iphys + ioff), buffer, count))
		return -EFAULT;
	ioff += count;
	return count;
}

static void build_atags(unsigned long atags, unsigned long initrd_start, unsigned long initrd_size)
{
	u32 *p = (u32 *)__va(atags);
	const char *cmd = CMDLINE;
	int len = strlen(cmd) + 1;
	int words = (len + 3) / 4;

	*p++ = 2; *p++ = ATAGS_MAGIC;
	*p++ = 4; *p++ = 0x54410002;
	*p++ = 0x04000000; *p++ = 0xc0000000;
	if (initrd_size) {
		*p++ = 4; *p++ = 0x54420005;
		*p++ = initrd_start; *p++ = initrd_size;
	}
	*p++ = 2 + words; *p++ = 0x54410009;
	memcpy(p, cmd, len);
	p += words;
	*p++ = 0; *p++ = 0;
}

static void handoff(void)
{
	unsigned long atags = ATAGS_PHYS;
	unsigned long stub = STUB_PHYS;
	unsigned long len = sideload_stub_end - sideload_stub_start;
	u32 *w;
	unsigned long i;

	if (koff < 0x100 || ((u32 *)__va(kphys))[0x24 / 4] != 0x016f2818)
		return;
	if (ioff < 6)
		return;

	memcpy((void *)__va(stub), sideload_stub_start, len);
	w = (u32 *)__va(stub);
	for (i = 0; i < len / 4; i++) {
		if (w[i] == 0xdeadbeef)
			w[i] = atags;
		if (w[i] == 0xfeedface)
			w[i] = kphys;
	}

	build_atags(atags, iphys, ioff);
	local_irq_disable();
	__raw_writew(0x7F34, (void __iomem *)0xe0002000);
	__raw_writew(0x5555, (void __iomem *)0xe0002002);
	__raw_writew(0xAAAA, (void __iomem *)0xe0002002);
	sideload_flush_cache();
	((void (*)(void))__va(stub))();
}

static int go_write(struct file *file, const char __user *buffer, unsigned long count, void *data)
{
	handoff();
	return count;
}

static int __init sideload_init(void)
{
	int i, j;
	unsigned long t;

	for (i = 0; i < BLOCKS; i++) {
		blocks[i] = __get_free_pages(GFP_KERNEL, KERNEL_ORDER);
		if (!blocks[i]) {
			for (j = 0; j < i; j++)
				free_pages(blocks[j], KERNEL_ORDER);
			printk(KERN_ERR "sideload: no contiguous pages\n");
			return -ENOMEM;
		}
	}
	for (i = 0; i < BLOCKS; i++)
		for (j = i + 1; j < BLOCKS; j++)
			if (blocks[j] < blocks[i]) {
				t = blocks[i]; blocks[i] = blocks[j]; blocks[j] = t;
			}
	guard = blocks[0];
	/* The zImage stays above the decompressed Image; the initrd is the
	 * highest block, out of its way.  blocks[1] is unused. */
	kva = blocks[BLOCKS - 2];
	iva = blocks[BLOCKS - 1];
	kphys = __pa(kva);
	iphys = __pa(iva);
	koff = ioff = 0;
	printk(KERN_INFO "sideload: guard=%lx\n", __pa(guard));

	reserve_entry = create_proc_entry("sideload-reserve", 0444, NULL);
	kernel_entry = create_proc_entry("sideload-kernel", 0200, NULL);
	initrd_entry = create_proc_entry("sideload-initrd", 0200, NULL);
	go_entry = create_proc_entry("sideload-go", 0200, NULL);
	if (!reserve_entry || !kernel_entry || !initrd_entry || !go_entry)
		return -ENOMEM;
	reserve_entry->read_proc = reserve_read;
	kernel_entry->write_proc = kernel_write;
	initrd_entry->write_proc = initrd_write;
	go_entry->write_proc = go_write;
	reserve_entry->owner = kernel_entry->owner = initrd_entry->owner = go_entry->owner = THIS_MODULE;

	printk(KERN_INFO "sideload: kernel=%lx initrd=%lx\n", kphys, iphys);
	return 0;
}

static void __exit sideload_exit(void)
{
	int i;
	remove_proc_entry("sideload-reserve", NULL);
	remove_proc_entry("sideload-kernel", NULL);
	remove_proc_entry("sideload-initrd", NULL);
	remove_proc_entry("sideload-go", NULL);
	for (i = 0; i < BLOCKS; i++)
		free_pages(blocks[i], KERNEL_ORDER);
}

module_init(sideload_init);
module_exit(sideload_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Reserve pages and hand off to a new ARM kernel");
