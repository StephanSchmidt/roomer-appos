#!/bin/sh
# Install the Roomer shell: curl -fsSL https://raw.githubusercontent.com/StephanSchmidt/roomer-appos/main/install.sh | sh
#
# Downloads the prebuilt binary for this OS and CPU from the latest GitHub
# release, checks its SHA-256 and puts it in ~/.local/bin.
#
#   ROOMER_VERSION=v0.1.0    install this release instead of the latest
#   ROOMER_INSTALL_DIR=dir  install here instead of ~/.local/bin
#
# Source: https://github.com/StephanSchmidt/roomer-appos/blob/main/install.sh

# Everything is in main(), which runs on the last line, so a download cut
# short never runs half a script.
main() {
  set -eu
  repo="StephanSchmidt/roomer-appos"
  version="${ROOMER_VERSION:-latest}"
  dir="${ROOMER_INSTALL_DIR:-$HOME/.local/bin}"

  case "$(uname -s)" in
    Linux) os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *) die "no prebuilt binary for $(uname -s); build from source: git clone https://github.com/$repo && make platform" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) die "no prebuilt binary for $(uname -m); build from source: git clone https://github.com/$repo && make platform" ;;
  esac
  # A shell running under Rosetta reports x86_64 on an Apple Silicon Mac.
  if [ "$os" = "apple-darwin" ] && [ "$arch" = "x86_64" ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
    arch="aarch64"
  fi
  target="$arch-$os"

  if [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi
  file="roomer-$target.tar.gz"

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  say "downloading $file ($version)"
  fetch "$base/$file" "$tmp/$file"
  fetch "$base/$file.sha256" "$tmp/$file.sha256"

  want="$(cut -d ' ' -f 1 <"$tmp/$file.sha256")"
  got="$(sha256 "$tmp/$file")"
  [ "$want" = "$got" ] || die "checksum mismatch for $file: expected $want, got $got"

  tar -xzf "$tmp/$file" -C "$tmp"
  mkdir -p "$dir"
  # Move into place, so a running roomer is replaced, not overwritten.
  cp "$tmp/roomer" "$dir/roomer.new"
  chmod 755 "$dir/roomer.new"
  mv -f "$dir/roomer.new" "$dir/roomer"

  say "installed $("$dir/roomer" --version 2>/dev/null || echo roomer) to $dir/roomer"
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "$dir is not on your PATH; add it, for example: export PATH=\"$dir:\$PATH\"" ;;
  esac
  say "next: roomer, then open http://127.0.0.1:8080"
}

say() { printf 'roomer: %s\n' "$1"; }
die() { printf 'roomer: error: %s\n' "$1" >&2; exit 1; }

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1" || die "download failed: $1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --https-only -O "$2" "$1" || die "download failed: $1"
  else
    die "need curl or wget"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    die "need sha256sum or shasum to check the download"
  fi
}

main "$@"
