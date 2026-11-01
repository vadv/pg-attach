#!/bin/bash
# Inner benchmark: runs INSIDE a fresh pg-attach-bdd image container.
# Args: TABLE(events|metrics) SIZE(s|m|l) METHOD
# Prints one CSV line: wall_s,cpu_s,rbytes,wbytes,rios,wios,lsn_bytes,rows,src_chk,dst_chk
set -eu
TABLE=$1 SIZE=$2 METHOD=$3

case "$SIZE" in
  s) ROWS=10000 ;; m) ROWS=100000 ;; l) ROWS=500000 ;;
  *) echo "bad size $SIZE" >&2; exit 1 ;;
esac
case "$TABLE" in events|metrics) ;; *) echo "bad table $TABLE" >&2; exit 1 ;; esac
case "$METHOD" in
  attach-local-nowal|attach-local-wal|attach-network|copy-text|copy-binary) ;;
  *) echo "bad method $METHOD" >&2; exit 1 ;;
esac

TWO_INSTANCE=0
case "$METHOD" in attach-network|copy-*) TWO_INSTANCE=1 ;; esac

GA=/tmp/dga GB=/tmp/dgb SA=/tmp/socka SB=/tmp/sockb
mkdir -p $SA $SB
PA=55432 PB=55433
psql_a() { psql -h $SA -p $PA -U postgres -d postgres -qAt -v ON_ERROR_STOP=1 "$@"; }
psql_b() { psql -h $SB -p $PB -U postgres -d postgres -qAt -v ON_ERROR_STOP=1 "$@"; }

initdb -D $GA -A trust >/dev/null
pg_ctl -D $GA -l /tmp/a.log -o "-k $SA -p $PA" -w start >/dev/null
if [ "$TWO_INSTANCE" = 1 ]; then
  initdb -D $GB -A trust >/dev/null
  pg_ctl -D $GB -l /tmp/b.log -o "-k $SB -p $PB" -w start >/dev/null
fi

case "$TABLE.$METHOD" in
  events.copy-*|metrics.copy-*) ;;
esac

psql_a -c "CREATE EXTENSION pg_attach;"
case "$TABLE" in
  events)
    psql_a -c "CREATE TABLE public.events(id bigint PRIMARY KEY, payload text NOT NULL, note text); CREATE INDEX ON public.events(note);"
    CHK="coalesce(sum(length(payload)),0)"
    COPYCOLS="id,payload,note"; DSTIDX="note" ;;
  metrics)
    psql_a -c "CREATE TABLE public.metrics(id bigint PRIMARY KEY, ts timestamptz NOT NULL, val double precision NOT NULL, tag text); CREATE INDEX ON public.metrics(ts);"
    CHK="coalesce(sum(length(tag)),0)"
    COPYCOLS="id,ts,val,tag"; DSTIDX="ts" ;;
esac

if [ "$TWO_INSTANCE" = 1 ]; then
  psql_b -c "CREATE EXTENSION pg_attach;"
  case "$METHOD" in
    copy-*)
      case "$TABLE" in
        events)  psql_b -c "CREATE TABLE public.events_copy(id bigint, payload text NOT NULL, note text);" ;;
        metrics) psql_b -c "CREATE TABLE public.metrics_copy(id bigint, ts timestamptz NOT NULL, val double precision NOT NULL, tag text);" ;;
      esac ;;
  esac
fi

case "$TABLE" in
  events)
    psql_a -c "INSERT INTO public.$TABLE SELECT g, repeat(md5(g::text||'seed'),320), CASE WHEN g%3=0 THEN NULL ELSE 'note-'||(g%100) END FROM generate_series(1,$ROWS) g;" ;;
  metrics)
    psql_a -c "INSERT INTO public.$TABLE SELECT g, '2024-01-01'::timestamptz + (g||' seconds')::interval, (g%1000)::float8/7, 'tag-'||(g%50) FROM generate_series(1,$ROWS) g;" ;;
esac
psql_a -c "CHECKPOINT;"
sync; sleep 2

snap_io() { awk '{r+=substr($2,8)+0; w+=substr($3,8)+0; ri+=substr($4,6)+0; wi+=substr($5,6)+0} END{printf "%d %d %d %d", r, w, ri, wi}' /sys/fs/cgroup/io.stat; }
snap_cpu() { awk '/^usage_usec/{print $2}' /sys/fs/cgroup/cpu.stat; }
snap_lsn() { L=$(psql_a -c "select pg_current_wal_lsn()::text"); M=0; if [ "$TWO_INSTANCE" = 1 ]; then M=$(psql_b -c "select pg_current_wal_lsn()::text"); fi; echo "$L $M"; }
lsn_num() { echo $(( 0x$(echo "$1" | cut -d/ -f1) * 16777216 + 0x$(echo "$1" | cut -d/ -f2) )); }

