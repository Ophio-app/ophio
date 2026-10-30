#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Leaves a sleeping child behind in each way a program can background work,
# and has each child record its own process id in the current directory.
record='echo $$ > "$0.pid.part" && mv "$0.pid.part" "$0.pid" && exec sleep 300'
sh -c "$record" background </dev/null >/dev/null 2>&1 &
nohup sh -c "$record" nohup </dev/null >/dev/null 2>&1 &
setsid sh -c "$record" setsid </dev/null >/dev/null 2>&1 &
# A daemon forks twice and starts its own session, so nothing waits for it.
(setsid sh -c "$record" daemon </dev/null >/dev/null 2>&1 &)
