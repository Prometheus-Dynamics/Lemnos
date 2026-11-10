// SPDX-License-Identifier: GPL-2.0
/*
 * Claims: a name held exclusively by a file (see lemnos.h). The kernel ends
 * the claim when the file's last copy closes, so a holder that crashes can't
 * leave one behind.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/anon_inodes.h>
#include <linux/fcntl.h>
#include <linux/file.h>
#include <linux/fs.h>
#include <linux/hashtable.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/stringhash.h>

#include "lemnos_internal.h"

/* Each claim is kernel memory, so their number has an end. */
#define LEM_MAX_CLAIMS 4096
#define LEM_CLAIM_HASH_BITS 8

struct lem_held {
	struct hlist_node node;
	u32 hash;
	char resource[LEMNOS_NAME_MAX];
	void *data;
	const struct lem_claim_ops *ops;	/* set once, after data; NULL until */
};

static DEFINE_HASHTABLE(lem_held, LEM_CLAIM_HASH_BITS);
static DEFINE_MUTEX(lem_held_lock);
static u32 lem_held_count;

static void lem_held_remove(struct lem_held *h)
{
	mutex_lock(&lem_held_lock);
	hash_del(&h->node);
	lem_held_count--;
	mutex_unlock(&lem_held_lock);
	kfree(h);
}

static const struct lem_claim_ops *lem_held_ops(struct lem_held *h)
{
	return smp_load_acquire(&h->ops);
}

static int lem_claim_release(struct inode *inode, struct file *file)
{
	struct lem_held *h = file->private_data;
	const struct lem_claim_ops *ops = lem_held_ops(h);

	/* Ended before the name comes free, so the next holder finds it let go. */
	if (ops)
		ops->release(h->data);
	lem_held_remove(h);
	return 0;
}

static ssize_t lem_claim_read(struct file *file, char __user *buf, size_t len, loff_t *ppos)
{
	struct lem_held *h = file->private_data;
	const struct lem_claim_ops *ops = lem_held_ops(h);

	return ops && ops->read ? ops->read(h->data, file, buf, len) : -EINVAL;
}

static __poll_t lem_claim_poll(struct file *file, poll_table *wait)
{
	struct lem_held *h = file->private_data;
	const struct lem_claim_ops *ops = lem_held_ops(h);

	return ops && ops->poll ? ops->poll(h->data, file, wait) : 0;
}

static const struct file_operations lem_claim_fops = {
	.owner = THIS_MODULE,
	.release = lem_claim_release,
	.read = lem_claim_read,
	.poll = lem_claim_poll,
	.llseek = noop_llseek,
};

static bool lem_is_claim_on(struct file *file, const char *resource)
{
	return file && file->f_op == &lem_claim_fops &&
	       !strcmp(((struct lem_held *)file->private_data)->resource, resource);
}

bool lem_claim_held(int fd, const char *resource)
{
	CLASS(fd, f)(fd);

	return lem_is_claim_on(fd_file(f), resource);
}

int lem_claim_attach(int fd, const char *resource, const struct lem_claim_ops *ops, void *data)
{
	CLASS(fd, f)(fd);
	struct lem_held *h;
	int ret = 0;

	if (!lem_is_claim_on(fd_file(f), resource))
		return -EPERM;
	h = fd_file(f)->private_data;
	mutex_lock(&lem_held_lock);
	if (h->ops) {
		ret = -EBUSY;
	} else {
		h->data = data;
		smp_store_release(&h->ops, ops);
	}
	mutex_unlock(&lem_held_lock);
	return ret;
}

void *lem_claim_attached(struct file *file, const struct lem_claim_ops *ops)
{
	struct lem_held *h;

	if (!file || file->f_op != &lem_claim_fops)
		return NULL;
	h = file->private_data;
	return lem_held_ops(h) == ops ? h->data : NULL;
}

/* Refused while the name is held or the table is full. */
static int lem_held_insert(struct lem_held *h)
{
	struct lem_held *other;
	int ret = 0;

	mutex_lock(&lem_held_lock);
	hash_for_each_possible(lem_held, other, node, h->hash) {
		if (!strcmp(other->resource, h->resource)) {
			ret = -EBUSY;
			goto out;
		}
	}
	if (lem_held_count >= LEM_MAX_CLAIMS) {
		ret = -ENOSPC;
		goto out;
	}
	hash_add(lem_held, &h->node, h->hash);
	lem_held_count++;
out:
	mutex_unlock(&lem_held_lock);
	return ret;
}

long lem_claim(struct lemnos_claim *c)
{
	size_t len = strnlen(c->resource, sizeof(c->resource));
	struct lem_held *h;
	int fd, ret;

	if (c->flags || !len || len == sizeof(c->resource))
		return -EINVAL;
	h = kzalloc(sizeof(*h), GFP_KERNEL);
	if (!h)
		return -ENOMEM;
	memcpy(h->resource, c->resource, len);
	h->hash = full_name_hash(NULL, h->resource, len);
	ret = lem_held_insert(h);
	if (ret) {
		kfree(h);
		return ret;
	}
	fd = anon_inode_getfd("[lemnos-claim]", &lem_claim_fops, h, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		lem_held_remove(h);
		return fd;
	}
	c->fd = fd;
	return 0;
}
