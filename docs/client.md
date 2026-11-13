# The client library and the C API

A program uses devices through the daemon's socket. There are three ways to speak to it: the Rust
client, the C API over that same client, or the [socket protocol](wire.md) itself, which needs
nothing from this repository.

The commands assume the daemon is running on the simulated board:

```sh
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/sim-board.toml &
```

## The Rust client

`lemnos-client` (`engine/client`) comes in three forms over one connection core:

| Type | Use it when |
|---|---|
| `Client` | Blocking calls. It can be shared between threads |
| `Session` | One thread owns it. `send_call` and `poll` never wait, for a loop that must not block |
| `AsyncClient` | The same calls as futures, with the `tokio` feature |

A whole program, `engine/client/examples/devices.rs`:

```rust file=engine/client/examples/devices.rs
//! Lists the devices a service has, then reads the first accelerometer through its contract.
//!
//! `cargo run -p lemnos-client --example devices -- SOCKET`

use std::io::Write;

use lemnos_client::Client;
use lemnos_contract_accelerometer::Accelerometer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args().nth(1).ok_or("usage: devices SOCKET")?;
    // The second argument is this program's name, as the service will know it.
    let client = Client::connect(&socket, "devices-example")?;
    let mut out = std::io::stdout().lock();

    for device in client.list()? {
        writeln!(
            out,
            "{} ({}): {:?}, {:?}, provides {}",
            device.name,
            device.driver,
            device.state,
            device.health,
            device.tags.join(" ")
        )?;
    }

    // An empty name means any device that provides the contract.
    let mut accelerometer = client.open::<dyn Accelerometer>("")?;
    let sample = accelerometer
        .read()
        .map_err(|status| format!("read refused: {status:?}"))?;
    writeln!(out, "x={} y={} z={} m/s²", sample.x, sample.y, sample.z)?;
    Ok(())
}
```

```sh
cargo run -q -p lemnos-client --example devices -- "$LEMNOSD_SOCKET"
```

What a client can do:

- **`list`, `find`**: the devices, and the first device with a tag. Each entry has the device's
  name, driver, tags, state, health, reason and `link`. `follow` walks the links.
- **`open::<dyn Contract>(name)`**: a typed handle. Its methods are the contract's own. An empty
  name means any device that provides the contract. The contract traits are the crates in
  `contracts/`.
- **`open_tag`, `describe`, `call`**: the same without the contract's types, for a tool that learns
  a device from its description. Arguments and replies are bytes, encoded as the
  [protocol](wire.md) says.
- **`subscribe`, `next_value`**: the daemon reads a method at a period and sends each value.
- **`set_power`**: off and on.
- **`start_job`, `wait_job`, `cancel_job`**: a long job. A typed handle's job method starts it and
  returns its number. `wait_job` passes each progress report to a closure and returns the job's
  status and result when it ends. Progress may skip steps: only the latest is kept. A `Session`
  starts one with `send_job` and sees `Incoming::JobStarted` and `Incoming::Job` from `poll`. A job
  open when the connection is lost ends as `INTERRUPTED`.
