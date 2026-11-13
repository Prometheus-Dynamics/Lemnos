#!/bin/sh
# Runs as root in a mount namespace, by testing/mdev/check.sh. Arguments: the BusyBox binary, and a
# work directory holding sys/ (a fake sysfs), etc/ (passwd, group, mdev.conf) and placeholders (one
# node name per line, relative to /dev). It makes a tmpfs /dev and /etc, binds the fake sysfs on
# /sys, places one placeholder file at each node name, runs `mdev -s`, and prints each placeholder's
# owner, group and mode as "path owner group mode".
#
# A placeholder stands in for the node: an unprivileged user namespace may not create a device node
# (mknod gives EPERM), and mdev applies its chown and chmod to a path that exists (its mknod gets
# EEXIST, which mdev ignores). The placeholder is at the path mdev computes, so the owner, group and
# mode it prints are the ones mdev gave that path.
set -eu
bb="$1"
w="$2"
mount -t tmpfs -o mode=0755 tmpfs /dev
mount --bind "$w/sys" /sys
mount -t tmpfs -o mode=0755 tmpfs /etc
cp "$w/etc/passwd" "$w/etc/group" "$w/etc/mdev.conf" /etc/
while read -r name; do
  mkdir -p "/dev/$(dirname "$name")"
  : > "/dev/$name"
  chmod 600 "/dev/$name"
done < "$w/placeholders"
"$bb" mdev -s 2>"$w/mdev.err" || true
while read -r name; do
  stat -c "%n %U %G %a" "/dev/$name" 2>/dev/null || echo "/dev/$name missing"
done < "$w/placeholders"
