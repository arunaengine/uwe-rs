#!/usr/bin/env bash
# Runs a workflow with the Kubernetes executor on a kind cluster, with object-store staging (with the
# uploaded binary, a helper image, or credentials), with kubedal staging, and with a shared claim.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
#
# Needs docker, kind, kubectl and helm, the static binary of `just release-binary` (IUNCTUS),
# a checkout of kubedal (KUBEDAL, default ../kubedal) and its image (KUBEDAL_IMAGE, default
# kubedal:e2e). MODES picks some of self, image-volume, init-container, kubedal, credentials and
# shared-claim;
# KIND_CLUSTER names the cluster (default iunctus-staging); KEEP_CLUSTER=1 keeps the cluster
# and the S3 server after the run.
set -euo pipefail

repository="$(cd "$(dirname "$0")/.." && pwd)"
binary="$(realpath "${IUNCTUS:-$repository/target/x86_64-unknown-linux-musl/release/iunctus}")"
kubedal="${KUBEDAL:-$repository/../kubedal}"
kubedal_image="${KUBEDAL_IMAGE:-kubedal:e2e}"
cluster="${KIND_CLUSTER:-iunctus-staging}"
server="$cluster-s3"
work="$(mktemp -d)"
# The folder that this machine and the node share, for the cache of shared paths.
shared="$work/shared"
mkdir "$shared"
cleanup() {
    if [ "${KEEP_CLUSTER:-0}" != 1 ]; then
        kind delete cluster --name "$cluster" > /dev/null 2>&1 || true
        docker stop "$server" > /dev/null 2>&1 || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

echo "=== kind cluster with image volumes"
printf 'kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nfeatureGates:\n  ImageVolume: true\n' \
    > "$work/kind.yaml"
printf 'nodes:\n- role: control-plane\n  extraMounts:\n  - {hostPath: "%s", containerPath: "%s"}\n' \
    "$shared" "$shared" >> "$work/kind.yaml"
kind create cluster --name "$cluster" --image "${KIND_IMAGE:-kindest/node:v1.34.0}" \
    --config "$work/kind.yaml" --kubeconfig "$work/kubeconfig" --wait 120s
export KUBECONFIG="$work/kubeconfig"

echo "=== S3 server on the network of kind, which this machine and the pods reach"
docker run -d --rm --name "$server" --network kind -e RUSTFS_ACCESS_KEY=staging-user \
    -e RUSTFS_SECRET_KEY=staging-secret-4410 rustfs/rustfs:latest > /dev/null
address="$(docker inspect -f '{{.NetworkSettings.Networks.kind.IPAddress}}' "$server")"
endpoint="http://$address:9000"
for _ in $(seq 60); do
    curl -sf --aws-sigv4 aws:amz:us-east-1:s3 --user staging-user:staging-secret-4410 -X PUT \
        "$endpoint/staging" > /dev/null 2>&1 && break
    sleep 1
done

echo "=== helper image and kubedal"
mkdir "$work/helper"
cp "$binary" "$work/helper/iunctus"
printf 'FROM scratch\nCOPY iunctus /iunctus\n' > "$work/helper/Dockerfile"
# Another run may build its own helper image at the same time.
helper="iunctus-helper:$cluster"
docker build -q -t "$helper" "$work/helper" > /dev/null
kind load docker-image --name "$cluster" "$helper" debian:stable-slim "$kubedal_image"
helm upgrade --install kubedal "$kubedal/charts/kubedal" --namespace kube-system \
    --set image.repository="${kubedal_image%%:*}" --set image.tag="${kubedal_image##*:}" \
    --set image.pullPolicy=Never --wait --timeout 180s > /dev/null
kubectl create namespace iunctus > /dev/null
kubectl apply -f - > /dev/null <<EOF
apiVersion: v1
kind: Secret
metadata: {name: staging-credentials, namespace: iunctus}
stringData: {access_key_id: staging-user, secret_access_key: staging-secret-4410}
---
apiVersion: v1
kind: Secret
metadata: {name: staging-storage, namespace: iunctus}
stringData:
  IUNCTUS_STAGING_STORAGE: |
    [storage.staging]
    type = "s3"
    prefixes = ["s3://staging/"]
    endpoint = "$endpoint"
    region = "us-east-1"
    access_key_id = "staging-user"
    secret_access_key = "staging-secret-4410"
---
apiVersion: kubedal.arunaengine.org/v1alpha1
kind: DataNode
metadata: {name: staging, namespace: iunctus}
spec:
  backend: S3
  read_only: false
  limit: 1Gi
  secretRef: {name: staging-credentials}
  config: {bucket: staging, endpoint: "$endpoint", region: us-east-1, root: /kubedal}
---
apiVersion: v1
kind: PersistentVolume
metadata: {name: iunctus-cache}
spec:
  capacity: {storage: 1Gi}
  accessModes: [ReadWriteMany]
  storageClassName: ""
  hostPath: {path: "$shared"}
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: iunctus-cache, namespace: iunctus}
spec:
  accessModes: [ReadWriteMany]
  storageClassName: ""
  volumeName: iunctus-cache
  resources: {requests: {storage: 1Gi}}
