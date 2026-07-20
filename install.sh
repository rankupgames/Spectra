#!/bin/sh
set -eu

repository=https://github.com/rankupgames/Spectra
version=${SPECTRA_VERSION:-latest}
install_dir=${SPECTRA_INSTALL_DIR:-"$HOME/.local/bin"}

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) echo "spectra: unsupported operating system" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) echo "spectra: unsupported CPU architecture" >&2; exit 1 ;;
esac

if [ "$version" = latest ]; then
  release_url="$repository/releases/latest/download"
else
  case "$version" in v*) tag=$version ;; *) tag=v$version ;; esac
  release_url="$repository/releases/download/$tag"
fi

archive="spectra-$arch-$os.tar.gz"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/spectra-install.XXXXXX")
trap 'rm -rf "$temporary"' EXIT INT TERM

curl --fail --location --silent --show-error "$release_url/$archive" -o "$temporary/$archive"
curl --fail --location --silent --show-error "$release_url/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected=$(awk -v file="$archive" '$2 == file { print $1 }' "$temporary/SHA256SUMS")
if [ -z "$expected" ]; then
  echo "spectra: release checksum is missing $archive" >&2
  exit 1
fi
actual=$(shasum -a 256 "$temporary/$archive" | awk '{ print $1 }')
if [ "$actual" != "$expected" ]; then
  echo "spectra: checksum verification failed" >&2
  exit 1
fi

tar -xzf "$temporary/$archive" -C "$temporary"
mkdir -p "$install_dir"
install -m 0755 "$temporary/spectra" "$install_dir/spectra"
echo "Installed Spectra to $install_dir/spectra"
case ":${PATH}:" in
  *":$install_dir:"*) ;;
  *) echo "Add $install_dir to PATH, then run: spectra install" ;;
esac
