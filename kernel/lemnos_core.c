// SPDX-License-Identifier: GPL-2.0
/*
 * lemnos: the one Lemnos kernel module. It claims platform blocks through
 * driver_override and gives one opener of /dev/lemnos what a driver in user
 * space needs from them (see lemnos.h): register regions, DMA buffers as
 * dma-bufs, dma-bufs mapped to device addresses, and interrupts as reads.
 *
 * Only the blocks listed in the "owned" parameter are claimed. The driver has
 * no match table, so it binds only where driver_override names it, after the
 * stock driver is unbound; it never replaces or patches one.
 *
 * Locking: lem_blocks_sem is held for reading across each request and for
 * writing while a block goes away or the file closes, so a request never sees
 * half a block. User memory is only touched outside it. The interrupt state is
 * Rust's (rs/irq.rs, IRQ_LOCK), the only lock taken in the handler.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/clk.h>
#include <linux/dma-buf.h>
#include <linux/dma-mapping.h>
#include <linux/fs.h>
#include <linux/interrupt.h>
#include <linux/iommu.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/poll.h>
#include <linux/property.h>
#include <linux/rwsem.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/xarray.h>

#include "lemnos.h"
#include "lemnos_internal.h"

/*
 * With the Rust owner (kernel/lemnos_entry.rs), the descriptors below are global: the Rust half
 * registers them. Otherwise they are static, as before.
 */
#ifdef LEMNOS_RUST_OWNER
#define LEM_STATIC
#else
#define LEM_STATIC static
#endif
#include "lemnos_pisp_be.h"
#include "lemnos_gsdma.h"
#include "lemnos_vip.h"

#define LEM_NAME "lemnos"
#define LEM_MAX_COMPATIBLE 8
#define LEM_READ_BATCH 8
/* mmap offset, in pages: the block number above these bits, the region below. */
#define LEM_REGION_BITS 8
#define LEM_MAX_BLOCK ((u32)min_t(u64, U32_MAX, BIT_ULL(BITS_PER_LONG - PAGE_SHIFT - LEM_REGION_BITS) - 1))

/* The lists and the test switch are Rust's (rs/lists.rs): a device's place in one, by its number there. */
enum { LEM_LIST_OWNED, LEM_LIST_CONFINED, LEM_LIST_UNCONFINED, LEM_LIST_PISP_BE, LEM_LIST_GSDMA, LEM_LIST_VIP9000 };
bool lem_list_has(struct device *dev, u32 which);
bool lem_test_irq_inject(void);

struct lem_block;

/*
 * The record of one interrupt. Its fields are Rust's (rs/irq.rs, LemIrq): read and written under IRQ_LOCK, and
 * laid out as the mirror there, which asserts the same size.
 */
struct lem_irq {
	struct lem_irq *next;		/* the ready queue, in fire order */
	int irq;
	bool masked;
	bool queued;
	u32 block_id;			/* the block's number, once numbered */
	u32 index;
	u32 count;
	u64 ns;
};
_Static_assert(sizeof(struct lem_irq) == 40, "struct lem_irq mirrors LemIrq in rs/irq.rs");

/* One fired interrupt, as a read reports it: the mirror of LemIrqEvent. */
struct lem_irq_event {
	u32 block_id;
	u32 index;
	u32 count;
	u64 ns;
};
_Static_assert(sizeof(struct lem_irq_event) == 24, "struct lem_irq_event mirrors LemIrqEvent in rs/irq.rs");

/* The Rust side (rs/irq.rs): the lock, the ready queue and each interrupt's mask state. */
void lem_irq_setup(void);
void lem_irq_init(struct lem_irq *q, int irq, u32 index);
void lem_irq_set_block(struct lem_irq *q, u32 block_id);
void lem_irq_fired(struct lem_irq *q, u64 ns);
bool lem_irq_unmask(struct lem_irq *q);
bool lem_irq_mask(struct lem_irq *q);
void lem_irq_forget(struct lem_irq *q);
bool lem_irq_pop(struct lem_irq_event *e);
bool lem_irq_any(void);

