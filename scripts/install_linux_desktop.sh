#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    printf 'usage: %s <Visionary executable>\n' "$0" >&2
    exit 2
fi

app_executable=$1
if [[ ! -f $app_executable || ! -x $app_executable ]]; then
    printf 'Visionary executable not found: %s\n' "$app_executable" >&2
    exit 1
fi
app_executable=$(realpath -- "$app_executable")

# Desktop Exec fields use double-quoted arguments with these four characters escaped.
escaped_executable=${app_executable//\\/\\\\}
escaped_executable=${escaped_executable//\"/\\\"}
escaped_executable=${escaped_executable//\`/\\\`}
escaped_executable=${escaped_executable//\$/\\\$}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(dirname -- "$script_dir")
data_home=${XDG_DATA_HOME:-"${HOME:?HOME is required when XDG_DATA_HOME is unset}/.local/share"}
applications_dir="$data_home/applications"
icons_dir="$data_home/icons/hicolor/scalable/apps"
desktop_source="$repo_dir/assets/linux/share/applications/Visionary.desktop"
icon_source="$repo_dir/assets/linux/share/icons/hicolor/scalable/apps/visionary.svg"
desktop_target="$applications_dir/Visionary.desktop"
icon_target="$icons_dir/visionary.svg"

mkdir -p -- "$applications_dir" "$icons_dir"
install -m 0644 -- "$icon_source" "$icon_target"

desktop_tmp=$(mktemp --tmpdir="$applications_dir" .Visionary.desktop.XXXXXX)
while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == Exec=* ]]; then
        printf 'Exec="%s"\n' "$escaped_executable"
    else
        printf '%s\n' "$line"
    fi
done < "$desktop_source" > "$desktop_tmp"
chmod 0644 -- "$desktop_tmp"
mv -f -- "$desktop_tmp" "$desktop_target"

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 --noincremental >/dev/null 2>&1
elif command -v kbuildsycoca5 >/dev/null 2>&1; then
    kbuildsycoca5 --noincremental >/dev/null 2>&1
fi

printf 'Registered Visionary desktop icon in %s\n' "$data_home"
