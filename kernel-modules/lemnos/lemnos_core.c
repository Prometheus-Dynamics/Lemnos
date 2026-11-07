// SPDX-License-Identifier: GPL-2.0
/*
 * lemnos: the one Lemnos kernel module. It claims a platform block through
 * driver_override and hands it to a driver in user space (see lemnos.h):
 * register regions, DMA buffers as dma-bufs, dma-bufs mapped to device
 * addresses, and interrupts forwarded as reads.
 *
 * Blocks whose DMA can reach any memory the job names, and whose jobs the
 * module must therefore write itself, are personalities in the same module
 * with their own uAPI: the PiSP back end (lemnos_pisp_be.c) and the RP1 CSI-2
 * receiver (lemnos_rp1_csi2.c).
 *
 * Only the blocks Lemnos owns are claimed: the "owned" parameter lists their
 * compatibles, and the generic driver refuses any other device. It has no
 * of_match_table, so it binds only where driver_override names it, after the
 * stock driver is unbound; it never replaces or patches one.
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
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/poll.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/xarray.h>

#include "lemnos.h"
#include "lemnos_personality.h"

#define LEM_NAME "lemnos"

static char *owned = "raspberrypi,pispbe,raspberrypi,rp1-cfe";
module_param(owned, charp, 0444);
MODULE_PARM_DESC(owned,
		 "Compatibles of the blocks Lemnos owns, comma-separated in pairs "
		 "(vendor,name); the generic driver claims no other device");

static bool test_map_unconfined;
module_param(test_map_unconfined, bool, 0444);
MODULE_PARM_DESC(test_map_unconfined,
		 "Test only: allow mapping the registers of a block whose DMA is not confined");

static bool test_irq_inject;
module_param(test_irq_inject, bool, 0444);
MODULE_PARM_DESC(test_irq_inject,
		 "Test only: LEMNOS_IOC_TEST_IRQ sets an interrupt pending at its controller");

/*
 * Blocks whose registers are never mapped to user space: their DMA reaches
 * memory the module did not map, so only the module may write their
 * addresses (docs/kernel-module.md).
 */
static const char *const unconfined[] = {
	"raspberrypi,pispbe",	/* its IOMMU passes every address below 40 GiB */
	"raspberrypi,rp1-cfe",	/* not shown confined: treated as not */
};

struct lem_irq {
	struct lem_dev *l;
	int irq;		/* 0: not forwarded */
	bool pending;		/* fired, not yet read */
	bool masked;
	u32 count;
	u64 ns;
};

struct lem_dev {
	struct device *dev;
	struct platform_device *pdev;
	struct miscdevice misc;
	char name[64];
	struct mutex open_lock;		/* one open file at a time */
	bool open;
	bool mappable;
	u32 regions;
	u32 nirqs;
	spinlock_t lock;		/* irq[] state */
	wait_queue_head_t wait;
	struct lem_irq irq[LEMNOS_MAX_IRQS];
};

/* An imported dma-buf, held by an open file. */
struct lem_import {
	struct dma_buf *buf;
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
};

struct lem_file {
	struct lem_dev *l;
	struct xarray imports;		/* handle -> struct lem_import */
};

/* --- the ownership check --- */

/* Whether `compatible` is one of the comma-separated pairs in `list`. */
static bool lem_listed(const char *list, const char *compatible)
{
	size_t n = strlen(compatible);
	const char *p = list;

	while (p && *p) {
		const char *comma = strchr(p, ',');
		const char *end = comma ? strchr(comma + 1, ',') : NULL;
		size_t len = end ? (size_t)(end - p) : strlen(p);

		if (len == n && !strncmp(p, compatible, n))
			return true;
		p = end ? end + 1 : NULL;
	}
	return false;
}

static bool lem_owned(struct device_node *np)
{
	const char *c;
	struct property *prop;

	of_property_for_each_string(np, "compatible", prop, c)
		if (lem_listed(owned, c))
			return true;
	return false;
}

static bool lem_unconfined(struct device_node *np)
{
	for (size_t i = 0; i < ARRAY_SIZE(unconfined); i++)
		if (of_device_is_compatible(np, unconfined[i]))
			return true;
	return false;
}

/* --- interrupts --- */

