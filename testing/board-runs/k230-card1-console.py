# The U-Boot console step (K230-USB-4, step A and after). The only reader and the only writer of the console
# ttys while it runs. It logs every byte received with a CLOCK_REALTIME stamp and every byte it sends.
# It sends ONE key when the autoboot countdown appears, then one command from --cmds at each prompt, and
# nothing else. It ends when the board is back as 29f1:0230 under a new device number (after the last
# command), or at a time limit, and says which.
#
# usage: card1-console.py --log FILE --cmds 'a;b;c' --reference C3-console.log [--limit SECONDS] [--hold SECONDS] TTY...
#
# card1 copy (B2 card writes, step 6; K230-USB.md "B2 card write run 1"): the bench console script unchanged except for one GATE. Before it sends
# the last command (`boot`), it compares everything the console port has said since "U-Boot SPL" with C3's record
# (--reference), byte for byte, with carriage returns dropped on both sides. One field is masked: the third word of
# the `md.l 0x91106000 4` line, wdt0's live counter (CCVR), a reading of elapsed time, in BOTH places md.l prints it:
# the hex word and its four characters in the ASCII column (columns 8..11, the word's bytes little-endian, '.' for a
# byte that does not print). The four characters must be the rendering of the hex word, and the word must lie in
# 0x03f00000..0x04000000 (within 1.4 s of the enable at TOP 10). Any other difference: `boot` is NOT sent, nothing
# else is sent, and the script waits for the board's return until --limit (the watchdog was armed by the commands
# before `boot`), then exits 2 with "GATE".
#
# Mask v1 (card1, 2026-11-18) masked the hex word only; C3 recorded `........}.......` (0x7d in its counter) and card1
# read `................` (0xa3), so v1 refused `boot` on the counter alone. `--self-test` checks both lines, the v1
# mask failing on them (`--self-test --mask v1`) and this one passing, and that a change outside the counter is
# still refused.
#   --hold: after the last command, keep capturing this long before looking for the board's return
#           (the 60 s kernel capture); the return is then waited for until --limit.
import ast, os, re, select, sys, termios, time

PROMPT = b"K230# "
STOP = b"Hit any key to stop autoboot"
PORT = "/sys/bus/usb/devices/3-4.2.1.4.1"

def arg(name, default=None):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default

log = open(arg("--log"), "a", buffering=1) if "--log" in sys.argv else sys.stdout
cmds = [c.strip().encode() for c in arg("--cmds", "").split(";") if c.strip()]
limit = float(arg("--limit", "120"))
hold = float(arg("--hold", "0"))
# --fallback CMD --fallback-after S: if the board is not back S seconds after the last command, send CMD once
# at the prompt it is still sitting at (a watchdog that did not fire, with U-Boot alive).
fallback = arg("--fallback"); fallback_after = float(arg("--fallback-after", "0")); fell_back = False
skip = set()
for name in ("--log", "--cmds", "--limit", "--hold", "--fallback", "--fallback-after", "--reference", "--mask"):
    if name in sys.argv:
        i = sys.argv.index(name); skip.update((i, i + 1))
ttys = [a for i, a in enumerate(sys.argv[1:], 1) if i not in skip]

COUNTER = re.compile(rb"(91106000: [0-9a-f]{8} [0-9a-f]{8} )([0-9a-f]{8})( [0-9a-f]{8}  )(.{8})(.{4})(.{4})")
SPL = b"U-Boot SPL"
MASK = "v1" if "--mask" in sys.argv and sys.argv[sys.argv.index("--mask") + 1] == "v1" else "v2"

def shown(word):
    """md.l's ASCII column for one word: its bytes little-endian, '.' for a byte that does not print."""
    return bytes(b if 0x20 <= b < 0x7f else 0x2e for b in int(word, 16).to_bytes(4, "little"))

def normal(stream):
    """From the first SPL banner on, carriage returns dropped, the counter masked; and the counter values
    (None for a counter whose ASCII characters are not its hex word's)."""
    at = stream.find(SPL)
    if at < 0:
        return None, []
    s = stream[at:].replace(b"\r", b"")
    counters = [int(m.group(2), 16) if MASK == "v1" or m.group(5) == shown(m.group(2)) else None
                for m in COUNTER.finditer(s)]
    if MASK == "v1":
        return COUNTER.sub(rb"\1--------\3\4\5\6", s), counters
    return COUNTER.sub(rb"\1--------\3\4----\6", s), counters

