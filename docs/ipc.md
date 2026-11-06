# lemnosd socket protocol

This is the byte-level specification of the `lemnosd` socket protocol, wire
`VERSION` 1. It describes what the code in this tree does. A client written in
another language needs only this file.

Sources, relative to the repository root:

- `crates/lemnos-ipc/src/wire/`: frames, encodings, requests and messages.
- `crates/lemnos-ipc/src/client*.rs`: the client library (connection flow).
- `crates/lemnosd/src/`: the service (what it sends, when it drops a client).

Line numbers are given as `file:line`. Where the implementation leaves a point
open, this file says "not specified by the implementation".

Section 15 holds test vectors. `crates/lemnos-ipc/src/spec_vectors.rs` encodes
the same values with the crate and compares the bytes with the hex here.

## Contents

1. Transport
2. Frames
3. Primitive encodings
4. Enum codes
5. Handshake and version
6. Requests
7. Messages
8. Replies and correlation
9. Unsolicited messages
10. Server connection lifecycle
11. Client library behaviour
12. Compatibility rules
13. Limits and queueing
14. Gaps and inconsistencies
15. Test vectors

## 1. Transport

| Item | Value | Source |
|---|---|---|
| Socket type | Unix domain, stream (`SOCK_STREAM`) | `lemnos-ipc/src/client.rs:15`, `lemnosd/src/service.rs:214` |
| Default path | `/run/lemnos/lemnosd.sock` | `lemnos-ipc/src/client.rs:28` |
| Path override (service) | `LEMNOSD_SOCKET` | `lemnosd/src/main.rs:23` |
| Listener | `lemnosd` binds. It removes a stale file at the path first, and creates the parent directory. | `lemnosd/src/service.rs:211-215` |
| Connecting side | Clients. The service never connects out. | `lemnos-ipc/src/client.rs:313` |
| Permissions | `lemnosd` sets the socket to 0660 after binding, whatever the umask. The packaged unit also sets `UMask=0007`, `RuntimeDirectory=lemnos`, `RuntimeDirectoryMode=0750`, `User=lemnos`, `Group=lemnos`. | `packaging/systemd/lemnosd.service:49-58` |
| Byte order | Little-endian for every integer and for `f64` bits | `wire/codec.rs:23-51` |
| Direction | Both sides send at any time. The client sends requests; the service sends replies and unsolicited messages. | `wire/mod.rs:1-16` |

A connection carries one byte stream in each direction. Frames are not
aligned to reads or writes. A reader must buffer and split on frame boundaries.

## 2. Frames

Every frame, in both directions:

```
offset  size  field
0       4     length  u32 LE. Number of bytes after this field (kind + payload).
4       2     kind    u16 LE. Request kind (section 6) or message kind (section 7).
6       n     payload n = length - 2 bytes. Layout set by the kind.
```

Rules (from `wire/codec.rs:158-171`):

- A frame is complete when `4 + length` bytes are buffered. Until then the
  decoder reports "need more bytes". Partial data is kept.
- `length` must satisfy `2 <= length <= limit`. A value outside that range is a
  decode error. It is checked before the body arrives.
- `limit` is `MAX_REQUEST_FRAME` (71 680 = 70 x 1024) for requests and
  `MAX_FRAME` (1 048 576) for messages (`wire/mod.rs:30`, `wire/mod.rs:34`).
- Requests and messages use separate kind numbers. Request kinds are 1 to 17
  and 20 to 30. Message kinds are 101 to 112. Kind 0 is unused.
- Trailing bytes after a decoded payload are not checked. See section 12.

## 3. Primitive encodings

| Type | Encoding | Source |
|---|---|---|
| `u8`, `u16`, `u32`, `u64` | Unsigned, little-endian, fixed width | `wire/codec.rs:27-42` |
| `i8` | One byte, two's complement | `wire/codec.rs:47`, `:123` |
| `i32` | Four bytes, little-endian, two's complement | `wire/codec.rs:43`, `:118` |
| `f64` | The IEEE-754 bit pattern as `u64` LE | `wire/codec.rs:51`, `:126` |
| bool | `u8`: encoders write 0 or 1. Decoders read any nonzero value as true. | `wire/codec.rs`, `wire/request.rs` |
| `str` | `u16` byte count, then UTF-8 bytes. Decoders reject invalid UTF-8. | `wire/codec.rs:54-59`, `:129-132` |
| `bytes` | `u32` byte count, then the bytes | `wire/codec.rs:61-65`, `:133-136` |
| list | Count, then the items. The count is `u16` unless the table says otherwise. | `wire/request.rs:178`, `wire/message.rs:96` |
| `opt u16` | `u16`. `0xFFFF` means None. | `wire/codec.rs:69-71`, `:143-145` |
| `opt u32` | `u32`. `0xFFFFFFFF` means None. | `wire/codec.rs:66-68`, `:140-142` |
| `opt colour` | `u8` flag (0 or 1), then a `u32` colour always. The colour is 0 when the flag is 0. | `wire/codec.rs:72-77`, `:146-150` |
| colour | `u32` = `0xWWRRGGBB` (white in the top byte) | `wire/look.rs:33-39`, `wire/mod.rs:272` |
| `NO_VALUE` | `i32::MIN` (`0x80000000`). A reading channel with no value. | `lemnos-device/src/model.rs:13` |
| timestamp | `u64` microseconds on `CLOCK_BOOTTIME`, taken when the read started. The helper falls back to `CLOCK_MONOTONIC` only if `CLOCK_BOOTTIME` cannot be read. | `wire/mod.rs:10-16`, `lemnos-linux-sys/src/time.rs:7-11` |

Notes on the table:

- A `str` longer than 65 535 bytes is cut to 65 535 bytes on encode. The cut
  may split a UTF-8 sequence. The decoder then rejects the frame (section 14).
- `opt` sentinels make `Some(0xFFFF)` and `Some(0xFFFFFFFF)` impossible to send.
  They decode as None (section 14).

### Fixed-point values

A reading channel carries an `i32` count and a channel `exponent` (`i8`). The
physical value is:

```
value = count x 10^exponent
```

(`lemnos-ipc/src/client.rs:137-144`.) The device list gives each channel's
exponent. Readings give only counts. Example: the BMI088 acceleration channel
has exponent -3, so its counts are mm/s^2.

A control has `min` and `max` as `i32` counts, and the same exponent. A `Set`
carries an `f64` value in the unit. The service computes
`count = round(value x 10^-exponent)` and refuses with `OutOfRange` when the
result is not finite or lies outside `[min, max]` (`lemnosd/src/service.rs:1606-1609`).
The reply carries the applied value in the unit.

## 4. Enum codes

Each table is from `wire/codec.rs:204-227`. The "decode fallback" column is what
a decoder returns for a code that is not in the table. No decoder rejects an
unknown code in these enums.

| `DeviceClass` | Code | | `Quantity` | Code |
|---|---|---|---|---|
| `Imu` | 0 | | `Acceleration` | 0 |
| `Accelerometer` | 1 | | `AngularRate` | 1 |
| `Gyroscope` | 2 | | `MagneticField` | 2 |
| `Magnetometer` | 3 | | `Voltage` | 3 |
| `PowerMonitor` | 4 | | `Current` | 4 |
| `Temperature` | 5 | | `Power` | 5 |
| `Fan` | 6 | | `Temperature` | 6 |
| `Lens` | 7 | | `RotationalSpeed` | 7 |
| `Light` | 8 | | `Ratio` | 8 |
| `Gpio` | 9 | | `Position` | 9 |
| `Orientation` | 10 | | `Level` | 10 |
| `Other` | 11 | | `Mode` | 11 |
| `PowerSwitch` | 12 | | `Color` | 12 |
| | | | `Angle` | 13 |
| | | | `Pressure` | 14 |
| | | | `Frequency` | 15 |

Decode fallbacks: `DeviceClass` -> `Other`, `Quantity` -> `Mode`.

| `Axis` | Code | | `DeviceStatus` | Code | | `ErrorKind` | Code |
|---|---|---|---|---|---|---|---|
| `None` | 0 | | `Available` | 0 | | `NotFound` | 0 |
| `X` | 1 | | `Degraded` | 1 | | `Unavailable` | 1 |
| `Y` | 2 | | `Faulted` | 2 | | `Busy` | 2 |
| `Z` | 3 | | `Missing` | 3 | | `PermissionDenied` | 3 |
| | | | | | | `Timeout` | 4 |
| | | | | | | `InvalidInput` | 5 |
| | | | | | | `Unsupported` | 6 |
| | | | | | | `Configuration` | 7 |
| | | | | | | `Nack` | 8 |
| | | | | | | `Overrun` | 9 |
| | | | | | | `Failed` | 10 |

