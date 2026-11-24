#!/usr/bin/env bash
# Runs the backend lib tests on the hive laptop (Rust never builds on the dev machine).
# Usage: scripts/hive-test.sh [--features F] [--restore] [-- <cargo test args>]
set -u

HOST=pragmatic-hive
CLONE_WIN='C:\llm-harness\local-llm-harness'
CLONE_FWD=C:/llm-harness/local-llm-harness
BUILD_DIR='C:\llm-harness\harness-build'
FEATURES=""
RESTORE=0
CARGO_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --features) FEATURES="$2"; shift 2 ;;
        --restore) RESTORE=1; shift ;;
        --) shift; CARGO_ARGS=("$@"); break ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ "$RESTORE" = 1 ]; then
    printf '@echo off\r\ncd /d %s\r\ngit checkout -- backend\r\ngit clean -fd backend/src backend/tests backend/vendor\r\ngit status --short backend\r\necho RESTORED\r\n' "$CLONE_WIN" > "$TMP/restore.cmd"
    scp -q "$TMP/restore.cmd" "$HOST:$CLONE_FWD/../harness-build/restore.cmd" || exit 1
    ssh "$HOST" "cmd /c ${BUILD_DIR}\restore.cmd" 2>&1 | grep -v '^\*\*'
    exit "${PIPESTATUS[0]}"
fi

# Ship the working tree: tar locally, scp, extract on the hive.
# Cargo.lock and the vendored crates are needed for the offline build. The hive clone sits at
# origin/main, so ship only the vendor files that differ from it (changed/added + untracked).
# Stale crate dirs left on the hive are harmless; --restore cleans what was shipped.
# Refresh origin/main first so the diff matches the hive clone; offline, the local copy is used.
git -C "$REPO_ROOT" fetch -q origin main 2>/dev/null || echo "note: origin/main not fetched; using the local copy" >&2
(cd "$REPO_ROOT" && {
    git diff --name-only --diff-filter=d origin/main -- backend/vendor
    git ls-files -o --exclude-standard backend/vendor
} > "$TMP/vendor-files.txt" &&
    tar -cf "$TMP/src.tar" backend/src backend/tests backend/Cargo.toml backend/Cargo.lock -T "$TMP/vendor-files.txt") || exit 1
scp -q "$TMP/src.tar" "$HOST:$CLONE_FWD/../harness-build/src.tar" || exit 1

FEATURE_ARGS=""
# Re-quote each cargo arg so spaces/specials survive the generated .cmd.
CARGO_QUOTED=""
for a in "${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}"; do
    case "$a" in
        *[!A-Za-z0-9_:./=-]*) CARGO_QUOTED="$CARGO_QUOTED \"${a//\"/\"\"}\"" ;;
        *) CARGO_QUOTED="$CARGO_QUOTED $a" ;;
    esac
done
CUDA_LINES=""
case "$FEATURES" in
    "") ;;
    *) FEATURE_ARGS="--features $FEATURES" ;;
esac
case "$FEATURES" in
    *llamacpp*|*candle-cuda*)
        CUDA_LINES='set "PATH=%PATH%;C:\Program Files\CMake\bin;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5\bin"
set "CUDA_PATH=C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5"
set "CUDA_COMPUTE_CAP=86"
set "CMAKE_CUDA_ARCHITECTURES=86"
set "CUDARC_CUDA_VERSION=13040"
set "NVCC_PREPEND_FLAGS=-Xcompiler /Zc:preprocessor"' ;;
esac

{
    echo '@echo off'
    echo 'call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul'
    [ -n "$CUDA_LINES" ] && echo "$CUDA_LINES"
    # Separate target dir: the live supervisor's target/release must stay untouched.
    echo 'set "CARGO_TARGET_DIR=C:\llm-harness\local-llm-harness\backend\target\hive-test"'
    echo "cd /d $CLONE_WIN"
    # NOTE: tar only adds/overwrites; files deleted locally stay in the hive clone until --restore.
    echo "tar -xf $BUILD_DIR\src.tar"
    echo "cd backend"
    echo "cargo test --release --offline --lib $FEATURE_ARGS ${CARGO_QUOTED}"
    echo 'echo TEST_EXIT=%ERRORLEVEL%'
} | sed 's/$/\r/' > "$TMP/run-tests.cmd"

scp -q "$TMP/run-tests.cmd" "$HOST:$CLONE_FWD/../harness-build/run-tests.cmd" || exit 1
ssh "$HOST" "cmd /c ${BUILD_DIR}\run-tests.cmd" 2>&1 | grep -v '^\*\*' | tee "$TMP/out.txt"
grep -q 'TEST_EXIT=0' "$TMP/out.txt"
