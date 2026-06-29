#!/usr/bin/env bash
# render the Homebrew cask for the current release and push it to the tap.
# expects the release DMG to be already built and HOMEBREW_TAP_TOKEN to be set.
#
# prerequisites:
#   - HOMEBREW_TAP_TOKEN: a PAT with write access to the tap repo below.
#   - the tap repo (aleksey925/homebrew-apps) must already exist.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
template="${script_dir}/homebrew-cask.rb.tmpl"

if [ -z "${GITHUB_REF_NAME:-}" ]; then
  echo "error: GITHUB_REF_NAME is not set (expected the release tag, e.g. v1.2.3)" >&2
  exit 1
fi

if [ -z "${HOMEBREW_TAP_TOKEN:-}" ]; then
  echo "error: HOMEBREW_TAP_TOKEN is not set (PAT with write access to the tap repo)" >&2
  exit 1
fi

# escape a string for safe use as a sed replacement (handles \, /, &).
escape_sed_replacement() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\//\\/}"
  s="${s//&/\\&}"
  printf '%s' "$s"
}

version="${GITHUB_REF_NAME#v}"
dmg="src-tauri/target/release/bundle/dmg/Meerkat_${version}_aarch64.dmg"
sha256="$(shasum -a 256 "$dmg" | awk '{print $1}')"

tap_dir="$(mktemp -d)"
trap 'rm -rf "$tap_dir"' EXIT

# pass the token via a per-command header instead of embedding it in the clone
# URL, so it is never written to $tap_dir/.git/config on disk.
auth_header="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$HOMEBREW_TAP_TOKEN" | base64 | tr -d '\n')"

tap_git() {
  git -C "$tap_dir" -c http.extraheader="$auth_header" "$@"
}

git -c http.extraheader="$auth_header" \
  clone "https://github.com/aleksey925/homebrew-apps.git" "$tap_dir"

mkdir -p "$tap_dir/Casks"
version_repl="$(escape_sed_replacement "$version")"
sha256_repl="$(escape_sed_replacement "$sha256")"
branch="$(git -C "$tap_dir" rev-parse --abbrev-ref HEAD)"

# the shared tap can move between our clone and push (another app or a
# different-tag run publishing). re-render from the latest remote state on each
# retry rather than merging: the cask is regenerated wholesale, so reset +
# re-render sidesteps conflicts on the version/sha lines.
for attempt in 1 2 3 4 5; do
  sed -e "s/__VERSION__/${version_repl}/g" -e "s/__SHA256__/${sha256_repl}/g" \
    "$template" > "$tap_dir/Casks/meerkat.rb"

  ruby -c "$tap_dir/Casks/meerkat.rb" >/dev/null

  git -C "$tap_dir" add Casks/meerkat.rb
  if git -C "$tap_dir" diff --cached --quiet; then
    echo "cask already up to date, nothing to push"
    exit 0
  fi

  git -C "$tap_dir" \
    -c user.name="Aleksey Petrunnik" \
    -c user.email="petrunnik.a@gmail.com" \
    commit -m "Brew cask update for meerkat version ${version}"

  if tap_git push; then
    exit 0
  fi

  echo "push rejected, syncing with origin and retrying (attempt ${attempt})" >&2
  tap_git fetch origin
  git -C "$tap_dir" reset --hard "origin/${branch}"
done

echo "error: failed to push cask update after multiple attempts" >&2
exit 1