Decode fallbacks: `Axis` -> `None`, `DeviceStatus` -> `Missing`,
`ErrorKind` -> `Failed`. The encoder writes 255 for a variant with no code.
Every variant has a code today.

In `Event::Status` the error byte is `255` for "no error" (`wire/message.rs:153`).

### Other codes

| Code set | Values | Source |
|---|---|---|
| Refusal code (u8) | 1 `NotAllowed`, 2 `OutOfRange`, 3 `UnknownDevice`, 4 `UnknownControl`, 5 `Device(kind)`, 6 `Owned`, 7 `Claimed`, 8 `UnknownHandle`, 9 `Unsupported`, 10 `UnknownChannel`. Any other nonzero code decodes as `Device(kind)`. | `wire/mod.rs:187-216` |
| Refusal kind (u8) | The `ErrorKind` code. Used only with code 5. Other codes send 0. | `wire/mod.rs:194`, `:203-215` |
| Calibration routine (u8) | 1 `AccelSix`, 2 `MagRotate`, 3 `GyroHold`. In a status, 0 means "none running". | `wire/calibration.rs:10-25`, `:48-68` |
| Calibration command (u8) | 0 `Start` (then one routine byte), 1 `Stop`, 2 `Apply`, 3 `Discard`, 4 `Reset` | `wire/calibration.rs:27-46` |
| LED status (u8) | 0 `Ok`, 1 `Warn`, 2 `Error`, 3 `Busy`, 4 `Off`. Decode fallback: `Off`. | `wire/request.rs:37-57` |
| LED effect (u8) | 0 none, 1 `Solid`, 2 `Blink`, 3 `Breathe`, 4 `Chase`. Decode fallback: none. | `wire/request.rs:59-79` |
| Easing (u8) | 0 none, 1 `Linear`, 2 `EaseIn`, 3 `EaseOut`, 4 `EaseInOut`, 5 `Sine`, 6 `CubicBezier` (then four `u16` parameters). Decode fallback: none. | `wire/request.rs:6-35` |
| System state (u8) | 0 `Updating` (then `opt u16` progress and phase), 1 `Booting`, 2 `Rebooting`, 3 `UpdateFailed`, 4 `RolledBack`, 5 `Confirmed` | `wire/request.rs:81-122` |
| System phase (u8) | 0 `Verifying`, 1 `Writing`, 2 `Staged`, 3 `Applying`. Decode fallback: `Applying`. | `wire/request.rs:87-112` |
| Raw line bias | 0 as-is, 1 pull-up, 2 pull-down, 3 disabled. Fallback: as-is. | `wire/raw.rs:153-208` |
| Raw line drive | 0 push-pull, 1 open-drain, 2 open-source. Fallback: push-pull. | `wire/raw.rs:166-170`, `:195-199` |
| Raw line edge | 0 none, 1 rising, 2 falling, 3 both. Fallback: none. | `wire/raw.rs:171-176`, `:200-205` |
| Raw safe state (u8) | 0 default (none), 1 input, 2 low, 3 high, 4 keep | `wire/raw.rs:210-228` |
| Look block (u8) | See section 6.3 | `wire/look.rs:18-26` |
| Look envelope (u8) | 0 solid, 1 blink, 2 breathe, 3 pulse | `wire/look.rs:28-31` |
| Look mode (u8) | 0 `Max`, 1 `Add`, 2 `Over` | `wire/look.rs:164-168`, `:391-396` |
| GPU lease mode (u8) | 0 exclusive, 1 shared. Fallback: exclusive. | `wire/gpu.rs` |
| GPU lease state (u8) | 0 granted, 1 queued, 2 refused, 3 yield asked, 4 overdue. Unknown: a decode error. | `wire/gpu.rs` |
| Welcome feature bits (u32) | bit 0: the GPU lease requests (6.6) | `wire/gpu.rs` |

## 5. Handshake and version

The steps the client library takes, in order (`lemnos-ipc/src/client.rs:312-370`):

1. Connect to the socket path.
2. Send `Hello` (request kind 1). It carries `VERSION` (1), the client's name,
   its priority, the `keep` flag and the `events` flag.
3. Read messages until `Welcome` (kind 101) arrives. Other messages are kept
   for later. The `Welcome` gives the board id and the client id.
4. Send `List` (request kind 2) and read until `Devices` (kind 102) arrives.
5. Re-send any subscriptions and held LED requests, with id 0 (section 11).

The server does not require `Hello` to come first. It answers a `Hello` with
`Welcome` (`lemnosd/src/service.rs:1180-1201`). The `Welcome` holds the
service's `VERSION`, the board id, the client's id for this connection and the
service's feature bits (section 4; appended within protocol 1).

**Version check.** The client refuses a `Welcome` whose version is not its own
`VERSION`. It drops the connection and returns a protocol error naming both
versions (`lemnos-ipc/src/client.rs`, `connect`). The service does not read
`Hello.version`. It answers every greeting with its own version, and leaves the
decision to the client. A version is a breaking change. Additions within a
version go in appended fields (section 12).

## 6. Requests

Client to service. Each row gives the kind, the payload in order, and the
reply. Field types are from section 3.

### 6.1 Request kinds

| Kind | Request | Payload (in order) | Reply | Source |
|---|---|---|---|---|
| 1 | `Hello` | `u16 version`, `str client`, `u8 priority`, `u8 keep`, `u8 events` (absent: 1) | `Welcome` (kind 101) | `request.rs:128-142`, `:376-383` |
| 2 | `List` | (empty) | `Devices` (102) | `request.rs:153`, `:390` |
| 3 | `Read` | `str device` | `Reading` (103), or `Reply` id 0 on refusal | `request.rs:155-159`, `:392` |
| 4 | `Subscribe` | `str device`, `u32 period_ms`, `u32 id` (absent: 0) | `Reply` (104), see 8 | `request.rs:160-170`, `:408-413` |
| 5 | `Set` | `u32 id`, `str device`, `str control`, `f64 value` | `Reply` (104) | `request.rs:185-194`, `:414-419` |
| 6 | `Get` | `u32 id`, `str device`, `str control` | `Reply` (104) | `request.rs:195-203`, `:420-424` |
| 7 | `Led` | See 6.2 | `Text` (108), see 8 | `request.rs:204-295`, `:425-507` |
| 8 | `Release` | `u32 id`, `str device` | `Reply` (104) | `request.rs:296-300`, `:508-511` |
| 9 | `Restore` | `u32 id`, `str device`, `str control` (empty: all controls) | `Reply` (104) | `request.rs:143-151`, `:384-388` |
| 10 | `Looks` | `u32 id`, `u8 op`, operation payload (6.5) | `Text` (108) | `request.rs:338-354`, `:539-560` |
| 11 | `SubscribeChannels` | `str device`, `u32 period_ms`, `u16 n`, `n` x `str channel`, `u32 id` (required) | `Reply` (104), see 8 | `request.rs:171-184`, `:393-407` |
| 12 | `WatchFrames` | `u32 id`, `str device`, `u16 fps` | `Reply` (104) with the granted rate, then `LightInfo` (110) and `Frame` (109) | `request.rs:301-305`, `:528-532` |
| 13 | `Calibration` | `u32 id`, `str device`, calibration command (section 4) | `Reply` (104), value 0 | `request.rs:355-364`, `:561-565` |
| 14 | `CalibrationStatus` | `u32 id`, `str device` | `CalibrationStatus` (111), or `Reply` on refusal | `request.rs:365-369`, `:566-569` |
| 15 | `LightSetting` | `u32 id`, `str device`, `u16 look_brightness` (0 to 1000), `u8 persist` | `Reply` (104), the value applied | `request.rs:325-337`, `:533-538` |
| 16 | `LedInputs` | `u32 id`, `str device`, `opt u16 progress`, `u8 spots_flag`, spots list if flag = 1 | `Text` (108), see 8 | `request.rs:306-324`, `:512-527` |
| 17 | `Stats` | (empty) | `Stats` (112) | `request.rs:154`, `:391` |
| 18 | `GpuAcquire` | `u32 id`, `str gpu`, `u8 mode`, `u8 priority`, `u8 wait`, `u32 max_hold_ms`, `str label` | `GpuLease` (113), see 6.6 | `wire/gpu.rs` |
| 19 | `GpuRelease` | `u32 id`, `u32 lease` | `Reply` (104) | `wire/request.rs` |
| 31 | `GpuStatus` | `u32 id` | `GpuState` (114) | `wire/request.rs` |
| 32 | `GpuHang` | `u32 id`, `u32 lease`, `str detail` | `Reply` (104) | `wire/request.rs` |
| 20-30 | Raw requests | See 6.4 | `Claimed` (106), `Data` (107), or `Reply` (104) | `raw.rs:136-151`, `:231-479` |

