# Looks

A **look** is what a light shows: a few layers of simple blocks (a fill, a
comet, a progress arc, a ripple, static pixels, a wash, a sparkle), an optional breathe or blink
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
parameters, a colour, a brightness and a **mode**. A look has at most one
`sparkle` layer. Layers are composited per
LED: `max` keeps the brighter channel of the layers so far, `add` sums them
(saturating). A layer's brightness scales that layer alone.

Every block takes `brightness` (0 to 1, default 1) and `mode` (`max`, `add` or
`over`, default `max`). The modes:

| Mode | Per channel | Use |
|---|---|---|
| `max` | the brighter of the two | the default; layers that do not overlap in colour |
| `add` | the sum, saturating at full | light stacked on light |
| `over` | alpha compositing: the layer covers what is below by its own level (its largest channel, `a`), so `below × (1 − a) + layer` | a coloured comet on a coloured glow: the head keeps its own colour, the glow shows where the head is off, and a tail blends from one to the other without the hue mixing a `max` gives |

Wire: mode codes `0` (`max`), `1` (`add`), `2` (`over`), appended. Units: periods are milliseconds, a **LED** count is LEDs (the
ring has 16 on the Raze, at most 64), and every other fraction is 0 to 1.

| Block | Keys (default) | Notes |
|---|---|---|
| `fill` | `color` (required) | Every LED the colour. |
| `comet` | `color` (required), `period_ms` (1200, 100 to 600000), `tail` (5 LEDs, above 0 to 64), `heads` (1, or 2 for opposite comets), `base` (0, the floor brightness), `reverse` (false, the other way round) | One turn per `period_ms`. The tail fades along `x^2.2` over `tail` LEDs. |
| `arc` | `fraction` (`"input"`: the request's progress, or a number 0 to 1), `color` (required), `track` (`101012`, the unfilled part), `head` (0.35, the leading LED mixed toward white), `sheen` (true, a soft white moving over the fill) | A progress gauge. The fill advances eased when its fraction changes. |
| `drain` | `color` (required), `fill_ms` (0, the fill grows from the top over this long), `start_ms` (0, when the drain starts), `duration_ms` (1000), `easing` (`ease-in`) | A fill of the colour that grows from the top, holds, then recedes toward the gravity bottom (both sides round) from `start_ms` over `duration_ms`, speeding up on an ease-in. The edge is one LED soft; the last LEDs at the bottom fade out. The bottom is the same one-shot gravity read a falling sparkle uses, `default_down` when there is none. `system.confirmed` drains. |
| `ripple` | `color` (required), `origin` (LED 0, 0 to 63), `speed` (12 LEDs a second), `width` (2.2 LEDs), `settle_ms` (1400, 0 to 60000), `glow` (0.15, the settling glow's peak) | A wave out from `origin` both ways round, then a glow that fades. `system.confirmed` is one. |
| `frame` | `pixels` (required: 1 to 64 colours) | Static pixels from LED 0; the rest are off. |
| `wash` | `color` (required), `envelope` (required: a breathe, pulse or blink, in the look envelope's form) | Every LED the colour, shaped by its **own** envelope, not the whole look's. A flash or a slow breathe under other layers. |
| `sparkle` | `color` or `colors` (required: one colour, or a list of 1 to 4 chosen at random per twinkle), `density` (1.2, twinkles a LED a second, 0 to 64), `density_end` (`density`, see below), `fade_ms` (0), `start_ms` (0), `min_ms` (350), `max_ms` (950), `base` (0, a floor in the first colour), `seed` (0: from the look's start time), `fall` (false), `fall_speed` (5, LEDs a second), `fall_accel` (6, LEDs a second squared) | Random twinkles: each LED starts one with probability `density` a second (`density * dt` per render, so the rate does not depend on the frame rate), unless it is mid-twinkle. See below. |

**Sparkle.** A twinkle lasts a random time in `min_ms` to `max_ms`. Its brightness
rises linearly over the first fifth of its length, then decays as `(1 - x)^2` to
off. The rate is `density` from the look's start, and nothing before `start_ms`.
With `fade_ms`, the rate ramps linearly from `density` to `density_end` over
`fade_ms` from `start_ms`, and the `base` fades to off by `start_ms + fade_ms`.
With `fall = true` each twinkle is a particle instead: it spawns within three
LEDs of the ring's top (the LED opposite the bottom), slides toward the bottom
by the shorter arc, accelerating from `fall_speed` at `fall_accel`, and fades as
it lands. Positions are fractional, drawn across the two neighbouring LEDs. At
most 16 particles are in the air; a spawn with none free is skipped. The
bottom comes from gravity when the look starts (see *Gravity* below), and is
`default_down` otherwise (the middle of the ring). Sparkles are seeded per look
start, so the same `seed` gives the same twinkles.

Colours are `"rrggbb"` (or `"#rrggbb"`), `"wwrrggbb"` for a white channel, or an
integer `0xWWRRGGBB`.

### The two sparkle looks

These are the built-in `system.booting` and `system.confirmed`, in look-file
form (copy them as a starting point; the built-ins take the board's colours,
and the look-file form uses these):

```toml
[looks."system.booting"]
layers = [
  { block = "wash", color = "ffffff", brightness = 0.09,
    envelope = { kind = "breathe", period_ms = 3000, depth = 0.55, easing = "ease-in-out" } },
  { block = "sparkle", color = "fff4e6", density = 1.2, base = 0.03, min_ms = 350, max_ms = 950 },
]

[looks."system.confirmed"]
layers = [
  { block = "wash", color = "c8ffd2",
    envelope = { kind = "pulse", attack_ms = 80, hold_ms = 0, decay_ms = 500, repeat = 1 } },
  { block = "drain", color = "00ff20", fill_ms = 700, start_ms = 1100, duration_ms = 2100, easing = "ease-in" },
  { block = "ripple", color = "00ff20", speed = 14, width = 1.6, settle_ms = 1, glow = 0 },
]
```

`system.booting` is the trial boot ("testing a new image"): a calm, starry
sparkle over a slow breathe. `system.confirmed` is the update confirmed: the
flash comes at once, the burst runs out from the top to the bottom and back in
about a second, the green fills the ring from the top by 0.7 s, holds, and then
drains toward the bottom from 1.1 s over 2.1 s (an ease-in, so it speeds up),
the last LEDs at the bottom fading out. The look runs about 3.2 s, and the
Confirmed hold in `lemnosd` is 3.3 s. The flash is above the fill cap (a
celebration), and the ring-wide brightness still scales it.

To make the drain start from a different place, change the `ripple`'s `origin`
or the gravity keys; the drain always goes toward the bottom.

### The trial boot's ember

On an update's trial boot the ring shows the reboot ember (`system.rebooting`)
from the moment `lemnosd` starts, not the sparkle: the updater's status reads
`trying` (or `rebooting`) from the start of the new image, so the restart's
look carries on. It holds the ember for `LEMNOSD_TRIAL_EMBER_MS` (4000 ms by
default) and then cross-fades into `system.booting` over 1 s. The status file
has no signal for when the self-test starts, so this is a timed hand-over. A
ring is never written off on that path (a stop with a restart still fades to
the ember). `lemnosd` logs the uptime of its first ring frame once per start
(`lemnosd: first ring frame written at N s since boot`), so the boot's timing
can be read from its log.

## Gravity: which way is down

A falling sparkle wants the ring's bottom, from the IMU. The ws2812 light
takes four more keys in `board.toml`:

```toml
[[devices]]
id = "status-ring"
# ...
[devices.config]
count = 16
gravity_device = "imu"          # the IMU whose acceleration gives down
gravity_plane = ["-y", "x"]     # the two IMU axes that span the ring's plane (signs allowed)
gravity_led0_deg = 0            # the angle, in that plane, of the first LED on the wire
default_down = 8                # the logical LED used when there is no gravity (default count / 2)
```

When a look with a falling sparkle starts, `lemnosd` reads the IMU **once**
(through the same one-shot read a client gets; no subscription, no stream).
The reading's acceleration is the support force, so *down* is its negation.
The in-plane part of that gravity gives an angle; the angle of the physical LED
on that side (from `gravity_led0_deg`, one LED every `360 / count` degrees,
counted round the plane from the first axis to the second) is the bottom. The
ring's `offset` and `direction` are honoured, so the bottom is a logical LED
that the strip shows at the physical bottom. Until the read returns (or when
the IMU is missing), the bottom is `default_down`. A board lying flat (less than
35% of the reading in the ring's plane) also uses `default_down`. Spawns after
the reading fall toward it; particles already in the air keep their path.

### Calibrating the bottom

Set the four keys, restart `lemnosd` (the board is read at start), and run:

```text
lemnos-ctl light gravity status-ring --seconds 30
```

Once a second it reads the IMU and prints the accelerometer, the in-plane share
of the reading, the down angle, and the bottom LED (and its physical index),
and it lights that LED green, its opposite LED dim. Tilt the board. The green LED
should be at the bottom. Then tune the keys:

- If it moves the wrong way round, flip the sign of one plane axis
  (`["-y", "x"]` becomes `["y", "x"]`).
- If it is a fixed number of LEDs off, change `gravity_led0_deg` by that angle
  (`360 / count` degrees an LED).
- If it does not move, the plane is wrong (the board is still in the plane's
  normal direction): choose the other two axes.

`--board PATH` names another board file (default `LEMNOSD_BOARD`, or
`/etc/lemnos/board.toml`). The helper clears its frame when it ends.

## Envelope and brightness

The **envelope** is a breathe, a blink or a pulse over the whole look:

```toml
envelope = { kind = "breathe", period_ms = 4000, depth = 0.18, easing = "ease-in-out" }
envelope = { kind = "blink", period_ms = 1000, duty = 0.5 }
envelope = { kind = "pulse", attack_ms = 60, hold_ms = 600, decay_ms = 1200, repeat = 1 }
envelope = "solid"                         # or "breathe"/"blink"/"pulse" with the defaults
```

- `breathe`: `period_ms` (2000), `depth` (0.6: the brightness eases down by this
  much and back), `easing` (`ease-in-out`; any easing `lemnos-light` knows, or
  `cubic-bezier(x1, y1, x2, y2)`).
- `blink`: `period_ms` (1000), `duty` (0.5, the fraction of the period lit).
- `pulse`: a one-shot flash then a glow to off. The level rises linearly over
  `attack_ms` (80), holds for `hold_ms` (0), then decays over `decay_ms` (1000)
  on a quadratic ease-out to 0, and is 0 after the last one. `repeat` (1) is
  how many times it runs; 0 keeps repeating. `attack_ms` and `hold_ms` are 0 to
  60000, `decay_ms` 1 to 60000.

The look's own `brightness` (0 to 1, default 1) and `min_brightness` (0 to 1,
default 0) follow, then the **ring-wide** brightness (below). A look's own
brightness may be above the fill cap (0.7): `system.confirmed` is. `min_brightness`
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
| `system.verifying` | a purple comet, 1200 ms, tail 8, base 0.18 (`verifying_*`) | comet 1.0 |
| `system.writing` | the blue arc over the faint track, sheen, the request's progress | arc 1.0 |
| `system.writing-unknown` | the blue comet, with the verifying timing | comet 1.0 |
| `system.staged` | the green breathe, 2200 ms, depth 0.45 (`staged_*`) | fill 0.85 |
| `system.booting` | the trial boot: a faint white breathe (3000 ms, about 4 to 9%) under a warm-white sparkle (`booting`, density 1.2, base 0.03); see below, and *The trial boot's ember* | wash 0.09, sparkle 1.0 |
| `system.rebooting` | the reboot ember: the rebooting colour at 44%, held at least 16% (about 22% on a default ring) | ember |
| `system.failed` `system.rolled-back` | the red breathe, 2400 ms, depth 0.9 (`failed_*`) | fill 0.7 |
| `system.confirmed` | the green pop: a pale green-white flash, a green burst from the top, the ring filled green and drained toward the bottom, about 3.2 s; see below | wash 1.0, drain 1.0, ripple 1.0 |
| `system.locate` | the `locate` colour, breathe or chase (`locate_effect`) | fill 0.7 |
| `pv.targets` | cyan `28c8ff` breathe, 4000 ms, depth 0.2 | fill 0.7 |
| `pv.searching` | violet `965aff` comet, 1600 ms, tail 7, base 0.18 | comet 1.0 |
| `pv.no-nt` | deep orange `ff5a00`, two comets, 2400 ms, tail 6, base 0.18 | comet 1.0 |
| `pv.no-nt-targets` | cyan `28c8ff` glow at 0.25, under a deep-orange twin comet (2400 ms, tail 6, base 0, `over`) | glow 0.25, comet 1.0 |
| `pv.error` | red `ff2828` breathe, 2000 ms, depth 0.85 | fill 0.7 |
| `pv.vision` | steady white `ffffff` | fill 0.7 |

These are the **scheme B** colours, the default preset (below). Scheme B is cool
colours for healthy and warm colours for trouble, and motion means looking: a
moving comet is a search or a wait, a steady colour is settled.

Every comet has a dim glow of its own colour under it: the default base is
0.18 (`spinner_base` for the spinner, the locate chase and an orbit;
`verifying_base` for the verifying and writing comets), so the LEDs between
the heads are never bare off. A look sets `base` to change it.

A comet's head is anti-aliased: the LED just ahead of a head is lit by the
fraction of the way the head has got to it, so a head moves from LED to LED
in steps of a fraction of a LED, not a whole LED at a time, for one head, two
and every tail. A test (`lemnos-light`, `comet_leading_edges_move_smoothly_at_one_ms_steps`)
checks that no LED changes by more than 9 of 255 in 1 ms over one turn.

`system.writing-unknown` is the write phase while its amount is unknown; the
update's other states are listed in `docs/system-service.md`.

## Presets

A **preset** is a named set of look definitions (a look file's text). Three are
built in: `scheme-a` (a traffic light: green targets, white searching, amber
no-NT, red error), `scheme-b` (the default: the scheme B colours above) and
`scheme-c` (motion only: solid when settled, a comet when moving). Users save
their own under `<LEMNOSD_STATE_DIR>/presets/<name>.toml` (default
`/var/lib/lemnos/presets`); built-in names cannot be saved over or deleted.

One preset is active, and the choice is saved (`<state>/presets/active`), so it
survives a restart. The active preset's looks sit **above the board's looks and
below every look file**: a look file in `LEMNOSD_LOOKS_DIR` or
`LEMNOSD_LOOKS_OVERRIDE_DIR` still wins its name.

```text
lemnos-ctl looks preset list                       # * marks the active one
lemnos-ctl looks preset show scheme-b              # its look file text
lemnos-ctl looks preset apply scheme-c             # switch; remembered across restarts
lemnos-ctl looks preset save night --file night.toml
lemnos-ctl looks preset save night2 --from-active  # copy the active preset
lemnos-ctl looks preset delete night               # the active one falls back to scheme-b
lemnos-ctl looks delete pv.searching               # remove a look file override
```

Preset changes (apply, save, delete) and look deletions are accepted from the
clients `atlas`, `lemnos-ctl` and `orion:<requested_by>`; any other client may
read the presets but not change them. The socket's permissions remain the
boundary. Protocol: `LooksOp` codes 4 to 9, appended (`PresetList`, `PresetShow`,
`PresetApply`, `PresetSave`, `PresetDelete`, `Delete`), answered with
`Message::Text`.

## Brightness

The **ring-wide** brightness scales every look: the board's
`look_brightness` (0 to 1, default **0.5**). A look's own brightness is 1.0 for
comets and arcs, and 0.7 for full-ring fills and breathes (the built-in fills,
status, locate, and `pv.targets`/`pv.error`/`pv.vision`), so a fill is not
brighter than a comet's head. A request's `--brightness` replaces the look's
own, and the ring-wide scale still applies. The reboot ember keeps a 16% floor.

To tune: set `look_brightness` on the light (`config` in `board.toml`), or
`brightness` on a look for a single look. `brightness` on the light is the
LED driver's own byte (`docs/board-definition.md`); it is not the look scale.

## Where looks come from, and reloads

From the lowest to the highest:

1. the built-ins;
2. the board's `[looks.<name>]` tables (`board.toml`, read at start);
3. the active preset (see *Presets*);
4. every `*.toml` in `LEMNOSD_LOOKS_DIR` (read-only, default
   `/etc/lemnos/looks.d`), in name order;
5. every `*.toml` in `LEMNOSD_LOOKS_OVERRIDE_DIR` (writable, default
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
