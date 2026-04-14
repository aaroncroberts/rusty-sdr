#!/bin/sh
# build_macos.sh — builds SDR++ for macOS with all required flags for a correct .app bundle.
# Usage: sh build_macos.sh [extra cmake args...]
#
# After this completes, run:
#   sh make_macos_bundle.sh ./build ./SDR++.app
#
# Requirements (install via Homebrew):
#   brew install cmake fftw glfw libusb airspy airspyhf hackrf rtl-sdr portaudio codec2 zstd
#   Plus: SDRplay API installer from sdrplay.com (required for sdrplay_source)

set -e

BUILD_DIR="./build"
JOBS=$(sysctl -n hw.logicalcpu 2>/dev/null || echo 4)

mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

cmake .. \
    -DCMAKE_BUILD_TYPE=Release \
    -DUSE_BUNDLE_DEFAULTS=ON \
    \
    -DOPT_BUILD_AUDIO_SOURCE=OFF \
    -DOPT_BUILD_AUDIO_SINK=OFF \
    -DOPT_BUILD_PORTAUDIO_SINK=ON \
    -DOPT_BUILD_NEW_PORTAUDIO_SINK=ON \
    \
    -DOPT_BUILD_SDRPLAY_SOURCE=ON \
    -DOPT_BUILD_PLUTOSDR_SOURCE=OFF \
    -DOPT_BUILD_M17_DECODER=OFF \
    -DOPT_BUILD_MIDI_CONTROLLER=ON \
    \
    "$@"

make -j"$JOBS"

echo ""
echo "Build complete. To create the .app bundle, run from the repo root:"
echo "  sh make_macos_bundle.sh ./build ./SDR++.app"
echo ""
echo "NOTE: SDRplay source requires the SDRplay API to be installed system-wide."
echo "Download from: https://www.sdrplay.com/api/"
