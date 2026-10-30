# Fonts

The terminal's fonts (`neochumby::term::font`): Frederic Cambus's
[Spleen](https://github.com/fcambus/spleen), every size it comes in, as the
PSF files its release ships for the Linux console, unchanged.

- Version: 2.2.0, from its release tarball,
  https://github.com/fcambus/spleen/releases/download/2.2.0/spleen-2.2.0.tar.gz
  (SHA-256 `ec42925c6b56d2138c862b2f97147c872e472f674bf03423417d827a08d69a89`)
- Licence: BSD-2-Clause, in `LICENSE.spleen`. The licence goes with
  every copy of the files, in source or in a binary built with them.

| File | Cell | On the 320x240 screen | SHA-256 |
| --- | --- | --- | --- |
| `spleen-5x8.psfu` | 5x8 | 64x30 | `fbbc21aab3241013ade8bc7d800b854e3d90f3115f2c619efc0b2a7bc52987cf` |
| `spleen-6x12.psfu` | 6x12 | 53x20 | `3ba9e7037e9ad8b23c16b3c9478fc5b8af915811894f5c5ad624b80f4ad3194e` |
| `spleen-8x16.psfu` | 8x16 | 40x15 | `5c894ef5250e9429ea4a22793bfa563cd923f86b9724194802f6acad67dae7b0` |
| `spleen-12x24.psfu` | 12x24 | 26x10 | `ba7490188ae7dde9934bab24498cff71ff48c89ebd8a244d4f8c642b354bb57c` |
| `spleen-16x32.psfu` | 16x32 | 20x7 | `b3b6067d4c00c2e8acae1df68c04ab35d23b6bec47120cb29ffa7bc9b975baad` |
| `spleen-32x64.psfu` | 32x64 | 10x3 | `4d023b23fc561903b738fd3a94d81096da0161edf09808372a1e4fe63b16467d` |

8x16 is PSF version 1, the others version 2; each has 512 glyphs and a
Unicode table. The dist kernels also have Spleen 5x8 built in as the
console's boot font (`dist/patches/linux/*/...-lib-fonts-add-Spleen-5x8.patch`,
from Spleen 2.1.0's BDF).
