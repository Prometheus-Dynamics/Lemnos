#!/bin/sh
# ExecStop of lemnos-guest-slow.service: marks the stop on the console, sleeps 5 s, marks the end. The uptime
# is read from /proc/uptime (the guest's own clock), so the host can check the delay without its own clock.
read -r up _ < /proc/uptime
echo "RESTART-GUEST SLOW-STOP-BEGIN uptime=$up" > /dev/console
sleep 5
read -r up _ < /proc/uptime
echo "RESTART-GUEST SLOW-STOP-END uptime=$up" > /dev/console
