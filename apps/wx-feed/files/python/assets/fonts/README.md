# Bundled fonts

The pages were designed in macOS Arial, Verdana and Arial Rounded, which exist only on a
Mac. These open fonts stand in for them so every server (a Mac, or Debian with no Arial)
renders the same pixels.

| File | Stands in for | Used by | Source | Licence |
|---|---|---|---|---|
| `LiberationSans-Bold.ttf` | Arial Bold (metric-compatible) | `wxfeed.py` `BOLD` | Debian `fonts-liberation` 1:2.1.5-3 (trixie) | SIL OFL 1.1 (`licenses/Liberation-2.1.5.txt`) |
| `LiberationSansNarrow-Bold.ttf` | Arial Narrow Bold (metric-compatible) | `wxfeed.py` `NARROW` | Debian `fonts-liberation` 1.07.4-11 (bookworm; Liberation 2.x dropped Narrow) | GPLv2 with the font exception (`licenses/Liberation-1.07.4.txt`) |
| `DejaVuSans.ttf`, `DejaVuSans-Bold.ttf` | Verdana, Verdana Bold (close, not metric-identical) | `screens.py` `VERD`, `VERDB` | Debian `fonts-dejavu-core` 2.37-8 | Bitstream Vera licence + public domain (`licenses/DejaVu.txt`) |
| `SNPro-Bold.otf` | Arial Rounded Bold | `screens.py` `ROUND` | Debian `fonts-sn-pro` 1.1.0-1 (Nunito-based) | SIL OFL 1.1, RFN (`licenses/SNPro.txt`) |

The licence files are the Debian packages' `copyright` files. The font exception and the
OFL both allow bundling the fonts with this program and using them to render images.

Checked 2026-10-28 by rendering every page and screen before and after with the same
fetched data: Liberation is metric-compatible (the ticker came out at exactly the same
5684 px), and nothing moved or overflowed; the differences are anti-aliasing and glyph
shape only.