struct lem_block {
	struct device *dev;
	struct platform_device *pdev;
	u32 id;
	bool mappable;
	bool unconfined;		/* mappable through unconfined= alone */
	u32 regions;
	u32 nirqs;
	struct lem_pb *pb;		/* the PiSP back end, or NULL */
	struct lem_gd *gd;		/* the K230 graphics DMA engine, or NULL */
	struct lem_rj *rj;		/* run through register jobs, or NULL */
	struct lem_vip *vip;		/* the VIP9000, or NULL */
	struct xarray imports;		/* handle -> an import (rs/dmabuf.rs) */
	atomic_t bufs;			/* DMA buffers not yet freed */
	struct lem_power *power;
	struct lem_irq irq[LEMNOS_MAX_IRQS];
};

static DECLARE_RWSEM(lem_blocks_sem);
static DEFINE_XARRAY_ALLOC1(lem_blocks);	/* number -> struct lem_block */
static u32 lem_next_id;
static struct address_space *lem_mapping;	/* the open file's; NULL when closed */
static atomic_t lem_opened = ATOMIC_INIT(0);
DECLARE_WAIT_QUEUE_HEAD(lem_wait);

/* --- interrupts --- */

static irqreturn_t lem_isr(int irq, void *data)
{
	(void)irq;
	lem_irq_fired(data, ktime_get_ns());
	wake_up_interruptible(&lem_wait);
	return IRQ_HANDLED;
}

static bool lem_any_ready(void)
{
	return lem_irq_any() || lem_event_pending();
}

static long lem_irq_enable(struct lem_block *l, u32 index)
{
	struct lem_irq *q;

	if (index >= l->nirqs)
		return -EINVAL;
	q = &l->irq[index];
	if (lem_irq_unmask(q))
		enable_irq(q->irq);
	return 0;
}

/* Masks every interrupt left enabled, as at bind, and forgets what was unread. */
static void lem_irq_mask_all(struct lem_block *l)
{
	for (u32 i = 0; i < l->nirqs; i++) {
		struct lem_irq *q = &l->irq[i];

		if (lem_irq_mask(q))
			disable_irq(q->irq);
	}
}

static ssize_t lem_read(struct file *file, char __user *ubuf, size_t len, loff_t *ppos)
{
	struct lemnos_event ev[LEM_READ_BATCH];
	size_t want = min_t(size_t, len / sizeof(ev[0]), ARRAY_SIZE(ev));
	size_t n = 0;
	int ret;

	if (!want)
		return -EINVAL;
	for (;;) {
		struct lem_irq_event e;

		while (n < want && lem_irq_pop(&e)) {
			ev[n++] = (struct lemnos_event){
				.block = e.block_id,
				.index = e.index,
				.count = e.count,
				.kind = LEMNOS_EVENT_IRQ,
				.ns = e.ns,
			};
		}
		while (n < want && lem_event_take(&ev[n]))
			n++;
		if (n)
			break;
		if (file->f_flags & O_NONBLOCK)
			return -EAGAIN;
		ret = wait_event_interruptible(lem_wait, lem_any_ready());
		if (ret)
			return ret;
	}
	if (copy_to_user(ubuf, ev, n * sizeof(ev[0])))
		return -EFAULT;
	return n * sizeof(ev[0]);
}

static __poll_t lem_poll(struct file *file, poll_table *wait)
{
	poll_wait(file, &lem_wait, wait);
	return lem_any_ready() ? EPOLLIN | EPOLLRDNORM : 0;
}

/* --- DMA memory: rs/dmabuf.rs --- */

long lem_alloc(struct device *dev, atomic_t *bufs, struct lemnos_alloc *a);
void lem_bufs_wait(struct device *dev, atomic_t *bufs);
long lem_import(struct device *dev, struct xarray *imports, struct lemnos_import *a);
long lem_release_import(struct xarray *imports, u32 handle);
void lem_imports_free_all(struct xarray *imports);

/* --- registers --- */

static loff_t lem_region_offset(u32 block, u32 index)
{
	return ((loff_t)block << LEM_REGION_BITS | index) << PAGE_SHIFT;
}

static long lem_region(struct lem_block *l, struct lemnos_region *r)
{
	struct resource *res;

	if (r->index >= l->regions)
		return -EINVAL;
	res = platform_get_resource(l->pdev, IORESOURCE_MEM, r->index);
	if (!res)
		return -EINVAL;
	r->start = res->start;
	r->size = resource_size(res);
	r->offset = lem_region_offset(l->id, r->index);
	return 0;
}

