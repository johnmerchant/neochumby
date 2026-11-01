# Security

## What neochumby protects, and what it doesn't

**Treat anything that runs on a Chumby as able to get root.** Its kernel is
Linux 7 on a board nothing else tests, with out-of-tree drivers of ours, on a
CPU with few hardening features. The jail below is there to
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
  can't read the Chumby's key or see its stick (FAT keeps no permissions, so
  the jail, not the file's mode, is what keeps the key from it), keep
  anything across a reboot beyond its own `storage` directory, or signal or
  trace other apps or root. It has limits
  on processes and open files. Its companion is opened for it by the
  session, so it reaches only its own. `home` and neochumby-classic run as root, unjailed, and can read the
  stick's key: they are as trusted as the session.
- **An app reaches another app's service only as a frontend its owner
  approved** (`frontend_<app>=` in the Chumby's settings). The service
  takes connections from its own app's uid and its approved frontends'
  only, as the kernel reports them. The session binds every service's
  socket before it runs any app and hands it to the service, so no app
  can take another's name, and a frontend takes only a socket the
  session made. With jails off (the emulator) every app is one user, and
  nothing tells them apart.
- **The jail refuses page-zero ELFs.** An ARM ELF with a `PT_LOAD`
  below `0x1000`, or one that needs an interpreter, is refused, as is one
  hidden among the app directory's executable files. One interpreter is allowed: the tree's own musl, which the jail
  of an app that names it binds at `/lib/ld-musl-arm.so.1` with the
  tree's other libraries (`lib/`). They are files of the tree like any
  other, in its manifest. An app's `/tmp`, its `storage` and the speaker handover
  are `noexec`. That closes one NULL-pointer family (the ARM ELF loader
  can map page zero even though `mmap` refuses it).
- **That stops apps that are buggy or careless, not ones written to
  attack.** The jail uses a mount namespace, a uid of the app's own and
  rlimits, not user, pid or network namespaces, seccomp or cgroups, and the
  kernel has no netfilter. Native code written to get root probably can. **An app is still
  trusted code: install apps only from sources you trust.** What the
  Chumby runs comes from the paired neochumbyd: guard that, and the tree
  it serves.
- **An app built as WebAssembly (`wasm` in its manifest) is held
  tighter.** The runner (`neochumby-wasm`) runs it in the same jail, with
  bounds-checked memory and only the host functions its `access` grants,
  refusing one that asks for more before it runs. On Linux 5 and later it
  then locks itself down with seccomp (about 25 system calls; anything else
  kills it) and, on the stable and mainline kernels, Landlock (no file
  system, TCP or signals outside). Its compiled code is only ever compiled
  by `tools/tree` or neochumbyd, never taken from its author. On a kernel without
  seccomp a WAMR bug is plausibly root, so an app from the catalog
  (`source=catalog`) runs there only if its owner trusts it
  (`trusted=APP:APP` in its settings); a catalog app is never given
  `root`, `system`, `terminal`, `logs` or `network`.
- **The network can't be taken from an app.** An app whose `access`
  doesn't list `network` gets no name lookup, and the session ends it if
  it finds an internet socket of its in `/proc/net` (it looks once a
  second). That finds a socket; it doesn't stop one. A packet sent between
  two looks goes unseen.
- **What the session does as root for an app** is narrow: the mute and
  volume, the backlight (the start app only), and the player
  `btplay`, where there is one, run with an `http://` URL checked character by character as
  one argument, never through a shell. `storage` is capped by measuring
  (every 2 seconds), not by the kernel, so an app can overshoot its 64 KiB
  briefly before it's ended. **`storage` is `noexec`, `nosuid` and
  `nodev`:** it's a bind mount of a directory in RAM, which the session copies
  to and from the stick, and which the jail remounts so in its own namespace.
  A kernel that refuses (EINVAL) leaves it executable, and the session logs
  that once; any other failure, and the app doesn't start.
- **The stock firmware runs first.** Linux 2.6.16 gets no fixes, and it has
  local privilege escalations that will never be fixed on it. It runs no app:
  only the boot hook, which hands over to Linux 7. The module, kernel, initramfs
  and handoff program are whatever is on the stick.
- **Nothing on the Chumby's flash is written.** `/psp` is only read, on stock,
  and only what Linux 7 needs of it goes over, in RAM: the network's
  configuration with its Wi-Fi password, the touchscreen's calibration, the
  Chumby's name and time zone, and the owner's SSH opt-in and keys. Linux 7 has
  no NAND driver. `tools/check-psp` fails the build on any new use of `/psp`.
- **The stick is the trust root.** Whoever can alter the stick can run code on
  the Chumby as root, at the next boot, on the stock firmware as well as Linux
  7, and thereby read its Wi-Fi password, which the hook hands over; and they can
  copy its key. There is no signature on the stick: nothing checks the hook, the
  module, the kernel or the tree on it, and nothing could, as the stock firmware
  runs the hook as it finds it. Keep the stick as you would a house key.
- **What a stolen stick gives.** The key on it lets its holder impersonate that
  one Chumby to the server, from any network that can reach neochumbyd's port:
  they can receive its synced tree (your game data among it), and have
  companions run under its name, within the same sandbox as ever, and write
  to that Chumby's log on the server. It gives them neither the owner's token, nor the server's private key,
  nor any other Chumby's key or settings. `neochumbyd forget NAME` revokes it at
  once: every later connection with that key is refused. The stick also holds
  no Wi-Fi password: that comes from the Chumby's own flash at each boot, and is
  not written to the stick.
- **The key is created on the machine making the stick**, by `tools/stick
  --pair` (`neochumbyd keygen`), which writes it directly to the stick, and
  nowhere else on that machine's disks (it can still reach swap, as anything in
  memory can). Only its public half goes to the server. **The stick is the
  identity, so it moves with the stick:** the server can't tell which Chumby it
  is in.
- **Delivery.** Every file of a tree comes over the pinned session, hashed as
  it arrives, and the Chumby boots the stick's set without the network. Its
  integrity rests on the pairing, and on the stick. There is no other way for a
  tree to reach a Chumby: neochumbyd serves no files over any other protocol.
  Writes stay inside the stick's `neochumby/`, and the set being run or live is
  never written.
- **A Chumby and its neochumbyd talk TLS 1.3,** and each side checks the
  other's pinned key on every connection before it carries anything else. Both
  keys are learnt when the stick is made, over ssh, never over this protocol.
  **A key neochumbyd doesn't have gets nothing:** anyone who can reach its port
  can finish the TLS handshake, which shows them the server's public key, and
  is then told `Unpaired` and hung up on, within two seconds, whatever they say.
  No request of theirs is kept, and nothing they send reaches any other code.
  Until a connection is known to be a registered Chumby's, neochumbyd holds it
  to a 30-second deadline and caps how many it takes (overall, and from one
  address).
- **An emulated Chumby's serial line carries the same sessions.** It has
  no network; `neochumbyd serve --listen-unix PATH` takes its line on a
  Unix socket, and each connection on the line is handled as one over TCP:
  the same handshake and keys. The line only carries bytes, so anyone who
  can connect to that socket reaches what anyone on the network can: keep it
  in a directory only you can reach.
- **Apps' programs on the server are sandboxed.** Companions (one per
  Chumby) and server programs (one per server, `server=`) run confined,
  by this system's sandbox (below), by a uid of each app's own
  (`--app-uids`, further below), or both. Under neochumbyd's state directory each
  may only read the tree and read and write its app's own directory
  (`apps/<app>`, which holds its HOME and TMPDIR). It can't read or change
  neochumbyd's key (`identity`), the registered keys, the owner's token or other
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
  token, registered keys or settings (closed to other users), change the tree,
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
- **Installing from the catalog checks what it fetches, and labels who
  built it.** `neochumbyd install crate:NAME@VERSION` refuses a yanked
  version; refuses a `.crate` whose sha256 isn't the one crates.io's
  sparse index records; and refuses a module that isn't fetched over
  HTTPS, isn't the sha256 its Cargo.toml pins, or isn't WebAssembly
  bytecode. It keeps its own copy of each module. It refuses a crate that
  ships or names an `.aot`: an author's compiled code is native code, and
  is never taken. The `.aot` is compiled on the server by wamrc, with
  bounds checks on. wamrc runs in a scratch directory with time, CPU and
  memory limits, and no network where the kernel lets a user make a
  network namespace. It refuses the access words `root`, `system`,
  `terminal`, `logs` and `network`: a catalog app's network belongs to its
  companion, host by host. Before anything is written it shows the owner
  what the app asks for, and the owner approves exactly that, access words
  and hosts.
