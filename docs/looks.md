# Looks

A **look** is what a light shows: a few layers of simple blocks (a fill, a
comet, a progress arc, a ripple, static pixels), an optional breathe or blink
over the whole, and a brightness. `lemnos-light` renders looks without `std` or
allocation; `lemnosd` names them, loads them from files, and reloads them
without a restart. Looks are data, so a new animation is a file, not a
rebuild.

- Built-in looks are compiled in (`lemnos-light`, `builtin.rs`), shaped by the
  board's light keys (`docs/board-definition.md`).
- A look file overrides a built-in of the same name, and adds new names.
- Clients ask for a look by name (`led look`), or send one in full (`led show`,
  `LedClient::show_spec`), with the same layering and brightness either way.

## Layers and blocks

A look has 1 to 4 **layers**, bottom first. Each layer is one block with its
parameters, a colour, a brightness and a **mode**. Layers are composited per
LED: `max` keeps the brighter channel of the layers so far, `add` sums them
(saturating). A layer's brightness scales that layer alone.

Every block takes `brightness` (0 to 1, default 1) and `mode` (`max` or `add`,
default `max`). Units: periods are milliseconds, a **LED** count is LEDs (the
ring has 16 on the Raze, at most 64), and every other fraction is 0 to 1.

| Block | Keys (default) | Notes |
|---|---|---|
| `fill` | `color` (required) | Every LED the colour. |
| `comet` | `color` (required), `period_ms` (1200, 100 to 600000), `tail` (5 LEDs, above 0 to 64), `heads` (1, or 2 for opposite comets), `base` (0, the floor brightness), `reverse` (false, the other way round) | One turn per `period_ms`. The tail fades along `x^2.2` over `tail` LEDs. |
| `arc` | `fraction` (`"input"`: the request's progress, or a number 0 to 1), `color` (required), `track` (`101012`, the unfilled part), `head` (0.35, the leading LED mixed toward white), `sheen` (true, a soft white moving over the fill) | A progress gauge. The fill advances eased when its fraction changes. |
| `ripple` | `color` (required), `origin` (LED 0, 0 to 63), `speed` (12 LEDs a second), `width` (2.2 LEDs), `settle_ms` (1400, 0 to 60000), `glow` (0.15, the settling glow's peak) | A wave out from `origin` both ways round, then a glow that fades. `system.confirmed` is one. |
| `frame` | `pixels` (required: 1 to 64 colours) | Static pixels from LED 0; the rest are off. |

Colours are `"rrggbb"` (or `"#rrggbb"`), `"wwrrggbb"` for a white channel, or an
integer `0xWWRRGGBB`.

## Envelope and brightness

The **envelope** is a breathe or a blink over the whole look:

```toml
envelope = { kind = "breathe", period_ms = 4000, depth = 0.18, easing = "ease-in-out" }
envelope = { kind = "blink", period_ms = 1000, duty = 0.5 }
envelope = "solid"                         # or "breathe"/"blink" with the defaults
```

- `breathe`: `period_ms` (2000), `depth` (0.6: the brightness eases down by this
  much and back), `easing` (`ease-in-out`; any easing `lemnos-light` knows, or
  `cubic-bezier(x1, y1, x2, y2)`).
- `blink`: `period_ms` (1000), `duty` (0.5, the fraction of the period lit).

The look's own `brightness` (0 to 1, default 1) and `min_brightness` (0 to 1,
default 0) follow, then the **ring-wide** brightness (below). `min_brightness`
is the least the look is shown at after that scale.

## A look file

A look file is a TOML file of `[looks.<name>]` tables. A name is 1 to 40 of
`a-z`, `0-9`, `.`, `-` and `_`.

```toml
# /etc/lemnos/looks.d/photonvision.toml (or the writable directory)
[looks."pv.searching"]
envelope = "solid"
brightness = 1.0
layers = [
  { block = "comet", color = "2bd47d", period_ms = 1600, tail = 6, base = 0.06 },
]

[looks."pv.no-nt"]
layers = [
  { block = "comet", color = "ffa424", period_ms = 2400, tail = 5, base = 0.05, heads = 2 },
]
```

An unknown key, a bad value or a bad name is an error that names the file and
the key (`looks.pv.no-nt.layers[0].heads: must be 1 or 2`). A file with any bad
look fails as a whole, and the looks it had keep working.

## Built-in looks

Their colours and timings come from the board's light keys (in brackets) and
the defaults shown. Each is also a valid `led look <name>`.

| Name | Look | Brightness |
|---|---|---|
| `status.ok` `status.warn` `status.error` `status.busy` | the board's `ok`/`warn`/`error`/`busy` colour, solid, or the status effect (`status_effect`, `error_effect`) | fill 0.7 |
| `status.off` | nothing | |
| `system.verifying` | a purple comet, 1200 ms, tail 7, base 0.05 (`verifying_*`) | comet 1.0 |
| `system.writing` | the blue arc over the faint track, sheen, the request's progress | arc 1.0 |
| `system.writing-unknown` | the blue comet, with the verifying timing | comet 1.0 |
| `system.staged` | the green breathe, 2200 ms, depth 0.45 (`staged_*`) | fill 0.7 |
| `system.booting` | two warm-white comets, 1800 ms, tail 5, base 0.04 (`booting_*`) | comet 1.0 |
| `system.rebooting` | the reboot ember: the rebooting colour at 12%, held at least 6% | ember |
| `system.failed` `system.rolled-back` | the red breathe, 2400 ms, depth 0.9 (`failed_*`) | fill 0.7 |
| `system.confirmed` | the green ripple (`confirmed`) | ripple 1.0 |
| `system.locate` | the `locate` colour, breathe or chase (`locate_effect`) | fill 0.7 |
| `pv.targets` | blue `2f7bff` breathe, 4000 ms, depth 0.18 | fill 0.7 |
| `pv.searching` | green `00ff20` comet, 1600 ms, tail 6, base 0.06 | comet 1.0 |
| `pv.no-nt` | amber `ffa424`, two comets, 2400 ms, tail 5, base 0.05 | comet 1.0 |
| `pv.no-nt-targets` | blue `2f7bff`, two comets, 2400 ms, tail 5, base 0.08 | comet 1.0 |
| `pv.error` | red `ff3b3b` breathe, 2000 ms, depth 0.85 | fill 0.7 |
| `pv.vision` | steady white `ffffff` | fill 0.7 |

`system.writing-unknown` is the write phase while its amount is unknown; the
update's other states are listed in `docs/system-service.md`.

## Brightness

The **ring-wide** brightness scales every look: the board's
`look_brightness` (0 to 1, default **0.5**). A look's own brightness is 1.0 for
comets and arcs, and 0.7 for full-ring fills and breathes (the built-in fills,
status, locate, and `pv.targets`/`pv.error`/`pv.vision`), so a fill is not
brighter than a comet's head. A request's `--brightness` replaces the look's
own, and the ring-wide scale still applies. The reboot ember keeps a 6% floor.

To tune: set `look_brightness` on the light (`config` in `board.toml`), or
`brightness` on a look for a single look. `brightness` on the light is the
LED driver's own byte (`docs/board-definition.md`); it is not the look scale.

## Where looks come from, and reloads

From the lowest to the highest:

1. the built-ins;
2. the board's `[looks.<name>]` tables (`board.toml`, read at start);
3. every `*.toml` in `LEMNOSD_LOOKS_DIR` (read-only, default
   `/etc/lemnos/looks.d`), in name order;
4. every `*.toml` in `LEMNOSD_LOOKS_OVERRIDE_DIR` (writable, default
   `/var/lib/lemnos/looks.d`; `off` turns it off). `looks save` writes here.

Within a directory a later file wins a name. A look that is used by a light
changes at once when its file changes: the directories are re-read about once
a second, and on `SIGHUP` (`systemctl kill -s HUP lemnosd`) or
`lemnos-ctl looks reload`. The service logs what loaded and what failed. A file
that fails keeps the looks it had.

On a read-only root, point `LEMNOSD_LOOKS_OVERRIDE_DIR` at `/data`. Board
looks are read when `lemnosd` starts, so a change to `board.toml` needs a
restart.

## Using looks

### `lemnos-ctl`

```text
lemnos-ctl led look <name> [--brightness F] [--progress F] [--seconds N]
lemnos-ctl led show --spec '<TOML body>' [--seconds N] [--brightness F] [--progress F]
lemnos-ctl led show --file <path> [--name N] [--seconds N]
lemnos-ctl led show --json '<JSON body>' [--seconds N]
lemnos-ctl looks list
lemnos-ctl looks show <name>                      # the resolved look, as TOML
lemnos-ctl looks reload
lemnos-ctl looks save <name> (--spec ... | --file ... | --json ...)
```

`led look` and `led show` wait for `lemnosd`'s answer. An unknown name, or an
invalid look, is refused with the reason, and the light keeps its look. A look
stays until it is replaced or cleared (`led off`), or for `--seconds N`. The
`--file` form takes a bare look body, or a look file (with `--name` when it has
more than one look). `--progress` feeds the look's arcs (0 to 1).

A bare look body is what `--spec` and `--json` take (`--json` is what Atlas
sends):

```bash
# A named look, from the look table:
lemnos-ctl --client atlas led look pv.searching --seconds 30

# A look in full, from the command line (the same look as above):
lemnos-ctl --client atlas led show \
  --spec 'layers = [{ block = "comet", color = "2bd47d", period_ms = 1600, tail = 6, base = 0.06 }]' \
  --seconds 30

# The same look as JSON:
lemnos-ctl --client atlas led show \
  --json '{"layers":[{"block":"comet","color":"2bd47d","period_ms":1600,"tail":6,"base":0.06}]}'

# Keep a look tried inline, under a name:
lemnos-ctl looks save pv.searching --spec 'layers = [{ block = "comet", color = "2bd47d", period_ms = 1600, tail = 6, base = 0.06 }]'
```

### Rust

```rust
use lemnos_ipc::{ClientOptions, LedClient};
use lemnos_light::{Fraction, LayerSpec, LookSpec, Rgbw};

let mut leds: LedClient = ClientOptions::new(socket, "atlas").keep_intents().leds()?;

// A named look (a built-in or from a look file):
leds.look("pv.searching")?;

// A look composed in code: a comet over a progress track, two layers.
let mut look = LookSpec::EMPTY;
look.push(LayerSpec::comet(Rgbw::rgb(0x2bd47d), 1600, 6000, 1, 60));
look.push(
    LayerSpec::arc(Fraction::Input, Rgbw::rgb(0x2f7bff), Rgbw::rgb(0x101012))
        .with_brightness(64)
        .with_mode(lemnos_light::Mode::Add),
);
leds.show_spec(&look)?;   // refused with the reason if invalid
```

`LookSpec` is plain data: `layers`, `envelope`, `brightness` and `floor` are
public, and `LookSpec::validate` says why a look cannot be shown.

## Protocol

- `LedShow::Look { name, progress }` (code 10) and `LedShow::Inline { spec,
  progress }` (code 11, the look in a compact binary form: `lemnos-ipc`,
  `wire/look.rs`). Both are appended; older codes are unchanged.
- `LedRequest` has a trailing reply `id`. A nonzero id gets a `Message::Text`
  reply: the empty text when the look is shown, or the refusal's reason.
- `Request::Looks` (kind 10) lists, shows, reloads or saves; it is answered
  with `Message::Text` (kind 108).

## Tests

`lemnos-light` checks every built-in look against `golden.txt`: frames
rendered at fixed times, hashed. A deliberate change to a built-in is recorded
with `LEMNOS_GOLDEN_UPDATE=<file> cargo test -p lemnos-light`. The look parser,
writer and error paths are tested in `lemnos-board` (`looks_tests.rs`), the
file table, reloads and saves in `lemnosd/tests/looks.rs`.
