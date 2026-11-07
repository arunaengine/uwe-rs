#!/usr/bin/env bash
# Runs workflows with the Kubernetes executor on a kind cluster, with object-store staging (with the
# uploaded binary, a helper image, or credentials), with kubedal staging, and with shared paths.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
#
# Needs docker, kind, kubectl and helm, the static binary of `just release-binary` (UWE),
# a checkout of kubedal (KUBEDAL, default ../kubedal) and its image (KUBEDAL_IMAGE, default
# kubedal:e2e). MODES picks some of self, image-volume, init-container, kubedal, credentials,
# shared-claim and shared-path; without a kubedal checkout the default leaves out kubedal;
# WORKFLOWS picks some of hello (the default), cwl and nf-core-demo; KIND_CLUSTER names the
# cluster (default uwe-staging); KEEP_CLUSTER=1 keeps the cluster and the S3 server after the run.
set -euo pipefail

repository="$(cd "$(dirname "$0")/.." && pwd)"
binary="$(realpath "${UWE:-$repository/target/x86_64-unknown-linux-musl/release/uwe}")"
kubedal="${KUBEDAL:-$repository/../kubedal}"
kubedal_image="${KUBEDAL_IMAGE:-kubedal:e2e}"
cluster="${KIND_CLUSTER:-uwe-staging}"
modes="self image-volume init-container kubedal credentials shared-claim"
[ -d "$kubedal" ] || modes="${modes/kubedal /}"
modes="${MODES:-$modes}"
workflows="${WORKFLOWS:-hello}"
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
cp "$binary" "$work/helper/uwe"
printf 'FROM scratch\nCOPY uwe /uwe\n' > "$work/helper/Dockerfile"
# Another run may build its own helper image at the same time.
helper="uwe-helper:$cluster"
docker build -q -t "$helper" "$work/helper" > /dev/null
kind load docker-image --name "$cluster" "$helper" debian:stable-slim
case " $modes " in *" kubedal "*)
    kind load docker-image --name "$cluster" "$kubedal_image"
    helm upgrade --install kubedal "$kubedal/charts/kubedal" --namespace kube-system \
        --set image.repository="${kubedal_image%%:*}" --set image.tag="${kubedal_image##*:}" \
        --set image.pullPolicy=Never --wait --timeout 180s > /dev/null
esac
kubectl create namespace uwe > /dev/null
kubectl apply -f - > /dev/null <<EOF
apiVersion: v1
kind: Secret
metadata: {name: staging-credentials, namespace: uwe}
stringData: {access_key_id: staging-user, secret_access_key: staging-secret-4410}
---
apiVersion: v1
kind: Secret
metadata: {name: staging-storage, namespace: uwe}
stringData:
  UWE_STAGING_STORAGE: |
    [storage.staging]
    type = "s3"
    prefixes = ["s3://staging/"]
    endpoint = "$endpoint"
    region = "us-east-1"
    access_key_id = "staging-user"
    secret_access_key = "staging-secret-4410"
---
apiVersion: v1
kind: PersistentVolume
metadata: {name: uwe-cache}
spec:
  capacity: {storage: 1Gi}
  accessModes: [ReadWriteMany]
  storageClassName: ""
  hostPath: {path: "$shared"}
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: uwe-cache, namespace: uwe}
spec:
  accessModes: [ReadWriteMany]
  storageClassName: ""
  volumeName: uwe-cache
  resources: {requests: {storage: 1Gi}}
EOF

export STAGING_KEY=staging-user STAGING_SECRET=staging-secret-4410
# The keys of the objects below a prefix of the bucket; a cleanup leaves no attempts.
keys() {
    curl -sf --aws-sigv4 aws:amz:us-east-1:s3 --user "$STAGING_KEY:$STAGING_SECRET" \
        "$endpoint/staging?list-type=2&prefix=$1"
}
# Runs the workflow $1 in the current folder with the other arguments of uwe, and checks results.
workflow() {
    local name="$1"
    shift
    case "$name" in
        hello)
            cp "$repository/demo/snakemake/hello.smk" . && "$binary" run hello.smk "$@" \
                && [ "$(cat hello/all.txt)" = "$(printf '%s world!\n' Hello Hola Bonjour Ciao)" ] ;;
        cwl)
            cp "$repository/demo/cwl/"{scatter-workflow.cwl,hello_world.cwl,scatter-job.yml} . \
                && "$binary" run scatter-workflow.cwl scatter-job.yml \
                    -default-image debian:stable-slim "$@" ;;
        nf-core-demo)
            "$binary" run nf-core/demo -r 32893afef8076a03a2767a020b3f0cab2e0b40b2 \
                -profile test,docker --outdir results "$@" \
                && [ -s results/multiqc/multiqc_report.html ] ;;
        *) echo "unknown workflow $name" >&2; return 1 ;;
    esac
}
export XDG_CONFIG_HOME="$work/config" UWE_SITE_CONFIG=""
status=0
case " $modes " in *" kubedal "*)
    kubectl apply -f - > /dev/null <<EOF
apiVersion: kubedal.arunaengine.org/v1alpha1
kind: DataNode
metadata: {name: staging, namespace: uwe}
spec:
  backend: S3
  read_only: false
  limit: 1Gi
  secretRef: {name: staging-credentials}
  config: {bucket: staging, endpoint: "$endpoint", region: us-east-1, root: /kubedal}
EOF
esac
for mode in $modes; do
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
        # The cache of uwe is on the claim, which the pods mount at the same path.
        export XDG_CACHE_HOME="$shared/cache"
        staging="[executor.kubernetes.shared_volume]
claim = \"uwe-cache\"
path = \"$shared\""
    elif [ "$mode" = shared-path ]; then
        # The cache of uwe is in the folder that the node mounts at the same path.
        export XDG_CACHE_HOME="$shared/cache"
        staging='staging = "shared-path"'
    fi
    cat > "$work/$mode.toml" <<EOF
[storage.staging]
type = "s3"
prefixes = ["s3://staging/"]
endpoint = "$endpoint"
region = "us-east-1"
access_key_id = { environment = "STAGING_KEY" }
secret_access_key = { environment = "STAGING_SECRET" }

# CWL tools have no network unless they ask, and Kubernetes cannot cut a pod off the network.
[network]
allowed_steps = ["*"]

[executor.kubernetes]
namespace = "uwe"
$staging
EOF
    for name in $workflows; do
        rm -rf "$XDG_CACHE_HOME" "$work/run"
        mkdir "$work/run"
        started=$SECONDS
        if (cd "$work/run" && workflow "$name" -executor kubernetes -kubeconfig "$work/kubeconfig" \
            -uwe-config "$work/$mode.toml") \
            && { [ "$mode" != credentials ] || { keys credentials/cas/ | grep -q '<Key>' \
                && ! keys credentials/attempts/ | grep -q '<Key>'; }; } \
            && { [ "$mode" != self ] || keys self/cas/ | grep -q "$(sha256sum "$binary" | cut -c 3-64)"; }; then
            echo "=== $mode $name: passed after $((SECONDS - started)) s"
        else
            echo "=== $mode $name: FAILED"
            kubectl -n uwe get pods,events 2>&1 | tail -20
            status=1
        fi
    done
done
exit "$status"
