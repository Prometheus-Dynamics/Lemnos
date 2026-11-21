# CMA-READ: CMA reading on Raze #2 after boot (read-only, 2026-11-21)

Board: Raze #2 (CM5 Rev 1.0), serial a317bcbee5226d57, boot b34fee0e-2dc9-4da1-a77f-791d01632baf, kernel 7.2.9-v8-16k.
Rev: origin/next 0d37cc55. Board locks held by flock over ssh for the read. Read-only.

Purpose: the standby and fragmentation trade-off in Atlas's note (marked unmeasured). Source: REMAINING.md "Board-run
item (next Raze session, read-only): CMA reading after boot and after a day".

## Hypothesis (written before the read)

After boot, CMA has most of its pages free, and the pagetypeinfo CMA column holds some order-9 and higher blocks.

## Reads (uptime 14476 s, about 4 h)

```
CmaTotal:          65536 kB
CmaFree:           48308 kB
```

`/proc/pagetypeinfo`, CMA column, node 1 zone DMA, free blocks per order 0..10:
`9 10 8 6 0 0 1 1 0 1 11`. Order 9 has 1 block and order 10 has 11, so 12 CMA blocks of order 9 or more are free.
The other nodes have no CMA blocks.

## Result

Read at boot plus about 4 h. The "after a day" reading cannot be taken today: the boot is under a day old. It stays
open for a later read on this boot, after its uptime reaches 24 h. The CMA figures above are the "after boot" half.

Board state: unchanged.
