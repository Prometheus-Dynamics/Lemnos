# R2-CFG: the board kit's s02 accepts CONFIG_RUST_IS_AVAILABLE (untested on board; PC tests only)

Not run on a board. The kit's step is a PC-side fixture change; nothing here touches Raze #2.

## Where the board kit is

The kit is not in this repository. `grep` for `s02` and `board-kit` under `testing/`, `scripts/` and
`docs/` finds nothing. The kit is in the gates directory, outside git:

- v2 (as the L1 pass ran it, unchanged): `/mnt/nvme-wd/lemnos-gates/L1-kit-bded77a0-v2/steps/s02-facts.sh`
  (sha256 `0286ba6796c12fb50a4f427d247339e640ac22c072f0efb5b682d8136f5d039a`)
- v3 (this change): `/mnt/nvme-wd/lemnos-gates/L1-kit-bded77a0-v3/steps/s02-facts.sh`
  (sha256 `e21e83b18cf065ddf61844d9f078f7568c46ee850bf5cabdf91339f4c4c49330`), tests in
  `L1-kit-bded77a0-v3/tests/config-tests.sh`, fixtures in `L1-kit-bded77a0-v3/tests/fixtures/`.

## What was wrong

s02 compares the whole decompressed `/proc/config.gz` with the dev build's `.config`
(`e3dc9f72...`). The dev build found a Rust toolchain and carries `CONFIG_RUST_IS_AVAILABLE=y`
(line 15). Raze #2's kernel does not carry it. Measured on the PC against the build tree's file:

- the dev `.config` raw: `e3dc9f72ac830a2d2591d48618e51adc538e44edb43865fd649dc4b8f0712753`
- the dev `.config` with that one line removed: `0d69ee4f5d9ccb585e308652484ae60bb26d4614d2b21ca7927f3470602bc2c7`
  (the same hash as the board's configuration, recorded by the PARKED note: the one difference is the probe line)

So s02 printed `config: DIFFERS from the dev .config` for a board whose only difference is a
toolchain-detected probe result. It exits 0, so the step did not fail, but the record misreads the board.

## The change (v3)

- The probe line is left out of BOTH sides of the comparison, and only that exact line
  (`grep -v -x -F 'CONFIG_RUST_IS_AVAILABLE=y'`). The same name with another value is a difference.
- Two expectations: the raw hash and the hash without the line. Either match gives a verdict:
  `the same as the dev .config` or `the same as the dev .config except CONFIG_RUST_IS_AVAILABLE=y`.
- Any other difference is still `DIFFERS` (raw and without the line), and an unreadable config still fails the step.
- The override variables `BR1_EXPECT_CFG` and `BR1_EXPECT_CFG_NORM` exist only for the tests.

## Tests

`L1-kit-bded77a0-v3/tests/config-tests.sh` runs s02 on fake machines with the real files:

| Case | Input | Expect |
|---|---|---|
| T1 | the real CONFIG_RUST build `.config` (carries the line) | the same as the dev `.config` |
| T2 | the same without the line (the board's shape) | accepted, the line named, not DIFFERS, exit 0 |
| T3 | the board's shape with another symbol changed | DIFFERS |
| T3b | the line present with another value | DIFFERS |
| T3c | another real CONFIG_RUST `.config` (`tmp/CZERO/arm64-rust/.config`) | DIFFERS |
| T4 | an empty config | FAILED, exit 1, other facts still read |
| T5 | the expectation constants against the fixture files | match |

Before the fix (run against the v2 steps, `STEPS=.../L1-kit-bded77a0-v2/steps bash tests/config-tests.sh`):

```
  FAIL  T2 accepted as the dev .config except the probe line: [config: the same as the dev .config except CONFIG_RUST_IS_AVAILABLE=y] not in [config sha256 (dev .config e3dc9f72...): 0d69ee4f5d9ccb585e308652484ae60bb26d4614d2b21ca7927f3470602bc2c7  -
  FAIL  T2b not reported as DIFFERS: [DIFFERS] found in [config sha256 ...
RESULT: 12 passed, 2 failed
```

After the fix (v3):

```
RESULT: 14 passed, 0 failed
```

The existing hold-test suite on v3 (`bash tests/hold-tests.sh`) passes: `RESULT: 242 passed, 0 failed`.
Its guard HS0 pinned the s02 diff to one line; it now pins the v1 change plus this config block (4 removed, 14 added).

## Consequence for the pass (not done here)

s02 is a script of the takeover pass, and the rehearsal marker keys on the exact script hashes. The v3
s02 has a new hash, so the PC dry tests above must be rerun, and the board rehearsal must be run again on
the v3 kit before any real pass. That is a scheduling decision and is not run here.

## Not done

- The step's checksum file (`SHA256SUMS` in the v3 kit) was regenerated; the v2 kit is unchanged.
- No board was touched and no board-side run was made.
