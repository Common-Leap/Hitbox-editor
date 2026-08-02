#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "$repo_dir"

cargo build "$@"

# Register the exact binary this build produced so desktop launchers and compositors without
# xdg-toplevel-icon support can still resolve Visionary's icon from its application ID.
if [[ ${VISIONARY_SKIP_DESKTOP_INSTALL:-0} != 1 ]]; then
    profile=debug
    target_triple=
    target_dir=${CARGO_TARGET_DIR:-"$repo_dir/target"}
    args=("$@")

    for ((i = 0; i < ${#args[@]}; i++)); do
        case "${args[i]}" in
            --release)
                profile=release
                ;;
            --profile)
                ((i += 1))
                profile=${args[i]:?--profile requires a value}
                ;;
            --profile=*)
                profile=${args[i]#--profile=}
                ;;
            --target)
                ((i += 1))
                target_triple=${args[i]:?--target requires a value}
                ;;
            --target=*)
                target_triple=${args[i]#--target=}
                ;;
            --target-dir)
                ((i += 1))
                target_dir=${args[i]:?--target-dir requires a value}
                ;;
            --target-dir=*)
                target_dir=${args[i]#--target-dir=}
                ;;
        esac
    done

    [[ $profile == dev ]] && profile=debug
    if [[ -z $target_triple || $target_triple == *linux* ]]; then
        [[ $target_dir == /* ]] || target_dir="$repo_dir/$target_dir"
        artifact_dir="$target_dir/$profile"
        [[ -n $target_triple ]] && artifact_dir="$target_dir/$target_triple/$profile"
        bash "$repo_dir/scripts/install_linux_desktop.sh" "$artifact_dir/visionary"
    fi
fi
