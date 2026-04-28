#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
DATA_ROOT="${THREAD_POOL_BENCH_DATA_DIR:-$SCRIPT_DIR}"

RAW_PBBS_DIR="$DATA_ROOT/raw/pbbs"
RAW_PBBS_SRC_DIR="$DATA_ROOT/raw/pbbsbench-src"
CPU_PBBS_DIR="$DATA_ROOT/cpu/pbbs"
CPU_RAYON_DIR="$DATA_ROOT/cpu/rayon"
IO_SMALL_DIR="$DATA_ROOT/io/small-files"
IO_MEDIUM_DIR="$DATA_ROOT/io/medium-files"
GRAPH_PBBS_DIR="$DATA_ROOT/graph/pbbs"

SMALL_FILE_TARGET=4096
MEDIUM_CHUNK_BYTES=262144
MEDIUM_SOURCE_BYTES=16777216

PBBS_BASE_URL="https://raw.githubusercontent.com/cmuparlay/pbbsbench/master/testData/data"
RAYON_TSP_BASE_URL="https://raw.githubusercontent.com/rayon-rs/rayon/main/rayon-demo/data/tsp"
PBBS_GIT_REPO="https://github.com/cmuparlay/pbbsbench"
PBBS_GENERATOR_CCFLAGS="-O2 -g -std=c++17 -DNDEBUG -I ."
PBBS_GRAPH_TARGETS=(
  "randLocalGraph_J_10_50000"
  "rMatGraph_J_12_50000"
)

# Returns success when file checksum matches the expected SHA-256 hash.
verify_sha256() {
  local file="$1"
  local expected="$2"
  local actual
  actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]]
}

# Downloads one file unless it already exists locally.
download_file() {
  local url="$1"
  local dest="$2"
  local expected_sha="$3"
  if [[ -f "$dest" ]]; then
    if verify_sha256 "$dest" "$expected_sha"; then
      echo "[skip] $dest"
      return
    fi
    echo "[warn] checksum mismatch, redownloading: $dest"
    rm -f "$dest"
  fi
  echo "[download] $url -> $dest"
  curl --fail --location --retry 3 --retry-delay 1 --output "$dest" "$url"
  if ! verify_sha256 "$dest" "$expected_sha"; then
    echo "[error] checksum verification failed: $dest" >&2
    exit 1
  fi
}

# Decompresses a .bz2 archive into plain text if the destination is missing.
decompress_bz2() {
  local src="$1"
  local dest="$2"
  if [[ -f "$dest" ]]; then
    echo "[skip] $dest"
    return
  fi
  echo "[extract] $src -> $dest"
  bunzip2 --keep --stdout "$src" > "$dest"
}

# Counts regular files in one directory.
count_regular_files() {
  local dir="$1"
  if [[ ! -d "$dir" ]]; then
    echo "0"
    return
  fi
  find "$dir" -type f | wc -l | tr -d ' '
}

# Ensures PBBS graph generator source tree exists with required submodules.
prepare_pbbs_source_tree() {
  mkdir -p "$(dirname "$RAW_PBBS_SRC_DIR")"
  if [[ -d "$RAW_PBBS_SRC_DIR" && ! -d "$RAW_PBBS_SRC_DIR/.git" ]]; then
    echo "[warn] removing incomplete pbbs source cache: $RAW_PBBS_SRC_DIR"
    rm -rf "$RAW_PBBS_SRC_DIR"
  fi
  if [[ ! -d "$RAW_PBBS_SRC_DIR/.git" ]]; then
    echo "[download] cloning pbbsbench source with submodules"
    git clone --depth 1 --recurse-submodules "$PBBS_GIT_REPO" "$RAW_PBBS_SRC_DIR"
  fi
  if [[ ! -f "$RAW_PBBS_SRC_DIR/parlay/parallel.h" ]]; then
    echo "[setup] syncing pbbsbench submodules"
    git -C "$RAW_PBBS_SRC_DIR" submodule update --init --recursive
  fi
}

