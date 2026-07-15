#!/usr/bin/env bash
# Find a software Vulkan/GL adapter for headless wgpu rendering (debug_driver
# screenshots, egui_kittest snapshot tests) and export the env vars that point
# the Vulkan loader at it.
#
#   source scripts/headless-gpu.sh            # export into the current shell
#   scripts/headless-gpu.sh cargo run ...     # or run one command with it set
#
# Fallback chain: SwiftShader Vulkan ICD (bundled with the pre-installed
# Chromium) -> lavapipe Mesa ICD -> software GL. If none is found it prints a
# hint and leaves the env unchanged (screenshots then just report ERR).

_find_icd() {
    # 1) SwiftShader shipped with Playwright's Chromium.
    local ss
    ss=$(find /opt/pw-browsers -name "vk_swiftshader_icd.json" 2>/dev/null | head -1)
    if [ -n "$ss" ]; then echo "$ss"; return 0; fi
    # 2) lavapipe (Mesa software Vulkan), if installed system-wide.
    local lp
    lp=$(find /usr/share/vulkan/icd.d /usr/lib*/vulkan* -name "lvp_icd*.json" 2>/dev/null | head -1)
    if [ -n "$lp" ]; then echo "$lp"; return 0; fi
    return 1
}

if icd=$(_find_icd); then
    # Point both the old (VK_ICD_FILENAMES) and new (VK_DRIVER_FILES) loader vars.
    export VK_ICD_FILENAMES="$icd"
    export VK_DRIVER_FILES="$icd"
    export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
    # Software GL fallback too, in case wgpu drops to GL.
    export LIBGL_ALWAYS_SOFTWARE=1
    echo "headless-gpu: using Vulkan ICD $icd" >&2
else
    export LIBGL_ALWAYS_SOFTWARE=1
    echo "headless-gpu: no Vulkan ICD found; set software GL only. Install one with:" >&2
    echo "  apt-get install -y mesa-vulkan-drivers libvulkan1" >&2
fi

# If given a command, run it with the env set; otherwise assume we were sourced.
if [ "$#" -gt 0 ]; then
    exec "$@"
fi