static irqreturn_t lem_isr(int irq, void *data)
{
	struct lem_irq *q = data;
	struct lem_dev *l = q->l;

	/* Level interrupts stay masked until the driver has cleared the cause. */
	disable_irq_nosync(irq);
	spin_lock(&l->lock);
	q->masked = true;
	q->pending = true;
	q->count++;
	q->ns = ktime_get_ns();
	spin_unlock(&l->lock);
	wake_up_interruptible(&l->wait);
	return IRQ_HANDLED;
}

static bool lem_any_pending(struct lem_dev *l)
{
	unsigned long flags;
	bool any = false;

	spin_lock_irqsave(&l->lock, flags);
	for (u32 i = 0; i < l->nirqs; i++)
		any |= l->irq[i].pending;
	spin_unlock_irqrestore(&l->lock, flags);
	return any;
}

static long lem_irq_enable(struct lem_dev *l, u32 index)
{
	struct lem_irq *q;
	unsigned long flags;
	bool unmask = false;

	if (index >= l->nirqs)
		return -EINVAL;
	q = &l->irq[index];
	spin_lock_irqsave(&l->lock, flags);
	if (q->masked) {
		q->masked = false;
		unmask = true;
	}
	spin_unlock_irqrestore(&l->lock, flags);
	if (unmask)
		enable_irq(q->irq);
	return 0;
}

/* Masks every interrupt left enabled, as at bind: for the next open. */
static void lem_irq_mask_all(struct lem_dev *l)
{
	for (u32 i = 0; i < l->nirqs; i++) {
		struct lem_irq *q = &l->irq[i];
		unsigned long flags;
		bool mask;

		spin_lock_irqsave(&l->lock, flags);
		mask = !q->masked;
		q->masked = true;
		q->pending = false;
		spin_unlock_irqrestore(&l->lock, flags);
		if (mask)
			disable_irq(q->irq);
	}
}

static ssize_t lem_read(struct file *file, char __user *ubuf, size_t len, loff_t *ppos)
{
	struct lem_file *f = file->private_data;
	struct lem_dev *l = f->l;
	struct lemnos_irq_event ev[LEMNOS_MAX_IRQS];
	size_t n = 0;
	unsigned long flags;
	int ret;

	if (len < sizeof(ev[0]))
		return -EINVAL;
	for (;;) {
		spin_lock_irqsave(&l->lock, flags);
		for (u32 i = 0; i < l->nirqs && (n + 1) * sizeof(ev[0]) <= len; i++) {
			struct lem_irq *q = &l->irq[i];

			if (!q->pending)
				continue;
			q->pending = false;
			ev[n].irq = i;
			ev[n].count = q->count;
			ev[n].ns = q->ns;
			n++;
		}
		spin_unlock_irqrestore(&l->lock, flags);
		if (n)
			break;
		if (file->f_flags & O_NONBLOCK)
			return -EAGAIN;
		ret = wait_event_interruptible(l->wait, lem_any_pending(l));
		if (ret)
			return ret;
	}
	if (copy_to_user(ubuf, ev, n * sizeof(ev[0])))
		return -EFAULT;
	return n * sizeof(ev[0]);
}

static __poll_t lem_poll(struct file *file, poll_table *wait)
{
	struct lem_file *f = file->private_data;

	poll_wait(file, &f->l->wait, wait);
	return lem_any_pending(f->l) ? EPOLLIN | EPOLLRDNORM : 0;
}

/* --- DMA buffers exported as dma-bufs --- */

struct lem_buf {
	struct device *dev;
	void *cpu;
	dma_addr_t addr;
	size_t size;
};

static struct sg_table *lem_buf_map(struct dma_buf_attachment *att,
				    enum dma_data_direction dir)
{
	struct lem_buf *b = att->dmabuf->priv;
	struct sg_table *sgt;
	int ret;

	sgt = kzalloc(sizeof(*sgt), GFP_KERNEL);
	if (!sgt)
		return ERR_PTR(-ENOMEM);
	ret = dma_get_sgtable(b->dev, sgt, b->cpu, b->addr, b->size);
	if (ret)
		goto free;
	ret = dma_map_sgtable(att->dev, sgt, dir, 0);
	if (ret)
		goto table;
	return sgt;
table:
	sg_free_table(sgt);
free:
	kfree(sgt);
	return ERR_PTR(ret);
}

