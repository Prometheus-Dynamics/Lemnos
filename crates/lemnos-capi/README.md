# lemnos-capi

Lemnos's C surface, version 1.0: the `lemnosd` device client behind ten `extern "C"` functions
declared in [`include/lemnos.h`](include/lemnos.h), built as a static and a shared library, and
`Client`, a Rust wrapper that calls through those same functions.

`lemnos.h` includes [`include/lemnos/abi.h`](include/lemnos/abi.h), the types every Lemnos
surface shares (status codes, handles, the handshake, message sinks, buffer and fence
descriptors). That file is a byte-identical copy of `lemnos-abi`'s, and a test checks it; ship
the whole `include/` directory.

The exported names are listed in [`lemnos.exports`](lemnos.exports), one per line, and a test
keeps that list, the header and the crate's exports in step. On Linux the shared library's
SONAME is `liblemnos_capi.so.<LEMNOS_ABI_MAJOR>` (now `liblemnos_capi.so.1`), so install it
under that name, with `liblemnos_capi.so` as the link for building against it.

The surface's process-wide state is its client handle table. A process contains the surface
once: either statically (this crate as an rlib, or `liblemnos_capi.a`) or by loading
`liblemnos_capi.so`, never both. Otherwise a handle from one copy could name another client in
the other.

This is enforced on Linux. The first copy to open a client claims the process with an abstract
unix socket named for its pid namespace and pid. Any other copy's `lemnos_client_open` then
returns `-EBUSY` with a message, before it connects. The socket is close-on-exec, and the kernel
frees it when the process ends. Without `/proc` the check is skipped.

The crates under the surface (`lemnos-ipc`, `lemnos-abi`) hold no process-wide state.

## Installing both forms

`scripts/install-capi.sh <prefix> [--target <triple>]` builds the crate in release and installs:
- the headers: `include/lemnos.h` and `include/lemnos/abi.h`;
- the shared form: `lib/liblemnos_capi.so.1`, with `liblemnos_capi.so` linking to it;
- the static form: `lib/liblemnos_capi.a`;
- two pkg-config files: `lemnos-capi` (shared) and `lemnos-capi-static`.

Build a program with one of them, never both:

```sh
cc app.c $(pkg-config --cflags --libs lemnos-capi)          # shared
cc app.c $(pkg-config --cflags --libs lemnos-capi-static)   # static
```

`lemnos-capi-static` adds the system libraries rustc lists for the archive, and
`-Wl,--gc-sections`. On x86_64 the stripped C test client is 443 952 B static and 10 968 B
shared, plus the library's 458 472 B stripped. Without `--gc-sections` the static client is
1 242 944 B.

The surface is versioned by the SONAME's major (`liblemnos_capi.so.1`) and by the
`lemnos_check_version` handshake (major, minor and layout hash). There is no symbol version
node: rustc gives a `cdylib` its own anonymous version script, which overrides a named one, so
every symbol would stay at `Base`.

## The `shared` feature

By default `Client` calls this crate's own exports: the surface is linked statically. With the
`shared` feature, `Client` instead links the installed library, `liblemnos_capi.so.1`, through
the normal dynamic linker (never dlopen), and the crate leaves its own exports out, so the
process holds the surface once, as the library. Signatures and behaviour are the same; a test
checks each declaration against the function it stands for.

- `LEMNOS_CAPI_LIB_DIR` names the directory holding `liblemnos_capi.so.1` at build time (an
  image's sysroot, for example). The program then finds it at run time where the loader
  looks, as for any library.
- Unset, the build script builds the library from this crate (without `shared`, `cargo build
  --offline` into its `OUT_DIR`), for development and this crate's tests, which find it by an
  rpath. A program built that way outside this crate needs `LD_LIBRARY_PATH` or the
  installed library.
- The crate's own `staticlib` and `cdylib` under `shared` export nothing: build the library
  without it. Linux only.

| Function | Does |
| --- | --- |
| `lemnos_version`, `lemnos_check_version` | the handshake (major, minor, layout hash) |
| `lemnos_client_open`, `lemnos_client_close` | connect to a `lemnosd` socket; a handle |
| `lemnos_device_count`, `lemnos_device_info`, `lemnos_channel_info` | the device list, by index |
| `lemnos_read` | a one-shot read into a `LemnosReading` |
| `lemnos_subscribe`, `lemnos_next_reading` | a stream of readings |

Statuses, handles, the message sink and the version struct are `lemnos-abi`'s: 0 is success, a
negated errno passes through, `LEMNOS_E_*` are Lemnos's own codes. Output structs start with
`struct_size`; a struct smaller than the surface's is refused. The device class, status,
quantity and axis numbers are the IPC wire's.

One call per reading or subscription event: the surface is meant to be crossed per job, not per
value.

```c
LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
lemnos_check_version(&mine);
LemnosHandle client;
lemnos_client_open("/run/lemnos/lemnosd.sock", "my-app", 0, &client, LEMNOS_NO_SINK);
LemnosReading reading = {.struct_size = sizeof reading};
lemnos_read(client, 0, &reading, LEMNOS_NO_SINK);
lemnos_client_close(client);
```

`tests/c/client.c` is a complete client; the tests build it against both libraries and run it on
mock hardware. The C boundary's unsafe code lives in this crate (its own lint table, one
operation per block, each with a `// SAFETY:` comment); its Rust API is safe.