# Builds PBBS graph files used by graph-traversal benchmarks.
prepare_graph_files() {
  mkdir -p "$GRAPH_PBBS_DIR"
  prepare_pbbs_source_tree
  local pbbs_data_dir="$RAW_PBBS_SRC_DIR/testData/graphData/data"
  for target in "${PBBS_GRAPH_TARGETS[@]}"; do
    if [[ ! -f "$GRAPH_PBBS_DIR/$target" ]]; then
      echo "[build] generating PBBS graph: $target"
      make -s -C "$pbbs_data_dir" CCFLAGS="$PBBS_GENERATOR_CCFLAGS" "$target"
      cp "$pbbs_data_dir/$target" "$GRAPH_PBBS_DIR/$target"
    else
      echo "[skip] $GRAPH_PBBS_DIR/$target"
    fi
  done
}

# Builds the small-file IO dataset by splitting CSV records into one-file-per-line.
prepare_small_files() {
  local existing
  existing="$(count_regular_files "$IO_SMALL_DIR")"
  if [[ "$existing" -ge "$SMALL_FILE_TARGET" ]]; then
    echo "[skip] $IO_SMALL_DIR already has $existing files"
    return
  fi
  echo "[build] generating $SMALL_FILE_TARGET files under $IO_SMALL_DIR"
  mkdir -p "$IO_SMALL_DIR"
  find "$IO_SMALL_DIR" -type f -name 'record_*' -delete
  head -n "$SMALL_FILE_TARGET" "$CPU_PBBS_DIR/covtype.data.train" \
    | split -l 1 -a 5 -d - "$IO_SMALL_DIR/record_"
}

# Builds medium-size chunk files for sequential and random read benchmarks.
prepare_medium_files() {
  local existing
  existing="$(count_regular_files "$IO_MEDIUM_DIR")"
  if [[ "$existing" -gt 0 ]]; then
    echo "[skip] $IO_MEDIUM_DIR already has $existing files"
    return
  fi
  echo "[build] generating medium chunk files under $IO_MEDIUM_DIR"
  mkdir -p "$IO_MEDIUM_DIR"
  find "$IO_MEDIUM_DIR" -type f -name 'chunk_*' -delete
  head -c "$MEDIUM_SOURCE_BYTES" "$CPU_PBBS_DIR/wikisamp.xml" \
    | split -b "$MEDIUM_CHUNK_BYTES" -a 4 -d - "$IO_MEDIUM_DIR/chunk_"
}

# Entrypoint that downloads, extracts, and materializes all dataset dimensions.
main() {
  mkdir -p \
    "$RAW_PBBS_DIR" \
    "$CPU_PBBS_DIR" \
    "$CPU_RAYON_DIR" \
    "$IO_SMALL_DIR" \
    "$IO_MEDIUM_DIR" \
    "$GRAPH_PBBS_DIR"

  download_file \
    "$PBBS_BASE_URL/covtype.data.train.bz2" \
    "$RAW_PBBS_DIR/covtype.data.train.bz2" \
    "a781446278db3a195f119a7e745b947ad111c4dee04dba5f583e2e50709e2556"
  download_file \
    "$PBBS_BASE_URL/wikisamp.xml.bz2" \
    "$RAW_PBBS_DIR/wikisamp.xml.bz2" \
    "05720f3edc57deb78b657ac24273fcd310b7b262c50e01db78e3903e015a7366"

  download_file \
    "$RAYON_TSP_BASE_URL/dj10.tsp" \
    "$CPU_RAYON_DIR/dj10.tsp" \
    "b7798239baaa2c7d15b037f7c17fd444d6440df0b0d17c9b48b2a66885d90490"
  download_file \
    "$RAYON_TSP_BASE_URL/dj15.tsp" \
    "$CPU_RAYON_DIR/dj15.tsp" \
    "f1ebe217ae1cb8479e32fd34bb221938545c68bd539d43c6f6e081f9e989c907"
  download_file \
    "$RAYON_TSP_BASE_URL/dj38.tsp" \
    "$CPU_RAYON_DIR/dj38.tsp" \
    "abdd86fd061acef713a571750a8f290cef804fe6b61eb768f8623be7ad8038bd"

  decompress_bz2 "$RAW_PBBS_DIR/covtype.data.train.bz2" "$CPU_PBBS_DIR/covtype.data.train"
  decompress_bz2 "$RAW_PBBS_DIR/wikisamp.xml.bz2" "$CPU_PBBS_DIR/wikisamp.xml"

  prepare_small_files
  prepare_medium_files
  prepare_graph_files

  echo "[done] datasets ready under: $DATA_ROOT"
}

main "$@"
