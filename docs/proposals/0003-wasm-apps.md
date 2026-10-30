# Proposal 0003: apps as WebAssembly, sandboxed, and a catalog (GET APPS)

Status: **proposed**, for John to read. The design below is the one agreed
before it was written down; the numbers are the spike's, measured on a
Chumby Classic (`dev/wasmbench`).

## Why

Every app today is native ARM code, and [SECURITY](../../SECURITY.md) says
so plainly: an app is trusted code, because a native program written to
attack the Chumby's 2.6.16 kernel probably can. That is fine for the apps
in this repository and for an owner's own. It rules out letting a Chumby
install other people's apps by tapping on them.

WebAssembly changes what an app is. A `.wasm` module can only compute and
call the functions its host gives it: it has no system calls, no pointers
outside its own memory and no way to jump into code it wasn't given. If the
host gives a clock app a clock, the screen and the touchscreen, that is all
it has, whatever its author meant.

So this proposal does two things:

1. **Apps may be WebAssembly,** run by a small runner on the Chumby, inside
   the jail every app has now, with a host API that grants only what the
   manifest's `access` lists, and a kernel sandbox on top where the kernel
   has one.
2. **A catalog,** GET APPS on the screen, from which an owner installs
   other people's WebAssembly apps, pinned and checked, without a PC.

Native apps stay as they are. Nothing here makes Doom slower.

## What the spike measured

A Chumby Classic (ARM926EJ-S at about 266 MHz, no FPU), its stock 2.6.16
kernel, one run per row after a warm-up, medians of three to five. Each
workload is the same C built twice: natively (static musl, soft float,
`-O2`) and to `wasm32-wasip1`. "×" is the speed against native: above 1 is
faster.

| Workload | native | WAMR AOT | WAMR AOT, no bounds checks | WAMR fast interpreter | wasm3 | wasmi 2 | Wasmtime (Pulley) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CoreMark (iterations/s) | 674 | 321 (**0.48×**) | 502 (0.75×) | 28 (0.04×) | 24 (0.035×) | 14 (0.02×) | 21 (0.03×) |
| Soft float, n-body | 803 ms | 882 ms (**0.91×**) | 876 ms (0.92×) | 1191 ms (0.67×) | 1656 ms (0.49×) | 2015 ms (0.40×) | 2320 ms (0.35×) |
| Soft float, matrix | 428 ms | 468 ms (**0.91×**) | 471 ms (0.91×) | 878 ms (0.49×) | 1775 ms (0.24×) | 1762 ms (0.24×) | 1946 ms (0.22×) |
| Pixels: fill, per pixel | 288 ms | 428 ms (**0.67×**) | 278 ms (1.04×) | 6305 ms | 7312 ms | 13843 ms | 10535 ms |
| Pixels: rectangles, per pixel | 377 ms | 619 ms (**0.61×**) | 366 ms (1.03×) | 3707 ms | 4233 ms | 8388 ms | 6285 ms |
| Pixels: bulk copy (`memory.copy`) | 419 ms | 166 ms (**2.5×**) | 166 ms | 258 ms | 259 ms | 260 ms | 260 ms |
| Pixels: blit, row by row | 892 ms | 498 ms (**1.8×**) | 490 ms | 1091 ms | 1178 ms | 1570 ms | 2950 ms |
| Load a 200 KB module (wall) | 12 ms | **62 ms** | | 848 ms | 488 ms | 160 ms | 29 ms |
| An empty `main` (wall) | 10 ms | 20 ms | | 18 ms | 72 ms | 38 ms | 24 ms |
| Peak RSS, CoreMark | 52 KB | 568 KB | 516 KB | 540 KB | 2420 KB | 2420 KB | 756 KB |

- **WAMR, AOT-compiled, is the runtime.** It is the only one near native:
  half of native on integer code, nine-tenths on soft float, and faster on
  bulk copies (its `memory.copy` is musl's `memcpy`, and the native
  workload's loop isn't). Its runtime is 259 KB and a running app about
  1 MB.
- **The fallback is WAMR's fast interpreter,** for a module with no
  compiled `.aot` beside it: about 0.04× on integer code, and 850 ms to load
  200 KB. Usable for a clock, not for a game, and the catalog says so.