### 6.2 `Led` payload

Kind 7. The fields are, in order (`request.rs:231-295` encode, `:425-507` decode):

| Field | Type | Notes |
|---|---|---|
| `device` | `str` | Empty: the board's status light. |
| `show` | `u8` tag, then the payload in the table below | |
| `effect` | `u8` | Effect code (section 4). |
| `period_ms` | `opt u32` | |
| `depth` | `opt u16` | Breathe depth or blink duty, thousandths. |
| `brightness` | `opt u16` | Thousandths. |
| `fade_ms` | `opt u32` | |
| `easing` | `u8` code, then four `u16` | Always four `u16`. Zero when unused. |
| `duration_ms` | `opt u32` | |
| `flags` | `u8` | Bit 0 is `test`. Absent: 0. |
| `id` | `u32` | Reply id. Absent: 0. |

Show tags (`request.rs:208-277`):

| Tag | Show | Payload |
|---|---|---|
| 0 | `Clear` | none |
| 1 | `Status` | `u8` LED status code |
| 2 | `Color` | `u32` colour |
| 3 | `Frame` | `u16 n`, then `n` x `u32` colour, one per LED from index 0 |
| 4 | `Locate` | none |
| 5 | `Pixels` | `u16 n`, then `n` x (`u16` index, `u32` colour) |
| 6 | `Progress` | `u16` fraction (thousandths), `opt colour` colour, `opt colour` background |
| 7 | `Indeterminate` | `opt colour` |
| 8 | `System` | system state (section 4) |
| 9 | `Orbit` | `opt colour`, `opt u16` tail, `u8` heads, `opt u16` base |
| 10 | `Look` | `str` look name, `opt u16` progress |
| 11 | `Inline` | look (6.3), `opt u16` progress |
| 12 | `LookSpots` | `str` look name, `opt u16` progress, spots list (6.3) |
| 13 | `InlineSpots` | look (6.3), `opt u16` progress, spots list (6.3) |

### 6.3 Look and spots encoding

Used by show tags 11, 12 and 13, and by `LedInputs` (kind 16). Source:
`wire/look.rs:41-407`.

A look:

| Field | Type | Notes |
|---|---|---|
| layer count | `u8` | 0 to 4. Encoder writes at most 4. A look with 0 layers decodes. The service refuses it with a reply, because a look needs a layer. |
| layers | see below | One per layer, bottom first |
| envelope | effect | Whole-look effect |
| brightness | `u8` | |
| floor | `u8` | |

A layer:

| Field | Type |
|---|---|
| block | `u8` block code, then the block fields |
| brightness | `u8` |
| mode | `u8`: 0 `Max`, 1 `Add`, 2 `Over` |

Block codes and fields (`look.rs:18-26`, `:41-172`, `:267-388`):

| Code | Block | Fields |
|---|---|---|
| 0 | fill | `colour` |
| 1 | comet | `colour`, `u32 period_ms`, `u16 tail`, `u8 heads`, `u16 base`, `u8 reverse` |
| 2 | arc | `u8 kind` (0 fixed, 1 input), `u16 value` (fixed fraction; 0 for input), `colour`, `colour track`, `u16 head`, `u8 sheen` |
| 3 | ripple | `u16 origin`, `colour`, `u16 speed`, `u16 width`, `u32 settle_ms`, `u16 glow` |
| 4 | frame | `u8 n` (at most 64), then `n` x `colour` |
| 5 | wash | `colour`, envelope effect |
| 6 | sparkle | `u8 n` (1 to 4), `n` x `colour`, `u32 density`, `u32 density_end`, `u32 fade_ms`, `u32 start_ms`, `u32 min_ms`, `u32 max_ms`, `u16 base`, `u32 seed`, `u8 fall`, `u32 fall_speed`, `u32 fall_accel` |
| 7 | drain | `colour`, `u32 fill_ms`, `u32 start_ms`, `u32 duration_ms`, easing code (section 4) and four `u16` |
| 8 | spots | `colour`, `u16 width`, `u32 follow_ms`, `u8 max_spots`, `u8 source` (0: fixed, then a spots list; 1: input) |

Effects inside a look (`look.rs:202-265`):

| Code | Effect | Fields |
|---|---|---|
| 0 | solid | none |
| 1 | blink | `u32 period_ms`, `u16 duty` |
| 2 | breathe | `u32 period_ms`, `u16 depth`, easing code and four `u16` |
| 3 | pulse | `u32 attack_ms`, `u32 hold_ms`, `u32 decay_ms`, `u8 repeat` |

A spots list (`look.rs:176-199`): `u8 count` (at most 8), then per spot
`u16 at`, `u16 level`, `opt colour`.

### 6.4 Raw requests

Kinds 20 to 30. Each carries `u32 id` first. `bus` strings name an I2C bus as
`1`, `i2c-1`, or a board selector such as `i2c:compatible=i2c-gpio`
(`wire/raw.rs:63-64`).

Line target (`raw.rs:241-243` encode, `:352-363` decode): `u8` kind. 0 is a
chip target: `str chip`, `u32 offset`. 1 is a name: `str name`, then `u32` 0.
The decoder treats any nonzero kind as a name (section 14).

Line configuration (`raw.rs:153-208`):

| Field | Type | Values |
|---|---|---|
| direction | `u8` | 0 input, 1 output |
| initial | `u8` | bool |
| active_low | `u8` | bool |
| bias | `u8` | section 4 |
| drive | `u8` | section 4 |
| edge | `u8` | section 4 |
| debounce_us | `u32` | |

PWM configuration (`raw.rs:274-283`): `u64 period_ns`, `u64 duty_ns`,
`u8 polarity` (1 inversed, else normal), `u8 enabled`.

| Kind | Request | Payload after `u32 id` | Reply | Source |
|---|---|---|---|---|
| 20 | LineClaim | line target, line configuration, `u8 on_release` (safe state) | `Claimed` (106) | `raw.rs:231-248`, `:352-371` |
| 21 | LineConfigure | `u32 handle`, line configuration | `Reply` (104) | `raw.rs:249-254`, `:372-376` |
| 22 | LineGet | `u32 handle` | `Reply` (104), value 0 or 1 | `raw.rs:255-259`, `:377-381` |
| 23 | LineSet | `u32 handle`, `u8 value` (bool) | `Reply` (104) | `raw.rs:260-264`, `:381-385` |
| 24 | PwmClaim | `u8 by_name` (1: name), `u32 chip`, `u32 channel`, `str name` (empty for a chip target) | `Claimed` (106) | `raw.rs:265-273`, `:386-400` |
| 25 | PwmConfigure | `u32 handle`, PWM configuration | `Reply` (104) | `raw.rs:274-283`, `:401-414` |
| 26 | Unclaim | `u32 handle` | `Reply` (104) | `raw.rs:284-288`, `:415-418` |
| 27 | I2cTransfer | `str bus`, `u16 address`, `u16 n`, `n` ops (below) | `Data` (107) | `raw.rs:289-304`, `:419-437` |
| 28 | I2cLock | `str bus`, `u16 address`, `u8 lock` | `Reply` (104) | `raw.rs:305-314`, `:438-443` |
| 29 | SpiTransfer | `u32 bus`, `u16 chip_select`, `u16 n`, `n` transfers (below) | `Data` (107) | `raw.rs:315-336`, `:444-470` |
| 30 | SpiLock | `u32 bus`, `u16 chip_select`, `u8 lock` | `Reply` (104) | `raw.rs:337-346`, `:471-476` |

I2C op (`raw.rs:297-301`, `:426-429`): `u8 tag`, then 0 is a write with
`bytes`, and 1 is a read with `u32 count`. The decoder reads any nonzero tag as
a read (section 14).

SPI transfer (`raw.rs:322-334`, `:452-463`): `u8 mode` (0 to 3), `u32 speed_hz`,
`u8 bits_per_word`, `bytes tx`, `u16 rx_len`, `u8 cs_change`, `u16 delay_us`.