static void lem_buf_unmap(struct dma_buf_attachment *att, struct sg_table *sgt,
			  enum dma_data_direction dir)
{
	dma_unmap_sgtable(att->dev, sgt, dir, 0);
	sg_free_table(sgt);
	kfree(sgt);
}

static int lem_buf_mmap(struct dma_buf *buf, struct vm_area_struct *vma)
{
	struct lem_buf *b = buf->priv;

	return dma_mmap_coherent(b->dev, vma, b->cpu, b->addr, b->size);
}

static void lem_buf_free(struct lem_buf *b)
{
	dma_free_coherent(b->dev, b->size, b->cpu, b->addr);
	put_device(b->dev);
	kfree(b);
}

static void lem_buf_release(struct dma_buf *buf)
{
	lem_buf_free(buf->priv);
}

static const struct dma_buf_ops lem_buf_ops = {
	.map_dma_buf = lem_buf_map,
	.unmap_dma_buf = lem_buf_unmap,
	.mmap = lem_buf_mmap,
	.release = lem_buf_release,
};

static long lem_alloc(struct lem_dev *l, struct lemnos_alloc __user *uarg)
{
	DEFINE_DMA_BUF_EXPORT_INFO(info);
	struct lemnos_alloc a;
	struct dma_buf *buf;
	struct lem_buf *b;
	int fd;

	if (copy_from_user(&a, uarg, sizeof(a)))
		return -EFAULT;
	if (a.flags || !a.size || a.size > SZ_1G)
		return -EINVAL;
	b = kzalloc(sizeof(*b), GFP_KERNEL);
	if (!b)
		return -ENOMEM;
	b->dev = get_device(l->dev);
	b->size = PAGE_ALIGN(a.size);
	b->cpu = dma_alloc_coherent(b->dev, b->size, &b->addr, GFP_KERNEL);
	if (!b->cpu) {
		put_device(b->dev);
		kfree(b);
		return -ENOMEM;
	}
	info.exp_name = LEM_NAME;
	info.owner = THIS_MODULE;
	info.ops = &lem_buf_ops;
	info.size = b->size;
	info.flags = O_RDWR;
	info.priv = b;
	buf = dma_buf_export(&info);
	if (IS_ERR(buf)) {
		lem_buf_free(b);
		return PTR_ERR(buf);
	}
	fd = dma_buf_fd(buf, O_CLOEXEC);
	if (fd < 0) {
		dma_buf_put(buf);	/* releases b */
		return fd;
	}
	a.fd = fd;
	a.addr = b->addr;
	if (copy_to_user(uarg, &a, sizeof(a)))
		return -EFAULT;		/* the fd stays the caller's */
	return 0;
}

/* --- dma-bufs imported to device addresses --- */

static void lem_import_free(struct lem_import *im)
{
	dma_buf_unmap_attachment_unlocked(im->att, im->sgt, DMA_BIDIRECTIONAL);
	dma_buf_detach(im->buf, im->att);
	dma_buf_put(im->buf);
	kfree(im);
}

static long lem_import(struct lem_file *f, struct lemnos_import __user *uarg)
{
	struct lem_dev *l = f->l;
	struct lemnos_import a;
	struct lem_import *im;
	struct scatterlist *sg;
	dma_addr_t next;
	u64 size = 0;
	u32 handle;
	int ret, i;

	if (copy_from_user(&a, uarg, sizeof(a)))
		return -EFAULT;
	im = kzalloc(sizeof(*im), GFP_KERNEL);
	if (!im)
		return -ENOMEM;
	im->buf = dma_buf_get(a.fd);
	if (IS_ERR(im->buf)) {
		ret = PTR_ERR(im->buf);
		kfree(im);
		return ret;
	}
	im->att = dma_buf_attach(im->buf, l->dev);
	if (IS_ERR(im->att)) {
		ret = PTR_ERR(im->att);
		dma_buf_put(im->buf);
		kfree(im);
		return ret;
	}
	im->sgt = dma_buf_map_attachment_unlocked(im->att, DMA_BIDIRECTIONAL);
	if (IS_ERR(im->sgt)) {
		ret = PTR_ERR(im->sgt);
		dma_buf_detach(im->buf, im->att);
		dma_buf_put(im->buf);
		kfree(im);
		return ret;
	}
	/* One contiguous range in the block's address space, or refused. */
	next = sg_dma_address(im->sgt->sgl);
	for_each_sgtable_dma_sg(im->sgt, sg, i) {
		if (sg_dma_address(sg) != next) {
			lem_import_free(im);
			return -EINVAL;
		}
		next += sg_dma_len(sg);
		size += sg_dma_len(sg);
	}
	ret = xa_alloc(&f->imports, &handle, im, xa_limit_32b, GFP_KERNEL);
	if (ret) {
		lem_import_free(im);
		return ret;
	}
	a.handle = handle;
	a.addr = sg_dma_address(im->sgt->sgl);
	a.size = size;
	if (copy_to_user(uarg, &a, sizeof(a))) {
		xa_erase(&f->imports, handle);
		lem_import_free(im);
		return -EFAULT;
	}
	return 0;
}