- **Installs, removals and the update setting are the owner's,** through
  the admin protocol with the owner's token, from the command line, which
  reads the token from the state directory, so is run by the server's admin.
- **Updates that ask for nothing new apply by themselves.** After each
  refresh, a catalog app whose listing has a newer version is updated if
  that version asks for no access word and no host the installed one
  wasn't granted; it is checked as an install is, and logged in
  `<state>/catalog.log`. One that asks for more waits for the owner. So an
  author can change an app's code, within what it was granted, without the
  owner looking again: if that matters to you, `neochumbyd catalog
  auto-update off`. `serve --no-catalog-refresh` stops neochumbyd asking
  crates.io and GitHub by itself at all.
- **"Verified build" is checked here, and is a label, not a requirement.**
  neochumbyd verifies GitHub's attestation for the app's `.wasm` itself,
  as a Sigstore bundle: its certificate chains to Sigstore's Fulcio (the
  public-good instance's roots are built into neochumbyd) and was valid
  when Rekor logged the signature, by the log's signed promise; the DSSE
  signature checks with that certificate; the certificate is for a GitHub
  Actions run of a workflow in the crate's own repository; and the signed
  statement is SLSA provenance naming the module's sha256. It doesn't
  check certificate transparency or Rekor's inclusion proof (the signed
  promise stands for it), and an attestation logged only in a Rekor v2 log
  is unverified for now. The label says a workflow in that repository
  built that file. It says nothing of whether the author is honest or the
  code is safe, and "unverified" doesn't mean malicious. The modules'
  sha256, pinned in the crate, are what is checked. None of this makes the
  Rust project or crates.io vouch for an app.