static int lem_map_region(struct lem_block *l, u32 index, struct vm_area_struct *vma)
{
	unsigned long len = vma->vm_end - vma->vm_start;
	struct resource *res;

	if (!l->mappable || !capable(CAP_SYS_RAWIO))
		return -EPERM;
	/* A private mapping would lose the offset the unbind finds it by. */
	if (!(vma->vm_flags & VM_SHARED) || index >= l->regions)
		return -EINVAL;
	res = platform_get_resource(l->pdev, IORESOURCE_MEM, index);
	if (!res || !PAGE_ALIGNED(res->start) || len > PAGE_ALIGN(resource_size(res)))
		return -EINVAL;
	vm_flags_set(vma, VM_IO | VM_DONTEXPAND | VM_DONTDUMP);
	vma->vm_page_prot = pgprot_noncached(vma->vm_page_prot);
	return io_remap_pfn_range(vma, vma->vm_start, res->start >> PAGE_SHIFT, len,
				  vma->vm_page_prot);
}

static int lem_mmap(struct file *file, struct vm_area_struct *vma)
{
	unsigned long block = vma->vm_pgoff >> LEM_REGION_BITS;
	u32 index = vma->vm_pgoff & (BIT(LEM_REGION_BITS) - 1);
	struct lem_block *l;
	int ret = -ENODEV;

	down_read(&lem_blocks_sem);
	l = xa_load(&lem_blocks, block);
	if (l)
		ret = lem_map_region(l, index, vma);
	up_read(&lem_blocks_sem);
	return ret;
}

/* --- the file --- */

/* Each request for a block starts with its number. */
union lem_args {
	u32 block;
	struct lemnos_next next;
	struct lemnos_info info;
	struct lemnos_region region;
	struct lemnos_alloc alloc;
	struct lemnos_import import;
	struct lemnos_release release;
	struct lemnos_irq irq;
	struct lemnos_claim claim;
	struct lemnos_stream_create stream;
	struct lemnos_pisp_be_create pisp_be;
	struct lemnos_pisp_be_version pisp_be_version;
	struct lemnos_gsdma_create gsdma;
	struct lemnos_regjob_open regjob;
	struct lemnos_vip_create vip;
	struct lemnos_i2c_transfer i2c;
	struct lemnos_i2c_smbus smbus;
	struct lemnos_spi_transfer spi;
	struct lemnos_spi_info spi_info;
	struct lemnos_gpio_request gpio_request;
	struct lemnos_gpio_config gpio_config;
	struct lemnos_gpio_value gpio_value;
	struct lemnos_pwm_request pwm_request;
	struct lemnos_pwm_state pwm_state;
	struct lemnos_power power;
	struct lemnos_thermal thermal;
	struct lemnos_thermal_watch thermal_watch;
	struct lemnos_pio_open pio_open;
	struct lemnos_pio_call pio_call;
	struct lemnos_pio_config_xfer pio_config;
	struct lemnos_pio_xfer pio_xfer;
};

static long lem_next(struct lemnos_next *n)
{
	unsigned long id = n->after;
	struct lem_block *l = xa_find_after(&lem_blocks, &id, ULONG_MAX, XA_PRESENT);

	if (!l)
		return -ENOENT;
	n->block = id;
	strscpy(n->name, dev_name(l->dev), sizeof(n->name));
	return 0;
}

