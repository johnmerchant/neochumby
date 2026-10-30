#!/usr/bin/env python3
"""Patches the fetched WAMR (build/src/wamr) for strict-alignment ARMv5.

The fast interpreter's loader, rewriting `select` to SELECT_64 for i64/f64
operands on a 32-bit target, patches the handler address with a plain
`*(uint32 *)(p_code_compiled_tmp - sizeof(uint32)) = ...`, but compiled
code is only 2-byte aligned. An ARMv5 (the Chumby's ARM926EJ-S) ignores the
low bits of an unaligned STR's address, so the address lands 2 bytes off and
a later dispatch jumps to garbage (SIGILL in Thumb state). QEMU stores it
unaligned as ARMv6+ does, so the emulator never shows it. Stored through
memcpy instead, which is byte stores where the alignment isn't known.

  patch-wamr.py WAMR_DIR"""
import re
import sys

p = sys.argv[1] + "/core/iwasm/interpreter/wasm_loader.c"
s = open(p).read()
if "WB_PUT_U32" in s:
    sys.exit(0)
pat = re.compile(r"\*\(uint32 \*\)\((p_code_compiled_tmp\s*-\s*sizeof\(uint32\))\)\s*=\s*([^;]+);")
s, n = pat.subn(r"WB_PUT_U32(\1, \2);", s)
if n != 3:
    sys.exit(f"patch-wamr: expected 3 sites, found {n}")
s = s.replace(
    '#include "wasm_loader.h"',
    '#include "wasm_loader.h"\n#define WB_PUT_U32(p, v) \\\n    do { uint32 wb_v = (uint32)(v); memcpy((p), &wb_v, sizeof wb_v); } while (0)',
    1,
)
open(p, "w").write(s)
print(f"patch-wamr: {n} sites")