def reference_stream(path):
    """The console port's bytes, rebuilt from a uboot-console log: rx lines up to the tx of the last command."""
    out = b""; port = None; last = cmds[-1] + b"\r"
    for l in open(path, encoding="utf-8", errors="strict"):
        parts = l.rstrip("\n").split(" ", 3)
        if len(parts) < 4 or parts[1] not in ("rx", "tx"):
            continue
        rest = parts[3]
        q = rest.rfind("'") if rest.startswith("b'") else rest.rfind('"')
        data = ast.literal_eval(rest[:q + 1]); note = rest[q + 1:].strip()
        if parts[1] == "tx":
            port = parts[2]
            if data == last:
                return out
            continue
        if port is not None and parts[2] != port:
            continue
        out += data + (b"" if note.startswith("(") else b"\n")
    raise SystemExit("reference: no tx of %r in %s" % (last, path))

def say(text):
    log.write("%.6f %s\n" % (time.time(), text))

def devnum():
    try:
        if open(PORT + "/idVendor").read().strip() != "29f1": return None
        return int(open(PORT + "/devnum").read())
    except OSError:
        return None

def open_tty(path):
    fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        a = termios.tcgetattr(fd)
        a[0] = 0; a[1] = 0; a[3] = 0                      # raw: no input or output processing, NO ECHO
        a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
        a[4] = a[5] = termios.B115200
        a[6][termios.VMIN] = 0; a[6][termios.VTIME] = 0
        termios.tcsetattr(fd, termios.TCSANOW, a)
    except termios.error:
        pass                                              # a pty in the rehearsal
    return fd

def self_test():
    """C3's recorded md.l line and card1's, each inside the same console text."""
    head = b"U-Boot SPL 2022.10\nK230# md.l 0x91106000 4\n"
    c3 = head + b"91106000: 00000001 0000000a 03fff87d 00000000  ........}.......\nK230# "     # C3-console.log
    card1 = head + b"91106000: 00000001 0000000a 03fff8a3 00000000  ................\nK230# "  # card1-C3-console.log
    off = head + b"91106000: 00000000 0000000a 03fff8a3 00000000  ................\nK230# "    # the dog not enabled
    lie = head + b"91106000: 00000001 0000000a 03fff8a3 00000000  ........}.......\nK230# "    # ASCII not the word's
    (r, _), (l, lc), (o, _), (x, xc) = normal(c3), normal(card1), normal(off), normal(lie)
    checks = [("card1's line equals C3's with the counter masked", l == r and lc == [0x03fff8a3]),
              ("a change outside the counter is still refused", o != r),
              ("ASCII characters that are not the word's are refused", MASK == "v1" or xc == [None])]
    for name, ok in checks:
        print("%s: %s (mask %s)" % ("PASS" if ok else "FAIL", name, MASK))
    sys.exit(0 if all(ok for _, ok in checks) else 1)

if "--self-test" in sys.argv:
    self_test()
REF, REF_COUNTERS = normal(reference_stream(arg("--reference")))
if REF is None:
    raise SystemExit("reference: no SPL banner")
fds = {open_tty(t): t for t in ttys}
raw = {}
buf = {fd: b"" for fd in fds}
line = {fd: b"" for fd in fds}
start = time.time()
old = devnum() if os.environ.get("REHEARSAL") != "1" else -1
say("console start on %s (raw, no echo); board dev %s; commands %r; limit %.0f s; hold %.0f s"
    % (ttys, old, [c.decode() for c in cmds], limit, hold))