IO1=$(snap_io); CPU1=$(snap_cpu); LSN1=$(snap_lsn)
T0=$(date +%s.%N)

case "$METHOD" in
  attach-local-nowal) psql_a -c "SELECT pg_attach_import_local('public.$TABLE','public.${TABLE}_copy',false);" >/dev/null ;;
  attach-local-wal)   psql_a -c "SELECT pg_attach_import_local('public.$TABLE','public.${TABLE}_copy',true);" >/dev/null ;;
  attach-network)     psql_b -c "SELECT pg_attach_import('host=$SA port=$PA dbname=postgres user=postgres','public.$TABLE','public.${TABLE}_copy',true);" >/dev/null ;;
  copy-text|copy-binary)
    FMT=text; [ "$METHOD" = copy-binary ] && FMT=binary
    psql_a -c "COPY (SELECT $COPYCOLS FROM public.$TABLE) TO STDOUT (FORMAT $FMT)" \
      | psql_b -c "COPY public.${TABLE}_copy FROM STDIN (FORMAT $FMT)" >/dev/null
    psql_b -c "ALTER TABLE public.${TABLE}_copy ADD PRIMARY KEY (id);" \
              -c "CREATE INDEX ON public.${TABLE}_copy($DSTIDX);" \
              -c "ANALYZE public.${TABLE}_copy;" >/dev/null ;;
esac

T1=$(date +%s.%N)
# flush dirty pages so writeback is fully counted; outside the wall window
if [ "$TWO_INSTANCE" = 1 ]; then psql_b -c "CHECKPOINT;" >/dev/null; else psql_a -c "CHECKPOINT;" >/dev/null; fi
sync
# wait until block-layer write counters stop moving (writeback settled)
prev=-1; stable=0; waited=0
while [ $stable -lt 2 ] && [ $waited -lt 300 ]; do
  cur=$(awk '{w+=substr($3,8)+0} END{print w+0}' /sys/fs/cgroup/io.stat)
  if [ "$cur" = "$prev" ]; then stable=$((stable+1)); else stable=0; fi
  prev=$cur; sleep 2; waited=$((waited+2))
done
IO2=$(snap_io); CPU2=$(snap_cpu); LSN2=$(snap_lsn)

ROWS_SRC=$(psql_a -c "select count(*) from public.$TABLE")
CHK_SRC=$(psql_a -c "select $CHK from public.$TABLE")
if [ "$TWO_INSTANCE" = 1 ]; then Q="-h $SB -p $PB"; else Q="-h $SA -p $PA"; fi
ROWS_DST=$(psql $Q -U postgres -d postgres -qAt -v ON_ERROR_STOP=1 -c "select count(*) from public.${TABLE}_copy")
CHK_DST=$(psql $Q -U postgres -d postgres -qAt -v ON_ERROR_STOP=1 -c "select $CHK from public.${TABLE}_copy")

read -r r1a r1b r1c r1d <<EOF2
$IO1
EOF2
read -r r2a r2b r2c r2d <<EOF2
$IO2
EOF2
WALL=$(awk "BEGIN{printf \"%.3f\", $T1-$T0}")
CPU=$(awk "BEGIN{printf \"%.3f\", ($CPU2-$CPU1)/1000000}")
RB=$((r2a-r1a)); WB=$((r2b-r1b)); RI=$((r2c-r1c)); WI=$((r2d-r1d))
lsn1=($LSN1); lsn2=($LSN2)
LSNBYTES=$(( $(lsn_num "${lsn2[0]}") - $(lsn_num "${lsn1[0]}") + $(lsn_num "${lsn2[1]}") - $(lsn_num "${lsn1[1]}") ))

[ "$ROWS_SRC" = "$ROWS_DST" ] && [ "$CHK_SRC" = "$CHK_DST" ] || { echo "VERIFY FAILED src=$ROWS_SRC/$CHK_SRC dst=$ROWS_DST/$CHK_DST" >&2; exit 1; }
echo "$WALL,$CPU,$RB,$WB,$RI,$WI,$LSNBYTES,$ROWS_SRC,$CHK_SRC,$CHK_DST"
