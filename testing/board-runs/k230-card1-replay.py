# The stand-in for the card1 gate rehearsal: a pty that replays the console port's bytes from C3's record
# (rebuilt as card1-console.py rebuilds them), stopping for the key after the countdown and for a command after each
# prompt. After `boot` it prints one line and ends. MUTATE=1 changes one byte of the bootargs line (the failing case);
# COUNTER=hex replaces the md.l counter word (the mask must accept a different reading).
import os, pty, re, select, sys, time
m, s = pty.openpty()
print(os.ttyname(s), flush=True)
REF = sys.argv[1]
import ast
def reference_stream(path):
    out = b""; port = None
    for l in open(path):
        parts = l.rstrip("\n").split(" ", 3)
        if len(parts) < 4 or parts[1] not in ("rx", "tx"):
            continue
        rest = parts[3]; q = rest.rfind("'") if rest.startswith("b'") else rest.rfind('"')
        data = ast.literal_eval(rest[:q + 1]); note = rest[q + 1:].strip()
        if parts[1] == "tx":
            port = parts[2]
            if data == b"boot\r":
                return out
            continue
        if port is not None and parts[2] != port:
            continue
        out += data + (b"" if note.startswith("(") else b"\r\n")
stream = reference_stream(REF)
if os.environ.get("MUTATE") == "1":
    stream = stream.replace(b"earlycon=sbi\r\n", b"earlycon=sbj\r\n", 1)
if os.environ.get("COUNTER"):
    w = os.environ["COUNTER"]; text = bytes(b if 0x20 <= b < 0x7f else 0x2e for b in int(w, 16).to_bytes(4, "little"))
    if os.environ.get("ASCII_KEEP") == "1":
        text = None                                       # the hex word changed, its ASCII characters left as C3's
    stream = re.sub(rb"(91106000: [0-9a-f]{8} [0-9a-f]{8} )[0-9a-f]{8}( [0-9a-f]{8}  .{8})(.{4})",
                    lambda m: m.group(1) + w.encode() + m.group(2) + (m.group(3) if text is None else text), stream)
time.sleep(1)
KEY = b"Hit any key to stop autoboot:  3 "
pieces = []; at = 0
for mark in [KEY] + [b"K230# "] * stream.count(b"K230# "):
    j = stream.index(mark, at) + len(mark); pieces.append(stream[at:j]); at = j
got = []
for p in pieces:
    os.write(m, p)
    r, _, _ = select.select([m], [], [], 10)
    if not r:
        sys.stderr.write("stand-in: nothing sent after %r; received %r\n" % (p[-40:], got)); sys.exit(0)
    got.append(os.read(m, 256))
    if got[-1] == b"boot\r":
        os.write(m, b"boot\r\nstand-in: booting\r\n"); time.sleep(1); break
sys.stderr.write("stand-in received: %r\n" % got)
