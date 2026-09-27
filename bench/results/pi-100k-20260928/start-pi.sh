#!/bin/bash
# start.sh BIN PORT: the app on a copy-free storage dir in the scratchpad
cd $SCRATCH/app
echo $$ > $SCRATCH/app/pid
SECRET_KEY_BASE=$(printf 'a%.0s' {1..128}) DISABLE_SSL=1 HTTP_PORT=$2 TARGET_PORT=$(($2+1)) CAMPFIRE_STORAGE_PATH=$SCRATCH/app/storage exec systemd-run --user --scope -q -p CPUQuota=120% -p MemoryMax=8G taskset -c 8-11 $1 server > $SCRATCH/app/log 2>&1
