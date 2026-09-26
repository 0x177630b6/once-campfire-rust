# Sourced by parity/bin/capture and parity/bin/compare. Runs parity/capture/cli.ts inside the
# pinned Playwright image (parity/Dockerfile.playwright), so captures never depend on the host.
#
# PARITY_CAPTURE_RUNTIME:
#   docker   docker build + docker run (the canonical runtime)
#   sandbox  the same pinned image bytes, materialized by rootfs.sh and run under bubblewrap
#   host     the host's Node and Playwright browsers; for developing the harness only, NOT canonical
# Default: docker when the daemon is reachable, else sandbox.
#
# The repo is mounted at its host path, so absolute paths mean the same thing on both sides, and
# the servers under test are reached over the host network.

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
PARITY=$ROOT/parity
SANDBOX=$PARITY/capture/sandbox

die() { echo "parity: $*" >&2; exit 1; }

capture_runtime() {
  if [ -n "${PARITY_CAPTURE_RUNTIME:-}" ]; then echo "$PARITY_CAPTURE_RUNTIME"
  elif docker info >/dev/null 2>&1; then echo docker
  elif command -v bwrap >/dev/null; then echo sandbox
  else die "neither Docker nor bubblewrap is available; PARITY_CAPTURE_RUNTIME=host runs non-canonically"
  fi
}

ensure_host_modules() {
  [ -d "$PARITY/node_modules/playwright" ] || (cd "$PARITY" && npm ci --no-audit --no-fund >&2)
}

docker_image() {
  local hash; hash=$(cat "$PARITY/Dockerfile.playwright" "$PARITY/package-lock.json" | sha256sum | cut -c1-12)
  local image=campfire-parity-playwright:$hash
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "parity: building $image" >&2
    docker build -q -f "$PARITY/Dockerfile.playwright" -t "$image" "$PARITY" >&2
  fi
  echo "$image"
}

# run_in_image ARGS... runs `node capture/cli.ts ARGS...` in parity/.
run_in_image() {
  local runtime; runtime=$(capture_runtime)
  case "$runtime" in
    docker)
      local image; image=$(docker_image)
      # Named, so parity_cleanup can stop it if the run is interrupted.
      local name=parity-capture-$$-$RANDOM
      CAPTURE_CONTAINERS+=("$name")
      docker run --rm --init --name "$name" --network host --ipc host \
        -u "$(id -u):$(id -g)" -e HOME=/tmp -e TZ=UTC -e CI="${CI:-}" -e PARITY_WORKERS="${PARITY_WORKERS:-}" \
        -v "$ROOT:$ROOT" --tmpfs "$PARITY/node_modules" -w "$PARITY" \
        "$image" node capture/cli.ts "$@" &
      wait $! # in the background so an interrupt runs the caller's trap (parity_cleanup) at once
      ;;
    sandbox)
      local paths rootfs modules
      paths=$("$SANDBOX/rootfs.sh") || die "could not materialize the Playwright image"
      read -r rootfs modules <<<"$paths"
      # The image is read-only; a tmpfs over the repo's top-level directory (e.g. /home) lets
      # bubblewrap create the mount point for the repo at its host path.
      local top; top="/$(echo "$ROOT" | cut -d/ -f2)"
      bwrap --ro-bind "$rootfs" / --tmpfs "$top" --bind "$ROOT" "$ROOT" --ro-bind "$modules" /node_modules \
        --tmpfs "$PARITY/node_modules" --dev /dev --proc /proc --tmpfs /tmp --tmpfs /dev/shm \
        --ro-bind /etc/resolv.conf /etc/resolv.conf --share-net --unshare-user --unshare-pid --unshare-ipc \
        --die-with-parent --clearenv \
        --setenv PATH /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        --setenv HOME /tmp --setenv TZ UTC --setenv LANG C.UTF-8 \
        --setenv PLAYWRIGHT_BROWSERS_PATH /ms-playwright --setenv CI "${CI:-}" --setenv PARITY_WORKERS "${PARITY_WORKERS:-}" \
        --chdir "$PARITY" node capture/cli.ts "$@"
      ;;
    host)
      echo "parity: WARNING running on the host; captures are not canonical" >&2
      ensure_host_modules
      (cd "$PARITY" && node capture/cli.ts "$@")
      ;;
    *) die "unknown PARITY_CAPTURE_RUNTIME=$runtime" ;;
  esac
}

# Resetting servers between captures of `mutates: true` states has to happen on the host (that's
# where parity/bin/reference runs), so the capture process asks for it through files in a control
# directory, and a host loop runs RESET_CMD with {port} {target} {url} substituted.
start_reset_loop() {
  local reset_cmd=$1
  RESET_CTRL=$(mktemp -d "$PARITY/out/.control.XXXXXX")
  (
    while [ -d "$RESET_CTRL" ]; do
      for req in "$RESET_CTRL"/req.*; do
        [ -e "$req" ] || continue
        read -r port target url <"$req"
        local cmd=${reset_cmd//\{port\}/$port}; cmd=${cmd//\{target\}/$target}; cmd=${cmd//\{url\}/$url}
        if sh -c "$cmd" >&2; then status=ok; else status=failed; fi
        echo "$status" >"$RESET_CTRL/done.${req##*/req.}"
        rm -f "$req"
      done
      sleep 0.2
    done
  ) &
  RESET_LOOP_PID=$!
  RESET_ARG="$SANDBOX/reset-request.sh $RESET_CTRL {port} {target} {url}"
}

CAPTURE_CONTAINERS=()

# EXIT/INT/TERM trap for the bin scripts: no browsers or reset loops outlive them.
parity_cleanup() {
  stop_reset_loop
  local name
  for name in "${CAPTURE_CONTAINERS[@]+"${CAPTURE_CONTAINERS[@]}"}"; do docker kill "$name" >/dev/null 2>&1 || true; done
}

stop_reset_loop() {
  [ -n "${RESET_CTRL:-}" ] && rm -rf "$RESET_CTRL"
  # The loop exits once its directory is gone; waiting (not killing) lets a reset in progress
  # finish, so it can't bring a server back up after the caller has taken it down.
  [ -n "${RESET_LOOP_PID:-}" ] && wait "$RESET_LOOP_PID" 2>/dev/null
  RESET_CTRL="" RESET_LOOP_PID=""
}
