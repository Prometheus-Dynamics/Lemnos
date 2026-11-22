#!/bin/sh
# ExecStop of lemnos-guest-blocker.service (case b). It runs during the shutdown, before lemnosd stops, and prints lemnosd's
# answer to a state query each second for 40 s, so the console shows what the daemon says while the reboot waits on this stop.
exec >/dev/console 2>&1
i=0
while [ $i -lt 40 ]; do
	i=$((i + 1))
	list=$(/usr/bin/lemnos-restart-lemnosd-check state /run/lemnos/lemnosd.sock 2>&1 | sed -n 's/.*STATE list //p')
	echo "LEMNOSD-RESTART-GUEST BLOCKER-STOP t=$i daemon answered: $(echo "$list" | grep -o '"slot_trial":"[^"]*"\|"state":"[a-z-]*"' | tr '\n' ' ')"
	sleep 1
done
echo "LEMNOSD-RESTART-GUEST BLOCKER-STOP done after $i s"
