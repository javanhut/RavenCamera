#!/bin/sh
# Install whatever Raven Camera's build needs and this machine lacks.
#
# `imlazy build`, `imlazy run` and the rest (and `make`) run this first, so
# a fresh Raven machine needs nothing but the one command. On a machine that
# already has everything it checks and says so; nothing is installed.
#
# Each package is checked on its own and only the missing ones are
# installed:
#
#   pkgconf           gtk4-rs and libadwaita-rs find the libraries through
#                     pkg-config.
#   gtk4, libadwaita  the toolkit. The image carries the libraries; the build
#                     also needs their .pc files and headers.
#
# Installed system-wide with `rvn`, which as a member of wheel goes through
# rvnd without a password prompt.
set -eu

missing=""
need() { missing="${missing} $1"; }

have_pc() { pkg-config --exists "$@" 2>/dev/null; }

if command -v pkg-config >/dev/null 2>&1; then
    have_pc "gtk4 >= 4.12" || need gtk4
    have_pc "libadwaita-1 >= 1.5" || need libadwaita
else
    need pkgconf
    need gtk4
    need libadwaita
fi

if [ -n "${missing}" ]; then
    echo "deps: installing${missing}"
    if ! command -v rvn >/dev/null 2>&1; then
        echo "deps: rvn is not available; install these with your package manager:${missing}" >&2
        exit 1
    fi
    # shellcheck disable=SC2086 # word splitting is the point: one name each
    rvn install --repo-only -y ${missing}
else
    echo "deps: everything Raven Camera's build needs is installed"
fi

# The encoders come from the RavenGUI checkout beside this one (see
# .cargo/config.toml). That is a clone, not a package, so it is only checked.
if [ -f .cargo/config.toml ] && [ ! -d ../RavenGUI/crates/raven-mp4 ]; then
    echo "deps: Raven Camera builds against ../RavenGUI (raven-h264, raven-rec, raven-aac, raven-mp4)." >&2
    echo "deps: clone RavenGUI next to this repository, or delete .cargo/config.toml to use the git versions." >&2
    exit 1
fi