static long lem_block_request(struct lem_block *l, unsigned int cmd, union lem_args *u)
{
	switch (cmd) {
	case LEMNOS_IOC_INFO:
		u->info = (struct lemnos_info){
			.block = l->id,
			.abi = LEMNOS_ABI,
			.flags = (l->mappable ? LEMNOS_INFO_MMIO_MAPPABLE : 0) |
				 (l->unconfined ? LEMNOS_INFO_MMIO_UNCONFINED : 0),
			.regions = l->regions,
			.irqs = l->nirqs,
			.dma_bits = fls64(dma_get_mask(l->dev)),
		};
		return 0;
	case LEMNOS_IOC_REGION:
		return lem_region(l, &u->region);
	case LEMNOS_IOC_ALLOC:
		return lem_alloc(l->dev, &l->bufs, &u->alloc);
	case LEMNOS_IOC_IMPORT:
		return lem_import(l->dev, &l->imports, &u->import);
	case LEMNOS_IOC_RELEASE:
		return lem_release_import(&l->imports, u->release.handle);
	case LEMNOS_IOC_IRQ_ENABLE:
		return lem_irq_enable(l, u->irq.index);
	case LEMNOS_IOC_STREAM_CREATE:
		return lem_stream_create(l->dev, &u->stream);
	case LEMNOS_IOC_PISP_BE_CREATE:
		if (!l->pb)
			return -ENOTTY;
		return lem_pb_create(l->pb, l->dev, &u->pisp_be);
	case LEMNOS_IOC_PISP_BE_VERSION:
		if (!l->pb)
			return -ENOTTY;
		return lem_pb_version(l->pb, &u->pisp_be_version);
	case LEMNOS_IOC_GSDMA_CREATE:
		if (!l->gd)
			return -ENOTTY;
		return lem_gd_create(l->gd, l->dev, &u->gsdma);
	case LEMNOS_IOC_REGJOB_OPEN:
		if (!l->rj)
			return -ENOTTY;
		return lem_rj_open(l->rj, &u->regjob);
	case LEMNOS_IOC_VIP_CREATE:
		if (!l->vip)
			return -ENOTTY;
		return lem_vip_create(l->vip, l->dev, &u->vip);
	case LEMNOS_IOC_POWER:
		/* A block the module drives keeps its own clocks. */
		if (l->pb || l->gd || l->rj || l->vip)
			return -ENOTTY;
		return lem_power_request(l->power, &u->power);
	case LEMNOS_IOC_TEST_IRQ:
		if (!lem_test_irq_inject())
			return -EPERM;
		if (u->irq.index >= l->nirqs)
			return -EINVAL;
		return irq_set_irqchip_state(l->irq[u->irq.index].irq, IRQCHIP_STATE_PENDING,
					     true);
	default:
		return -ENOTTY;
	}
}

/* Requests that don't name a block. */
static long lem_unit_request(unsigned int cmd, union lem_args *u)
{
	switch (cmd) {
	case LEMNOS_IOC_CLAIM:
		return lem_claim(&u->claim);
	case LEMNOS_IOC_I2C_TRANSFER:
		return lem_i2c_transfer(&u->i2c);
	case LEMNOS_IOC_I2C_SMBUS:
		return lem_i2c_smbus(&u->smbus);
	case LEMNOS_IOC_SPI_TRANSFER:
		return lem_spi_transfer(&u->spi);
	case LEMNOS_IOC_SPI_INFO:
		return lem_spi_info(&u->spi_info);
	case LEMNOS_IOC_GPIO_REQUEST:
		return lem_gpio_request(&u->gpio_request);
	case LEMNOS_IOC_GPIO_CONFIG:
		return lem_gpio_config(&u->gpio_config);
	case LEMNOS_IOC_GPIO_GET:
		return lem_gpio_get(&u->gpio_value);
	case LEMNOS_IOC_GPIO_SET:
		return lem_gpio_set(&u->gpio_value);
	case LEMNOS_IOC_PWM_REQUEST:
		return lem_pwm_request(&u->pwm_request);
	case LEMNOS_IOC_PWM_APPLY:
		return lem_pwm_apply(&u->pwm_state);
	case LEMNOS_IOC_PWM_GET:
		return lem_pwm_get(&u->pwm_state);
	case LEMNOS_IOC_PIO_OPEN:
		return lem_pio_open(&u->pio_open);
	case LEMNOS_IOC_PIO_CALL:
		return lem_pio_call(&u->pio_call);
	case LEMNOS_IOC_PIO_CONFIG_XFER:
		return lem_pio_config_xfer(&u->pio_config);
	case LEMNOS_IOC_PIO_XFER:
		return lem_pio_xfer(&u->pio_xfer);
	case LEMNOS_IOC_THERMAL_READ:
		return lem_thermal_read(&u->thermal);
	case LEMNOS_IOC_THERMAL_WATCH:
		return lem_thermal_watch(&u->thermal_watch);
	default:
		return -ENOIOCTLCMD;
	}
}

