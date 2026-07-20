#!/bin/sh
set -eu

tag=${1:?release tag is required}
checksums=${2:?checksum file is required}
output=${3:?output directory is required}
version=${tag#v}

case "$tag" in
  v[0-9]*) ;;
  *) echo "spectra: release tag must be v-prefixed" >&2; exit 1 ;;
esac

checksum() {
  value=$(awk -v file="$1" '$2 == file { print $1 }' "$checksums")
  if [ -z "$value" ]; then
    echo "spectra: checksum is missing $1" >&2
    exit 1
  fi
  printf '%s' "$value"
}

render() {
  template=$1
  destination=$2
  sed \
    -e "s|@TAG@|$tag|g" \
    -e "s|@VERSION@|$version|g" \
    -e "s|@MACOS_ARM64_SHA@|$(checksum spectra-aarch64-apple-darwin.tar.gz)|g" \
    -e "s|@MACOS_X64_SHA@|$(checksum spectra-x86_64-apple-darwin.tar.gz)|g" \
    -e "s|@LINUX_ARM64_SHA@|$(checksum spectra-aarch64-unknown-linux-gnu.tar.gz)|g" \
    -e "s|@LINUX_X64_SHA@|$(checksum spectra-x86_64-unknown-linux-gnu.tar.gz)|g" \
    -e "s|@WINDOWS_X64_SHA@|$(checksum spectra-x86_64-pc-windows-msvc.zip)|g" \
    "$template" > "$destination"
}

mkdir -p "$output"
render packaging/homebrew/spectra.rb.template "$output/spectra.rb"
render packaging/scoop/spectra.json.template "$output/spectra.json"
