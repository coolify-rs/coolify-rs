#!/bin/sh
# The VM of compat/diff/metrics_diff.py: a real Sentinel with metrics and traffic on, a real Traefik and
# real containers, so both sides read the same source without touching the shared test VM.
#   compat/diff/metrics_vm.sh up <token>     creates the VM when missing and (re)starts the containers;
#                                            APP1_ID, APP2_ID, DB_ID, SERVICE_ID, SERVICE_APP_ID set their labels
#   compat/diff/metrics_vm.sh traffic        sends requests through Traefik
#   compat/diff/metrics_vm.sh run            runs the script on stdin on the VM
#   compat/diff/metrics_vm.sh down           deletes the VM
# Images pulled on the VM: coollabsio/sentinel:1.1.3, traefik:v3.6, nginx:alpine, redis:alpine.
set -e
VM=coolify-rs-agent-metrics
APP=metricsapp1aaaaaaaaaaaaaa
APP2=metricsapp2aaaaaaaaaaaaaa
DB=metricsdb1aaaaaaaaaaaaaaa
SERVICE=metricssvc1aaaaaaaaaaaaaa
run() { orb -m "$VM" -u root sh -s; }

case "$1" in
up)
    TOKEN=$2
    if ! orb list | grep -q "^$VM "; then
        orb create ubuntu:noble "$VM" >/dev/null
        run <<'EOF'
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq docker.io openssh-server curl jq >/dev/null 2>&1
systemctl enable --now ssh docker >/dev/null 2>&1
mkdir -p /root/.ssh
chmod 700 /root/.ssh
EOF
        orb -m coolify-rs-e2e -u root cat /root/.ssh/authorized_keys | orb -m "$VM" -u root tee /root/.ssh/authorized_keys >/dev/null
    fi
    run <<EOF
set -e
docker network create coolify >/dev/null 2>&1 || true
mkdir -p /data/coolify/proxy /data/coolify/sentinel
touch /data/coolify/proxy/access.log
for c in coolify-sentinel coolify-proxy $APP $APP2 $DB $SERVICE web-$SERVICE; do docker rm -f \$c >/dev/null 2>&1 || true; done
docker run -d --name coolify-proxy --restart unless-stopped --network coolify -p 80:80 \
  -v /var/run/docker.sock:/var/run/docker.sock -v /data/coolify/proxy:/traefik \
  --label coolify.managed=true --label coolify.proxy=true traefik:v3.6 \
  --entrypoints.http.address=:80 --providers.docker=true --providers.docker.exposedbydefault=false \
  --accesslog=true --accesslog.filepath=/traefik/access.log --accesslog.format=json \
  --accesslog.bufferingsize=0 --accesslog.fields.headers.names.User-Agent=keep \
  --accesslog.fields.headers.names.X-Forwarded-For=keep --accesslog.fields.headers.names.Referer=keep >/dev/null
for row in "$APP:app1.metrics.test:${APP1_ID:-0}" "$APP2:app2.metrics.test:${APP2_ID:-0}"; do
  c=\$(echo \$row | cut -d: -f1); host=\$(echo \$row | cut -d: -f2); id=\$(echo \$row | cut -d: -f3)
  docker run -d --name \$c --restart unless-stopped --network coolify \
    --label coolify.managed=true --label coolify.applicationId=\$id --label coolify.pullRequestId=0 --label coolify.type=application \
    --label traefik.enable=true --label "traefik.http.routers.http-0-\$c.rule=Host(\\\`\$host\\\`)" \
    --label traefik.http.routers.http-0-\$c.entrypoints=http \
    --label traefik.http.services.http-0-\$c.loadbalancer.server.port=80 nginx:alpine >/dev/null
done
docker run -d --name web-$SERVICE --restart unless-stopped --network coolify \
  --label coolify.managed=true --label coolify.serviceId=${SERVICE_ID:-0} --label coolify.type=service --label coolify.service.subId=${SERVICE_APP_ID:-0} --label coolify.service.subType=application --label coolify.service.subName=web \
  --label traefik.enable=true --label "traefik.http.routers.http-0-$SERVICE-web.rule=Host(\\\`svc.metrics.test\\\`)" \
  --label traefik.http.routers.http-0-$SERVICE-web.entrypoints=http \
  --label traefik.http.services.http-0-$SERVICE-web.loadbalancer.server.port=80 nginx:alpine >/dev/null
docker run -d --name $DB --restart unless-stopped --network coolify \
  --label coolify.managed=true --label coolify.databaseId=${DB_ID:-0} --label coolify.type=database redis:alpine >/dev/null
docker run -d --name coolify-sentinel --restart unless-stopped \
  -e TOKEN=$TOKEN -e DEBUG=false -e PUSH_ENDPOINT=http://127.0.0.1:9/ -e PUSH_INTERVAL_SECONDS=60 \
  -e COLLECTOR_ENABLED=true -e COLLECTOR_REFRESH_RATE_SECONDS=5 -e COLLECTOR_RETENTION_PERIOD_DAYS=7 \
  -e TRAFFIC_ENABLED=true -e TRAFFIC_PROXY_TYPE=auto -e TRAFFIC_ACCESS_LOG_PATH=/data/coolify/proxy/access.log \
  -e TRAFFIC_TOPN=50 -e TRAFFIC_SAMPLE_THRESHOLD=0 -e TRAFFIC_RETENTION_1H_DAYS=30 -e TRAFFIC_RETENTION_1D_DAYS=395 \
  -e GEOIP_ENABLED=false -e GEOIP_REFRESH_DAYS=30 -e TRAFFIC_IP_MODE=full \
  -v /var/run/docker.sock:/var/run/docker.sock -v /data/coolify/sentinel:/app/db \
  -v /data/coolify/proxy:/data/coolify/proxy:ro --pid host --userns host \
  --label coolify.managed=true ghcr.io/coollabsio/sentinel:1.1.3 >/dev/null
EOF
    ;;
traffic)
    run <<'EOF'
ua1='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
ua2='Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1'
ua3='Mozilla/5.0 (compatible; GPTBot/1.0; +https://openai.com/gptbot)'
for host in app1.metrics.test app1.metrics.test app2.metrics.test svc.metrics.test; do
  for i in 1 2 3 4 5 6; do
    curl -s -o /dev/null -H "Host: $host" -H "User-Agent: $ua1" -H "Referer: https://news.example.test/a" -H "X-Forwarded-For: 203.0.113.$i" http://127.0.0.1/
    curl -s -o /dev/null -H "Host: $host" -H "User-Agent: $ua2" -H "X-Forwarded-For: 198.51.100.$i" http://127.0.0.1/index.html
    curl -s -o /dev/null -H "Host: $host" -H "User-Agent: $ua3" http://127.0.0.1/missing-$i
  done
done
EOF
    ;;
run)
    # A script on stdin, as the coordinator's rerun and the fake Sentinel use.
    run
    ;;
down)
    orb delete -f "$VM" >/dev/null
    ;;
*)
    echo "usage: $0 up <token> | traffic | run | down" >&2
    exit 2
    ;;
esac