static long lem_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	void __user *uarg = (void __user *)arg;
	size_t size = _IOC_SIZE(cmd);
	union lem_args u = {};
	long ret;

	if (_IOC_TYPE(cmd) != 'L' || size > sizeof(u) || size < sizeof(u.block))
		return -ENOTTY;
	if ((_IOC_DIR(cmd) & _IOC_WRITE) && copy_from_user(&u, uarg, size))
		return -EFAULT;

	ret = lem_unit_request(cmd, &u);
	if (ret == -ENOIOCTLCMD) {
		down_read(&lem_blocks_sem);
		if (cmd == LEMNOS_IOC_NEXT) {
			ret = lem_next(&u.next);
		} else {
			struct lem_block *l = xa_load(&lem_blocks, u.block);

			ret = l ? lem_block_request(l, cmd, &u) : -ENODEV;
		}
		up_read(&lem_blocks_sem);
	}

	/* A failed copy leaves what was made to the close: the caller has lost it. */
	if (!ret && (_IOC_DIR(cmd) & _IOC_READ) && copy_to_user(uarg, &u, size))
		return -EFAULT;
	return ret;
}

static int lem_open(struct inode *inode, struct file *file)
{
	if (atomic_xchg(&lem_opened, 1))
		return -EBUSY;
	lem_events_reset();
	down_write(&lem_blocks_sem);
	lem_mapping = file->f_mapping;
	up_write(&lem_blocks_sem);
	return 0;
}

static int lem_release(struct inode *inode, struct file *file)
{
	struct lem_block *l;
	unsigned long id;

	down_write(&lem_blocks_sem);
	lem_mapping = NULL;
	xa_for_each(&lem_blocks, id, l) {
		/* The block may still be running what the driver set up: stop listening first. */
		lem_irq_mask_all(l);
		lem_imports_free_all(&l->imports);
	}
	up_write(&lem_blocks_sem);
	lem_events_reset();
	atomic_set(&lem_opened, 0);
	return 0;
}

static const struct file_operations lem_fops = {
	.owner = THIS_MODULE,
	.open = lem_open,
	.release = lem_release,
	.read = lem_read,
	.poll = lem_poll,
	.mmap = lem_mmap,
	.unlocked_ioctl = lem_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

LEM_STATIC struct miscdevice lem_misc = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = LEM_NAME,
	.fops = &lem_fops,
};

/* --- binding --- */

static int lem_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct lem_block *l;
	struct clk *clk;
	int ret;

	if (!lem_block_owned(dev))
		return dev_err_probe(dev, -ENODEV, "not a block Lemnos owns (see owned=)\n");
	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return -ENOMEM;
	l->dev = dev;
	l->pdev = pdev;
	xa_init_flags(&l->imports, XA_FLAGS_ALLOC1);
	atomic_set(&l->bufs, 0);
	platform_set_drvdata(pdev, l);

	while (l->regions < LEMNOS_MAX_REGIONS &&
	       platform_get_resource(pdev, IORESOURCE_MEM, l->regions))
		l->regions++;
	l->mappable = device_iommu_mapped(dev) && lem_list_has(dev, LEM_LIST_CONFINED);
	if (!l->mappable && lem_list_has(dev, LEM_LIST_UNCONFINED)) {
		dev_warn(dev, "unconfined: registers mappable though no IOMMU confines this block's DMA\n");
		l->mappable = true;
		l->unconfined = true;
	}

	/* The module drives this block itself: nothing of it reaches user space. */
	if (lem_list_has(dev, LEM_LIST_PISP_BE)) {
		l->mappable = false;
		l->unconfined = false;
		l->pb = lem_pb_probe(pdev);
		if (IS_ERR(l->pb))
			return dev_err_probe(dev, PTR_ERR(l->pb), "PiSP back end\n");
		goto ready;
	}
	if (lem_list_has(dev, LEM_LIST_GSDMA)) {
		l->mappable = false;
		l->unconfined = false;
		l->gd = lem_gd_probe(pdev);
		if (IS_ERR(l->gd))
			return dev_err_probe(dev, PTR_ERR(l->gd), "K230 graphics DMA engine\n");
		goto ready;
	}
	if (lem_list_has(dev, LEM_LIST_VIP9000)) {
		l->mappable = false;
		l->unconfined = false;
		l->vip = lem_vip_probe(pdev);
		if (IS_ERR(l->vip))
			return dev_err_probe(dev, PTR_ERR(l->vip), "VIP9000\n");
		goto ready;
	}
	l->rj = lem_rj_probe(pdev);
	if (IS_ERR(l->rj))
		return dev_err_probe(dev, PTR_ERR(l->rj), "register jobs\n");
	if (l->rj) {
		l->mappable = false;
		goto ready;
	}
	if (lem_test_irq_inject())
		dev_warn(dev, "test_irq_inject: interrupts can be raised from user space\n");

	/* Clocked while bound, as the stock driver keeps it. */
	clk = devm_clk_get_optional_enabled(dev, NULL);
	if (IS_ERR(clk))
		return dev_err_probe(dev, PTR_ERR(clk), "no clock\n");

	while (l->nirqs < LEMNOS_MAX_IRQS) {
		struct lem_irq *q = &l->irq[l->nirqs];
		int irq = platform_get_irq_optional(pdev, l->nirqs);

		if (irq <= 0)
			break;
		lem_irq_init(q, irq, l->nirqs);
		/* Not shared: a forwarded interrupt is masked until user space clears it. */
		ret = devm_request_irq(dev, irq, lem_isr, IRQF_NO_AUTOEN, dev_name(dev), q);
		if (ret)
			return ret;
		l->nirqs++;
	}

