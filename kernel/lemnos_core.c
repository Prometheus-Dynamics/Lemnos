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
 * half a block. User memory is only touched outside it. lem_lock guards the
 * interrupt state and is the only lock taken in the handler.
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
#include "lemnos_pisp_be.h"

#define LEM_NAME "lemnos"
#define LEM_MAX_COMPATIBLE 8
#define LEM_READ_BATCH 8
/* mmap offset, in pages: the block number above these bits, the region below. */
#define LEM_REGION_BITS 8
#define LEM_MAX_BLOCK ((u32)min_t(u64, U32_MAX, BIT_ULL(BITS_PER_LONG - PAGE_SHIFT - LEM_REGION_BITS) - 1))

static char *owned = "";
module_param(owned, charp, 0444);
MODULE_PARM_DESC(owned,
		 "Compatibles of the blocks Lemnos owns, comma-separated in pairs "
		 "(vendor,name); no other device is claimed");

static char *confined = "";
module_param(confined, charp, 0444);
MODULE_PARM_DESC(confined,
		 "Compatibles of the blocks whose DMA their IOMMU really confines; "
		 "no other block's registers can be mapped");

static char *pisp_be = "";
module_param(pisp_be, charp, 0444);
MODULE_PARM_DESC(pisp_be,
		 "Compatibles of the blocks driven by the PiSP back end code; their "
		 "registers and interrupts never reach user space");

static bool test_map_unconfined;
module_param(test_map_unconfined, bool, 0444);
MODULE_PARM_DESC(test_map_unconfined,
		 "Test only: allow mapping the registers of a block whose DMA is not confined");

static bool test_irq_inject;
module_param(test_irq_inject, bool, 0444);
MODULE_PARM_DESC(test_irq_inject,
		 "Test only: LEMNOS_IOC_TEST_IRQ sets an interrupt pending at its controller");

struct lem_block;

struct lem_irq {
	struct list_head ready;		/* on lem_ready while fired and unread */
	struct lem_block *l;
	int irq;
	bool masked;
	u32 count;
	u64 ns;
};

struct lem_block {
	struct device *dev;
	struct platform_device *pdev;
	u32 id;
	bool mappable;
	u32 regions;
	u32 nirqs;
	struct lem_pb *pb;		/* the PiSP back end, or NULL */
	struct xarray imports;		/* handle -> struct lem_import */
	atomic_t bufs;			/* DMA buffers not yet freed */
	struct lem_irq irq[LEMNOS_MAX_IRQS];
};

/* An imported dma-buf. */
struct lem_import {
	struct dma_buf *buf;
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
};

static DECLARE_RWSEM(lem_blocks_sem);
static DEFINE_XARRAY_ALLOC1(lem_blocks);	/* number -> struct lem_block */
static u32 lem_next_id;
static struct address_space *lem_mapping;	/* the open file's; NULL when closed */
static atomic_t lem_opened = ATOMIC_INIT(0);
static DEFINE_SPINLOCK(lem_lock);		/* lem_ready, and every lem_irq */
static LIST_HEAD(lem_ready);
static DECLARE_WAIT_QUEUE_HEAD(lem_wait);
/* Global: the last buffer's free may race the block being freed. */
static DECLARE_WAIT_QUEUE_HEAD(lem_drained);

/* --- which blocks --- */

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

/* Whether any of the device's compatibles is in `list`, from any firmware description. */
static bool lem_device_listed(struct device *dev, const char *list)
{
	const char *c[LEM_MAX_COMPATIBLE];
	int n = device_property_read_string_array(dev, "compatible", c, ARRAY_SIZE(c));

	for (int i = 0; i < n; i++)
		if (lem_listed(list, c[i]))
			return true;
	return false;
}

bool lem_block_owned(struct device *dev)
{
	return lem_device_listed(dev, owned);
}

/* --- interrupts --- */

static irqreturn_t lem_isr(int irq, void *data)
{
	struct lem_irq *q = data;

	/* Level interrupts stay masked until the driver has cleared the cause. */
	disable_irq_nosync(irq);
	spin_lock(&lem_lock);
	q->masked = true;
	q->count++;
	q->ns = ktime_get_ns();
	if (list_empty(&q->ready))
		list_add_tail(&q->ready, &lem_ready);
	spin_unlock(&lem_lock);
	wake_up_interruptible(&lem_wait);
	return IRQ_HANDLED;
}

static bool lem_any_ready(void)
{
	unsigned long flags;
	bool any;

	spin_lock_irqsave(&lem_lock, flags);
	any = !list_empty(&lem_ready);
	spin_unlock_irqrestore(&lem_lock, flags);
	return any;
}

static long lem_irq_enable(struct lem_block *l, u32 index)
{
	struct lem_irq *q;
	unsigned long flags;
	bool unmask = false;

	if (index >= l->nirqs)
		return -EINVAL;
	q = &l->irq[index];
	spin_lock_irqsave(&lem_lock, flags);
	if (q->masked) {
		q->masked = false;
		unmask = true;
	}
	spin_unlock_irqrestore(&lem_lock, flags);
	if (unmask)
		enable_irq(q->irq);
	return 0;
}

