#!/usr/bin/env bash
# Build XivLinkpearl (Release) and print the path to add under
#   Dalamud Settings -> Experimental -> Dev Plugin Locations
# This script never edits Dalamud's configuration and never copies into ~/.xlcore;
# it stages the build in ../ffxiv-linkpearl-build/devplugin (LINKPEARL_STAGE overrides).
#
# Environment:
#   DOTNET            dotnet executable (default: ~/.dotnet/dotnet, then dotnet on PATH)
#   LINKPEARL_ARTIFACTS  build output root (default: ../ffxiv-linkpearl-build/artifacts next to the repo)
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
project="$repo/src/XivLinkpearl.Plugin/XivLinkpearl.Plugin.csproj"

dotnet="${DOTNET:-}"
if [[ -z "$dotnet" ]]; then
  if [[ -x "$HOME/.dotnet/dotnet" ]]; then
    dotnet="$HOME/.dotnet/dotnet"
  elif command -v dotnet >/dev/null 2>&1; then
    dotnet="$(command -v dotnet)"
  else
    echo "install-dev: dotnet SDK not found (set DOTNET=/path/to/dotnet)" >&2
    exit 1
  fi
fi

artifacts="${LINKPEARL_ARTIFACTS:-$(cd "$repo/.." && pwd -P)/ffxiv-linkpearl-build/artifacts}"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 LINKPEARL_ARTIFACTS="$artifacts"

echo "install-dev: building $project (Release) into $artifacts" >&2
"$dotnet" build "$project" -c Release -nologo -v quiet >&2

dll="$artifacts/bin/XivLinkpearl.Plugin/release/XivLinkpearl.dll"
if [[ ! -f "$dll" ]]; then
  echo "install-dev: build finished but $dll is missing" >&2
  exit 1
fi

# Stage into a fixed directory so Dalamud (auto-reload) only ever sees complete
# builds, never a half-written bin/ from an in-progress compile.
stage="${LINKPEARL_STAGE:-$(cd "$repo/.." && pwd -P)/ffxiv-linkpearl-build/devplugin}"
mkdir -p "$stage"
src_dir="$(dirname "$dll")"
for f in XivLinkpearl.json XivLinkpearl.Core.dll XivLinkpearl.Core.pdb Linkpearl.Interop.dll Linkpearl.Interop.pdb linkpearl.dll XivLinkpearl.deps.json XivLinkpearl.pdb XivLinkpearl.dll; do
  [[ -f "$src_dir/$f" ]] || continue
  cp "$src_dir/$f" "$stage/.$f.tmp" && mv -f "$stage/.$f.tmp" "$stage/$f"
done
dll="$(readlink -f "$stage/XivLinkpearl.dll")"

# Wine maps the host root to drive Z:, so /a/b/c becomes Z:\a\b\c.
windows_path="Z:${dll//\//\\}"

cat >&2 <<EOF

Built: $dll

In game, once: /xlsettings -> Experimental -> Dev Plugin Locations, add the path below,
"Save and close". Dev plugins are added disabled: in /xlplugins -> Dev Tools ->
Installed Dev Plugins, enable "XivLinkpearl" and tick "Start on boot".
After rebuilding, reload XivLinkpearl from /xlplugins (or tick its "Automatic reloading").
EOF
printf '%s\n' "$windows_path"