ready:
	l->power = lem_power_create(dev);
	if (IS_ERR(l->power)) {
		ret = PTR_ERR(l->power);
		goto unwind;
	}
	ret = xa_alloc_cyclic(&lem_blocks, &l->id, l, XA_LIMIT(1, LEM_MAX_BLOCK), &lem_next_id,
			      GFP_KERNEL);
	if (ret < 0) {
		lem_power_destroy(l->power);
		goto unwind;
	}
	for (u32 i = 0; i < l->nirqs; i++)
		lem_irq_set_block(&l->irq[i], l->id);
	lem_power_set_block(l->power, l->id);
	lem_event_post(LEMNOS_EVENT_BLOCK_BOUND, l->id, 0, 0);
	dev_info(dev, "block %u: %u regions, %u interrupts, %u-bit DMA, registers %smappable%s\n",
		 l->id, l->regions, l->nirqs, fls64(dma_get_mask(dev)),
		 l->mappable ? "" : "not ", l->unconfined ? " (unconfined)" : "");
	return 0;
unwind:
	/* No session can exist yet: the block was never published. */
	if (l->rj)
		lem_rj_remove(l->rj);
	return ret;
}

static void lem_remove(struct platform_device *pdev)
{
	struct lem_block *l = platform_get_drvdata(pdev);
	struct device *dev = &pdev->dev;

	down_write(&lem_blocks_sem);
	xa_erase(&lem_blocks, l->id);
	if (lem_mapping)
		unmap_mapping_range(lem_mapping, lem_region_offset(l->id, 0),
				    (loff_t)BIT(LEM_REGION_BITS) << PAGE_SHIFT, 1);
	for (u32 i = 0; i < l->nirqs; i++) {
		struct lem_irq *q = &l->irq[i];

		devm_free_irq(dev, q->irq, q);
		lem_irq_forget(q);
	}
	lem_imports_free_all(&l->imports);
	xa_destroy(&l->imports);
	/* A stream pins its dma-bufs mapped to this device. */
	lem_streams_block_gone(dev);
	/* After the streams: every job has stopped or the block is wedged. */
	if (l->pb)
		lem_pb_remove(l->pb);
	if (l->gd)
		lem_gd_remove(l->gd);
	if (l->rj)
		lem_rj_remove(l->rj);
	if (l->vip)
		lem_vip_remove(l->vip);
	lem_power_destroy(l->power);
	up_write(&lem_blocks_sem);
	lem_event_post(LEMNOS_EVENT_BLOCK_UNBOUND, l->id, 0, 0);

	/*
	 * A buffer can only be freed while the device still has its DMA setup,
	 * and mappings of it may exist in user space, so the unbind waits for
	 * them. Killing their holder ends the wait.
	 */
	lem_bufs_wait(dev, &l->bufs);
}

/* No match table: bound only through driver_override. */
LEM_STATIC struct platform_driver lem_driver = {
	.probe = lem_probe,
	.remove = lem_remove,
	.driver = {
		.name = LEM_NAME,
	},
};
