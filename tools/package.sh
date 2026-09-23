#!/usr/bin/env bash
# Package a built XivLinkpearl plugin for Dalamud, and write the one-entry plugin
# repository listing that spacegho.st/mods/ffxiv/plugins.json is assembled from.
# Publishes nothing: the release workflow attaches what this writes.
#
#   dotnet build XivLinkpearl.slnx -c Release && tools/package.sh
#
# Output, all under $OUT (default ../ffxiv-linkpearl-build/release):
#   latest.zip                  the plugin folder as Dalamud installs it (XivLinkpearl.json,
#                               XivLinkpearl.dll, XivLinkpearl.Core.dll, Linkpearl.Interop.dll and
#                               the native linkpearl.dll, no .pdb). The name is stable on purpose, so
#                               .../releases/latest/download/latest.zip never changes.
#   XivLinkpearl-<version>.zip       the same bytes under a self-describing name
#   pluginmaster.json           stable-channel listing (one entry, a JSON array)
#   pluginmaster-testing.json   testing-channel listing, when TESTING=1
#
# Environment:
#   BIN                the build output to package (default
#                      $LINKPEARL_ARTIFACTS/bin/XivLinkpearl.Plugin/release, else
#                      ../ffxiv-linkpearl-build/artifacts/bin/XivLinkpearl.Plugin/release)
#   OUT                where to write (default ../ffxiv-linkpearl-build/release)
#   REPO               owner/name on GitHub (default $GITHUB_REPOSITORY, else
#                      Spaceghost/ffxiv-linkpearl)
#   TESTING            1 to write pluginmaster-testing.json instead of pluginmaster.json
#   RELEASE_TAG        the tag being released; the listing then carries the changelog
#   SOURCE_DATE_EPOCH  timestamp for the zip entries and LastUpdate (default: the last
#                      commit's time, else 0), so the same build packs to the same bytes
#
# Exit codes: 0 done, 1 nothing to package or a missing file, 127 a missing tool.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v zip >/dev/null || { echo "error: zip is required" >&2; exit 127; }
command -v python3 >/dev/null || { echo "error: python3 is required" >&2; exit 127; }

ARTIFACTS="${LINKPEARL_ARTIFACTS:-$ROOT/../ffxiv-linkpearl-build/artifacts}"
BIN="${BIN:-$ARTIFACTS/bin/XivLinkpearl.Plugin/release}"
OUT="${OUT:-$ROOT/../ffxiv-linkpearl-build/release}"
REPO="${REPO:-${GITHUB_REPOSITORY:-Spaceghost/ffxiv-linkpearl}}"
MANIFEST="$ROOT/src/XivLinkpearl.Plugin/XivLinkpearl.json"

[[ -f "$BIN/XivLinkpearl.dll" ]] || {
  echo "error: $BIN/XivLinkpearl.dll missing; run: dotnet build XivLinkpearl.slnx -c Release" >&2
  exit 1
}
[[ -f "$MANIFEST" ]] || { echo "error: $MANIFEST missing" >&2; exit 1; }

VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["AssemblyVersion"])' "$MANIFEST")"
[[ -n "$VERSION" ]] || { echo "error: no AssemblyVersion in $MANIFEST" >&2; exit 1; }
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || echo 0)"
fi

STAGE="${TMPDIR:-/tmp}/xivlinkpearl-package-$$"
trap 'rm -rf "$STAGE"' EXIT
rm -rf "$STAGE"
mkdir -p "$STAGE/XivLinkpearl" "$OUT"

# Everything the built plugin needs and nothing else: the managed assemblies and the
# manifest. Dalamud provides its own assemblies (the SDK keeps those out of the output),
# .pdb and .deps.json are for developers, and DalamudPackager leaves its own zip and
# staging folder in the output.
( cd "$BIN" && find . -type f -print0 | while IFS= read -r -d '' f; do
    # A case pattern's * matches slashes too, so the narrow paths come first.
    case "$f" in
      ./runtimes/win-x64/native/*) ;;                                  # native code for the game's process, if any
      ./runtimes/* | ./XivLinkpearl/*) continue ;;                          # other platforms; DalamudPackager's own staging copy
      ./*.deps.json | ./*.runtimeconfig.json | *.pdb | *.zip) continue ;;
      ./*/*) continue ;;                                               # nothing else from a subfolder
      ./*.dll | ./*.json) ;;
      *) continue ;;
    esac
    mkdir -p "$STAGE/XivLinkpearl/$(dirname "$f")"
    cp "$f" "$STAGE/XivLinkpearl/$f"
  done )
cp "$MANIFEST" "$STAGE/XivLinkpearl/XivLinkpearl.json"

# Dalamud unpacks the zip straight into the plugin folder, so the files sit at the root.
find "$STAGE/XivLinkpearl" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
rm -f "$OUT/latest.zip" "$OUT/XivLinkpearl-$VERSION.zip"
( cd "$STAGE/XivLinkpearl" && find . -type f | LC_ALL=C sort | sed 's|^\./||' |
    TZ=UTC zip -X -D -q "$OUT/latest.zip" -@ )
cp "$OUT/latest.zip" "$OUT/XivLinkpearl-$VERSION.zip"
echo "== $OUT/latest.zip"
# Listed once and kept: `unzip -l | grep -q` makes unzip die of SIGPIPE, which under
# `set -o pipefail` fails the check even when the file is there.
ZIP_ENTRIES="$(unzip -Z1 "$OUT/latest.zip")"
printf '%s\n' "$ZIP_ENTRIES"
# What Dalamud opens the zip for. A missing manifest installs a plugin that cannot load.
# linkpearl.dll is the native library: without it the plugin loads and cannot connect.
for want in XivLinkpearl.dll XivLinkpearl.json XivLinkpearl.Core.dll Linkpearl.Interop.dll linkpearl.dll ; do
  printf '%s\n' "$ZIP_ENTRIES" | grep -qxF "$want" || { echo "error: $want is not in the zip" >&2; exit 1; }
done

# The listing: the shipped manifest plus the fields a plugin repository adds.
CHANNEL=stable
LISTING="$OUT/pluginmaster.json"
if [[ "${TESTING:-0}" == 1 ]]; then CHANNEL=testing; LISTING="$OUT/pluginmaster-testing.json"; fi
# With RELEASE_TAG (the release workflow sets it), the listing also carries what changed,
# from the changelog, for the installer to show.
NOTES=()
if [[ -n "${RELEASE_TAG:-}" ]]; then
  NOTES=(--changelog "$(python3 "$ROOT/tools/releasekit.py" installer-notes "$RELEASE_TAG")")
fi
python3 "$ROOT/tools/pluginmaster.py" \
  --manifest "$MANIFEST" --repo "$REPO" --channel "$CHANNEL" \
  --last-update "$SOURCE_DATE_EPOCH" "${NOTES[@]}" --out "$LISTING"
echo "== $LISTING"
cat "$LISTING"
