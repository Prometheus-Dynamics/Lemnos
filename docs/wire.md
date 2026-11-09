# The Lemnos socket protocol

Version 1. A client written in any language needs only this file. The test vectors at the end are
checked against the code by `engine/wire`, in both directions.

## Transport

A Unix domain stream socket. Both sides send at any time. A connection carries one byte stream each
way, and frames are not aligned to reads. All integers and floats are little-endian.

## Frames

```text
offset  size  field
0       4     length: the bytes after this field
4       2     kind
6       ...   the kind's fields, in order
```

The largest frame is 1 MiB, header included. A sender refuses to build a larger one. A receiver
closes the connection on a larger length, without reading it.

| Type | Encoding |
|---|---|
| `u8` to `u64`, `i8` to `i64`, `f32`, `f64` | fixed width |
| `bool` | one byte, 0 or 1. Anything else is malformed |
| `str`, `bytes` | a `u32` length, then the bytes. A `str` is valid UTF-8 |

## The kinds

| Kind | Name | Fields | Direction |
|---|---|---|---|
| 1 | Hello | version u16, name str, events bool | to host |
| 2 | Welcome | version u16, features u64, host str | to client |
| 3 | List | id u32 | to host |
| 4 | Device | id u32, device u32, name str, driver str, tags str, state u8, health u8, reason str | to client |
| 5 | ListDone | id u32 | to client |
| 6 | Open | id u32, device str, tag str | to host |
| 7 | Opened | id u32, device u32, contract u8 | to client |
| 8 | Describe | id u32, device u32 | to host |
| 9 | Description | id u32, bytes | to client |
| 10 | Call | id u32, device u32, contract u8, method u16, reply_capacity u32, args bytes | to host |
| 11 | Reply | id u32, bytes | to client |
| 12 | Subscribe | id u32, device u32, contract u8, method u16, period_us u64, only_changes bool | to host |
| 13 | Subscribed | id u32, subscription u64, period_us u64 | to client |
| 14 | Unsubscribe | id u32, subscription u64 | to host |
| 15 | Value | subscription u64, stamp_ns u64, bytes | to client |
| 16 | SetPower | id u32, device u32, on bool | to host |
| 17 | StreamOpen | id u32, device u32, name str | to host |
| 18 | StreamReady | id u32, buffers u32, descriptors u8 | to client |
| 19 | Event | device u32, state u8, health u8, reason str | to client |
| 20 | Refusal | id u32, status i32, text str | to client |

`state`: 0 running, 1 backing off, 2 off. `health`: 0 ok, 1 degraded, 2 failed. `status` is a
`lemnos-abi` status: 0 is success, a negative Linux errno is that errno, and lower codes are Lemnos's.
`tags` is comma-separated. An empty `device` in Open means any device with that tag.

`contract` is the tag's position in the device's list, which Opened reports. `method` is the number
the contract declares. `args`, `bytes` and values mean something only to the contract. The host never
reads them, and no field names a kind of device.

## Rules

Each of these closes a way the earlier protocol could go wrong.

1. **The version is checked.** The first frame is Hello. A host answers a different version with
   Refusal and closes. A client refuses a different version in Welcome.
2. **Every request gets an answer**, matched by `id`: its own reply or a Refusal. Nothing is ignored
   in silence. `id` 0 is allowed and is not special.
3. **A malformed frame closes the connection** on either side, because the stream can no longer be
   trusted. That covers an impossible length, an unknown kind, a short field, a bad `bool` and bad
   UTF-8.
4. **Values are refused, never cut.** Something too large for a frame is an error to whoever tried
   to send it. No text is shortened.
5. **Nothing is unbounded.** A client that lets more than 4 MiB of answers queue up is closed. Value
   frames are dropped first when a client falls behind; replies never are.
6. **Closing ends everything.** Whatever a connection held ends when it closes, greeted or not:
   subscriptions, claims and streams.
7. **A repeated Hello changes nothing** and is answered with Welcome.
8. **Trailing bytes after the known fields are ignored.** This is how a field is appended
   compatibly: an older reader stops before it.
9. **The socket is mode 0660** after binding, whatever the umask.

## Descriptors

StreamReady says how many file descriptors arrive with it, as `SCM_RIGHTS` on the same `sendmsg`.

## Description

The `bytes` of a Description, in order: a `u16` contract count, then for each its tag (str), a `u16`
method count, and for each method its id u16, name str, a `u16` parameter count with each parameter's
name str and shape, its returned shape and a has-out-buffer bool. Then a `u16` item count, and for
each item its kind u8 (0 value, 1 control, 2 action), name str, unit str, exponent i8, min and max
(each a presence bool, then an i64 when present), contract u8, method u16, and field (a presence
bool, then a str when present).

