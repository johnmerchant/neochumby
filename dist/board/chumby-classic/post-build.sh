#!/bin/sh
# Buildroot's post-build script (BR2_ROOTFS_POST_BUILD_SCRIPT): takes out
# of the rootfs ($1, Buildroot's target/) the shared libraries nothing in
# the image links, which the toolchain and ncurses install anyway. The
# rootfs is the zImage's built-in initramfs, and the sideloader reads the
# whole zImage through a 7 MiB window (tools/neochumby-kernel-sideloader's
# SRC_MAP_LEN), so every library counts.
#
#   - the toolchain's C++, OpenMP and atomics runtimes (libstdc++, libgomp,
#     libatomic): the image's programs are C, and ncinit, the one in Rust,
#     is static;
#   - ncurses' narrow-character libraries and its forms and menus: the
#     apps link the wide-character libncursesw and libpanelw only
#     (crates/neochumby-ncurses).
set -eu
target=${1:?usage: post-build.sh TARGET_DIR}
cd "$target"
rm -f usr/lib/libstdc++.so* usr/lib/libgomp.so* usr/lib/libatomic.so* lib/libatomic.so*
rm -f usr/lib/libncurses.so* usr/lib/libpanel.so* usr/lib/libform.so* usr/lib/libmenu.so*
rm -f usr/lib/libformw.so* usr/lib/libmenuw.so*
# What linked one of them would now fail to start: none may.
for f in $(find bin sbin usr/bin usr/sbin lib usr/lib -type f 2>/dev/null); do
  case $(head -c 4 "$f" | od -An -c | tr -d ' ') in
    177ELF) ;;
    *) continue ;;
  esac
  if readelf -d "$f" 2>/dev/null | grep -E 'NEEDED.*(libstdc\+\+|libgomp|libatomic|lib(ncurses|panel|form|menu)\.so|lib(form|menu)w)'; then
    echo "post-build.sh: $f needs a library taken out" >&2
    exit 1
  fi
done