static long lem_release_import(struct lem_file *f, u32 __user *uarg)
{
	struct lem_import *im;
	u32 handle;

	if (get_user(handle, uarg))
		return -EFAULT;
	im = xa_erase(&f->imports, handle);
	if (!im)
		return -ENOENT;
	lem_import_free(im);
	return 0;
}

/* --- registers --- */

static long lem_region(struct lem_dev *l, struct lemnos_region __user *uarg)
{
	struct lemnos_region r;
	struct resource *res;

	if (copy_from_user(&r, uarg, sizeof(r)))
		return -EFAULT;
	if (r.index >= l->regions)
		return -EINVAL;
	res = platform_get_resource(l->pdev, IORESOURCE_MEM, r.index);
	if (!res)
		return -EINVAL;
	r.reserved = 0;
	r.start = res->start;
	r.size = resource_size(res);
	return copy_to_user(uarg, &r, sizeof(r)) ? -EFAULT : 0;
}

static int lem_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct lem_file *f = file->private_data;
	struct lem_dev *l = f->l;
	unsigned long len = vma->vm_end - vma->vm_start;
	struct resource *res;

	if (!l->mappable || !capable(CAP_SYS_RAWIO))
		return -EPERM;
	if (vma->vm_pgoff >= l->regions)
		return -EINVAL;
	res = platform_get_resource(l->pdev, IORESOURCE_MEM, vma->vm_pgoff);
	if (!res || !PAGE_ALIGNED(res->start) || len > PAGE_ALIGN(resource_size(res)))
		return -EINVAL;
	vm_flags_set(vma, VM_IO | VM_DONTEXPAND | VM_DONTDUMP);
	vma->vm_page_prot = pgprot_noncached(vma->vm_page_prot);
	return io_remap_pfn_range(vma, vma->vm_start, res->start >> PAGE_SHIFT, len,
				  vma->vm_page_prot);
}

/* --- the file --- */

static long lem_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct lem_file *f = file->private_data;
	struct lem_dev *l = f->l;
	void __user *uarg = (void __user *)arg;

	switch (cmd) {
	case LEMNOS_IOC_INFO: {
		struct lemnos_info info = {
			.abi = LEMNOS_ABI,
			.flags = l->mappable ? LEMNOS_INFO_MMIO_MAPPABLE : 0,
			.regions = l->regions,
			.irqs = l->nirqs,
			.dma_bits = fls64(dma_get_mask(l->dev)),
		};

		return copy_to_user(uarg, &info, sizeof(info)) ? -EFAULT : 0;
	}
	case LEMNOS_IOC_REGION:
		return lem_region(l, uarg);
	case LEMNOS_IOC_ALLOC:
		return lem_alloc(l, uarg);
	case LEMNOS_IOC_IMPORT:
		return lem_import(f, uarg);
	case LEMNOS_IOC_RELEASE:
		return lem_release_import(f, uarg);
	case LEMNOS_IOC_IRQ_ENABLE: {
		u32 index;

		if (get_user(index, (u32 __user *)uarg))
			return -EFAULT;
		return lem_irq_enable(l, index);
	}
	case LEMNOS_IOC_TEST_IRQ: {
		u32 index;

		if (!test_irq_inject)
			return -EPERM;
		if (get_user(index, (u32 __user *)uarg))
			return -EFAULT;
		if (index >= l->nirqs)
			return -EINVAL;
		return irq_set_irqchip_state(l->irq[index].irq, IRQCHIP_STATE_PENDING, true);
	}
	default:
		return -ENOTTY;
	}
}

