/*
 * Checks the layouts and request numbers in drivers/k230isp-sys/src/uapi.rs against the C compiler.
 * Not part of the crate build: ./run.sh compiles it for riscv64 (the board) and for the host.
 *
 * The structures and requests are written out as vvcam_isp.h and vvcam_event.h declare them
 * (k230_linux_sdk at fc6cc21, buildroot-overlay/package/vvcam/isp/), with the asm-generic _IOC
 * encoding riscv uses. Compiling is the check: every assertion is static.
 */
#include <stddef.h>
#include <stdint.h>

#define IOC(dir, type, nr, size) (((dir) << 30) | ((type) << 8) | (nr) | ((size) << 16))
#define IOW(type, nr, t) IOC(1u, (type), (nr), sizeof(t))
#define IOR(type, nr, t) IOC(2u, (type), (nr), sizeof(t))
#define IOWR(type, nr, t) IOC(3u, (type), (nr), sizeof(t))

typedef struct {
	uint32_t addr;
	uint32_t value;
} vvcam_isp_reg_t;

typedef struct {
	uint32_t type;
	uint32_t id;
} vvcam_subscription_t;

typedef struct {
	uint32_t irq_value;
} vvcam_irqevent;

typedef struct {
	uint32_t type;
	uint32_t id;
	union {
		vvcam_irqevent irqevent;
	};
	uint64_t timestamp;
} vvcam_event_t;

_Static_assert(sizeof(vvcam_isp_reg_t) == 8, "RegPair");
_Static_assert(offsetof(vvcam_isp_reg_t, value) == 4, "RegPair.value");
_Static_assert(sizeof(vvcam_subscription_t) == 8, "Subscription");
_Static_assert(offsetof(vvcam_subscription_t, id) == 4, "Subscription.id");
#if UINTPTR_MAX == 0xffffffffffffffffu
_Static_assert(sizeof(vvcam_event_t) == 24, "Event");
_Static_assert(offsetof(vvcam_event_t, irqevent) == 8, "Event.irq_value");
_Static_assert(offsetof(vvcam_event_t, timestamp) == 16, "Event.timestamp");
_Static_assert(IOR('v', 0x06, vvcam_event_t) == 0x80187606u, "DQEVENT");
#endif
_Static_assert(IOW('v', 0x01, uint32_t) == 0x40047601u, "RESET");
_Static_assert(IOWR('v', 0x02, vvcam_isp_reg_t) == 0xC0087602u, "READ_REG");
_Static_assert(IOW('v', 0x03, vvcam_isp_reg_t) == 0x40087603u, "WRITE_REG");
_Static_assert(IOW('v', 0x04, vvcam_subscription_t) == 0x40087604u, "SUBSCRIBE_EVENT");
_Static_assert(IOW('v', 0x05, vvcam_subscription_t) == 0x40087605u, "UNSUBSCRIBE_EVENT");
