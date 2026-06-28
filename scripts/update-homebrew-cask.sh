#!/usr/bin/env bash
# render the Homebrew cask for the current release and push it to the tap.
# expects the release DMG to be already built and HOMEBREW_TAP_TOKEN to be set.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
template="${script_dir}/homebrew-cask.rb.tmpl"

version="${GITHUB_REF_NAME#v}"
dmg="src-tauri/target/release/bundle/dmg/Meerkat_${version}_aarch64.dmg"
sha256="$(shasum -a 256 "$dmg" | awk '{print $1}')"

tap_dir="$(mktemp -d)"
git clone "https://x-access-token:${HOMEBREW_TAP_TOKEN}@github.com/aleksey925/homebrew-apps.git" "$tap_dir"

mkdir -p "$tap_dir/Casks"
sed -e "s/__VERSION__/${version}/g" -e "s/__SHA256__/${sha256}/g" \
  "$template" > "$tap_dir/Casks/meerkat.rb"

git -C "$tap_dir" add Casks/meerkat.rb
if git -C "$tap_dir" diff --cached --quiet; then
  echo "cask already up to date, nothing to push"
  exit 0
fi

git -C "$tap_dir" \
  -c user.name="Aleksey Petrunnik" \
  -c user.email="petrunnik.a@gmail.com" \
  commit -m "Brew cask update for meerkat version ${GITHUB_REF_NAME}"
git -C "$tap_dir" push