- **wasmi, wasm3 and Wasmtime's Pulley interpreter are ruled out** on the
  Chumby: each is slower than WAMR's own interpreter on most workloads,
  none comes near its AOT, and wasmi and wasm3 take 2 MB more.
- **Bounds checks stay on.** WAMR leaves them out on 64-bit hosts because a
  guard region catches what they would; on 32-bit ARM there is no room for
  one, and without them a module reads and writes anywhere in the runner.
  They cost about a third on per-pixel loops and CoreMark, and nothing on
  soft float or copies. That cost is why drawing goes through the host
  (below), not through per-pixel loops in wasm.

What the spike also found:

- **WAMR must be built for `ARMV5TE`,** not `ARM` (which assumes ARMv7
  instructions), and compiled with `wamrc --target=armv5te
  --target-abi=eabi --cpu=arm926ej-s`.
- **zig can't link WAMR on its own** for `arm-linux-musleabi`: its
  compiler-rt has no `__sync_*` (an ARMv5 has no `ldrex`) and only the
  `__aeabi_*` soft-float names, while WAMR's AOT relocator wants the libgcc
  names (`__addsf3`...). Rust's `compiler_builtins` has both, and linked by
  rustc, WAMR gets it for free.
- **wasm apps get 1 MiB stacks,** not the 16 MiB that zig and Rust ask for
  by default, which some runtimes commit up front.
- **A WAMR bug that only a real ARM926 shows.** The fast interpreter's
  loader stores three handler addresses through `*(uint32 *)p` where `p`
  is only 2-byte aligned. An ARMv5 ignores the low bits of an unaligned
  `STR`'s address, so the address lands 2 bytes off and a later dispatch
  jumps into garbage (SIGILL, in Thumb state). QEMU does the store as an
  ARMv6 would, so the emulator never shows it. We carry a fix (the stores go
  through `memcpy`), and an upstream report is drafted for John to file.
- **The emulator can't be trusted for alignment bugs.** Anything touching
  WAMR's loader or AOT code is run on a real Chumby before it merges.

## The sandbox on the Chumby

### One app, one runner, one jail

A wasm app's manifest names a `.wasm` instead of a native `exec`. The
session starts `neochumby-wasm`, the runner, in the app's jail exactly as it
starts a native app now ([jail.rs](../../crates/neochumby/src/bin/neochumby/jail.rs)):
its own uid, chrooted into a tmpfs holding its directory read-only, a
`/tmp` of its own, `nosuid,nodev`, rlimits on processes, files and core
dumps, and only the devices its `access` lists. The runner's binary is
bound into the jail read-only beside the app. One process per app: a
module that breaks WAMR has broken one jail, not the session.

The runner then:

1. Opens what the app's `access` grants (the framebuffer, the touchscreen,
   the companion channel), and nothing else.
2. Reads the module, and its `.aot` if there is one and it was compiled for
   this runner (below).
3. **Checks the module's imports against the grant.** A module that imports
   `neochumby.fb_blit` without `screen` in its `access` is refused before it
   runs a single instruction, with a line in the log naming the missing
   word. Nothing is left unlinked to trap later.
4. Instantiates it with its linear memory capped (`max_memory_pages`, by
   default 16 MiB) and a 1 MiB stack, and sets `RLIMIT_AS` on itself.
5. **Locks itself down** on a kernel that can (below), then calls the
   module's `_start`.

### The host API

Narrow, and capability-shaped: each import exists only for an app granted
the word it belongs to. Drawing is whole operations, so pixel loops run as
native code in the runner, not as bounds-checked wasm.

| Import (module `neochumby`) | Needs |
| --- | --- |
| `fb_fill(x, y, w, h, rgb565)`, `fb_blit(x, y, w, h, ptr, stride)`, `fb_text(x, y, ptr, len, scale, rgb565)`, `fb_size()` | `screen` |
| `touch_poll(ptr) -> n`: the next touch, mapped through the Chumby's calibration | `touch` |
| `accel(ptr) -> ok` | `accel` |
| `companion_send(ptr, len)`, `companion_recv(ptr, cap, timeout_ms) -> n`: neochumby-proto's typed channel to the app's own companion | a `companion` in the manifest |
| `request(ptr, len)`: a line on the session's request pipe (`open <app>`) | always |
| `now_ms()`, `sleep_ms(ms)`, `log(ptr, len)` | always |

