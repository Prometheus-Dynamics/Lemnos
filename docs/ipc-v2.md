# Protocol 2

The design for the next version of the `lemnosd` socket protocol (`docs/ipc.md` is protocol 1,
as coded). Written 2026-11-06.

**Outcome: there is no protocol 2 yet, and nothing needs one.** The two gaps that were set aside
for it (`docs/ipc.md`, section 14, items 10 and 14) fit within protocol 1's own rules for
appended fields and feature bits, and are fixed there. This file keeps the rule for when a
version is due, how one would be negotiated, and the list of changes that would wait for it.

## 1. What a version is for

A version number changes only for a change that protocol 1's compatibility rules
(`docs/ipc.md`, section 12) cannot carry:
- **Appended fields.** An older peer stops before the new bytes, and the decoder gives a
  default when they are absent.
- **Feature bits** in `Welcome.features`. A client sends a new request kind, or a new meaning
  of an old field, only to a service that sets the bit. The service sends a new message kind
  only to a client that asked for it.

A change that fits these rules is made in protocol 1, with a feature bit when the other side
must know about it. A change that does not fit is listed in section 4 and waits.

## 2. The two gaps, closed in protocol 1

### 2.1 Optional sentinels (item 10)

`opt u16` sends `0xFFFF` for None, and `opt u32` sends `0xFFFFFFFF`, so `Some` of those values
could not be sent. Every optional field in protocol 1 is one of two kinds:
- **Thousandths**: progress, base, depth and brightness, which the service clamps to 1000; and
  an orbit's tail, in thousandths of an LED, on a light of at most 64 LEDs (64 000).
- **Milliseconds**: `period_ms`, `fade_ms`, `duration_ms`. `0xFFFFFFFF` ms is 49.7 days.

So no meaningful value meets a sentinel. **Rule: the encoder writes `Some(0xFFFF)` as `0xFFFE`
and `Some(0xFFFFFFFF)` as `0xFFFFFFFE`.** It saturates one below the sentinel, and a value
always arrives as `Some`. No byte changes for any value sent before, and no peer needs to know.
A new optional field must have a range below its sentinel, or use a flag byte as `opt colour`
does.

### 2.2 Reads matched by device name (item 14)

A `Reading` has no id, so two overlapping `Read`s of one device could not be told apart.
- **`Read` gains an appended `u32 id`.** Absent means 0. An older service ignores the tail.
- **A `Reading` that answers a `Read` with a nonzero id carries that id appended.** An older
  client ignores the tail. Readings for subscriptions, and answers to id 0, carry no tail, so
  the subscribed path costs no byte more.
- **A refused `Read` is answered with `Reply` and the read's id**, not id 0.
- **Feature bit `FEATURE_READ_ID` (value 2, bit 1).** The service sets it when it echoes ids. A client then
  matches a read's answer by id alone. Without the bit, it matches by device name as before.

## 3. Negotiation, when a version comes

The rules the next version must follow, so that both sides keep working across the change:
- **`Hello` is frozen.** It is the one frame each side must read before the version is known.
  Its layout stays byte for byte. New greeting fields are appended, as now.
- **The service answers the version the client asked for**, when it speaks it, and otherwise
  its highest version below that. The `Welcome` names the version chosen. Until then the
  connection is protocol 1. (Today the service does not read `Hello.version`; protocol 1
  clients send 1, so the rule holds for them.)
- **The client accepts a `Welcome` with any version it implements.** Today that is only 1, so
  it refuses any other (section 5 of `docs/ipc.md`). A client for version 2 would also accept 1
  and fall back.
- **The service speaks both versions** for as long as protocol 1 clients exist. Each
  connection's codec is chosen at its `Hello`.

## 4. Changes that wait for a version

None is needed now. Each is listed with what it would buy:
- **Appended fields become required.** Each decoder then drops its "absent means" branch, and
  `SubscribeChannels` and `Subscribe` read their ids the same way (section 14, item 12).
- **Optionals as a flag byte**, as `opt colour` is. The rule in 2.1 makes this unneeded for the
  fields that exist.
- **An unknown request kind answered, not closed.** Frames carry their length, so the service
  could skip the frame and answer `Reply` id 0 `Unsupported`. Feature bits already keep a client
  from sending kinds a service does not know, so this only helps a misbehaving client.
- **A kind registry.** Ranges reserved per area (requests 1-99, messages 101-199), written down
  in `docs/ipc.md`, so that two additions cannot pick the same number.