static int lem_open(struct inode *inode, struct file *file)
{
	struct lem_dev *l = container_of(file->private_data, struct lem_dev, misc);
	struct lem_file *f;

	f = kzalloc(sizeof(*f), GFP_KERNEL);
	if (!f)
		return -ENOMEM;
	mutex_lock(&l->open_lock);
	if (l->open) {
		mutex_unlock(&l->open_lock);
		kfree(f);
		return -EBUSY;
	}
	l->open = true;
	mutex_unlock(&l->open_lock);
	f->l = l;
	xa_init_flags(&f->imports, XA_FLAGS_ALLOC1);
	file->private_data = f;
	return 0;
}

static int lem_release(struct inode *inode, struct file *file)
{
	struct lem_file *f = file->private_data;
	struct lem_dev *l = f->l;
	struct lem_import *im;
	unsigned long handle;

	/* The block may still be running what the driver set up: stop listening first. */
	lem_irq_mask_all(l);
	xa_for_each(&f->imports, handle, im)
		lem_import_free(im);
	xa_destroy(&f->imports);
	kfree(f);
	mutex_lock(&l->open_lock);
	l->open = false;
	mutex_unlock(&l->open_lock);
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

/* --- binding --- */

static int lem_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct lem_dev *l;
	struct clk *clk;
	int ret;

	if (!dev->of_node || !lem_owned(dev->of_node))
		return dev_err_probe(dev, -ENODEV, "not a block Lemnos owns (see owned=)\n");
	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return -ENOMEM;
	l->dev = dev;
	l->pdev = pdev;
	mutex_init(&l->open_lock);
	spin_lock_init(&l->lock);
	init_waitqueue_head(&l->wait);
	platform_set_drvdata(pdev, l);

	while (l->regions < LEMNOS_MAX_REGIONS &&
	       platform_get_resource(pdev, IORESOURCE_MEM, l->regions))
		l->regions++;
	l->mappable = device_iommu_mapped(dev) && !lem_unconfined(dev->of_node);
	if (!l->mappable && test_map_unconfined) {
		dev_warn(dev, "test_map_unconfined: registers mappable on a block whose DMA is not confined\n");
		l->mappable = true;
	}
	if (test_irq_inject)
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
		q->l = l;
		q->irq = irq;
		q->masked = true;
		/* Not shared: a forwarded interrupt is masked until user space clears it. */
		ret = devm_request_irq(dev, irq, lem_isr, IRQF_NO_AUTOEN, dev_name(dev), q);
		if (ret)
			return ret;
		l->nirqs++;
	}

	snprintf(l->name, sizeof(l->name), "%s-%s", LEM_NAME, dev_name(dev));
	l->misc.minor = MISC_DYNAMIC_MINOR;
	l->misc.name = l->name;
	l->misc.fops = &lem_fops;
	l->misc.parent = dev;
	ret = misc_register(&l->misc);
	if (ret)
		return ret;
	dev_info(dev, "%u regions, %u interrupts, %u-bit DMA, registers %smappable, on /dev/%s\n",
		 l->regions, l->nirqs, fls64(dma_get_mask(dev)), l->mappable ? "" : "not ",
		 l->name);
	return 0;
}

static void lem_remove(struct platform_device *pdev)
{
	struct lem_dev *l = platform_get_drvdata(pdev);

	misc_deregister(&l->misc);
}

/* No of_match_table: bound only through driver_override. */
static struct platform_driver lem_driver = {
	.probe = lem_probe,
	.remove = lem_remove,
	.driver = {
		.name = LEM_NAME,
	},
};

static struct platform_driver *const lem_drivers[] = {
	&lem_driver,
	&lemnos_pisp_be_driver,
	&lemnos_rp1_csi2_driver,
};

static int __init lem_init(void)
{
	return platform_register_drivers(lem_drivers, ARRAY_SIZE(lem_drivers));
}

static void __exit lem_exit(void)
{
	platform_unregister_drivers(lem_drivers, ARRAY_SIZE(lem_drivers));
}

module_init(lem_init);
module_exit(lem_exit);

MODULE_DESCRIPTION("Lemnos: platform blocks for drivers in user space");
MODULE_LICENSE("GPL");
MODULE_IMPORT_NS("DMA_BUF");
