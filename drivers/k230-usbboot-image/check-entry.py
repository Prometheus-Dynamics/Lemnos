#!/usr/bin/env python3
"""Checks the stage image's code against the state the boot ROM leaves the core in.

Input: `llvm-objdump -d --no-show-raw-insn` output of the image's ELF, on stdin or as a file.
The rules, each a refusal with the line that broke it:

1. No vector instruction (`v*`). The image is built for riscv64imac (no V). At the trap that started
   this check (run-L2, 2026-11-16) mstatus.VS was Off, so a vector instruction would trap at once.
2. No floating-point instruction (`f*` except `fence`, `fence.i`). The target has no F or D, and the
   core's FS state at that trap was not one the image was built for.
3. CSR access only to the names the entry and the stage use. Any other CSR is refused.
4. Every branch or jump target that is inside the image is the address of an instruction. A target
   in the middle of an instruction (the run-L2 trap was at entry+6, mid-instruction) is refused.
5. The image starts at the entry symbol, which is at the load address.

`--self-test` feeds each rule a listing that must fail, and a clean listing that must pass; the
script refuses to run a check it has not seen fail.
"""
import re
import sys

LOAD = 0x8036_0000
CSR_ALLOWED = {
    "mstatus", "mtvec", "mepc", "mcause", "mtval", "mie", "mip", "mscratch",
    "time", "cycle", "instret",
}
BRANCHES = {"beq", "bne", "blt", "bge", "bltu", "bgeu", "beqz", "bnez", "bgez", "bltz",
            "blez", "bgtz", "j", "jal", "c.j", "c.jal", "call", "tail"}
LINE = re.compile(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$")


def parse(text):
    rows = []
    for line in text.splitlines():
        m = LINE.match(line)
        if m:
            rows.append((int(m.group(1), 16), m.group(2), m.group(3).strip(), line.strip()))
    return rows


def check(rows, load=LOAD):
    """Returns a list of refusals; empty means the listing passes."""
    refusals = []
    if not rows:
        return ["no instructions in the listing"]
    if rows[0][0] != load:
        refusals.append(f"the first instruction is at {rows[0][0]:#x}, not the load address {load:#x}")
    starts = {r[0] for r in rows}
    for addr, mnem, operands, line in rows:
        if mnem.startswith("v"):
            refusals.append(f"vector instruction (the image is rv64imac): {line}")
        if mnem.startswith("f") and not mnem.startswith("fence"):
            refusals.append(f"floating-point instruction (no F or D in the image): {line}")
        if mnem.startswith("csr") or mnem in ("csrr", "csrw", "csrs", "csrc", "csrrw", "csrrs", "csrrc",
                                              "csrrwi", "csrrsi", "csrrci", "csrwi", "csrsi", "csrci"):
            name = operands.split(",")[0].split()[-1] if operands else ""
            csr = operands.split(",")[-1].strip() if "," in operands else name
            if csr not in CSR_ALLOWED and name not in CSR_ALLOWED:
                refusals.append(f"CSR access to a name the image does not use: {line}")
        if mnem in BRANCHES:
            m = re.search(r"0x([0-9a-f]+)", operands)
            if m:
                target = int(m.group(1), 16)
                if load <= target < load + 0x20_0000 and target not in starts:
                    refusals.append(f"branch to {target:#x}, in the middle of an instruction: {line}")
    return refusals


SELF_TESTS = [
    ("clean", "80360000:\tauipc\tt6, 0xc\n80360004:\taddi\tt6, t6, -0x7f0\n80360008:\tbeqz\tt6, 0x80360004 <x>\n", True),
    ("vector", "80360000:\tauipc\tt6, 0xc\n80360004:\tvsetvli\tt0, zero, e8, m1\n", False),
    ("float", "80360000:\tauipc\tt6, 0xc\n80360004:\tfld\tft0, 0(t6)\n", False),
    ("csr", "80360000:\tcsrw\tsatp, zero\n", False),
    ("mid-instruction branch", "80360000:\tj\t0x80360006 <x>\n80360004:\taddi\tt6, t6, -1\n80360008:\taddi\tt0, t0, 1\n", False),
    ("fence is not float", "80360000:\tfence\n80360004:\tfence.i\n", True),
]


def self_test():
    for name, text, expect_pass in SELF_TESTS:
        got = check(parse(text))
        if (not got) != expect_pass:
            print(f"self-test '{name}' gave {got or 'pass'}, expected {'pass' if expect_pass else 'refusal'}")
            return 1
    print(f"self-test: {len(SELF_TESTS)} listings behave as expected")
    return 0


def main(argv):
    if argv[1:] == ["--self-test"]:
        return self_test()
    text = open(argv[1]).read() if len(argv) > 1 else sys.stdin.read()
    if self_test():
        return 1
    refusals = check(parse(text))
    for r in refusals:
        print("REFUSED:", r)
    print("entry check holds" if not refusals else f"{len(refusals)} refusals")
    return 0 if not refusals else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
