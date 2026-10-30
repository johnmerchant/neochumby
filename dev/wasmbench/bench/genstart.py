#!/usr/bin/env python3
"""Writes startup.c: a WASI command whose main does nothing, with enough
distinct code kept alive (a table of functions, which main can reach but
doesn't call) for its .wasm to be about 200 KB. Its runtime is how long a
runtime takes to load, validate (or compile) and instantiate a module of an
app's size, and call into it.

  genstart.py OUT.c [FUNCS]"""
import sys

out = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 900
with open(out, "w") as f:
    f.write("#include <stdint.h>\n")
    for i in range(n):
        # A little of everything: arithmetic, a loop, a branch, a load and
        # store, and a call to the previous function.
        f.write(f"""
__attribute__((noinline)) uint32_t f{i}(uint32_t a, uint32_t *m)
{{
    uint32_t s = a * {i * 2654435761 % 2**32}u + {i};
    for (uint32_t k = 0; k < (a & 7); k++) {{
        s ^= m[(s + k) & 15] + {i * 40503 % 65536}u;
        s = (s << {i % 13 + 1}) | (s >> {31 - i % 13});
        if (s & {1 << (i % 31)}u) m[k & 15] += s; else s -= m[{i % 16}];
    }}
    return {"s" if i == 0 else f"s + (a > {i} ? f{i - 1}(a - 1, m) : 0)"};
}}
""")
    f.write("typedef uint32_t (*fn)(uint32_t, uint32_t *);\n")
    f.write("fn table[] = {" + ",".join(f"f{i}" for i in range(n)) + "};\n")
    f.write("""
volatile int never;
int main(void)
{
    static uint32_t m[16];
    if (never)
        return (int)table[never % (sizeof table / sizeof *table)](never, m);
    return 0;
}
""")
