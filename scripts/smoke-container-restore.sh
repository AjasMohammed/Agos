#!/usr/bin/env bash
set -euo pipefail

image=${1:?Usage: smoke-container-restore.sh IMAGE}
workdir=$(mktemp -d)
prefix=agentos-smoke-${workdir##*.}
container=$prefix
source_volume=$prefix-source
restore_volume=$prefix-restored

cleanup() {
    status=$?
    if (( status != 0 )); then
        docker logs "$container" >&2 || true
    fi
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker volume rm "$source_volume" "$restore_volume" >/dev/null 2>&1 || true
    rm -rf -- "$workdir"
}
trap cleanup EXIT

# Exercise the packaged config without downloading an embedding model in CI.
docker run --rm --entrypoint cat "$image" /etc/agentos/config.toml |
    sed '/^\[memory\]$/a disable_embedder = true' > "$workdir/config.toml"
chmod 644 "$workdir/config.toml"

boot() {
    local volume=$1 auto_init=$2 readiness api
    docker run -d --name "$container" --read-only \
        --security-opt no-new-privileges:true --tmpfs /tmp --tmpfs /run \
        --tmpfs /var/log/agentos:uid=65532,gid=65532,mode=0755 \
        -p 127.0.0.1::9091 -p 127.0.0.1::8080 \
        -e AGENTOS_DATA_DIR=/var/lib/agentos/data \
        -e AGENTOS_AUTO_INIT_VAULT="$auto_init" \
        -e AGENTOS_VAULT_PASSPHRASE=release-smoke-test-only \
        -v "$volume:/var/lib/agentos/data" \
        -v "$workdir/config.toml:/etc/agentos/config.toml:ro" \
        "$image" >/dev/null
    readiness=$(docker port "$container" 9091/tcp)
    api=$(docker port "$container" 8080/tcp)
    for _ in {1..60}; do
        if curl -fsS --max-time 1 "http://$readiness/readyz" >/dev/null 2>&1 &&
           curl -fsS --max-time 1 "http://$api/api/v1/openapi.json" >/dev/null 2>&1; then
            return
        fi
        sleep 1
    done
    echo "AgentOS readiness or API did not come up" >&2
    return 1
}

boot "$source_volume" true
printf '%s\n' smoke-only | docker exec -i "$container" agentos secret set RELEASE_SMOKE --scope global
docker stop "$container" >/dev/null
docker rm "$container" >/dev/null

# Copy all persisted files together from the stopped writer. Exclude the
# daemon's local socket. Restore into a volume that has never held state.
docker run --rm --user 0 --entrypoint tar -v "$source_volume:/state:ro" \
    "$image" --exclude=./agentos.sock -C /state -cf - . > "$workdir/state.tar"
docker run --rm -i --user 0 --entrypoint tar -v "$restore_volume:/state" \
    "$image" -C /state -xpf - < "$workdir/state.tar"

boot "$restore_volume" false
docker exec "$container" agentos secret list | grep -F RELEASE_SMOKE
echo "Release image booted and restored its vault from a stopped-instance backup"
