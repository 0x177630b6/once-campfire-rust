#!/bin/bash
# run100k.sh BIN LABEL [CLIENTS] [extra loadgen args...]: the app on 4 cores, N cable clients, idle hold, then fan-out.
S=$(dirname $0); BIN=$1; LABEL=$2; N=${3:-100000}; shift 3
LG=/home/dhh/Work/basecamp/once-campfire-rust/target/bench/release/loadgen
kill $(cat $S/app/pid) 2>/dev/null; sleep 1; ( $S/${START:-start.sh} $BIN 4555 & ); sleep 2
PID=$(cat $S/app/pid); C=$(cat $S/cookie); STREAMS=$(python3 -c "import json; print(','.join(json.load(open('$S/scrape.json'))['streams']))")
( while sleep 1; do
  a=$(awk '{print $14+$15}' /proc/$PID/stat 2>/dev/null); p=$(awk '/^Pss:/{print $2}' /proc/$PID/smaps_rollup 2>/dev/null)
  l=$(pgrep -x loadgen | head -1); lc=$( [ -n "$l" ] && awk '{print $14+$15}' /proc/$l/stat || echo 0)
  [ -n "$a" ] && echo "$(date +%s.%N) $p $a $lc"
done ) > $S/samp-$LABEL.txt 2>/dev/null & SAMP=$!
taskset -c 12-23 $LG cable --base http://127.0.0.1:4555 --cookie "$C" --room 1 --streams "$STREAMS" --clients $N \
  --sources ${SOURCES:-127.0.0.10,127.0.0.11,127.0.0.12,127.0.0.13,127.0.0.14,127.0.0.15,127.0.0.16,127.0.0.17,127.0.0.18,127.0.0.19} --hold-secs 30 --latency-msgs 10 --tput-secs 10 --posters 2 "$@" \
  > $S/cable-$LABEL.json 2> $S/cable-$LABEL.err
kill $SAMP
python3 - $S $LABEL <<'PY'
import json, sys, os
S, label = sys.argv[1:]
TCK = os.sysconf("SC_CLK_TCK")
r = json.load(open(f"{S}/cable-{label}.json"))
print(label, "ready", r["ready"], "failed", r["failed"], "connect_s", r["connect_secs"], "post->all p50/p99", r["latency"]["all_clients"].get("p50_ms"), r["latency"]["all_clients"].get("p99_ms"), "deliveries/s", round(r["throughput"]["delivered_msgs_per_sec"] * r["ready"]))
rows = [tuple(map(float, l.split())) for l in open(f"{S}/samp-{label}.txt") if len(l.split()) == 4]
phases = {}
for line in open(f"{S}/cable-{label}.err"):
    if line.startswith("PHASE"):
        _, name, ms = line.split(); phases[name] = int(ms) / 1000
names = sorted(phases, key=phases.get)
for a, b in zip(names, names[1:] + [None]):
    t1 = phases[b] if b else rows[-1][0]
    sel = [x for x in rows if phases[a] <= x[0] <= t1]
    if len(sel) < 2: continue
    dt = sel[-1][0] - sel[0][0]
    print(f"  {a:18s} {dt:5.1f}s app {(sel[-1][2]-sel[0][2])/TCK/dt:.2f} cores  loadgen {(sel[-1][3]-sel[0][3])/TCK/dt:.2f} cores  pss {max(x[1] for x in sel)/1024:.0f} MB")
PY
