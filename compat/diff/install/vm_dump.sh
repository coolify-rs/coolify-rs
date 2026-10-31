#!/bin/bash
# Runs on an install VM and writes what the comparison needs into the folder given as $1.
# Secrets stay out of the output: environment values, password hashes and encrypted columns are
# replaced by their shape in install_diff.py, which reads the raw files, then the folder is deleted.
set -eu
OUT="$1"
mkdir -p "$OUT"
PSQL="docker exec -i coolify-db psql -U coolify -d coolify -X -q -A -t"

docker exec coolify-db pg_dump -U coolify -d coolify -s --no-owner --no-privileges | grep -v '^\\' >"$OUT/schema.sql"
$PSQL -c "SELECT id, migration, batch FROM migrations ORDER BY id" >"$OUT/migrations.txt"
$PSQL -c "SELECT last_value, is_called FROM migrations_id_seq" >"$OUT/migrations_seq.txt"

: >"$OUT/rows.jsonl"
for table in $($PSQL -c "SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY 1"); do
    has_id=$($PSQL -c "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='$table' AND column_name='id'")
    order=""
    [ "$has_id" = "1" ] && order="ORDER BY t.id"
    $PSQL -c "SELECT json_build_object('table','$table','row',row_to_json(t)) FROM public.\"$table\" t $order" >>"$OUT/rows.jsonl"
done
: >"$OUT/sequences.txt"
for seq in $($PSQL -c "SELECT sequencename FROM pg_sequences WHERE schemaname='public' ORDER BY 1"); do
    echo "$seq $($PSQL -c "SELECT last_value || ' ' || is_called FROM public.\"$seq\"")" >>"$OUT/sequences.txt"
done

find /data/coolify -printf '%y %m %U:%G %s %p\n' | sort -k5 >"$OUT/tree.txt"
find /root/.ssh -printf '%y %m %U:%G %p\n' | sort -k4 >"$OUT/root_ssh.txt"
awk '{print $1, $NF}' /root/.ssh/authorized_keys >"$OUT/authorized_keys.txt"
ssh-keygen -lf /root/.ssh/authorized_keys | awk '{print $1, $NF}' | sed 's/^\([0-9]*\) .* /\1 /' >>"$OUT/authorized_keys.txt" || true
cp /etc/docker/daemon.json "$OUT/daemon.json"
docker inspect coolify coolify-db coolify-redis >"$OUT/containers.json"
docker ps -a --format '{{.Names}}|{{.Image}}' | sort >"$OUT/ps.txt"
docker network ls --format '{{.Name}}|{{.Driver}}' | sort >"$OUT/networks.txt"
docker volume ls --format '{{.Name}}' | sort >"$OUT/volumes.txt"
chmod -R a+rX "$OUT"
