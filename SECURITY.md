# Security

## What neochumby protects, and what it doesn't

**Treat anything that runs on a Chumby as able to get root.** Its stock
kernel, Linux 2.6.16, gets no fixes, and it has local privilege
escalations that will never be fixed on it. The jail below is there to
keep an app that misbehaves from disrupting the others and the Chumby's
normal running. It is not a security boundary.

**The security boundary is the server.** neochumbyd is where community
code runs (apps' companions and server programs), where the keys are, and
where what each Chumby runs comes from. So the work goes there: sandboxing
what neochumbyd runs, and making sure a Chumby runs only what its paired
neochumbyd delivered.

- **The session runs as root; apps don't.** Each app runs as a user of its
  own, chrooted in a private mount namespace that holds its own files, a
  `/tmp` of its own and only the devices its manifest's `access` lists
  ([CONTRIBUTING](CONTRIBUTING.md#the-app-contract) has the list). It
  can't read the Chumby's key, write to `/psp` (where something would
  survive a reboot) beyond its own `storage` directory, or signal or trace
  other apps or root. It has limits
  on processes and open files. Its companion is opened for it by the
  session, so it reaches only its own. `home` alone runs as root, as it
  pairs the Chumby.
- **The jail refuses page-zero ELFs.** A static ARM ELF with a `PT_LOAD`
  below `0x1000`, or one that needs an interpreter, is refused, as is one
  hidden among the app directory's executable files. An app's `/tmp` and
  the speaker handover are `noexec`. That closes one NULL-pointer family
  (the ARM ELF loader can map page zero even though `mmap` refuses it); it
  does not fix the 2.6.16 kernel.
- **That stops apps that are buggy or careless, not ones written to
  attack.** The stock kernel (Linux 2.6.16, from 2006) has no user, pid
  or network namespaces, no seccomp, no cgroups and no netfilter, and it
  has known local privilege escalations that will never be fixed on it.
  Native code written to get root there probably can. **An app is still
  trusted code: install apps only from sources you trust.** What the
  Chumby runs comes from the paired neochumbyd: guard that, and the tree
  it serves.
- **The network can't be taken from an app.** An app whose `access`
  doesn't list `network` gets no name lookup, and the session ends it if
  it finds an internet socket of its in `/proc/net` (it looks once a
  second). That finds a socket; it doesn't stop one. A packet sent between
  two looks goes unseen.
- **What the session does as root for an app** is narrow: the mute and
  volume, the backlight (the start app only), and the stock player
  `btplay`, run with an `http://` URL checked character by character as
  one argument, never through a shell. `storage` is capped by measuring
  (every 2 seconds), not by the kernel, so an app can overshoot its 64 KiB
  briefly before it's ended. **`storage` is not `noexec`:** it's a bind
  mount from `/psp`, and 2.6.16 can't change a bind mount's flags, so an
  app could put a page-zero ELF there and run it. Until that's closed, an
  app with `storage` is trusted code, as any app is (above).
- **What each delivery guarantees** (README, "Delivery: NFS or sync"):
  - **`sync`:** every file comes over the pinned session, hashed as it
    arrives, and the Chumby boots the stick's set without the network. Its
    integrity rests on the pairing alone.
  - **`nfs`** (the default): the tree is served read-only over NFSv3,
    without authentication. Every file is served as root's, without setuid
    or setgid bits, and the Chumby mounts it `nosuid,nodev`. neochumbyd
    also serves the tree's manifest, signed with its key. Once
    `neochumby-boot` is in the Chumby's `/psp` (pairing and the session
    install it), the tree is mounted `noexec`, and the Chumby runs only
    copies, in RAM, of programs that match that manifest. It reads the
    apps' manifests and the boot hook only as checked, too. Someone who
    can answer for the server can't run code of their own this way. They
    can still: serve an older tree that neochumbyd once served; feed an
    app crafted data from its directory, which isn't checked; and, on a
    Chumby paired before `neochumby-boot` existed, have the one unchecked
    boot that installs it. Anyone who can reach the NFS port can read the
    tree (our binaries and your game data). Limit it to the network your
    Chumbys are on.
  - **A stick made by hand** (`tools/stick`) runs whatever is on it.
- **A Chumby and its neochumbyd talk TLS 1.3,** and each side checks the
  other's pinned key on every connection before it carries anything else.
  Pairing pins the keys after someone types the code from the Chumby's
  screen into `neochumbyd approve`. The code is 128 bits from the
  connection and both keys, too long for anyone in the middle to make both
  ends show alike. Until a connection is known to be a paired Chumby's,
  neochumbyd holds it to a 30-second deadline and caps how many it takes
  (overall, and from one address), and at most eight pairing requests wait
  at once.
- **An emulated Chumby's serial line carries the same sessions.** It has
  no network; `neochumbyd serve --listen-unix PATH` takes its line on a
  Unix socket, and each connection on the line is handled as one over TCP:
  the same handshake, keys and pairing. The line only carries bytes, so
  anyone who can connect to that socket can ask to pair, as anyone on the
  network can: keep it in a directory only you can reach.
- **A Chumby pairing trusts whoever answers.** It can't tell neochumbyd's
  approval from an impostor's, so someone in the middle while it pairs
  can have it pin their key, even though neochumbyd never approves them.
  Pair on a network you trust.
- **Apps' programs on the server are sandboxed.** Companions (one per
  Chumby) and server programs (one per server, `server=`) run confined,
  by this system's sandbox (below), by a uid of each app's own
  (`--app-uids`, further below), or both. Under neochumbyd's state directory each
  may only read the tree and read and write its app's own directory
  (`apps/<app>`, which holds its HOME and TMPDIR). It can't read or change
  neochumbyd's key (`identity`), the pairings, the owner's token or other
  apps' directories. Each also starts in a session of its own with no
  terminal, with only the descriptors it's given, its output through
  neochumbyd's log, and only a short list of neochumbyd's environment
  (`PATH`, the locale, `TZ`, `USER` and any `NEOCHUMBY_` variable).

  | System | Sandbox |
  | --- | --- |
  | Linux 5.13 or later, with Landlock on | Landlock: besides the above, it may read only the system (`/usr`, `/etc`, `/proc`, `/sys`, `/run`, `/var/log` and so on) and use the null and random devices. It can't ptrace or read the memory of processes outside its sandbox, neochumbyd's included |
  | macOS (for development) | Apple's sandbox (`sandbox-exec`): it may write only its own directory, read nothing in the home directory but the tree, and connect to no Unix socket in the state directory. It can read the rest of the system |
  | anything else, or Linux without Landlock | none: neochumbyd starts no companion or server program, unless it has `--app-uids` |

  With neither this system's sandbox nor `--app-uids`, `neochumbyd serve
  --unsandboxed` runs them anyway, as neochumbyd's user with all of its
  access, and neochumbyd says so in its log at every start. Where there's
  a sandbox, the flag does nothing. Run as root with neither, neochumbyd
  refuses even with the flag. Some kernels leave Landlock out (some
  hypervisor distributions, such as Proxmox's, do), and a container
  shares its host's kernel: if your server runs in one, check
  neochumbyd's log as it starts, and use `--app-uids`.

  neochumbyd logs, as it starts, which sandbox each app's programs get and
  what the running kernel leaves open. Not confined, on any system:
  - **the network.** Landlock's TCP rules (ABI 4, Linux 6.7) let a server
    program bind only the ports its manifest lists (`listen`), but it can
    connect anywhere, and UDP isn't covered. So an app's programs can
    reach your LAN and your Chumbys;
  - **CPU and memory.** Limit neochumbyd's service with your init system
    (`MemoryMax=` and `TasksMax=` in systemd, say), which covers what it
    runs. `RLIMIT_NPROC` counts by user, so it would limit neochumbyd too;
  - **with Landlock: `chmod`, `chown`, extended attributes and times** of
    files a program can reach; **truncating** before ABI 3 (Linux 6.2);
    **signalling** neochumbyd and other processes, and their abstract Unix
    sockets, before ABI 6 (Linux 6.12).

  A uid per app closes those last ones: a program can't truncate, chmod
  or chown what isn't its own, or signal another user's processes.
- **Wasm companions are held tighter still.** A companion that is a
  `.wasm` file (as catalog apps' are) runs in a process of neochumbyd's
  own, in the same sandbox and under the same uid as a native companion,
  and inside that under Wasmtime, granted nothing but its frames on stdin
  and stdout, its own state directory (no other file), the Chumby's name,
  and HTTPS GETs, without redirects, to the hosts its owner approved for
  it. Any other host is refused, and so is an approved one whose name
  resolves to a loopback, private, link-local, CGNAT, multicast,
  unspecified or reserved address, IPv4 or IPv6, checked as it is looked
  up and again on the connected socket, so DNS rebinding doesn't reach
  your LAN. It has 64 MiB of memory, and is stopped if it runs five
  seconds without calling the host. Wasm companions need neochumbyd built
  with the `wasm-companions` feature, as release builds are; a neochumbyd
  built without it starts none.
- **A uid per app** (`--app-uids FIRST-LAST`, neochumbyd started as root):
  neochumbyd runs as the user owning its state directory, and each app's
  companions and server program under one uid of the app's own, with no
  other groups and no capabilities, in a directory only that uid can
  write. They can't read neochumbyd's key (`identity`, 0600), its owner
  token, pairings or settings (closed to other users), change the tree,
  or reach another app's files. On a kernel with Landlock, Landlock
  applies too. Under uids alone, they can still read world-readable files,
  the tree included. The LOGS app's companion, with no other groups, reads
  only the host's logs any user may (`/var/log/syslog` is often the `adm`
  group's, the journal `systemd-journal`'s).
- **Which sandbox: the trade-off.** To start programs as other uids and
  stop them, neochumbyd keeps `CAP_SETUID` (with `CAP_SETGID`,
  `CAP_CHOWN` and `CAP_KILL`). A neochumbyd that keeps `CAP_SETUID` can
  make itself root on its host, so a flaw in neochumbyd itself is worse
  under `--app-uids` than in an unprivileged neochumbyd. On a host whose
  kernel has Landlock, run neochumbyd unprivileged, with Landlock. Per-app
  uids are for hosts and containers without Landlock: kernels built
  without it (for people who run a hypervisor like Proxmox, the host's
  kernel may be one), and neochumbyd in LXC or Docker, where it's root in
  the container anyway. Both together close what Landlock leaves open.
- **The admin socket** pairs Chumbys and changes their settings. It has no
  path. Only an app whose owner allows it (`neochumbyd allow <app>
  pairing`; `disallow` takes it back, at once) gets one, as an inherited
  descriptor, made for each run of its server program. Treat `pairing` as
  handing that app your neochumbyd: the page sees the owner's token as the
  person types it, and draws the approval screen itself. The token
  protects against a page with a bug, not a hostile one. A program given
  the socket should mark it close-on-exec, so that programs it runs don't
  get it.
- **Every change through the admin socket needs the owner's token**
  (`neochumbyd token`): approving, refusing and forgetting pairings, and
  settings (which include remote input, `dev=on`, and where a Chumby's
  apps come from). Pairing is only as safe as the person who types the
  code, so a web page must not approve for anyone who can merely reach
  it: it asks for the token. A wrong token is answered after a second. If
  the token may have leaked, `neochumbyd token new` replaces it at once.
- **Who can reach the state directory can do everything neochumbyd
  does.** We suggest a dedicated `neochumbyd` user and group, with the
  people who administer it in that group:

  ```sh
  useradd --system --user-group --home-dir /var/lib/neochumbyd neochumbyd
  install -d -o neochumbyd -g neochumbyd -m 2770 /var/lib/neochumbyd
  usermod -aG neochumbyd alice        # an admin
  ```

  The directory is setgid (the 2 in 2770), so what an admin creates in it
  belongs to the group. `owner-token` is 0640 in the directory's group,
  whoever writes it. So an admin running `neochumbyd token` or
  `neochumbyd token new` as themselves leaves a token the daemon can
  still read. The daemon's key, `identity`, stays 0600, the daemon's
  alone. With `--app-uids`, make the directory 2771 instead: apps' uids
  pass through it to their own directories and the tree.
- **neochumby never starts SSH.** The Chumby's stock SSH, with its
  well-known root password, stays off unless its owner turns it on.
- **A neochumby stick runs code on any Chumby it's plugged into at boot,**
  as the stock firmware's USB hooks allow.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