- **A Chumby's GET APPS only asks.** It names the version and the exact
  access its owner allowed on its screen; neochumbyd fetches and checks
  the app again, refuses it if it asks for anything else, and installs
  only once the owner approves the code the Chumby shows, through the
  admin protocol with the owner's token (`neochumbyd approve CODE` on the
  command line, as the server's admin). Removing an app is approved the same way, and so is an update
  that asks for more; one that asks for nothing new needs only the
  Chumby's owner, unless automatic updates are off. The code only names
  the request (the Chumby's connection is already authenticated); it
  isn't a secret. An app installed from the catalog can't name
  neochumbyd's catalog as its companion.
- **What neochumbyd fetches for a Chumby is limited.** GET APPS shows only
  apps already in the cache, and looking one up (its details, which mean
  fetching its crate) is for apps in the cache only, from crates.io and
  GitHub only (a module hosted anywhere else is fetched at install, not to
  show it), kept once looked up, and at most a few a minute for each
  Chumby. A Chumby can't make neochumbyd fetch a URL of its choosing.
- **The admin protocol has no socket anyone can reach.** It runs over a
  connected pair inside the command line's own process. No app is given it.
  It needs the owner's token for every change (`neochumbyd token new` replaces
  the token at once), and a wrong token is answered after a second.
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
  as the stock firmware's USB hooks allow. Without one, the Chumby boots its
  stock firmware as it always did.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