EOF

export STAGING_KEY=staging-user STAGING_SECRET=staging-secret-4410
# The keys of the objects below a prefix of the bucket; a cleanup leaves no attempts.
keys() {
    curl -sf --aws-sigv4 aws:amz:us-east-1:s3 --user "$STAGING_KEY:$STAGING_SECRET" \
        "$endpoint/staging?list-type=2&prefix=$1"
}
export XDG_CONFIG_HOME="$work/config" IUNCTUS_SITE_CONFIG=""
status=0
for mode in ${MODES:-self image-volume init-container kubedal credentials shared-claim}; do
    echo "=== $mode"
    export XDG_CACHE_HOME="$work/cache"
    staging="staging = \"object-store\"
staging_root = \"s3://staging/objects\"
helper_image = \"$helper\"
helper_mount = \"$mode\""
    if [ "$mode" = self ]; then
        staging='staging = "object-store"
staging_root = "s3://staging/self"'
    elif [ "$mode" = kubedal ]; then
        staging='staging = "kubedal"
staging_root = "s3://staging/kubedal"
kubedal_data_node = "staging"'
    elif [ "$mode" = credentials ]; then
        staging='staging = "object-store"
staging_root = "s3://staging/credentials"
staging_access = "credentials"
staging_secret = "staging-storage"
staging_cleanup = true
helper_image = "'"$helper"'"'
    elif [ "$mode" = shared-claim ]; then
        # The cache of iunctus is on the claim, which the pods mount at the same path.
        export XDG_CACHE_HOME="$shared/cache"
        staging="[executor.kubernetes.shared_volume]
claim = \"iunctus-cache\"
path = \"$shared\""
    fi
    cat > "$work/$mode.toml" <<EOF
[storage.staging]
type = "s3"
prefixes = ["s3://staging/"]
endpoint = "$endpoint"
region = "us-east-1"
access_key_id = { environment = "STAGING_KEY" }
secret_access_key = { environment = "STAGING_SECRET" }

[executor.kubernetes]
namespace = "iunctus"
$staging
EOF
    rm -rf "$XDG_CACHE_HOME" "$work/run"
    mkdir "$work/run"
    cp "$repository/demo/snakemake/hello.smk" "$work/run/"
    if (cd "$work/run" && "$binary" run hello.smk -executor kubernetes -kubeconfig "$work/kubeconfig" \
        -iunctus-config "$work/$mode.toml") \
        && [ "$(cat "$work/run/hello/all.txt")" = "$(printf '%s world!\n' Hello Hola Bonjour Ciao)" ] \
        && { [ "$mode" != credentials ] || { keys credentials/cas/ | grep -q '<Key>' \
            && ! keys credentials/attempts/ | grep -q '<Key>'; }; } \
        && { [ "$mode" != self ] || keys self/cas/ | grep -q "$(sha256sum "$binary" | cut -c 3-64)"; }; then
        echo "=== $mode: passed"
    else
        echo "=== $mode: FAILED"
        kubectl -n iunctus get pods,events 2>&1 | tail -20
        status=1
    fi
done
exit "$status"
