#!/usr/bin/env bash
# Write the update manifest the app's self-update reads (docs/self-update.md).
#
# Usage:
#   ./scripts/update-manifest.sh VERSION DOWNLOAD_BASE_URL ASSET... > update-manifest.json
#
# DOWNLOAD_BASE_URL is the release's download folder, e.g.
# https://github.com/stippi/code-assistant/releases/download/v0.2.19
# Every ASSET is listed under its file name with URL, sha256 and size.

set -euo pipefail

if [[ $# -lt 3 ]]; then
  sed -n '2,9p' "$0" >&2
  exit 1
fi

VERSION="$1"
BASE_URL="${2%/}"
shift 2

assets='{}'
for file in "$@"; do
  name="$(basename "$file")"
  sha256="$(sha256sum "$file" | cut -d' ' -f1)"
  size="$(wc -c < "$file" | tr -d ' ')"
  assets="$(jq \
    --arg name "$name" \
    --arg url "$BASE_URL/$name" \
    --arg sha256 "$sha256" \
    --argjson size "$size" \
    '.[$name] = {url: $url, sha256: $sha256, size: $size}' <<< "$assets")"
done

jq -n --arg version "$VERSION" --argjson assets "$assets" \
  '{version: $version, assets: $assets}'
