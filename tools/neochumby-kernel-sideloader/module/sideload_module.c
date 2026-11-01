// SPDX-License-Identifier: MIT
//
// Reserve contiguous blocks and hand off to a Linux 7 zImage.  Loaded on the
// stock 2.6.16, this module owns the pages; root writes the images through
// /proc and writes /proc/sideload-go to jump.

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
#define BLOCKS 2
#define MAX_BLOCKS 16
/*
 * The zImage's block must be above the kernel it unpacks, or it moves itself
 * out of the way, over who knows what. Linux 7 unpacks to under 11 MiB.
 */
#define KERNEL_MIN 0xc0b00000UL
/*
 * Where the stub puts the initrd, with the MMU off and nothing else running:
 * a block as high as this is rarely free to reserve, and after the jump
 * nothing on the stock system matters. Below the ATAGS and the stub.
 */
#define INITRD_DEST 0xc3000000UL
#define KERNEL_SIZE (1UL << (KERNEL_ORDER + PAGE_SHIFT))
#define INITRD_SIZE (1UL << (INITRD_ORDER + PAGE_SHIFT))
#define ATAGS_PHYS 0xc3f00000UL
#define STUB_PHYS 0xc3f80000UL
#define ATAGS_MAGIC 0x54410001
#define CMDLINE "console=ttymxc0,115200 rdinit=/init neochumby.sideloaded=1"

extern char sideload_stub_start[], sideload_stub_end[];
extern void sideload_flush_cache(void);

static unsigned long kva, iva;
static unsigned long blocks[BLOCKS];
static unsigned long kphys, iphys;
static unsigned long koff, ioff;
static struct proc_dir_entry *reserve_entry, *regs_entry, *kernel_entry, *initrd_entry, *go_entry;

static int reserve_read(char *page, char **start, off_t off, int count, int *eof, void *data)
{
	return sprintf(page, "%lx %lx\n", kphys, iphys);
}

/*
 * What the running stock system left in the hardware it set up, read only, to
 * compare with what Linux 7 programs: the blocks' offsets from 0x10000000, the
 * kernel's fixed mapping of them being 0xe0000000.
 */
static int regs_read(char *page, char **start, off_t off, int count, int *eof, void *data)
{
	static const struct { unsigned long base, len; const char *name; } r[] = {
		{ 0x24000, 0x20, "usbotg" },
		{ 0x24080, 0x20, "usbh" },
		{ 0x240e8, 0x20, "usbh-roothub" },
		{ 0x24600, 0x04, "usbctrl" },
		{ 0x24840, 0x04, "misc" },
		{ 0x27000, 0x28, "ccm" },
		{ 0x15100, 0x44, "gpio-b" },
		{ 0x15300, 0x40, "gpio-d" },
		{ 0x15220, 0x10, "gpio-c" },
		{ 0x15400, 0x44, "gpio-e" },
		{ 0x17000, 0x20, "cspi3" },
		{ 0x21000, 0x70, "lcdc" },
	};
	int n = 0, i;
	unsigned long o;

	for (i = 0; i < ARRAY_SIZE(r); i++) {
		n += sprintf(page + n, "%s %08lx:", r[i].name, 0x10000000UL + r[i].base);
		for (o = 0; o < r[i].len; o += 4)
			n += sprintf(page + n, " %08x", __raw_readl((void __iomem *)(0xe0000000UL + r[i].base + o)));
		n += sprintf(page + n, "\n");
	}
	*eof = 1;
	return n;
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
		if (w[i] == 0xcafe0001)
			w[i] = iphys;
		if (w[i] == 0xcafe0002)
			w[i] = INITRD_DEST;
		if (w[i] == 0xcafe0003)
			w[i] = (ioff + 3) & ~3UL;
	}

	build_atags(atags, INITRD_DEST, ioff);
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
	unsigned long got[MAX_BLOCKS];
	int n = 0, i, j, keep;
	unsigned long t;

	/* Every block there is: only the highest are any use. */
	while (n < MAX_BLOCKS) {
		got[n] = __get_free_pages(GFP_KERNEL | __GFP_NOWARN, KERNEL_ORDER);
		if (!got[n])
			break;
		n++;
	}
	for (i = 0; i < n; i++)
		for (j = i + 1; j < n; j++)
			if (got[j] < got[i]) {
				t = got[i]; got[i] = got[j]; got[j] = t;
			}
	/* The highest clear of where the stub writes. */
	keep = n - 1;
	while (keep >= 0 && got[keep] + KERNEL_SIZE > INITRD_DEST)
		keep--;
	if (keep < 1 || got[keep] < KERNEL_MIN) {
		printk(KERN_ERR "sideload: %d blocks, none high enough:", n);
		for (i = 0; i < n; i++)
			printk(" %lx", got[i]);
		printk("\n");
		for (i = 0; i < n; i++)
			free_pages(got[i], KERNEL_ORDER);
		return -ENOMEM;
	}
	kva = got[keep];
	iva = got[keep - 1];
	blocks[0] = kva;
	blocks[1] = iva;
	for (i = 0; i < n; i++)
		if (got[i] != kva && got[i] != iva)
			free_pages(got[i], KERNEL_ORDER);
	kphys = __pa(kva);
	iphys = __pa(iva);
	koff = ioff = 0;

	reserve_entry = create_proc_entry("sideload-reserve", 0444, NULL);
	regs_entry = create_proc_entry("sideload-regs", 0444, NULL);
	kernel_entry = create_proc_entry("sideload-kernel", 0200, NULL);
	initrd_entry = create_proc_entry("sideload-initrd", 0200, NULL);
	go_entry = create_proc_entry("sideload-go", 0200, NULL);
	if (!reserve_entry || !regs_entry || !kernel_entry || !initrd_entry || !go_entry)
		return -ENOMEM;
	reserve_entry->read_proc = reserve_read;
	regs_entry->read_proc = regs_read;
	regs_entry->owner = THIS_MODULE;
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
	remove_proc_entry("sideload-regs", NULL);
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