A shape is a u8 code: 0 unit, 1 bool, 2 u8, 3 u16, 4 u32, 5 u64, 6 i8, 7 i16, 8 i32, 9 i64, 10 f32,
11 f64, 12 str, 13 bytes, 14 option (followed by its shape), 15 array (a u32 length, then its shape),
16 record (a u16 count, then each field's name str and shape). Shapes nest at most 8 deep.

## Test vectors

Each vector is one complete frame.

### hello

`Hello { version: 1, name: "tool", events: true }`

<!-- vector: hello -->
```hex
0d 00 00 00 01 00 01 00 04 00 00 00 74 6f 6f 6c 01
```

### welcome

`Welcome { version: 1, features: 0, host: "h" }`

<!-- vector: welcome -->
```hex
11 00 00 00 02 00 01 00 00 00 00 00 00 00 00 00 01 00 00 00 68
```

### list

`List { id: 1 }`

<!-- vector: list -->
```hex
06 00 00 00 03 00 01 00 00 00
```

### device

`Device { id: 1, device: 2, name: "a", driver: "b", tags: "c/1", state: 1, health: 2, reason: "r" }`

<!-- vector: device -->
```hex
22 00 00 00 04 00 01 00 00 00 02 00 00 00 01 00 00 00 61 01 00 00 00 62 03 00 00 00 63 2f 31 01 02 01 00 00 00 72
```

### list-done

`ListDone { id: 1 }`

<!-- vector: list-done -->
```hex
06 00 00 00 05 00 01 00 00 00
```

### open

`Open { id: 2, device: "", tag: "c/1" }`

<!-- vector: open -->
```hex
11 00 00 00 06 00 02 00 00 00 00 00 00 00 03 00 00 00 63 2f 31
```

### opened

`Opened { id: 2, device: 3, contract: 1 }`

<!-- vector: opened -->
```hex
0b 00 00 00 07 00 02 00 00 00 03 00 00 00 01
```

### describe

`Describe { id: 3, device: 3 }`

<!-- vector: describe -->
```hex
0a 00 00 00 08 00 03 00 00 00 03 00 00 00
```

### description

`Description { id: 3, bytes: [9] }`

<!-- vector: description -->
```hex
0b 00 00 00 09 00 03 00 00 00 01 00 00 00 09
```

### call

`Call { id: 7, device: 3, contract: 1, method: 2, reply_capacity: 16, args: [aa, bb] }`

<!-- vector: call -->
```hex
17 00 00 00 0a 00 07 00 00 00 03 00 00 00 01 02 00 10 00 00 00 02 00 00 00 aa bb
```

### reply

`Reply { id: 7, bytes: [1] }`

<!-- vector: reply -->
```hex
0b 00 00 00 0b 00 07 00 00 00 01 00 00 00 01
```

### subscribe

`Subscribe { id: 8, device: 3, contract: 0, method: 1, period_us: 5000, only_changes: true }`

<!-- vector: subscribe -->
```hex
16 00 00 00 0c 00 08 00 00 00 03 00 00 00 00 01 00 88 13 00 00 00 00 00 00 01
```

### subscribed

`Subscribed { id: 8, subscription: 4, period_us: 5000 }`

<!-- vector: subscribed -->
```hex
16 00 00 00 0d 00 08 00 00 00 04 00 00 00 00 00 00 00 88 13 00 00 00 00 00 00
```

### unsubscribe

`Unsubscribe { id: 9, subscription: 4 }`

<!-- vector: unsubscribe -->
```hex
0e 00 00 00 0e 00 09 00 00 00 04 00 00 00 00 00 00 00
```

### value

`Value { subscription: 4, stamp_ns: 1000, bytes: [5, 6] }`

<!-- vector: value -->
```hex
18 00 00 00 0f 00 04 00 00 00 00 00 00 00 e8 03 00 00 00 00 00 00 02 00 00 00 05 06
```

### set-power

`SetPower { id: 10, device: 3, on: false }`

<!-- vector: set-power -->
```hex
0b 00 00 00 10 00 0a 00 00 00 03 00 00 00 00
```

### stream-open

`StreamOpen { id: 11, device: 3, name: "s" }`

<!-- vector: stream-open -->
```hex
0f 00 00 00 11 00 0b 00 00 00 03 00 00 00 01 00 00 00 73
```

### stream-ready

`StreamReady { id: 11, buffers: 4, descriptors: 2 }`

<!-- vector: stream-ready -->
```hex
0b 00 00 00 12 00 0b 00 00 00 04 00 00 00 02
```

### event

`Event { device: 3, state: 0, health: 0, reason: "" }`

<!-- vector: event -->
```hex
0c 00 00 00 13 00 03 00 00 00 00 00 00 00 00 00
```

### refusal

`Refusal { id: 12, status: -2, text: "no" }`

<!-- vector: refusal -->
```hex
10 00 00 00 14 00 0c 00 00 00 fe ff ff ff 02 00 00 00 6e 6f
```