Limits checked by the service (`lemnosd/src/raw.rs:26-28`, `:430-436`, `:484-491`):

- I2C: the sum of write bytes and read counts must be at most 4096. The
  address must be at most 0x7F. The op list must not be empty.
- SPI: the sum of `tx.len()` and `rx_len` over all transfers must be at most
  65 536. The transfer list must not be empty.

Admission (`lemnosd/src/raw.rs:126-152`). If the board lists raw clients, a
client not in the list is refused. The refusal keeps the reply's shape: `Claimed`
for claims, `Data` for transfers, `Reply` for the rest. Each carries
`NotAllowed`.

### 6.5 `Looks` operations

Kind 10. Payload: `u32 id`, `u8 op`, then:

| `op` | Operation | Payload | Writes? |
|---|---|---|---|
| 0 | List | none | no |
| 1 | Show | `str name` | no |
| 2 | Reload | none | no |
| 3 | Save | `str name`, `str text` | yes |
| 4 | PresetList | none | no |
| 5 | PresetShow | `str name` | no |
| 6 | PresetApply | `str name` | yes |
| 7 | PresetSave | `str name`, `str text` | yes |
| 8 | PresetDelete | `str name` | yes |
| 9 | Delete | `str name` | yes |

Source: `wire/request.rs:338-354`, `:539-560`. The service refuses the writing
operations 6, 7, 8 and 9 unless the client matches the board's look writers
(`lemnosd/src/service.rs:1505-1532`). Op 3 is not in that list. The service
answers each operation with `Text` (108).

### 6.6 GPU lease

Kinds 18, 19, 31 and 32, added within protocol 1. A client sends them only to
a service whose `Welcome` has feature bit 0 set: an older service does not know
the kinds and closes the connection (section 12). Source: `wire/gpu.rs`,
`lemnosd/src/gpu.rs`.

The lease is advisory. The GPU's user-space driver opens the render node
itself, so the service enforces nothing: a lease orders the clients that ask
and says who holds the GPU. A client that never asks is not affected.

- **GPUs.** The service lists the render nodes under `/sys/class/drm`
  (`renderD*`) each time a lease is asked for or the state is read. A GPU is
  named by its kernel driver (`v3d`), with a number after it when the board has
  more than one of that driver (`v3d1`), in node order. `gpu` empty means the
  first. An unknown name is refused with `UnknownDevice`; so is any request on a
  host with no render node.
- **Modes.** An exclusive lease is held alone. Shared leases are held together,
  never beside an exclusive one.
- **Order.** Waiters are ordered by `priority` (higher first), then by arrival.
  The first waiter is granted as soon as it fits beside the holders; a waiter
  behind it waits even if it would fit, so a shared stream cannot starve an
  exclusive waiter of the same priority.
- **`wait` 0** takes the lease only if it would be granted at once, otherwise
  `GpuLease` refused with `Claimed`. **`wait` 1** queues: `GpuLease` queued, with
  the number of leases ahead (holders and waiters), then a second `GpuLease`
  with the same id when granted.
- **Yield.** When the first waiter is held back by a holder of lower priority,
  that holder gets `GpuLease` "yield asked", once per lease. It decides whether
  to release.
- **Overdue.** A holder past its `max_hold_ms` (0: no limit) gets `GpuLease`
  "overdue", once, and the state shows it overdue. Nothing is taken back. The
  service logs it.
- **Release.** `GpuRelease` ends a held or a queued lease of this connection,
  else `UnknownHandle`. Closing the connection ends all of them, whatever
  `keep` says.
- **Hangs.** `GpuHang` records a hang the client saw under one of its leases
  (else `UnknownHandle`): the GPU's hang count, and its last report with the
  client, the label and `detail`. The service logs it.

`GpuLease` (113) payload: `u32 id` (the acquire's), `u32 lease` (0 when
refused), `u8 state` (section 4), `u8 code`, `u8 kind` (the refusal, for state
2; else 0), `u16 ahead` (for state 1; else 0).

`GpuState` (114) payload: `u32 id`, `u16 n`, then per GPU: `str name`,
`str driver`, `str node`, the holders, the waiters (in grant order), `u32 hangs`,
`u8 has_last_hang`, and if 1: `str client`, `str label`, `str detail`,
`u64 ago_ms`. The holders and the waiters are each `u16 n`, then per lease:
`u32 lease`, `u32 client_id`, `str client`, `str label`, `u8 mode`,
`u8 priority`, `u64 for_ms` (held or waited for), `u32 max_hold_ms`,
`u8 overdue`.

## 7. Messages

Service to client. Source: encode `wire/message.rs:33-214`, decode
`wire/message.rs:216-417`, types `wire/mod.rs:548-591`.

| Kind | Message | Payload (in order) | Source |
|---|---|---|---|
| 101 | `Welcome` | `u16 version`, `str board`, `u32 client_id`, `u32 features` (absent: 0) | `message.rs:72-80`, `:219-223` |
| 102 | `Devices` | device list (below), then the reason list (below) | `message.rs:94-127`, `:224-271` |
| 103 | `Reading` | `str device`, `u64 timestamp_us`, `u8 status`, `u16 n`, `n` x `i32` value | `message.rs:128`, `:287-302` |
| 104 | `Reply` | `u32 id`, `u8 code`, `u8 kind`, `f64 value` | `message.rs:129-140`, `:303-316` |
| 105 | `Event` | `u8` event tag, then the event payload (7.2) | `message.rs:141-188`, `:317-349` |
| 106 | `Claimed` | `u32 id`, `u8 code`, `u8 kind`, `u32 handle` (0 on refusal) | `message.rs:189-200`, `:350-362` |
| 107 | `Data` | `u32 id`, `u8 code`, `u8 kind`, `bytes` (empty on refusal) | `message.rs:201-212`, `:397-409` |
| 108 | `Text` | `u32 id`, `u8 code` (0 ok, 1 error), `str text` | `message.rs:63-71`, `:388-396` |
| 109 | `Frame` | `str device`, `u32 seq`, `u16 n`, `n` x `u32` colour | `message.rs:35-44`, `:363-376` |
| 110 | `LightInfo` | `str device`, `u16 count`, `u16 offset`, `u8 clockwise`, `str look`, `str layer`, `str owner`, `u16 look_brightness` (absent: 1000) | `message.rs:45-56`, `:377-387` |
| 111 | `CalibrationStatus` | `u32 id`, `str device`, status (below) | `message.rs:57-62`, `:410-414` |
| 112 | `Stats` | `u16 n`, `n` x (`str device`, `u64 reads`, `u64 total_us`, `u64 max_us`, `u64 last_us`, `u64 misses`) | `message.rs:81-93`, `:272-286` |
| 113 | `GpuLease` | See 6.6 | `wire/gpu.rs` |
| 114 | `GpuState` | See 6.6 | `wire/gpu.rs` |

For `Reply`, `Claimed`, `Data` and `Text`, `code` 0 is success. Any other code
is a refusal, and the refusal is described by the code and kind in section 4.
For `Text`, code 1 carries the reason in `text`.

Device list (`message.rs:97-118`, `:228-252`):

| Field | Type | Notes |
|---|---|---|
| `id` | `str` | The board definition's device id |
| `label` | `str` | |
| `class` | `u8` | Section 4 |
| `model` | `str` | |
| `status` | `u8` | Section 4 |
| `pixels` | `u16` | LEDs on a light; 0 otherwise |
| channel count | `u16` | |
| channels | per channel: `str name`, `u8 quantity`, `u8 axis`, `i8 exponent` | |
| control count | `u16` | |
| controls | per control: `str name`, `u8 quantity`, `i8 exponent`, `i32 min`, `i32 max` | |

After the whole device list comes the reason list (`message.rs:120-125`):
`u16 n` (the device count), then `n` x `str reason`. The decoder reads it only
when the count matches the device count. If it does not, the bytes are ignored
and every reason is empty (`message.rs:265-269`). Clients from older services
stop before the reason list.

Calibration status (`calibration.rs:48-61` encode, `:63-90` decode): `u32 revision`,
`u8 running` (0 none, otherwise a routine code), `u16 progress`,
`u8 candidate`, `u8 failed`, then three parts in order, each `u32 samples`,
`u16 confidence`, `u16 coverage`, `u16 residual`, `u8 active`.

### 7.2 Event payloads

`Event` (kind 105). The tag is the first byte of the payload (`message.rs:141-188`).