- **`usb_endpoints`, `usb_endpoints_release`**: a USB function's two bulk endpoints (`usb.function/1`), lent to this connection as `out` and `input` descriptors that the caller reads and writes itself. One connection holds them at a time; another gets `BUSY`. The caller passes the stream platform that takes descriptors off the socket (`lemnos-linux-stream`'s `LinuxStreams` on Linux), as for a stream. See [the wire's Endpoints section](wire.md#endpoints).
- **`open_stream`, `open_stream_writable`**: a device's [frame stream](streams.md). The stream says its buffer length (`buffer_len`) and count (`buffer_count`).
  `close_stream`
  gives it back; dropping it does the same, with the close sent on the client's next request. Closing
  the client gives up every stream it holds.
- **`packages`**: what became of the daemon's driver packages.
- **`next_event`**: a device's state, reason or health changed. Events come when the client asked
  for them in its Hello (the default). They are read off the socket inside any call and in
  `next_value`, and `next_event` only takes what has arrived by then. After a list, a client gets
  every later change of each device's state, reason or health, including a change of health with
  no change of state (about a quarter of a second after it), and a device that comes up after the
  list. An event may repeat a state the client has; the newest one is current. Up to 64 events
  queue. Past that the oldest go, so a client that stops reading should list the devices again.

### Device descriptions

`Client::describe(device)` returns a `DeviceDescription`. Besides its contracts and items it has
what a generic panel shows, with no code for the device:

- `label`, the board's name for the device (its id when the board names none);
- `kind`, the grouping its contracts give it (`sensor`, `power`, ...), `other` when none;
- `bus` (empty when none) and `address` (`Option<u8>`, the 7-bit I2C address);
- `state` (`Option<u8>`: 0 running, 1 retrying, 2 off) and `reason`;
- for each item, `reading` (`Option<Reading>`), `step`, `warn_low`, `warn_high` and `group`, next to
  its `unit`, `exponent`, `min` and `max`.

A value an older host does not describe reads as unknown (`None`, or empty text). `lemnos-ctl show`
prints exactly these, one panel per device.

### Versions at connect

The first frame each side sends carries the build's core hash and its Lemnos version, then the
hashes of its wire parts (see [`wire.md`](wire.md#core-and-parts)). A client that finds the
service's core hash different from its own refuses at once with `Error::LayoutMismatch`, whose text
is `core protocol differs`, with both hashes and the service's Lemnos version. A part that differs
is refused with `Error::PartMismatch`, which names the part. Both come in every form:
`Client::connect`, `Session::connect`, `AsyncClient::connect`, and the C API, which returns
`VERSION_MISMATCH` with the same text. Only the core and the parts are compared at connect. The
Lemnos version string is not: a client and a service from different revisions interoperate when
the interface they use is the same. Nothing else is checked for a refused connection.

Every typed open runs the bind check once, when it opens: `Client::open::<C>` on the blocking
client, `Session::open_typed::<C>` on a session, and `AsyncClient::open_typed::<C>` on the async
client. The rule, and the refusal texts, are under [The typed bind](#the-typed-bind). That read of
the description is the only extra cost of the check. Calls on the open handle, and every other path,
pay nothing for it: the steady-state calls allocate nothing, which `engine/client/tests/alloc.rs`
measures.

### The typed bind

A typed client was built with a contract `C`: its tag, its base hash and each of its methods, base
and added. Before it calls anything, the bind check compares them with the device's description, in
this order:

1. the device provides the contract's tag (else `ContractMissing`: the device does not provide it);
2. the device's base hash for the contract equals the client's base hash (else
   `ContractMismatch`, which names the contract and both hashes: a base method is laid out
   differently);
3. for every method the client has, the device has a method with the same number and name and the
   same method hash (else the refusal below); and the device has every added field the client's
   method uses, by the field's hash (else the field refusal below).

Extra methods and fields on the device are ignored, so a new driver serves an old client. The
refusal texts, for contract `camera.sensor/1`, method 12 `identity`:

- ``contract camera.sensor/1: this client needs method 12 `identity`; the device's driver does not provide it``
  (`MethodMissing`, `NOT_SUPPORTED`);
- ``contract camera.sensor/1: this client needs method 12 `identity`; the device's driver has another shape``
  (`MethodShape`, `VERSION_MISMATCH`);
- ``contract camera.sensor/1: this client needs field `embedded_data` of method 1 `info`; the device's driver does not provide it``
  (`FieldMissing`, `NOT_SUPPORTED`);
- ``contract camera.sensor/1: a base method is laid out differently (the device's base hash ..., this client's ...)``
  (`ContractMismatch`, `VERSION_MISMATCH`).

Nothing is called on a contract the check refuses. The wire carries no per-contract additions level:
the per-method check above gives the same guarantee, and says which method is missing.

### Typed opens and the C API

`lemnos_contract_hash` writes the base hash of the contract an open handle is for, as the device's
description has it, and its additions level (how many of its methods are additions). A C caller
compares the hash with the one its header was built with. The C API has no typed open: `lemnos_open`
takes a tag and checks nothing, so a C caller that needs the bind check compares the hash itself.

### Untyped tools

`lemnos-ctl` and the Orion bridge are untyped by design. They learn each device from its description
and call by method number and bytes, so they do not run the bind check and do not refuse a contract
at open: they are built without the contract traits and need not be built from the daemon's Lemnos
revision. A method the device's driver does not provide is refused by name, as `NOT_SUPPORTED`, the
same as for any caller.

The connection looks after itself. If the daemon restarts, the client reconnects, subscribes again
and finds its handles again by name and tag. A call made while the daemon is away fails with
`Disconnected`; the next one after it is back works. `Client::connecting` returns at once and
connects whenever the daemon appears.

A call answers `BUSY` when the device's driver is in another call or a tick right then. The host
never waits on a driver, because a driver may be stuck. Ask again.

Steady-state calls, values and frames allocate nothing in the client.

## The C API

`lemnos-capi` (`engine/capi`) is the same client behind `extern "C"` functions, declared in
`include/lemnos.h`. It builds as a static and a shared library. The header is the reference: every
function, struct and code is there with its rule.

| Function | What it does |
|---|---|
| `lemnos_version`, `lemnos_check_version` | The ABI handshake. Call `lemnos_check_version` first |
| `lemnos_client_open`, `lemnos_client_close` | Connect and disconnect |
| `lemnos_device_count`, `lemnos_device_info`, `lemnos_device_text` | The device list |
| `lemnos_open` | Open a contract on a device, by tag |
| `lemnos_contract_hash` | The base hash and additions level of an opened contract, as the device describes it |
| `lemnos_call` | Call a method by number, with its arguments and reply as bytes |
| `lemnos_job_start`, `lemnos_job_cancel`, `lemnos_job_next` | Long jobs: start one, cancel it, and read its progress and its end |
| `lemnos_subscribe`, `lemnos_unsubscribe`, `lemnos_next_value` | Values at a period |
| `lemnos_set_power` | Off and on |
| `lemnos_item_count`, `lemnos_item_info`, `lemnos_item_text` | What a device says about itself |
| `lemnos_package_list`, `lemnos_package_info`, `lemnos_package_text` | What became of the daemon's driver packages |
| `lemnos_stream_open`, `lemnos_stream_info`, `lemnos_stream_close` | Take a device's [frame stream](streams.md), read-only or `LEMNOS_STREAM_WRITABLE`, and give it back. Waits for the service's answer, up to the client's timeout. `lemnos_client_close` gives up every stream still open |
| `lemnos_stream_next_frame`, `lemnos_stream_release` | Take the next frame, read in place, and give its buffer back |
| `lemnos_stream_read`, `lemnos_stream_write`, `lemnos_stream_post_job` | Copy out of a buffer or into one, and post a job |
| `lemnos_stream_buffer_handle` | A new descriptor of a buffer's own dma-buf, to hand to a GPU or an encoder |
| `lemnos_stream_plane`, `lemnos_stream_plane_handle` | On a stream whose buffers are in planes: a held frame's part in one plane, read in place, and a new descriptor of one plane's dma-buf |

Every call returns a status: 0 for success, a negative number for an error, and a positive count
where the call says so. Output structs start with their own size. Text comes back through a
callback or into a buffer the caller gives. A panic inside the library comes back as a status and
never unwinds into C.

A stream works as it does in the Rust client. The Rust wrapper's stream is closed when it is dropped; a C program closes its stream with `lemnos_stream_close`, or gives every stream up with
`lemnos_client_close`. `lemnos_stream_next_frame` gives the frame's bytes
in place, valid until the frame is released, and allocates nothing. On a stream of dma-bufs
(`LEMNOS_STREAM_BUFFER_HANDLES` in its info) each buffer has its own descriptor, and
`lemnos_stream_buffer_handle` gives the caller a new one to close when done. A stream of shared
memory has none and answers `-EOPNOTSUPP`. When the buffers are in planes (`planes` in the info is
above 1, from the board file's `planes`), each plane is a dma-buf of its own: a frame has no one
place in memory, so `lemnos_stream_next_frame` gives its data as `NULL`, `lemnos_stream_plane`
gives each plane's part, `lemnos_stream_plane_handle` each plane's descriptor, and
`lemnos_stream_read` and `lemnos_stream_write` address the planes end to end. Calls on one stream wait for each other, never for the
client's other calls, so one thread can wait for frames while another calls methods. The C tests
(`engine/capi/tests/c/`) run each of these against the daemon on the simulated board.

Install the headers, both libraries and their `pkg-config` files under a prefix:

```sh slow
scripts/install-capi.sh "$TMPDIR/lemnos"
```

A whole program, `docs/examples/devices.c`:

```c file=docs/examples/devices.c
/* Lists the devices a Lemnos service has, through the C API.
 *
 *   cc devices.c $(pkg-config --cflags --libs lemnos-capi) -o devices
 *   ./devices SOCKET
 */
#include <stdio.h>
#include <string.h>

#include "lemnos.h"

/* Receives an error's text. It is not NUL-terminated. */
static void collect(void *context, const uint8_t *text, size_t len) {
    char *out = context;
    if (len > 255) len = 255;
    memcpy(out, text, len);
    out[len] = 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: devices SOCKET\n");
        return 2;
    }
    char message[256] = {0};
    LemnosMessageSink sink = {message, collect};

    /* First, check that this program and the library agree on the interface. */
    LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
    int32_t status = lemnos_check_version(&mine);
    if (status != LEMNOS_OK) {
        fprintf(stderr, "version: status %d\n", status);
        return 1;
    }

    LemnosHandle client = 0;
    status = lemnos_client_open(argv[1], "devices-example", 0, &client, sink);
    if (status != LEMNOS_OK) {
        fprintf(stderr, "open: status %d %s\n", status, message);
        return 1;
    }

    /* A count is a positive status. */
    int32_t count = lemnos_device_count(client);
    if (count < 0) {
        fprintf(stderr, "list: status %d\n", count);
        return 1;
    }
    for (int32_t device = 0; device < count; device++) {
        char name[64], driver[64], tags[256];
        size_t len = 0;
        LemnosDeviceInfo info = {sizeof info, 0, 0, 0};
        if (lemnos_device_text(client, device, LEMNOS_DEVICE_NAME, name, sizeof name, &len) != LEMNOS_OK ||
            lemnos_device_text(client, device, LEMNOS_DEVICE_DRIVER, driver, sizeof driver, &len) != LEMNOS_OK ||
            lemnos_device_text(client, device, LEMNOS_DEVICE_TAGS, tags, sizeof tags, &len) != LEMNOS_OK ||
            lemnos_device_info(client, device, &info) != LEMNOS_OK) {
            fprintf(stderr, "device %d: could not be read\n", device);
            return 1;
        }
        printf("%s (%s): state %u, health %u, provides %s\n", name, driver, info.state, info.health, tags);
    }
    return lemnos_client_close(client) == LEMNOS_OK ? 0 : 1;
}
```

Build it against the shared library and run it:

```sh slow
export PKG_CONFIG_PATH="$TMPDIR/lemnos/lib/pkgconfig"
cc -std=c11 -Wall -Werror docs/examples/devices.c $(pkg-config --cflags --libs lemnos-capi) -o "$TMPDIR/devices"
LD_LIBRARY_PATH="$TMPDIR/lemnos/lib" "$TMPDIR/devices" "$LEMNOSD_SOCKET"
```

Use `lemnos-capi-static` in place of `lemnos-capi` to link the static library. Never link both.

The interface has a version (`LEMNOS_ABI_MAJOR`, `LEMNOS_ABI_MINOR`) and a hash of its layouts. A
program built against a different major version or layout is refused by `lemnos_check_version`
before it can call anything else.

```sh
kill %1
wait
```
