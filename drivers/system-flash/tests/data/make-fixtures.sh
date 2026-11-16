#!/bin/sh
# Regenerates the threaded-xz fixtures in this directory from a fixed recipe, with the xz and
# openssl found on PATH (the fixtures were made with xz 5.8.2):
#
#   sh make-fixtures.sh [OUT_DIR]          OUT_DIR defaults to this directory
#
# Written here: threaded-recipe-64m.img.xz, blocks-8192.img.xz, blocks-8193.img.xz and
# threaded-record.xz. The tests never run xz or openssl.
#
# Not written here: threaded-64m.img.xz, which the image builder made from its own raw image. Its
# sha256 is checked below. `xz -6 --check=crc64 --block-size=64MiB -T2` on that raw image gives the
# same bytes. The raw image is not in the repository.
set -eu
DIR=$(cd "$(dirname "$0")" && pwd)
OUT=${1:-$DIR}
PYTHON=${PYTHON:-python3}
# The image builder's command, in one place. Thread counts 2 and up give the same bytes; -T1 gives
# a different (single-thread) layout with no sized block headers, so do not use it.
XZ_CMD="xz -6 --check=crc64 --block-size=64MiB -T2"

echo "xz: $(xz --version | head -1)"
if [ -f "$DIR/threaded-64m.img.xz" ]; then
  want=87de9f1d5b4f15861242925106a7661459fa614b1a9e0f450ce8ea747d1deff0
  got=$(sha256sum "$DIR/threaded-64m.img.xz" | cut -d' ' -f1)
  [ "$got" = "$want" ] || { echo "threaded-64m.img.xz: sha256 $got, expected $want" >&2; exit 1; }
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/make-fixtures.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

# The recipe's image: 160 MiB in 32 units of 5 MiB. Zeros, except: unit 6 is one repeated text
# line, and units 3, 4 and 20 start with a 16 KiB chunk of hash-chain bytes (not compressible).
# Unit 4 repeats unit 3's chunk 5 MiB later, so a match reaches back past 1 MiB of dictionary.
$PYTHON -I - "$TMP/recipe.img" <<'PY'
import hashlib, sys
MIB = 1 << 20
def chain(seed, n):
    out, h = bytearray(), seed
    while len(out) < n:
        h = hashlib.sha256(h).digest()
        out += h
    return bytes(out[:n])
chunk_a = chain(b"lemnos threaded fixture A", 16 << 10)
chunk_b = chain(b"lemnos threaded fixture B", 16 << 10)
text = (b"the first-flash fixture repeats this line\n" * (5 * MIB // 40 + 1))[: 5 * MIB]
with open(sys.argv[1], "wb") as f:
    for i in range(32):
        if i in (3, 4, 20):
            chunk = chunk_b if i == 20 else chunk_a
            f.write(chunk + bytes(5 * MIB - len(chunk)))
        elif i == 6:
            f.write(text)
        else:
            f.write(bytes(5 * MIB))
PY
$XZ_CMD -c "$TMP/recipe.img" > "$OUT/threaded-recipe-64m.img.xz"

# The block-count streams: 64-byte blocks of zeros, check none (the count is what is tested; a
# check adds about 8 bytes a block). 8192 blocks is the largest stream accepted; 8193 is refused.
for n in 8192 8193; do
  head -c $((n * 64)) /dev/zero > "$TMP/zeros-$n.raw"
  xz -6 --check=none --block-size=64 -T2 -c "$TMP/zeros-$n.raw" > "$OUT/blocks-$n.img.xz"
done

# The signature record for the builder's image (the sample's sha256 and size), as the second
# stream. Ed25519 over the first-flash message (tag, version, board, format, size, sha256), seed 7
# repeated 32 times (the test vector's key). The record format is that of drivers/slot-store.
# The check: the same steps reproduce vector-record.bin for small.img, byte for byte.
record() { # $1 = size, $2 = sha256 hex, $3 = out file
  $PYTHON -I - "$1" "$2" "$TMP/msg.bin" <<'PY'
import struct, sys
size, sha = int(sys.argv[1]), bytes.fromhex(sys.argv[2])
board, fmt = b"flash-test", b"disk"
msg = (b"lemnos-first-flash\0" + struct.pack("<I", 1)
       + struct.pack("<I", len(board)) + board
       + struct.pack("<I", len(fmt)) + fmt
       + struct.pack("<Q", size) + sha)
open(sys.argv[3], "wb").write(msg)
PY
  # The seed as PKCS#8 (Ed25519), and its public key.
  printf '302e020100300506032b657004220420' | xxd -r -p > "$TMP/key.der"
  printf '%s' "$(printf '07%.0s' $(seq 32))" | xxd -r -p >> "$TMP/key.der"
  openssl pkey -inform DER -in "$TMP/key.der" -out "$TMP/key.pem" 2>/dev/null
  openssl pkey -in "$TMP/key.pem" -pubout -outform DER 2>/dev/null | tail -c 32 > "$TMP/pub.bin"
  openssl pkeyutl -sign -inkey "$TMP/key.pem" -rawin -in "$TMP/msg.bin" -out "$TMP/sig.bin"
  $PYTHON -I - "$TMP/pub.bin" "$TMP/sig.bin" "$3" <<'PY'
import hashlib, sys
pub, sig = open(sys.argv[1], "rb").read(), open(sys.argv[2], "rb").read()
assert len(pub) == 32 and len(sig) == 64
rec = b"LMNSSIG1" + bytes([1, 0, 0, 0]) + hashlib.sha256(pub).digest() + sig
assert len(rec) == 108
open(sys.argv[3], "wb").write(rec)
PY
}
record 16384 f30a82fe97de9b784dd7b5735958b28dbfa95f94c0ba6d4834b441ab78b65c58 "$TMP/vector.bin"
cmp "$TMP/vector.bin" "$DIR/vector-record.bin" || { echo "the record steps do not reproduce vector-record.bin" >&2; exit 1; }
record 167772160 326c678268e0a2ca05ce03d3c5cc2b0737f6137bb484a53017a998cfd507a16f "$TMP/sample.bin"
xz -0 --check=crc64 -c "$TMP/sample.bin" > "$OUT/threaded-record.xz"

echo "wrote threaded-recipe-64m.img.xz, blocks-8192.img.xz, blocks-8193.img.xz, threaded-record.xz"