| Tag | Event | Payload | Source |
|---|---|---|---|
| 0 | `Status` | `str device`, `u8 status`, `u8 error` (255: none), `str reason` (absent on decode: empty) | `message.rs:144-155`, `:318-329` |
| 1 | `Control` | `str device`, `str control`, `f64 value`, `str by` (the writer's client name) | `message.rs:156-163`, `:330-335` |
| 2 | `LedOwner` | `str device`, `str owner` (empty: nobody), `str layer` | `message.rs:164-170`, `:336-340` |
| 3 | `Edge` | `u32 handle`, `u8 rising`, `u64 timestamp_ns`, `u32 seq` | `message.rs:171-182`, `:341-346` |
| 4 | `Dropped` | `u32 count` | `message.rs:183-185`, `:347` |

Any other event tag is a decode error.

Some `Control` events name something that is not a control of the device.
These are sent only when the action succeeded or reverted something. A
`Release` sends control `release`. A `Restore` sends control `restore` (when its
control name is empty) or the control name it was given. A client's cleanup
sends `restore`. The value is 0 in each case
(`lemnosd/src/service.rs:1338-1353`, `lemnosd/src/service_raw.rs:84-108`).

## 8. Replies and correlation

- The client chooses each `id`. Ids are `u32`. The client library starts at 1
  and wraps past 0, so it never sends 0 as a live id (`client.rs:304-308`).
- The service echoes the id. It does not generate ids.
- An id of 0 means "no id". Clients before protocol 1 send 0 everywhere. The
  service sends a reply with id 0 only where the table says so.
- Ids are per connection. Two connections may use the same id.
- A `Reading` carries no id. A one-shot `Read` is matched by device name
  (`client.rs:583-596`).

What each request is answered with (`lemnosd/src/service.rs:1178-1533`):

| Request | Answer | Notes |
|---|---|---|
| `Hello` | `Welcome` (always) | No id |
| `List` | `Devices` | No id |
| `Stats` | `Stats` | No id |
| `Read` | `Reading` when the read is served, else `Reply` id 0 | Refusals: `UnknownDevice`, or the device's error kind (`:1226-1270`) |
| `Subscribe`, `SubscribeChannels` | `Reply` with the granted period | Sent when id is nonzero, or when refused (then even with id 0). A refusal does not add a subscription. (`:1275-1302`) |
| `Set` | `Reply` with the applied value | On success also a `Control` event to events clients (`:1303-1320`) |
| `Get` | `Reply` | (`:1321-1328`) |
| `Restore` | `Reply` value 0 | (`:1330-1337`) |
| `Release` | `Reply` value 0 | On success also a `Control` event (`:1338-1353`) |
| `Led` | `Text` only when id is nonzero | "no light" error, look refusal, or "every intent slot of the light is taken" (`:1354-1425`). Silent when the device's write policy refuses the client (`:1376-1378`). |
| `LedInputs` | `Text` only when id is nonzero | Silent when the write policy refuses (`:1451-1453`) |
| `LightSetting` | `Reply` with the applied value (thousandths) | `look_brightness` above 1000 is refused (`:1007-1009`) |
| `WatchFrames` | `Reply` with the granted fps (`min(fps, 60)`; 0 for a stop) | Then `LightInfo` and `Frame` messages (section 9) (`:1021-1050`, `:1477-1483`) |
| `Calibration` | `Reply` value 0 | (`:1484-1494`) |
| `CalibrationStatus` | `CalibrationStatus` or `Reply` refusal | (`:1495-1504`) |
| `Looks` | `Text` always | Refusals too. Writing ops need look writers (6.5). (`:1505-1533`) |
| raw requests | `Claimed`, `Data` or `Reply` with the request's id | (`lemnosd/src/raw.rs:120-152`) |
| `GpuAcquire` | `GpuLease` with the request's id, always | Queued leases get a second one when granted (6.6) |
| `GpuRelease`, `GpuHang` | `Reply` value 0, or `UnknownHandle` | |
| `GpuStatus` | `GpuState` with the request's id | |

A request with id 0 that gets no answer is a fire-and-forget request. A
request with an id that gets no answer is an open question for the client.
Section 14 lists the one case where that happens.

## 9. Unsolicited messages

These messages come without a request. They are the only way data reaches a
client after its requests are answered.

| Message | When | To | Source |
|---|---|---|---|
| `Reading` (103) | Each read of a device that a subscription is due for. Its channels are the subscription's channels. Channels outside the subscription carry `NO_VALUE`. | The subscribing client | `lemnosd/src/service.rs:456-510`, `:625-660`; `lemnosd/src/devices.rs:44` |
| `Event` (105), tag 0 `Status` | A device's status changes | Clients with `greeted` and `events` set | `lemnosd/src/service.rs:315-318`, `status_event` |
| `Event` (105), tag 1 `Control` | A control write or release; a restore | Clients with `greeted` and `events` set | `lemnosd/src/service.rs:315-318`, `:1303-1320`, `:1338-1353`; `service_raw.rs:84-108` |
| `Event` (105), tag 2 `LedOwner` | The owner of a light's intent changes; and once per light right after the `Welcome` of a connection's first `Hello` with `events` set, giving the current owner (empty: nobody) | Clients with `greeted` and `events` set | `lemnosd/src/service.rs`, `render_lights`, `send_light_owners` |
| `Event` (105), tag 3 `Edge` | An edge on a line the client claimed with edge detection | The claiming client, if `greeted`. Edges belong to the claim, not to the broadcasts, so the `events` flag does not apply. | `lemnosd/src/service_raw.rs:30-40` |
| `Event` (105), tag 4 `Dropped` | The client's queue dropped events (section 13) | That client | `lemnosd/src/clients.rs:154-161` |
| `LightInfo` (110) | A watched light's description changes, and once when a watch starts | The watcher | `lemnosd/src/service.rs:1055-1101` |
| `Frame` (109) | A watched light's frame changes, at most `fps` a second | The watcher | `lemnosd/src/service.rs:1055-1101` |
| `GpuLease` (113) | A queued lease is granted; a holder is asked to yield, or is past its time (6.6). Carries the acquire's id. | The lease's client, greeted or not; never broadcast | `lemnosd/src/gpu.rs` |

"Greeted" means the connection has sent `Hello`. "events" is the `events`
flag from `Hello`. A client that does not send `Hello` gets replies but no
broadcasts (`service.rs:316`, `:1195`).

## 10. Server connection lifecycle

What `lemnosd` does with a connection (`lemnosd/src/service.rs:1880-1945`,
`clients.rs`, `service_raw.rs:84-108`).

1. **Accept.** The connection gets the next client id. Ids start at 1 and skip
   0 on wrap (`service.rs:252`, `:1883-1884`). The connection is not greeted.
   Its owner id is `u32::MAX - id`, outside the range of named owners, so its
   LED intents are its own until `Hello` names it.
2. **Requests.** Requests are run in the order they arrive. Nothing requires
   `Hello` first. A request from a connection that has not sent `Hello` is
   run (`service.rs:1178`).
3. **Hello.** Sets the client's name, priority, `keep` flag and `events` flag.
   Its owner id is the one shared by every connection with the same name. Then
   `Welcome` is sent. A second `Hello` changes none of this, and is answered
   with another `Welcome` (`service.rs:1180-1201`, `:1165-1173`).
4. **Bad input.** A frame whose length is out of range, or whose payload does
   not decode, closes the connection. No reply is sent. The requests decoded
   earlier from the same read are dropped with it (`clients.rs:99-103`,
   `service.rs:1897-1903`). A request larger than 71 680 bytes is one such case.
   The reference client refuses to send one (section 11).
5. **Read end.** End of stream, or a read error, closes the connection
   (`clients.rs:85-96`).
6. **Write error.** A write error closes the connection (`clients.rs:178-181`).
7. **Close.** `drop_closed` runs (`service.rs:1919-1943`):
   - The connection's subscriptions and frame watches are removed.
   - `client_closed` runs for every connection, greeted or not:
     - Raw claims: without `keep`, the connection's claims and locks are
       released. With `keep`, they stay held with no holder, until released by
       name (`raw.rs:530-546`).
     - Control writes: without `keep`, they are reverted, and a `Control` event
       with `restore` goes to events clients. With `keep`, they stay
       (`service_raw.rs:84-108`).
     - LED intents: forgotten, unless `keep` is set or another connection shares
       its owner id (`service.rs:1935-1940`).
     - GPU leases, held or queued: ended whatever `keep` says, and the next
       waiters granted (6.6).
8. **Falling behind.** Answers are never dropped (section 13). A connection
   with more than 4 MiB of answers waiting has stopped reading, and is closed.

The service sends nothing unprompted except the messages in section 9. There is
no keepalive, no ping, and no idle timeout.

## 11. Client library behaviour

These rules are the reference client's (`lemnos-ipc/src/client.rs`). A client in
another language need not follow them, but the server's behaviour in section 10
is what it will see.

- **Timeouts.** Connecting sets the socket's read and write timeouts to the
  client's timeout. The default is 2 s (`client.rs:30`, `:313-315`). Each
  request waits up to that long for its answer (`client.rs:430-447`).
- **Waiting for an answer.** While a request waits, other messages are queued
  as events. A late answer is not discarded. It arrives later as an ordinary
  message (`client.rs:430-447`).
- **Reconnecting.** Off unless asked for. Retries back off from 100 ms, doubling
  up to 5 s, and reset to 100 ms after a successful connect (`client.rs:31-32`,
  `:286-298`, `:506-516`, `:368`).
- **After a (re)connect.** The client re-sends, in order: `Hello`, `List`, each
  subscription with id 0 (`Subscribe` or `SubscribeChannels`), then held LED
  requests (`client.rs:318-360`).
- **Subscriptions.** `Subscribe` with period 0 ends the subscription. A refused
  subscription is not kept (`client.rs:609-629`).
- **Events flag.** Device clients send `events = 1` by default. LED clients send
  0 (`client.rs:227-239`, `:323`).
- **Priority.** Default 50 (`client.rs:173`).
- **Bad frames.** A message that does not decode drops the connection: the
  buffer is cleared, `Disconnected` is reported, and the call returns the
  protocol error. A client that reconnects starts clean.
- **Frame size.** A request that would encode to more than 71 680 bytes is not
  sent. The call returns a protocol error, and the connection stays up.

## 12. Compatibility rules

Only what the code does:

- **Appended fields.** These were added within protocol 1. Older peers stop
  before them, and the decoder fills in a default when the bytes are absent.
  - `Hello.events`: absent means 1 (`request.rs:381-382`).
  - `Subscribe.id`: absent means 0 (`request.rs:411-412`).
  - LED `flags` and `id`: absent means 0 and 0 (`request.rs:491-493`).
  - `Devices` reasons: absent means empty reasons (`message.rs:265-270`).
  - `Event::Status.reason`: absent means empty (`message.rs:324-328`).
  - `LightInfo.look_brightness`: absent means 1000 (`message.rs:385-386`).
  - `Welcome.features`: absent means 0, no features (`message.rs`).
- **Trailing bytes.** No decoder rejects trailing bytes, by design: that is
  how an older peer skips a field appended after its time. Only the appended
  fields above check for their presence.
- **Unknown request kind.** A decode error, and the service closes the
  connection (`wire/request.rs:570`).
- **Unknown message kind.** A decode error on the client (`message.rs:415`).
- **Unknown sub-tags are errors.** The decoder refuses them, and the connection
  is closed on the service side. The list:
  - LED show (`request.rs:481`), `LedInputs` spots flag (`request.rs:519`),
    looks operation (`request.rs:558`), system state (`request.rs:120`).
  - Calibration command (`calibration.rs:44`) and routine (`calibration.rs:23`).
  - Event tag (`message.rs:348`).
  - Look: layer count above 4 (`look.rs:269`), block code (`look.rs:388`),
    envelope code (`look.rs:263`), arc fraction kind (`look.rs:292`), spots
    source (`look.rs:378`), look mode (`look.rs:391`), breathe and drain easing
    codes (`look.rs:250`, `:336`), spot count above 8 (`look.rs:188`), frame
    above 64 pixels (`look.rs:313`), sparkle colour count outside 1 to 4
    (`look.rs:348`).
- **Unknown codes in enums decode to a fallback** (section 4), with no error.
  This covers the device enums, the raw bias, drive and edge codes, the LED
  status and effect codes, the easing code of an LED request, and system phase.
- **New request kinds are gated by a feature bit.** The GPU lease kinds (6.6)
  are sent only to a service whose `Welcome` sets its bit, so an older service
  never sees them. The service sends `GpuLease` only to a client that asked for
  a lease, so an older client never sees it.
- **No rule is given for reserving kinds.** Each kind number is used once in the
  source. A new kind is refused by an older decoder.

## 13. Limits and queueing

Frame limits (section 2): requests 71 680, messages 1 048 576.

Fixed limits in the service and library:

| Limit | Value | Source |
|---|---|---|
| Per-client queue (readings and events only) | 64 KiB. Above it, readings go first, then events. | `lemnosd/src/clients.rs:13` |
| Readings and events drained | Once the queue is below 32 KiB, the service sends one `Dropped` with the count | `lemnosd/src/clients.rs:154-161` |
| Watch rate | At most 60 fps | `lemnosd/src/service.rs:58` |
| LEDs on a light | 64 | `lemnosd/src/light.rs:11` |
| Look layers | 4 | `lemnos-light/src/look.rs:17` |
| Look frame pixels | 64 | `lemnos-light/src/look.rs:19` |
| Sparkle colours | 4 | `lemnos-light/src/sparkle.rs:23` |
| Spots per list | 8 | `lemnos-light/src/spots.rs:28` |
| I2C bytes per transaction | 4096 | `lemnosd/src/raw.rs:26` |
| SPI bytes per transaction | 65 536 | `lemnosd/src/raw.rs:28` |
| Client read buffer | 1 KiB, grows for large requests and shrinks back | `lemnosd/src/clients.rs:16-17` |

A subscription's granted period is the requested one, or the device's cap if
longer, or its last read's duration if longer
(`lemnosd/src/service.rs:766-790`, `:794-824`). A read that is fresher than the
device's cap is served from the last reading.

The queue rules, exactly as coded (`lemnosd/src/clients.rs:112-162`):

- Readings are dropped first, oldest first, silently.
- Events are dropped next, and each drop is counted. The count goes out as one
  `Dropped` once the queue is below half its limit.
- Every other message is queued, and is never dropped. Past 4 MiB of them the
  client is closed (`lemnosd/src/clients.rs`, `MAX_ANSWERS_QUEUED`).
- The frame being written is never dropped.

## 14. Gaps and inconsistencies

Facts about the code as it stood when this file was written. Entries marked fixed have been changed since. The rest stand.

1. **Fixed (2026-11-05):** the client refuses a mismatched `Welcome` (section 5). Was: **Version is never checked.** Neither `Hello` nor `Welcome` is compared
   with `VERSION` (section 5).
2. **Fixed (2026-11-05):** edges belong to the claim. The `Hello` doc now says so. Was: **Edge events ignore the events flag.** The `Request::Hello` doc says that
   without `events` the service sends no events (`wire/mod.rs:395-398`). Edges
   still go to a greeted claimant (`lemnosd/src/service_raw.rs:30-40`).
3. **Fixed (2026-11-05):** past 4 MiB of answers the client is closed. Was: **Only readings and events are bounded.** Replies, `Text`, `Claimed`, `Data`,
   `Devices`, `Stats`, `Welcome`, `Frame` and `LightInfo` are queued without a
   limit (`lemnosd/src/clients.rs:115-122`, `:29-36`). A client that stops
   reading makes the service's memory grow for that client. The crate's own
   description of the queue (`lemnos-ipc/src/lib.rs`) says that replies and
   frames are never dropped, which is accurate. It does not say they are
   unbounded.
4. **Fixed (2026-11-05):** a bad frame drops the connection (section 11). Was: **The client does not close on a bad message.** When `decode_message` fails,
   the bytes stay in the buffer and the client returns the protocol error. The
   next receive decodes the same bytes and fails the same way
   (`lemnos-ipc/src/client.rs:392-395`). The client stays connected. A caller
   must reconnect itself.
5. **Fixed (2026-11-05):** `client_closed` runs for every connection, and an ungreeted connection owns its intents alone (section 10). Was: **Requests from a connection that never sent `Hello` are cleaned up only if
   the connection is greeted.** `drop_closed` calls `client_closed` only for
   greeted connections (`lemnosd/src/service.rs:1933-1934`). `client_closed`
   is what ends raw claims (`service_raw.rs:85`), reverts control writes, and
   forgets LED intents. So a connection that writes or claims without `Hello`
   leaves those in place after it closes.
6. **Fixed (2026-11-05):** the service answers with a `Text` error naming the light. Was: **A refused LED request gets no answer.** A `Led` or `LedInputs` from a
   client the device's write policy does not allow is ignored without a reply,
   even with a nonzero id (`lemnosd/src/service.rs:1376-1378`, `:1451-1453`).
   The client waits for its timeout.
7. **Fixed (2026-11-05):** the decoder takes 0 layers, and the service refuses the look with a reply. Was: **An inline look with no layers closes the connection.** The encoder writes a
   layer count of 0 for a look with no layers (`wire/look.rs:42-43`). The
   decoder refuses 0 (`wire/look.rs:267-271`), so the service drops the
   connection. The test that found this is described in section 15.
8. **Kept as the rule (2026-11-06):** trailing bytes are not checked (section 12).
   This is how an appended field reaches an older peer, which stops before it, so
   refusing a tail would break every appended field.
9. **Fixed (2026-11-06):** a long string is cut at a character boundary, so the frame still decodes. Was: **Strings are cut, not refused.** A `str` over 65 535 bytes is truncated on
   encode, possibly inside a UTF-8 sequence, and the decoder then refuses the
   frame (`wire/codec.rs:54-59`).
10. **A limit of protocol 1:** fixing it changes the wire, so it waits for a version 2. **Optional sentinels collide with values.** `Some(0xFFFF)` as `opt u16` and
    `Some(0xFFFFFFFF)` as `opt u32` decode as None (`wire/codec.rs:141`,
    `:144`). A sender must avoid these values.
11. **Fixed (2026-11-06):** a line target kind or I2C op tag other than 0 or 1 is refused. Was: **Lenient tags.** The raw line target treats any nonzero kind as a name
    (`wire/raw.rs:354-363`). The I2C op decoder treats any nonzero tag as a
    read (`wire/raw.rs:426-429`). The encoder writes 0 and 1.
12. **Kept (2026-11-06):** `SubscribeChannels` was added with its id, so no older sender omits it; `Subscribe` gained its id later. **`SubscribeChannels` requires its id.** Its decoder reads `u32 id`
    unconditionally (`wire/request.rs:402-405`). `Subscribe` accepts the
    absence of its id. The two are not symmetric.
13. **Fixed (2026-11-05):** the reference client refuses to send an oversized request. Was: **Frame-size arithmetic.** The comment on `MAX_REQUEST_FRAME`
    (`wire/mod.rs:31-33`) reasons from payload bytes only. Each SPI transfer
    adds 15 bytes of header, and each I2C op adds 5. A request within the
    64 KiB SPI byte limit can exceed the 71 680-byte frame limit when it has
    many transfers. The service then closes the connection and sends no
    refusal.
14. **A limit of protocol 1:** fixing it changes the wire, so it waits for a version 2. A client should not overlap reads of one device. **Reads are matched by device name.** A `Reading` has no id. Two
    overlapping `Read` calls for one device cannot be told apart.
15. **Fixed (2026-11-06):** a repeated `Hello` is answered with `Welcome` and changes nothing. Was: **Hello repeats are allowed.** A second `Hello` changes the name, owner,
    priority and flags, and sends a second `Welcome` (section 10, step 3).
16. **Fixed (2026-11-06):** a reason count that does not match is refused. Was: **Devices reasons are dropped silently** when their count does not match
    (`wire/message.rs:265-269`).
17. **Fixed (2026-11-06):** `lemnosd` sets the socket to 0660 after binding. Was: **Socket mode comes from the umask.** The code sets no permissions. The
    packaged unit's umask and directory mode are what keep the socket
    private (section 1).

## 15. Test vectors

Each vector below is one complete frame: the 4-byte length, the kind, and the
payload. The hex is the frame's bytes in order. Each vector's annotation lists
the byte offsets.

The vectors are checked by `crates/lemnos-ipc/src/spec_vectors.rs`. The test
reads this file. It finds each vector by its marker line, parses the hex,
builds the same value with the crate's types, and asserts equality in both
directions. It also asserts that every marker here has a pin there, and the
reverse.

### 15.1 `hello`

`Request::Hello { version: 1, client: "example", priority: 50, keep: false, events: true }`

<!-- vector: hello -->
```hex
10 00 00 00 01 00 01 00 07 00 65 78 61 6d 70 6c 65 32 00 01
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `10 00 00 00` | length = 16 |
| 4-5 | `01 00` | kind 1 (`Hello`) |
| 6-7 | `01 00` | version 1 |
| 8-9 | `07 00` | client name length 7 |
| 10-16 | `65 78 61 6d 70 6c 65` | "example" |
| 17 | `32` | priority 50 |
| 18 | `00` | keep false |
| 19 | `01` | events true |

### 15.2 `subscribe`

`Request::Subscribe { id: 7, device: "imu", period_ms: 10 }`

<!-- vector: subscribe -->
```hex
0f 00 00 00 04 00 03 00 69 6d 75 0a 00 00 00 07 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `0f 00 00 00` | length = 15 |
| 4-5 | `04 00` | kind 4 (`Subscribe`) |
| 6-7 | `03 00` | device length 3 |
| 8-10 | `69 6d 75` | "imu" |
| 11-14 | `0a 00 00 00` | period 10 ms |
| 15-18 | `07 00 00 00` | id 7 |

### 15.3 `i2c-transfer`

`Request::Raw(RawRequest::I2cTransfer { id: 5, bus: "i2c-1", address: 0x48, ops: [Write([0x00]), Read(2)] })`

<!-- vector: i2c-transfer -->
```hex
1c 00 00 00 1b 00 05 00 00 00 05 00 69 32 63 2d 31 48 00 02 00 00 01 00 00 00 00 01 02 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `1c 00 00 00` | length = 28 |
| 4-5 | `1b 00` | kind 27 (`I2cTransfer`) |
| 6-9 | `05 00 00 00` | id 5 |
| 10-11 | `05 00` | bus length 5 |
| 12-16 | `69 32 63 2d 31` | "i2c-1" |
| 17-18 | `48 00` | address 0x48 |
| 19-20 | `02 00` | op count 2 |
| 21 | `00` | op 1 tag: write |
| 22-25 | `01 00 00 00` | write length 1 |
| 26 | `00` | write byte 0x00 |
| 27 | `01` | op 2 tag: read |
| 28-31 | `02 00 00 00` | read count 2 |

### 15.4 `led-look`

`Request::Led(LedRequest { device: "", show: Look { name: "arc", progress: Some(250) }, effect: None, period_ms: None, depth: None, brightness: Some(500), fade_ms: None, easing: None, duration_ms: Some(3000), test: false, id: 4 })`

<!-- vector: led-look -->
```hex
2b 00 00 00 07 00 00 00 0a 03 00 61 72 63 fa 00 00 ff ff ff ff ff ff f4 01 ff ff ff ff 00 00 00 00 00 00 00 00 00 b8 0b 00 00 00 04 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `2b 00 00 00` | length = 43 |
| 4-5 | `07 00` | kind 7 (`Led`) |
| 6-7 | `00 00` | device "" (the status light) |
| 8 | `0a` | show tag 10 (`Look`) |
| 9-10 | `03 00` | name length 3 |
| 11-13 | `61 72 63` | "arc" |
| 14-15 | `fa 00` | progress 250 |
| 16 | `00` | effect none |
| 17-20 | `ff ff ff ff` | period None |
| 21-22 | `ff ff` | depth None |
| 23-24 | `f4 01` | brightness 500 |
| 25-28 | `ff ff ff ff` | fade None |
| 29 | `00` | easing none |
| 30-37 | `00 00 00 00 00 00 00 00` | four easing parameters, zero |
| 38-41 | `b8 0b 00 00` | duration 3000 ms |
| 42 | `00` | flags (test false) |
| 43-46 | `04 00 00 00` | id 4 |

### 15.5 `welcome`

`Message::Welcome { version: 1, board: "example-board", client_id: 3, features: 1 }`

<!-- vector: welcome -->
```hex
1b 00 00 00 65 00 01 00 0d 00 65 78 61 6d 70 6c 65 2d 62 6f 61 72 64 03 00 00 00 01 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `1b 00 00 00` | length = 27 |
| 4-5 | `65 00` | kind 101 (`Welcome`) |
| 6-7 | `01 00` | version 1 |
| 8-9 | `0d 00` | board length 13 |
| 10-22 | `65 78 61 6d 70 6c 65 2d 62 6f 61 72 64` | "example-board" |
| 23-26 | `03 00 00 00` | client id 3 |
| 27-30 | `01 00 00 00` | features: bit 0, the GPU lease |

A service from before the features field sends the same frame without bytes
27-30 (length 23); it decodes with features 0.

### 15.6 `reading`

`Message::Reading(RawReading { device: "imu", timestamp_us: 1_000_000, status: Available, values: [9806, NO_VALUE, -3] })`

The middle channel has no value. Its count is `NO_VALUE`. The first count times
its exponent gives the value; the exponent comes from the device list, not from
this frame.

<!-- vector: reading -->
```hex
1e 00 00 00 67 00 03 00 69 6d 75 40 42 0f 00 00 00 00 00 00 03 00 4e 26 00 00 00 00 00 80 fd ff ff ff
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `1e 00 00 00` | length = 30 |
| 4-5 | `67 00` | kind 103 (`Reading`) |
| 6-7 | `03 00` | device length 3 |
| 8-10 | `69 6d 75` | "imu" |
| 11-18 | `40 42 0f 00 00 00 00 00` | timestamp 1 000 000 us (LE u64) |
| 19 | `00` | status Available |
| 20-21 | `03 00` | value count 3 |
| 22-25 | `4e 26 00 00` | 9806 |
| 26-29 | `00 00 00 80` | NO_VALUE (`i32::MIN`) |
| 30-33 | `fd ff ff ff` | -3 |

The fast path `encode_reading` must produce the same bytes. The test checks it.

### 15.7 `reply-refused`

`Message::Reply { id: 4, result: Err(Refusal::Device(ErrorKind::Busy)) }`

<!-- vector: reply-refused -->
```hex
10 00 00 00 68 00 04 00 00 00 05 02 00 00 00 00 00 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `10 00 00 00` | length = 16 |
| 4-5 | `68 00` | kind 104 (`Reply`) |
| 6-9 | `04 00 00 00` | id 4 |
| 10 | `05` | refusal code 5 (`Device`) |
| 11 | `02` | kind 2 (`Busy`) |
| 12-19 | `00 00 00 00 00 00 00 00` | value 0.0 |

### 15.8 `reply-ok`

`Message::Reply { id: 2, result: Ok(0.5) }`

<!-- vector: reply-ok -->
```hex
10 00 00 00 68 00 02 00 00 00 00 00 00 00 00 00 00 00 e0 3f
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `10 00 00 00` | length = 16 |
| 4-5 | `68 00` | kind 104 (`Reply`) |
| 6-9 | `02 00 00 00` | id 2 |
| 10 | `00` | code 0 (ok) |
| 11 | `00` | kind (unused) |
| 12-19 | `00 00 00 00 00 00 e0 3f` | value 0.5 (f64 LE) |

### 15.9 `devices`

`Message::Devices(vec![DeviceDesc { id: "imu", label: "IMU", class: Imu, model: "bmi088", status: Available, reason: "", channels: [acceleration.x (Acceleration, X, exponent -3), angular_rate.z (AngularRate, Z, exponent -6)], controls: [], pixels: 0 }])`

<!-- vector: devices -->
```hex
48 00 00 00 66 00 01 00 03 00 69 6d 75 03 00 49 4d 55 00 06 00 62 6d 69 30 38 38 00 00 00 02 00 0e 00 61 63 63 65 6c 65 72 61 74 69 6f 6e 2e 78 00 01 fd 0e 00 61 6e 67 75 6c 61 72 5f 72 61 74 65 2e 7a 01 03 fa 00 00 01 00 00 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `48 00 00 00` | length = 72 |
| 4-5 | `66 00` | kind 102 (`Devices`) |
| 6-7 | `01 00` | device count 1 |
| 8-9 | `03 00` | device id length 3 |
| 10-12 | `69 6d 75` | "imu" |
| 13-14 | `03 00` | label length 3 |
| 15-17 | `49 4d 55` | "IMU" |
| 18 | `00` | class 0 (`Imu`) |
| 19-20 | `06 00` | model length 6 |
| 21-26 | `62 6d 69 30 38 38` | "bmi088" |
| 27 | `00` | status 0 (`Available`) |
| 28-29 | `00 00` | pixels 0 |
| 30-31 | `02 00` | channel count 2 |
| 32-33 | `0e 00` | channel 1 name length 14 |
| 34-47 | `61 63 63 65 6c 65 72 61 74 69 6f 6e 2e 78` | "acceleration.x" |
| 48 | `00` | quantity 0 (`Acceleration`) |
| 49 | `01` | axis 1 (`X`) |
| 50 | `fd` | exponent -3 |
| 51-52 | `0e 00` | channel 2 name length 14 |
| 53-66 | `61 6e 67 75 6c 61 72 5f 72 61 74 65 2e 7a` | "angular_rate.z" |
| 67 | `01` | quantity 1 (`AngularRate`) |
| 68 | `03` | axis 3 (`Z`) |
| 69 | `fa` | exponent -6 |
| 70-71 | `00 00` | control count 0 |
| 72-73 | `01 00` | reason list count 1 |
| 74-75 | `00 00` | reason "" |

### 15.10 `event-control`

`Message::Event(Event::Control { device: "fan0", control: "duty", value: 0.5, by: "example" })`

<!-- vector: event-control -->
```hex
20 00 00 00 69 00 01 04 00 66 61 6e 30 04 00 64 75 74 79 00 00 00 00 00 00 e0 3f 07 00 65 78 61 6d 70 6c 65
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `20 00 00 00` | length = 32 |
| 4-5 | `69 00` | kind 105 (`Event`) |
| 6 | `01` | event tag 1 (`Control`) |
| 7-8 | `04 00` | device length 4 |
| 9-12 | `66 61 6e 30` | "fan0" |
| 13-14 | `04 00` | control length 4 |
| 15-18 | `64 75 74 79` | "duty" |
| 19-26 | `00 00 00 00 00 00 e0 3f` | value 0.5 (f64 LE) |
| 27-28 | `07 00` | writer length 7 |
| 29-35 | `65 78 61 6d 70 6c 65` | "example" |

### 15.11 `data-reply`

`Message::Data { id: 5, result: Ok([0x12, 0x34]) }`

<!-- vector: data-reply -->
```hex
0e 00 00 00 6b 00 05 00 00 00 00 00 02 00 00 00 12 34
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `0e 00 00 00` | length = 14 |
| 4-5 | `6b 00` | kind 107 (`Data`) |
| 6-9 | `05 00 00 00` | id 5 |
| 10 | `00` | code 0 (ok) |
| 11 | `00` | kind (unused) |
| 12-15 | `02 00 00 00` | byte count 2 |
| 16-17 | `12 34` | the bytes |

### 15.12 `gpu-acquire`

`Request::GpuAcquire { id: 6, acquire: GpuAcquire { gpu: "v3d", mode: Exclusive, priority: 50, wait: true, max_hold_ms: 60000, label: "bench" } }`

<!-- vector: gpu-acquire -->
```hex
19 00 00 00 12 00 06 00 00 00 03 00 76 33 64 00 32 01 60 ea 00 00 05 00 62 65 6e 63 68
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `19 00 00 00` | length = 25 |
| 4-5 | `12 00` | kind 18 (`GpuAcquire`) |
| 6-9 | `06 00 00 00` | id 6 |
| 10-11 | `03 00` | gpu length 3 |
| 12-14 | `76 33 64` | "v3d" |
| 15 | `00` | mode 0 (exclusive) |
| 16 | `32` | priority 50 |
| 17 | `01` | wait |
| 18-21 | `60 ea 00 00` | max_hold_ms 60000 |
| 22-23 | `05 00` | label length 5 |
| 24-28 | `62 65 6e 63 68` | "bench" |

### 15.13 `gpu-lease`

`Message::GpuLease { id: 6, lease: 1, state: Queued { ahead: 2 } }`

<!-- vector: gpu-lease -->
```hex
0f 00 00 00 71 00 06 00 00 00 01 00 00 00 01 00 00 02 00
```

| Offset | Bytes | Field |
|---|---|---|
| 0-3 | `0f 00 00 00` | length = 15 |
| 4-5 | `71 00` | kind 113 (`GpuLease`) |
| 6-9 | `06 00 00 00` | id 6 |
| 10-13 | `01 00 00 00` | lease 1 |
| 14 | `01` | state 1 (queued) |
| 15-16 | `00 00` | refusal code and kind (unused) |
| 17-18 | `02 00` | ahead 2 |
