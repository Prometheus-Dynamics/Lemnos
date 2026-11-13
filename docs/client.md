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
- **`open_stream`, `open_stream_writable`**: a device's [frame stream](streams.md). `close_stream`
  gives it back; dropping it does the same, with the close sent on the client's next request. Closing
  the client gives up every stream it holds.
- **`packages`**: what became of the daemon's driver packages.
- **`next_event`**: a device's state or reason changed, with its health. Events come when the
  client asked for them in its Hello (the default). They are read off the socket inside any call
  and in `next_value`, and `next_event` only takes what has arrived by then. After a list, a client
  gets every later change of each device's state or reason, including a device that comes up after
  the list. An event may repeat a state the client has; the newest one is current. Up to 64 events
  queue. Past that the oldest go, so a client that stops reading should list the devices again.

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
| `lemnos_version`, `lemnos_check_version` | The handshake. Call `lemnos_check_version` first |
| `lemnos_client_open`, `lemnos_client_close` | Connect and disconnect |
| `lemnos_device_count`, `lemnos_device_info`, `lemnos_device_text` | The device list |
| `lemnos_open` | Open a contract on a device, by tag |
| `lemnos_call` | Call a method by number, with its arguments and reply as bytes |
| `lemnos_subscribe`, `lemnos_unsubscribe`, `lemnos_next_value` | Values at a period |
| `lemnos_set_power` | Off and on |
| `lemnos_item_count`, `lemnos_item_info`, `lemnos_item_text` | What a device says about itself |
| `lemnos_package_list`, `lemnos_package_info`, `lemnos_package_text` | What became of the daemon's driver packages |
| `lemnos_stream_open`, `lemnos_stream_info`, `lemnos_stream_close` | Take a device's [frame stream](streams.md), read-only or `LEMNOS_STREAM_WRITABLE`, and give it back. Waits for the service's answer, up to the client's timeout. `lemnos_client_close` gives up every stream still open |
| `lemnos_stream_next_frame`, `lemnos_stream_release` | Take the next frame, read in place, and give its buffer back |
| `lemnos_stream_read`, `lemnos_stream_write`, `lemnos_stream_post_job` | Copy out of a buffer or into one, and post a job |
| `lemnos_stream_buffer_handle` | A new descriptor of a buffer's own dma-buf, to hand to a GPU or an encoder |

Every call returns a status: 0 for success, a negative number for an error, and a positive count
where the call says so. Output structs start with their own size. Text comes back through a
callback or into a buffer the caller gives. A panic inside the library comes back as a status and
never unwinds into C.

A stream works as it does in the Rust client. The Rust wrapper's stream is closed when it is dropped; a C program closes its stream with `lemnos_stream_close`, or gives every stream up with
`lemnos_client_close`. `lemnos_stream_next_frame` gives the frame's bytes
in place, valid until the frame is released, and allocates nothing. On a stream of dma-bufs
(`LEMNOS_STREAM_BUFFER_HANDLES` in its info) each buffer has its own descriptor, and
`lemnos_stream_buffer_handle` gives the caller a new one to close when done. A stream of shared
memory has none and answers `-EOPNOTSUPP`. Calls on one stream wait for each other, never for the
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