Plus a deliberately small `wasi_snapshot_preview1`, so that a program built
for `wasm32-wasip1` with Rust's std (or C with wasi-libc) runs unchanged:
`fd_write` to stdout and stderr, `fd_read` from stdin, clocks, `random_get`,
`poll_oneoff` on clocks (sleeping), `proc_exit`, empty arguments and
environment, and no preopened directories, so no file system at all. A
WASI call outside that list isn't linked, and the module is refused at
load as in step 3.

**Text-mode apps** (the manifest's terminal mode, now the default for apps
without the screen) need nothing more: the session gives the runner the
VT (5+) or the library's terminal (2.6) as stdin and stdout, as it would a
native text-mode app, and `fd_write`/`fd_read` reach exactly those, with
touch arriving on stdin as the terminal encodes it. **Framebuffer apps** use
the `fb_*` and `touch_poll` imports.

### The kernel sandbox, where there is one

| | 2.6.16 (stock, classic) | 5.9.16 (dist) | stable, mainline (7.x, dist) |
| --- | --- | --- | --- |
| wasm's own isolation (bounds-checked memory, typed calls, no syscalls) | yes | yes | yes |
| The jail: own uid, chroot, mount namespace, `nosuid,nodev`, rlimits | yes | yes | yes |
| Imports checked against `access` before the module runs | yes | yes | yes |
| `no_new_privs`, and a seccomp-bpf allowlist of about 25 calls (kill the process on anything else) | no: the kernel has no seccomp filters | yes | yes |
| Landlock: no file system, no TCP, no signals or abstract sockets outside the jail | no | no (needs 5.13) | yes, as far as the kernel's Landlock ABI goes |
| Kernel with known, unfixed local root exploits | **yes** | some (5.9 is out of support) | no known |
| **Catalog (untrusted) wasm apps** | **refused**, unless the owner marks the app trusted | allowed | allowed |

The seccomp filter is installed after the runner has opened everything it
will use: from then on it may read and write the descriptors it has, map
anonymous memory (never executable), sleep, read clocks and exit. No
`open`, no `socket`, no `ioctl`, no `execve`, no `kill` of anyone else, no
`ptrace`. It checks the architecture first. Code built for 2.6.16 can't use
rustix (it calls the kernel directly and needs 3.2), so the filter and
Landlock are made with libc in `neochumby-sys`, like every other raw call,
and the same runner binary finds at run time whether the kernel has them
(`PR_SET_NO_NEW_PRIVS` fails with `EINVAL` on 2.6.16, Landlock's calls with
`ENOSYS` before 5.13).

On Linux 5 and later there is no CHUMBY MODE: the dist never hands the
Chumby back to the stock software. Only 2.6.16 has that path, and nothing
here touches it.

### What contains a runaway app

AOT code has **no instruction metering**: WAMR's AOT can't count
instructions without making all code slow. A module that loops forever
is contained the way a native one is:

- **the escape hatch,** holding the top-left corner for 3 s, ends the app
  in front and its whole process group, the runner included;
- **rlimits:** its address space, processes and files;
- **the memory cap** on its linear memory: `memory.grow` past it fails
  inside the module, as wasm says it does;
- a service (in the background) is niced, so a busy loop there slows the
  app in front without stopping the escape hatch.

## Threat model

**What we defend against:** an app from the catalog, written by someone we
don't know, that tries to read or change anything outside its own
directory, its own `/tmp` and what its `access` grants: other apps' files,
the Chumby's key, `/psp`, the network, other processes, the kernel. Also an
author who publishes a good app and later an update that asks for more.

**How:**

- The module is never native code we didn't compile. **A `.aot` is never
  accepted from an author:** it is native code, and one could leave out the
  bounds checks. It is compiled only by neochumbyd or by `tools/tree` and
  `tools/stick` on the PC, from the pinned `.wasm`, never on the Chumby.
- WAMR is the first wall. A module that breaks out of WAMR (a WAMR or LLVM
  bug) is native code in the runner's process, and then meets the jail and,
  on 5+, seccomp and Landlock, which leave it almost no kernel to attack.
