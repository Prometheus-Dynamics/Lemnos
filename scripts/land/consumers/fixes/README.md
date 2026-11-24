# Consumer fixes

One file per consumer pin, `<label>-<commit8>.json` (labels in `../consumers.tsv`): the scratch edits that
make that pinned rev of the consumer compile against Lemnos at all, applied to both sides of the compare
(base and candidate), so they never hide a break the candidate adds. They are gate inputs: a change to
one changes the lane's cache key.

Each entry is one edit, with a one-line reason in `why` that names what it bridges at that pin:

    {"file": "src/x.rs", "old": "old text", "new": "new text", "why": "pin f50137ff predates the rename of A to B"}
    {"file": "src/y.rs", "line": 12, "col": 5, "insert": " field: 0,", "why": "pin f50137ff: struct S gained field"}

An empty list means the pin compiles as it is. Every list was regenerated empty on 2026-11-24, after the
machine copy of this data was lost. The lane compares base against candidate, so an empty list can cost
coverage (errors behind a break both sides share are not reached) but never gives a false pass.
