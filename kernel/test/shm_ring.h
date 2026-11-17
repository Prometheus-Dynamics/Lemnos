/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The consumer's side of a module frame stream, for the module checks: the word numbers of the
 * shared-memory ring (shm/README.md, layout 2) and of the job block that follows the pool page.
 * Every number here is the module's (kernel/lemnos_stream.c), and the Rust crates pin the same
 * ring. A frame is found through the index, read once and checked against its stamp; a buffer is
 * given back by its marks, not by a ring word, so a consumer that holds nothing gets its buffer
 * back as soon as the module takes it.
 */
#ifndef LEMNOS_SHM_RING_H
#define LEMNOS_SHM_RING_H

#include <fcntl.h>
#include <linux/memfd.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "../lemnos.h"

/* The seal calls, for a C library that does not declare them without _GNU_SOURCE. */
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_SEAL_SEAL 0x0001
#define F_SEAL_SHRINK 0x0002
#define F_SEAL_GROW 0x0004
#endif

#define SHM_W_HEAD	8	/* frames published, ever */
#define SHM_W_END	10	/* the end word: LEMNOS_RING_STATE_* */
#define SHM_LANE_HEAD	16	/* the one field's lane head */
#define SHM_INDEX	17	/* frame_slots index words, from here */
#define SHM_SLOT_WORDS	10	/* stamp, time, meta, lane sequence, then the 48-byte entry */
#define SHM_ENTRY	4	/* the entry's first word within a slot */
#define SHM_TICKETS	8	/* the pool page: one ticket word per buffer, from its start + 8 */
#define SHM_READERS	64	/* the pool page's cut-off words after the tickets */

/* The word the pool page starts at: after the frame ring. */
static inline size_t shm_page_at(uint32_t frame_slots)
{
	return SHM_INDEX + frame_slots + (size_t)frame_slots * SHM_SLOT_WORDS;
}

/* The control block: after the pool page's tickets and cut-off words. */
static inline size_t shm_control(uint32_t frame_slots, uint32_t buffers)
{
	return shm_page_at(frame_slots) + SHM_TICKETS + buffers + SHM_READERS;
}

#define SHM_JOB_HEAD 2 /* jobs posted (the consumer's), from the control block */
#define SHM_JOB_TAIL 3 /* jobs taken (the module's) */
#define SHM_JOB_SLOTS 4 /* job slots from here: each 1 + job_bytes / 8 words, the length first */

/* The index entry's slot number for frame `seq`: the lane slot it was published in. */
static inline size_t shm_slot_of(const volatile uint64_t *r, uint32_t frame_slots, uint64_t seq)
{
	return (size_t)(r[SHM_INDEX + (seq & (frame_slots - 1))] >> 16);
}

/* A frame the module published, read by its index and checked against its stamp (read twice). */
struct shm_frame {
	uint32_t buffer, bytes, fence;
	uint64_t sequence, identity, timestamp_ns, end_ns;
};

/*
 * Frame `seq`, if the module has published it: 1 with *f filled, 0 while it is not out yet, and -1
 * when its slot holds another frame (the module reused the slot: the consumer was too slow).
 */
static inline int shm_frame_read(const volatile uint64_t *ring, uint32_t frame_slots, uint64_t seq,
				 struct shm_frame *f)
{
	if (__atomic_load_n(&ring[SHM_W_HEAD], __ATOMIC_ACQUIRE) <= seq)
		return 0;
	size_t slot = (size_t)(__atomic_load_n(&ring[SHM_INDEX + (seq & (frame_slots - 1))], __ATOMIC_ACQUIRE) >> 16);
	size_t at = SHM_INDEX + frame_slots + slot * SHM_SLOT_WORDS;
	uint64_t stamp = __atomic_load_n(&ring[at], __ATOMIC_ACQUIRE);
	uint64_t pair, fence, end, time, identity, sequence_word;

