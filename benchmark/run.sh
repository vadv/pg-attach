#!/bin/sh
# Outer benchmark orchestrator. Env: PG_VERSION (17), SIZES (default "m l"),
# TABLES (default "events metrics"), REPEATS (default 3).
# Writes CSV to target/benchmark/results-$PG_VERSION.csv and prints medians.
set -eu
PG_VERSION=${PG_VERSION:-17}
SIZES=${SIZES:-"m l"}
TABLES=${TABLES:-"events metrics"}
REPEATS=${REPEATS:-3}
METHODS=${METHODS:-"attach-local-nowal attach-local-wal attach-network copy-text copy-binary dump-text dump-fc"}
root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
image="pg-attach-bdd-pg${PG_VERSION}:dev"
out="$root/target/benchmark/results-$PG_VERSION.csv"
mkdir -p "$(dirname "$out")"
echo "table,size,method,repeat,wall_s,cpu_s,rbytes,wbytes,rios,wios,lsn_bytes,rows,src_chk,dst_chk" >"$out"

for table in $TABLES; do
  for size in $SIZES; do
    for method in $METHODS; do
      i=1
      while [ "$i" -le "$REPEATS" ]; do
        echo "== $table $size $method run $i" >&2
        line=$(docker run --rm --user postgres --shm-size=512m -v "$root/benchmark:/bench:ro" \
          "$image" bash "/bench/bench-inner.sh" "$table" "$size" "$method")
        echo "$table,$size,$method,$i,$line" >>"$out"
        i=$((i + 1))
      done
    done
  done
done

echo "--- medians (wall_s / cpu_s / rbytes / wbytes / lsn_bytes) ---"
awk -F, 'NR>1 {
  k=$1" "$2" "$3
  n[k]++
  w[k,n[k]]=$5; c[k,n[k]]=$6; r[k,n[k]]=$7; b[k,n[k]]=$8; l[k,n[k]]=$11
} END {
  for (k in n) keys[++m]=k
  # insertion order lost; sort by table,size,method via asort alternative
  as=0
  for (i=1;i<=m;i++) for (j=i+1;j<=m;j++) if (keys[j]<keys[i]){t=keys[i];keys[i]=keys[j];keys[j]=t}
  printf "%-32s %10s %10s %12s %12s %12s\n","table size method","wall_s","cpu_s","rbytes","wbytes","lsn_bytes"
  for (i=1;i<=m;i++){
    k=keys[i]; N=n[k]+0.5; idx=int(N); if(N==idx) idx=idx-0.5 # median low/high pick
    # simple median: sort each array
    for(x=1;x<=n[k];x++){for(y=x+1;y<=n[k];y++){
      if(w[k,y]<w[k,x]){t=w[k,x];w[k,x]=w[k,y];w[k,y]=t}
      if(c[k,y]<c[k,x]){t=c[k,x];c[k,x]=c[k,y];c[k,y]=t}
      if(r[k,y]<r[k,x]){t=r[k,x];r[k,x]=r[k,y];r[k,y]=t}
      if(b[k,y]<b[k,x]){t=b[k,x];b[k,x]=b[k,y];b[k,y]=t}
      if(l[k,y]<l[k,x]){t=l[k,x];l[k,x]=l[k,y];l[k,y]=t}
    }}
    lo=int((n[k]+1)/2); hi=lo+(n[k]%2==0?1:0)
    printf "%-32s %10.3f %10.3f %12d %12d %12d\n", k, (w[k,lo]+w[k,hi])/2, (c[k,lo]+c[k,hi])/2, (r[k,lo]+r[k,hi])/2, (b[k,lo]+b[k,hi])/2, (l[k,lo]+l[k,hi])/2
  }
}' "$out"