gated = None
stopped = False; port = None; sent = 0; last_sent_at = None; last_rx = time.time(); outcome = None; gone = False
while True:
    now = time.time()
    if now - start > limit:
        outcome = "LIMIT: %.0f s passed; stopped=%s, commands sent %d of %d, last byte received %.1f s ago" % (
            limit, stopped, sent, len(cmds), now - last_rx)
        break
    r, _, _ = select.select(list(fds), [], [], 0.1)
    for fd in r:
        try:
            data = os.read(fd, 4096)
        except (BlockingIOError, OSError):
            continue
        if not data:
            continue
        last_rx = time.time()
        raw[fd] = raw.get(fd, b"") + data
        line[fd] += data
        while b"\n" in line[fd]:
            l, line[fd] = line[fd].split(b"\n", 1)
            say("rx %s %r" % (fds[fd][-7:], l.rstrip(b"\r")))
        buf[fd] = (buf[fd] + data)[-8192:]
        if not stopped and STOP in buf[fd]:
            os.write(fd, b" "); stopped = True; port = fd; buf[fd] = b""
            say("tx %s %r (the one key that stops autoboot)" % (fds[fd][-7:], b" "))
        elif stopped and fd == port and buf[fd].endswith(PROMPT) and sent < len(cmds):
            if line[fd]:
                say("rx %s %r (prompt)" % (fds[fd][-7:], line[fd])); line[fd] = b""
            if sent == len(cmds) - 1 and gated is None:
                live, counters = normal(raw.get(fd, b""))
                why = None
                if live != REF:
                    n = next((i for i in range(min(len(live or b""), len(REF))) if live[i] != REF[i]),
                             min(len(live or b""), len(REF)))
                    why = "differs from the reference at byte %d: live %r, reference %r" % (
                        n, (live or b"")[max(0, n - 40):n + 40], REF[max(0, n - 40):n + 40])
                elif len(counters) != 1 or counters[0] is None or not 0x03f00000 <= counters[0] <= 0x04000000:
                    why = "the watchdog counter word %r is outside 0x03f00000..0x04000000" % counters
                if why:
                    gated = False
                    say("GATE: NOT sending %r: %s" % (cmds[sent], why))
                    sent = len(cmds); last_sent_at = time.time(); buf[fd] = b""
                    if os.environ.get("REHEARSAL") == "1":
                        outcome = "GATE: stopped without %r" % cmds[-1]; break
                    continue
                gated = True
                say("GATE: %d bytes since the SPL banner equal C3's record (counter %08x masked, reference %s); sending %r"
                    % (len(live), counters[0], ["%08x" % c for c in REF_COUNTERS], cmds[sent]))
            c = cmds[sent]; sent += 1
            os.write(fd, c + b"\r"); buf[fd] = b""; last_sent_at = time.time()
            say("tx %s %r" % (fds[fd][-7:], c + b"\r"))
    for fd in fds:                                        # a partial line that has sat for a second
        if line[fd] and time.time() - last_rx > 1.0:
            say("rx %s %r (partial)" % (fds[fd][-7:], line[fd])); line[fd] = b""
    if (fallback and not fell_back and sent == len(cmds) and last_sent_at
            and time.time() - last_sent_at > fallback_after and port is not None):
        os.write(port, fallback.encode() + b"\r"); fell_back = True
        say("tx %s %r (FALLBACK: the board was not back %.0f s after the last command)"
            % (fds[port][-7:], fallback.encode() + b"\r", fallback_after))
        if os.environ.get("REHEARSAL") == "1":
            outcome = "REHEARSAL END: fallback sent"; break
    if outcome:
        break
    if os.environ.get("REHEARSAL") == "1":
        if fallback:
            continue
        if sent == len(cmds) and last_sent_at and time.time() - last_sent_at > max(hold, 1.0):
            outcome = "REHEARSAL END: all %d commands sent" % sent; break
        continue
    d = devnum()
    if d is None:
        gone = True
    if sent == len(cmds) and last_sent_at and time.time() - last_sent_at >= hold and d is not None and (gone or d != old) and d != old:
        outcome = "BACK: 29f1:0230 at dev %d (was %d), %.1f s after the last command%s" % (
            d, old, time.time() - last_sent_at, " (after the FALLBACK command)" if fell_back else "")
        break
for fd in fds:
    if line[fd]:
        say("rx %s %r (partial, at the end)" % (fds[fd][-7:], line[fd]))
if gated is False and not outcome.startswith("GATE"):
    outcome = "GATE: stopped without %r; then %s" % (cmds[-1], outcome)
say("console end: %s" % outcome)
print(outcome)
sys.exit(2 if outcome.startswith("GATE") else 0 if outcome.startswith(("BACK", "REHEARSAL END")) else 1)
