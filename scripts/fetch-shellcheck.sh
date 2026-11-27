#!/usr/bin/env bash
# Fetches the pinned ShellCheck release for this machine into <directory> (once; a cached copy is
# checked again) and prints the binary's path. The sha256 pins are the digests GitHub publishes
# for the release assets of koalaman/shellcheck, the project's own releases.
#
#   fetch-shellcheck.sh <directory>
set -euo pipefail

version=v0.11.0
declare -A sha256=(
  [x86_64]=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198
  [aarch64]=12b331c1d2db6b9eb13cfca64306b1b157a86eb69db83023e261eaa7e7c14588
)
out="${1:?usage: fetch-shellcheck.sh <directory>}"
arch="$(uname -m)"
[[ -n "${sha256[$arch]:-}" ]] || { echo "fetch-shellcheck.sh: no pinned release for $arch" >&2; exit 2; }
mkdir -p "$out"
out="$(cd "$out" && pwd)"
exec 9>"$out/.fetch-shellcheck.lock"
flock 9
name="shellcheck-$version.linux.$arch.tar.xz"
tarball="$out/$name"
if [[ ! -f "$tarball" ]]; then
  curl -fsSL -o "$tarball.part" "https://github.com/koalaman/shellcheck/releases/download/$version/$name"
  mv "$tarball.part" "$tarball"
fi
echo "${sha256[$arch]}  $tarball" | sha256sum -c - >&2
bin="$out/shellcheck-$version/shellcheck"
if [[ ! -x "$bin" ]]; then
  rm -rf "$out/shellcheck-$version"
  xz -dc "$tarball" | tar -C "$out" -xf -
fi
[[ "$("$bin" --version)" == *"version: ${version#v}"* ]] || { echo "fetch-shellcheck.sh: $bin is not $version" >&2; exit 1; }
echo "$bin"
