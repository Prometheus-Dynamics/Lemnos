# lemnos-flash-wire

The wire of the flash link: the request and reply frames, the CRC, the reset kinds and the identity
record that a stage answers with. It is shared by the PC-side flasher and the on-chip stages it loads.
no_std, no dependencies, no `unsafe`.

The PC side is another project. It pins this crate by a locked revision (see "How the PC side pins
it"). Nothing in this crate knows which chip is flashed.

## Layout

All sizes are bytes. Integers are little-endian unless marked `be`.

**Request**, 12 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | magic `LM` |
| 2 | 1 | op: 1 read, 2 write, 3 end, 4 identify |
| 3 | 1 | reserved, meaning per op (below) |
| 4 | 2 | count, 1 to 128 sectors |
| 6 | 4 | lba, first sector |
| 10 | 2 be | CRC-16/CCITT-FALSE over bytes 0 to 9 |

Reserved byte, by op: read and identify take 0; write takes `1` (the compared mark) or 0, and any
other bit is refused; end takes the reset kind (0 normal, 3 maskrom).

**Chunk** (host to stage, after a write request): `count * 512` payload bytes, then CRC-16 (be) over the payload.

**Read reply** (stage to host): `count * 512` bytes, then CRC-16 (be) over them.

**Write, end replies**: one status byte: 0 ok, 1 crc, 2 storage, 3 verify, 4 not compared.

**Identify reply**: the identity record (below).

**Identity record**:

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | magic `LI` |
| 2 | 2 | length of the whole record, head, entries and trailing bytes |
| 4 | 1 | count of entries |
| 5 | 9 each | family (1 byte), hash (8 bytes, little-endian) |

This build lists five families: 1 frame, 2 ops, 3 status, 4 reset, 5 identity.

## Hash

Each family has a 64-bit FNV-1a, computed at compile time (`src/layout.rs`) from the declarations in
the code: the constants, the ops and the statuses themselves, not a number kept by hand. A change to any
declared value moves the hash of its family, and so the wire hash. `WIRE_HASH` folds the family ids and
hashes in order. `WIRE_SHORT` is a 16-bit number derived from `WIRE_HASH` for naming an image or a log
line; it identifies nothing on its own.

The values this build computes:

| Name | Value |
|---|---|
| wire hash | `0x2DED_8934_37C3_D947` |
| wire short | `19037` |
| frame family (1) | `0x7E49_7C0D_EC42_79FF` |
| ops family (2) | `0x89BF_43BE_BC6E_C950` |
| status family (3) | `0x246D_E5A0_33B0_58C9` |
| reset family (4) | `0x40FE_F3FE_A154_445F` |
| identity family (5) | `0xB71D_E980_887B_B9AD` |

The tests pin these values (`src/layout.rs`), so a layout change fails the build until the pins change
in the same commit. The pins were also recomputed outside the crate by a separate FNV implementation over
the same byte walk, and they agree.

## Rules

- **Compatibility is structural.** A version number is never compared. A peer agrees with this crate
  when it lists each family with the same hash.
- **Additive changes.** A reader skips trailing bytes it does not know: the record's length covers them,
  so a field appended to the end of the identity record moves no hash. A family a peer lists that this
  build does not know is ignored, so a peer with an extra family still connects.
- **Skew is a typed refusal.** `identity::check` returns `Skew::Malformed`, `Skew::Missing { family }`
  or `Skew::Hash { family, ours, theirs }`. Nothing is guessed from a mismatch.
- **Fixed-size frames do not grow.** A request is 12 bytes and a reply has a fixed shape. A new op, a new
  status, or a new reserved bit changes its family's declaration and so its hash; it is a skew, not a
  silent extension.
- **Reset kinds.** Only 3 is the maskrom reset. Any other reserved byte on an END is a normal reset, as
  the first stage has always read it. A refused or unknown reserved bit on any other op is a
  `FrameError::Reserved`.
- **CRC.** CRC-16/CCITT-FALSE: polynomial 0x1021, initial 0xFFFF, no reflection, no final XOR. Check value
  `0x29B1` for `"123456789"`.

## How the PC side pins it

Depend on this crate at a locked revision of the repository that holds it, never on a path into a
checkout that moves. In the PC side's own lock file, pin the revision; in its code, assert the wire
hash at start:

```rust
assert_eq!(lemnos_flash_wire::WIRE_HASH, EXPECTED_WIRE_HASH);
```

On connect, read the identity record and call `lemnos_flash_wire::identity::check(&record)`. Treat any
`Skew` as a refusal that names the family. Do not compare the revision string or any version number.

## Tests

- `tests/frames.rs`: a vector for every request op and its refusals; the CRC check value; the reset
  kinds. The expected bytes were computed with a separate CRC implementation.
- `src/frame.rs`, `src/identity.rs`, `src/layout.rs`: unit tests for the CRC, the record's check (a hash
  mismatch, a missing family, trailing bytes, a foreign family, malformed bytes) and the pinned hashes.