/* Masks every interrupt left enabled, as at bind, and forgets what was unread. */
static void lem_irq_mask_all(struct lem_block *l)
{
	for (u32 i = 0; i < l->nirqs; i++) {
		struct lem_irq *q = &l->irq[i];
		unsigned long flags;
		bool mask;

		spin_lock_irqsave(&lem_lock, flags);
		mask = !q->masked;
		q->masked = true;
		list_del_init(&q->ready);
		spin_unlock_irqrestore(&lem_lock, flags);
		if (mask)
			disable_irq(q->irq);
	}
}

static ssize_t lem_read(struct file *file, char __user *ubuf, size_t len, loff_t *ppos)
{
	struct lemnos_irq_event ev[LEM_READ_BATCH];
	size_t want = min_t(size_t, len / sizeof(ev[0]), ARRAY_SIZE(ev));
	size_t n = 0;
	unsigned long flags;
	int ret;

	if (!want)
		return -EINVAL;
	for (;;) {
		struct lem_irq *q;

		spin_lock_irqsave(&lem_lock, flags);
		while (n < want &&
		       (q = list_first_entry_or_null(&lem_ready, struct lem_irq, ready))) {
			list_del_init(&q->ready);
			ev[n++] = (struct lemnos_irq_event){
				.block = q->l->id,
				.irq = q - q->l->irq,
				.count = q->count,
				.ns = q->ns,
			};
		}
		spin_unlock_irqrestore(&lem_lock, flags);
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

/* --- DMA buffers exported as dma-bufs --- */

struct lem_buf {
	struct lem_block *l;
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

/* The stream test source writes through this; nothing else in the kernel needs it. */
static int lem_buf_vmap(struct dma_buf *buf, struct iosys_map *map)
{
	struct lem_buf *b = buf->priv;

	iosys_map_set_vaddr(map, b->cpu);
	return 0;
}

static void lem_buf_free(struct lem_buf *b)
{
	struct lem_block *l = b->l;

	dma_free_coherent(b->dev, b->size, b->cpu, b->addr);
	put_device(b->dev);
	kfree(b);
	/* The last touch of l: once this reaches zero the unbind may free it. */
	if (atomic_dec_and_test(&l->bufs))
		wake_up_all(&lem_drained);
}

static void lem_buf_release(struct dma_buf *buf)
{
	lem_buf_free(buf->priv);
}

static const struct dma_buf_ops lem_buf_ops = {
	.map_dma_buf = lem_buf_map,
	.unmap_dma_buf = lem_buf_unmap,
	.mmap = lem_buf_mmap,
	.vmap = lem_buf_vmap,
	.release = lem_buf_release,
};

static long lem_alloc(struct lem_block *l, struct lemnos_alloc *a)
{
	DEFINE_DMA_BUF_EXPORT_INFO(info);
	struct dma_buf *buf;
	struct lem_buf *b;
	int fd;

	if (a->flags || !a->size || a->size > SZ_1G)
		return -EINVAL;
	b = kzalloc(sizeof(*b), GFP_KERNEL);
	if (!b)
		return -ENOMEM;
	b->l = l;
	b->dev = get_device(l->dev);
	b->size = PAGE_ALIGN(a->size);
	b->cpu = dma_alloc_coherent(b->dev, b->size, &b->addr, GFP_KERNEL);
	if (!b->cpu) {
		put_device(b->dev);
		kfree(b);
		return -ENOMEM;
	}
	atomic_inc(&l->bufs);
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
	a->size = b->size;
	a->fd = fd;
	a->addr = b->addr;
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

static void lem_imports_free_all(struct lem_block *l)
{
	struct lem_import *im;
	unsigned long handle;

	xa_for_each(&l->imports, handle, im) {
		xa_erase(&l->imports, handle);
		lem_import_free(im);
	}
}

static long lem_import(struct lem_block *l, struct lemnos_import *a)
{
	struct lem_import *im;
	struct scatterlist *sg;
	dma_addr_t next;
	u64 size = 0;
	u32 handle;
	int ret, i;

	im = kzalloc(sizeof(*im), GFP_KERNEL);
	if (!im)
		return -ENOMEM;
	im->buf = dma_buf_get(a->fd);
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
	ret = xa_alloc(&l->imports, &handle, im, xa_limit_32b, GFP_KERNEL);
	if (ret) {
		lem_import_free(im);
		return ret;
	}
	a->handle = handle;
	a->addr = sg_dma_address(im->sgt->sgl);
	a->size = size;
	return 0;
}

static long lem_release_import(struct lem_block *l, u32 handle)
{
	struct lem_import *im = xa_erase(&l->imports, handle);

	if (!im)
		return -ENOENT;
	lem_import_free(im);
	return 0;
}

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
			.flags = l->mappable ? LEMNOS_INFO_MMIO_MAPPABLE : 0,
			.regions = l->regions,
			.irqs = l->nirqs,
			.dma_bits = fls64(dma_get_mask(l->dev)),
		};
		return 0;
	case LEMNOS_IOC_REGION:
		return lem_region(l, &u->region);
	case LEMNOS_IOC_ALLOC:
		return lem_alloc(l, &u->alloc);
	case LEMNOS_IOC_IMPORT:
		return lem_import(l, &u->import);
	case LEMNOS_IOC_RELEASE:
		return lem_release_import(l, u->release.handle);
	case LEMNOS_IOC_IRQ_ENABLE:
		return lem_irq_enable(l, u->irq.index);
	case LEMNOS_IOC_STREAM_CREATE:
		return lem_stream_create(l->dev, &u->stream);
	case LEMNOS_IOC_PISP_BE_CREATE:
		if (!l->pb)
			return -ENOTTY;
		return lem_pb_create(l->pb, l->dev, &u->pisp_be);
	case LEMNOS_IOC_TEST_IRQ:
		if (!test_irq_inject)
			return -EPERM;
		if (u->irq.index >= l->nirqs)
			return -EINVAL;
		return irq_set_irqchip_state(l->irq[u->irq.index].irq, IRQCHIP_STATE_PENDING,
					     true);
	default:
		return -ENOTTY;
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

	if (cmd == LEMNOS_IOC_CLAIM) {
		ret = lem_claim(&u.claim);
	} else {
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
		lem_imports_free_all(l);
	}
	up_write(&lem_blocks_sem);
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

static struct miscdevice lem_misc = {
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

	if (!lem_device_listed(dev, owned))
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
	l->mappable = device_iommu_mapped(dev) && lem_device_listed(dev, confined);
	if (!l->mappable && test_map_unconfined) {
		dev_warn(dev, "test_map_unconfined: registers mappable on a block whose DMA is not confined\n");
		l->mappable = true;
	}

	/* The module drives this block itself: nothing of it reaches user space. */
	if (lem_device_listed(dev, pisp_be)) {
		l->mappable = false;
		l->pb = lem_pb_probe(pdev);
		if (IS_ERR(l->pb))
			return dev_err_probe(dev, PTR_ERR(l->pb), "PiSP back end\n");
		goto ready;
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
		INIT_LIST_HEAD(&q->ready);
		q->l = l;
		q->irq = irq;
		q->masked = true;
		/* Not shared: a forwarded interrupt is masked until user space clears it. */
		ret = devm_request_irq(dev, irq, lem_isr, IRQF_NO_AUTOEN, dev_name(dev), q);
		if (ret)
			return ret;
		l->nirqs++;
	}

ready:
	ret = xa_alloc_cyclic(&lem_blocks, &l->id, l, XA_LIMIT(1, LEM_MAX_BLOCK), &lem_next_id,
			      GFP_KERNEL);
	if (ret < 0)
		return ret;
	dev_info(dev, "block %u: %u regions, %u interrupts, %u-bit DMA, registers %smappable\n",
		 l->id, l->regions, l->nirqs, fls64(dma_get_mask(dev)),
		 l->mappable ? "" : "not ");
	return 0;
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
		unsigned long flags;

		devm_free_irq(dev, q->irq, q);
		spin_lock_irqsave(&lem_lock, flags);
		list_del_init(&q->ready);
		spin_unlock_irqrestore(&lem_lock, flags);
	}
	lem_imports_free_all(l);
	xa_destroy(&l->imports);
	/* A stream pins its dma-bufs mapped to this device. */
	lem_streams_block_gone(dev);
	/* After the streams: every job has stopped or the block is wedged. */
	if (l->pb)
		lem_pb_remove(l->pb);
	up_write(&lem_blocks_sem);

	/*
	 * A buffer can only be freed while the device still has its DMA setup,
	 * and mappings of it may exist in user space, so the unbind waits for
	 * them. Killing their holder ends the wait.
	 */
	while (!wait_event_timeout(lem_drained, !atomic_read(&l->bufs), 10 * HZ))
		dev_warn(dev, "unbind waits for %d DMA buffers still held\n",
			 atomic_read(&l->bufs));
}

/* No match table: bound only through driver_override. */
static struct platform_driver lem_driver = {
	.probe = lem_probe,
	.remove = lem_remove,
	.driver = {
		.name = LEM_NAME,
	},
};

static int __init lem_init(void)
{
	int ret = platform_driver_register(&lem_driver);

	if (ret)
		return ret;
	ret = lem_rp1_csi2_register();
	if (ret)
		goto driver;
	ret = misc_register(&lem_misc);
	if (ret)
		goto csi2;
	return 0;
csi2:
	lem_rp1_csi2_unregister();
driver:
	platform_driver_unregister(&lem_driver);
	return ret;
}

static void __exit lem_exit(void)
{
	misc_deregister(&lem_misc);
	lem_rp1_csi2_unregister();
	platform_driver_unregister(&lem_driver);
}

module_init(lem_init);
module_exit(lem_exit);

MODULE_DESCRIPTION("Lemnos: platform blocks for drivers in user space");
MODULE_LICENSE("GPL");
MODULE_IMPORT_NS("DMA_BUF");
