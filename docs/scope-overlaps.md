# Scope overlaps

Lemnos's scope is user-space hardware access: buses, devices, drivers, board files,
ownership, and the daemon that serves them (README.md, Scope). Three parts of the tree
do not touch hardware, and a consumer could reasonably expect to own them. This note
lays out the options. Nothing is moved: the owner decides.

| Part | What it computes | Hardware access | Used by |
|---|---|---|---|
| `lemnos-light` | LED frames: looks, layers, easing, spots, the spring that moves them | none (`lemnos-device` only) | `lemnosd` (look service), `lemnos-ipc` (wire), `lemnos-ctl`, `lemnos-board` |
| `lemnos-fusion` | IMU orientation (Mahony, Madgwick) and sensor calibration | none (no dependencies) | `lemnos-drivers-bmi088`, `lemnos-drivers-bmm150`, `lemnosd` |
| `lemnosd/src/clients.rs` | the per-client write queue: readings dropped first, events counted, replies kept | sockets only | `lemnosd` (every client connection) |

## 1. `lemnos-light` (LED rendering)

The engine turns a look into LED frames for `lemnosd`. The LED driver (`lemnos-drivers-ws2812`)
does not depend on the engine: it writes the frames it is given. The wire types are in `lemnos-ipc`. The named looks are not: the built-in set was `status.*` and `system.*` (the board's
own states), `detect.*` and `link.*` (a consumer's states), and the presets (`scheme-a`,
`scheme-b`, `scheme-c`) are a consumer's choice. Section 1 moved the last three out.

- **A. Keep as it is.** The renderer is a no-std library that turns the daemon's looks
  into the ring's frames. Consumers configure it through look files (already supported,
  a look file of a built-in's name replaces it). Cost: the daemon carries an effects
  engine that a consumer may not want.
- **B. Keep the engine, move the named looks out.** `builtin.rs` (the `status.*`,
  `system.*` and `detect.*`/`link.*` definitions) and `presets.rs` become a consumer's
  look files. Lemnos keeps the engine, the wire, and an empty default set. Cost: a
  board without a look file shows an unlit ring, so the board files need a default.
- **C. Move the engine to a separate crate group** (`lemnos-light` with its own
  repository or workspace), depended on by Lemnos's LED driver only. Cost: a second
  release train for a library that changes with the daemon's wire.

**Decided: B** (the user, 2026-11-03). The engine is generic and is what the driver needs; the named
looks are product decisions and belong to the consumer that knows what its states mean.
The rename already moved the vision-specific names off the built-in list. The remaining
step is to make the built-in set minimal (`status.*` for the board's own states) and to
ship the consumer presets as a documented example.

## 2. `lemnos-fusion` (IMU fusion and calibration)

A filter and a fit over sensor samples, with no hardware and no dependencies. Both IMU
drivers (`lemnos-drivers-bmi088`, `lemnos-drivers-bmm150`) depend on it, and so does `lemnosd`.

- **A. Keep as it is.** Fusion is a sensor concern: the drivers that depend on it hand it
  their samples and calibration state, and any board with an IMU uses it.
- **B. Keep the filter, move calibration routines out.** The calibrate module (the
  routines that decide when a fit is accepted, and the policy of their dwell and
  forgetting) is a product decision about a board's calibration workflow.
- **C. Move the crate to a separate group**, as for LED rendering.

**Decided: A** (the user, 2026-11-03). The drivers and the filter share the sample clock and the calibration
state, so splitting them would put one sensor's state in two crates. The calibration policy is the only candidate for
moving, and only if a second board wants a different workflow. Decide that when it happens.

## 3. `lemnosd/src/clients.rs` (fan-out policy)

Not a fan-out to hardware: the daemon's client sockets. Each client has a bounded write
queue (64 KiB). When a client falls behind, readings go first (the next reading supersedes
them), then events (counted, reported as `Event::Dropped`), and replies are never dropped.

- **A. Keep in `lemnosd`.** The policy belongs to the daemon that owns the sockets; no
  other crate sees a client's queue.
- **B. Move the queue into `lemnos-ipc`** as a reusable client-side buffer. The drop
  classes are the wire's (readings, events, replies), so the policy is part of the protocol.
- **C. Move to consumers.** Not applicable: a consumer does not run the socket server.

**Decided: A, with the drop classes documented in `lemnos-ipc`** (the user, 2026-11-03). The queue is
the daemon's, and its classes already come from the wire. Moving it would split one policy
across two crates for no reuse.

## Decisions (user, 2026-11-03) and what was done

- **1B, `lemnos-light` keeps the engine; the named looks move out.** Done on dev, with no
  compatibility path (nothing is released):
  - The built-in set is `status.*` only (`builtin.rs`). `system.*`, `detect.*` and `link.*`
    are look files in `examples/looks` (`looks.d/system.toml`, `looks.d/detect.toml`), and the
    scheme presets are `examples/looks/presets/scheme-{a,b,c}.toml`. The look-file format is unchanged.
  - Lemnosd has no built-in presets. None is active until one is applied; the active one is
    recorded as before (`<state>/presets/active`).
  - Board files have `default_look` (a look name, `status.ok` by default). A light shows it when no
    owner holds an intent, and in place of a named look nothing defines, so it never goes dark.
    The keys of the moved looks (`locate`, `updating`, `verifying*`, `staged*`, `booting*`,
    `rebooting`, `failed*`, `confirmed`, `idle`, `locate_effect`) are retired: a board that sets
    one is refused, and the error names where the setting now lives (`RETIRED_LIGHT_KEYS`).
  - Verified: `lemnosd/tests/example_looks.rs` renders each moved look from the example set and
    checks it frame for frame against the recorded golden frames of the old built-ins (20
    scenarios). `lemnos-light` keeps the old definitions as a test-only reference
    (`src/reference.rs`), so the engine tests and the golden file are unchanged.
  - Migration (before a release drops the built-ins): the device package installs the example set
    as in `docs/looks.md`, *Migrating a device package*.
- **2A, `lemnos-fusion` stays as it is.** No code change.
- **3A, the client queue stays in `lemnosd`.** The drop classes (readings, then events counted and
  reported as `Event::Dropped`, and never replies) and the 64 KiB per-client bound are documented
  in the `lemnos-ipc` crate docs. No code change.