	if (stamp != (seq + 1) << 1)
		return -1;
	time = __atomic_load_n(&ring[at + 1], __ATOMIC_ACQUIRE);
	pair = __atomic_load_n(&ring[at + SHM_ENTRY + 1], __ATOMIC_ACQUIRE);
	sequence_word = __atomic_load_n(&ring[at + SHM_ENTRY + 2], __ATOMIC_ACQUIRE);
	identity = __atomic_load_n(&ring[at + SHM_ENTRY + 3], __ATOMIC_ACQUIRE);
	fence = __atomic_load_n(&ring[at + SHM_ENTRY + 4], __ATOMIC_ACQUIRE);
	end = __atomic_load_n(&ring[at + SHM_ENTRY + 5], __ATOMIC_ACQUIRE);
	if (__atomic_load_n(&ring[at], __ATOMIC_ACQUIRE) != stamp)
		return -1;
	f->buffer = (uint32_t)pair;
	f->bytes = (uint32_t)fence;
	f->fence = (uint32_t)(fence >> 32);
	f->sequence = sequence_word;
	f->identity = identity;
	f->timestamp_ns = time;
	f->end_ns = end;
	return 1;
}

/*
 * A consumer's marks: its page of hold marks (shm/ring-holds' layout 2), made on first use and
 * registered with the stream. Word 0 is the layout (2) in its top byte and the version below it;
 * word 1 + b is buffer b's mark, nonzero while the consumer holds it. The module reads the page
 * read-only and reuses a buffer only when no reader marks it.
 */
struct shm_marks {
	int fd, reader_fd;
	uint64_t *page;
	uint32_t buffers;
	uint64_t version;
};

static inline void shm_marks_init(struct shm_marks *m)
{
	m->fd = m->reader_fd = -1;
	m->page = NULL;
	m->buffers = 0;
	m->version = 0;
}

static inline void shm_marks_close(struct shm_marks *m)
{
	if (m->page)
		munmap(m->page, (1 + 2 * (size_t)m->buffers) * 8);
	if (m->reader_fd >= 0)
		close(m->reader_fd);
	if (m->fd >= 0)
		close(m->fd);
	shm_marks_init(m);
}

/* Makes the page and registers it on `stream`. 0, or -1 (nothing made). */
static inline int shm_marks_open(struct shm_marks *m, int stream, uint32_t buffers)
{
	struct lemnos_stream_reader r;
	size_t bytes = (1 + 2 * (size_t)buffers) * 8;

	shm_marks_init(m);
	m->buffers = buffers;
	m->fd = (int)syscall(__NR_memfd_create, "lemnos-marks", MFD_CLOEXEC | MFD_ALLOW_SEALING);
	if (m->fd < 0 || ftruncate(m->fd, (off_t)bytes) ||
	    fcntl(m->fd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL))
		goto fail;
	m->page = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, m->fd, 0);
	if (m->page == MAP_FAILED) {
		m->page = NULL;
		goto fail;
	}
	m->page[0] = 2ull << 56;
	memset(&r, 0, sizeof(r));
	r.fd = m->fd;
	if (ioctl(stream, LEMNOS_STREAM_IOC_READER, &r))
		goto fail;
	m->reader_fd = r.reader_fd;
	return 0;
fail:
	shm_marks_close(m);
	return -1;
}

/* Writes one mark: the version is odd while it changes, and moves on after, as shm/ring-holds does. */
/*
 * The consumer read the frame with ring sequence `seq` from `buffer`: its taken stamp, which is one
 * more than the sequence (1 + buffers + buffer, shm-ring-holds' taken_at).
 */
static inline void shm_marks_taken(struct shm_marks *m, uint32_t buffer, uint64_t seq)
{
	__atomic_store_n(&m->page[1 + m->buffers + buffer], seq + 1, __ATOMIC_SEQ_CST);
}

static inline void shm_marks_set(struct shm_marks *m, uint32_t buffer, uint64_t mark)
{
	const uint64_t mask = (1ull << 56) - 1;

	m->version = (m->version + 1) & mask;
	__atomic_store_n(&m->page[0], 2ull << 56 | m->version, __ATOMIC_SEQ_CST);
	__atomic_store_n(&m->page[1 + buffer], mark, __ATOMIC_SEQ_CST);
	m->version = (m->version + 1) & mask;
	__atomic_store_n(&m->page[0], 2ull << 56 | m->version, __ATOMIC_SEQ_CST);
}

#endif