- On 2.6.16 the second wall is the jail alone, and that kernel has local
  root exploits that will never be fixed. One WAMR bug there is plausibly
  root. That is why **untrusted catalog apps are 5+ only**: on 2.6.16 wasm
  runs our own apps, and apps the owner explicitly marks trusted
  (`neochumbyd set NAME trusted=APP,...`).
- `root`, `system` and `terminal` are never granted to a catalog app.
  `network` on the Chumby is refused by default: a catalog app's network
  belongs to its companion, on the server, where it is granted host by host.

**What we don't defend against:**

- A malicious neochumbyd or someone who can write the tree. They can put
  native code in front of the Chumby already.
- Denial of service: an app can use its CPU share and its memory cap, and
  be ugly. The escape hatch ends it.
- Side channels between apps on the same CPU. The ARM926 has no
  speculation to speak of; timing channels exist and we ignore them.
- Bugs in the stock 2.6.16 kernel reachable through the calls a jail needs
  (`read`, `write`, `mmap`). Hence the 5+ rule above.
- Who wrote an app. "Verified build" (below) says which repository's CI
  built the `.wasm`, not that its author is honest.

## Compiling

- **`.aot` is compiled only by neochumbyd, or by `tools/tree` and
  `tools/stick` on the PC,** with `wamrc --target=armv5te --target-abi=eabi
  --cpu=arm926ej-s`, bounds checks on, never on the Chumby (wamrc is LLVM;
  it wouldn't fit).
- **Compiled modules are cached** by the `.wasm`'s sha256, the WAMR version
  and the flags. The tree records all three beside the `.aot`. The session
  and the tree ship together, so when the session's WAMR changes, the
  next `tools/tree` or neochumbyd start recompiles.
- The runner checks the record against its own WAMR version and the
  `.wasm`'s and the `.aot`'s sha256. **An `.aot` that doesn't match is
  refused,** and the runner falls back to the interpreter, saying so.
- **No `.aot`** (no compiler on the PC that built the stick, or one
  compiled for another WAMR): the runner uses the fast interpreter, and the
  catalog says it will be slow.
- wamrc runs in a sandbox of its own on the server: a time limit, a memory
  limit, no network, a scratch directory, as its own uid where neochumbyd
  has them.

## Companions and server programs

A catalog app's companion is wasm too, run by neochumbyd under **Wasmtime**
(Cranelift, on x86_64 or aarch64). It gets nothing by default:

- its own state directory (a WASI preopen, and nothing else of the disk);
- the typed companion channel to its app (neochumby-proto's `channel`);
- **network, host by host,** each host approved by the owner at install
  (`network = ["api.example.com"]` in the manifest, shown before install);
  a connection to any other is refused;
- fuel or epoch interruption, so a runaway companion is stopped, and a
  memory cap.

It runs inside the per-app uid jail that #53 brings for native companions,
once #53 is merged. Until then a wasm companion's sandbox is Wasmtime
alone, and native companions stay trusted code as SECURITY says.

## The catalog

Code and docs say **catalog**; the screen says **GET APPS**. It is never
called the crates.io app store, and nothing in it implies the Rust project
endorses an app.

### What an app is

**A real crate on crates.io, containing its source.** Its `Cargo.toml`
says what it is, as any app's does, and pins its prebuilt modules:

```toml
[package]
name = "neochumby-weather"
version = "1.2.0"
repository = "https://github.com/someone/neochumby-weather"
keywords = ["neochumby-app"]

[package.metadata.neochumby]
title = "WEATHER"
access = ["screen", "touch"]
neochumbyd = "required"
network = ["api.open-meteo.com"]    # the companion's hosts

[package.metadata.neochumby.wasm]
app = { url = "https://github.com/someone/neochumby-weather/releases/download/v1.2.0/app.wasm", sha256 = "9f2c..." }
companion = { url = "https://github.com/someone/neochumby-weather/releases/download/v1.2.0/companion.wasm", sha256 = "41ab..." }
```

The URLs must be HTTPS; a GitHub release asset is typical. There is no
`.aot` key, and a crate that ships one in its files is refused.

### Installing

`crate:NAME@VERSION` is the only way to name a catalog app: versions are
pinned and **never updated by themselves**.

1. **The crate.** The installer downloads the `.crate` and checks it
   against the checksum in the crates.io sparse index
   (`index.crates.io`), then reads the manifest from its `Cargo.toml`. (The
   crates.io API doesn't expose `package.metadata`, so the crate itself is
   the source of truth.)
2. **The modules.** It fetches each `.wasm`, checks its sha256 against the
   manifest, and keeps its own copy: a release asset that later changes or
   disappears doesn't change an installed app.
3. **Provenance, as a label.** If GitHub has an artifact attestation
   (Sigstore) for the asset, from the crate's `repository`, the catalog
   shows **verified build**; otherwise **unverified**. Only a label for now.
4. **The owner approves its access.** The `access` list and the
   companion's hosts are shown before install, with the publisher (the
   crate's owners on crates.io), its downloads and whether it's verified.
   An update that asks for more asks again. `root`, `system` and `terminal`
   are refused outright, `network` on the Chumby by default.
5. **Compile.** wamrc, sandboxed, with time and memory limits (above).
6. **The tree.** The app joins the tree as `apps/<name>/`, its manifest
   marked as a catalog app's (the installer writes that; an author can't).

Installing needs the owner: the request is made on neochumbyd's admin
socket with the owner's token (#9's admin socket, once #9 is merged; until
then `neochumbyd install` as the server's admin). A Chumby's own GET APPS
asks its neochumbyd, which shows a code to type on the server, as pairing
does.

### Discovery

Once a day, on the server, cached:

- **crates.io:** crates with the keyword `neochumby-app`, sorted by
  downloads, 100 to a page, at most one request a second, with a
  User-Agent naming neochumbyd and its repository.
- **GitHub:** repositories with the topic `neochumby-app`, joined to crates
  by the crate's `repository`, with ETags, so an unchanged search costs
  nothing. `GITHUB_TOKEN`, if set, raises GitHub's limit.
- **Ranked** by recent downloads, then stars.

Chumbys and the web page read only the cache: GET APPS never makes a
Chumby, or a page view, call crates.io or GitHub.

### Stick-only Chumbys

A Chumby with no neochumbyd gets catalog apps on its stick from the PC:
`tools/stick STICK crate:NAME@VERSION` does the fetch, the checks and the
compile there. GET APPS on the Chumby itself needs neochumbyd.

## Manifest keys this adds

| Key | Where | What |
| --- | --- | --- |
| `[package.metadata.neochumby.wasm]` `app`, `companion` | the crate | `{ url, sha256 }` of each prebuilt module (catalog apps) |
| `wasm = "<bin>"` | the crate | this repository's and an owner's own: build `<bin>` for `wasm32-wasip1` and run it under the runner, instead of `exec` |
| `network = ["host", ...]` | the crate | the companion's hosts (catalog apps) |
| `wasm=<file>.wasm`, `aot=<file>.aot`, `aot-key=...` | the tree's manifest | written by tools/tree and neochumbyd |
| `source=catalog` | the tree's manifest | written by the installer only; such an app is refused on 2.6.16 unless trusted |

The schema lives in one place, `neochumby-manifest`, as now.

## The work, in pull requests

1. **This proposal,** with the spike's harness as `dev/wasmbench/`.
2. **WAMR for the Chumby:** a crate building WAMR (AOT and the fast
   interpreter, no WASI, no threads, no SIMD) with the alignment fix, and
   a small safe interface to it, its `unsafe` in one module.
3. **The runner,** with the host API and the jail, and seccomp and Landlock
   on 5+. Hostile-module tests: out-of-bounds access, a forged `.aot`,
   access not granted, a runaway loop, a forbidden syscall.
4. **`tools/tree` and `tools/stick` compile `.aot`** (wamrc, built or
   fetched like QEMU is).
5. **One real app ported,** the clock or LOGS, with CPU per frame against
   native.
6. **Wasmtime companions** on neochumbyd.
7. **The catalog:** discovery, install, and GET APPS on the web page and
   the Chumby.

## Open questions

- **Should "verified build" become required** for catalog apps, once
  enough authors publish attestations?
- **WAMR's fix as a patch or a fork.** CONTRIBUTING asks third-party C
  changes to go in a fork, pinned as a submodule; the alignment fix is one
  three-line change waiting on upstream. A patch applied by the crate's
  build until upstream takes it is proposed instead, so that no fork has to
  be kept.
- **Trusted apps on 2.6.16:** is `trusted=` per Chumby (a setting) the right
  place, or should it be per server?
