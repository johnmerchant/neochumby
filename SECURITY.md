# Security

## What neochumby protects, and what it doesn't

- **The session runs as root; apps don't.** Each app runs as a user of its
  own, chrooted in a private mount namespace that holds its own files, a
  `/tmp` of its own and only the devices its manifest's `access` lists
  ([CONTRIBUTING](CONTRIBUTING.md#the-app-contract) has the list). It
  can't read the Chumby's key, write to `/psp` (where something would
  survive a reboot), or signal or trace other apps or root. It has limits
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
  trusted code: install apps only from sources you trust.**
- **The network can't be taken from an app.** An app whose `access`
  doesn't list `network` gets no name lookup, and the session ends it if
  it finds an internet socket of its in `/proc/net` (it looks once a
  second). That finds a socket; it doesn't stop one. A packet sent between
  two looks goes unseen.
- **The app tree is served over NFSv3,** read-only and without
  authentication. Every file is served as root's, without setuid or
  setgid bits, and the Chumby mounts it `nosuid,nodev`. Anyone who can reach a neochumbyd's NFS port can read the
  tree (our binaries and your game data). Limit it to the network your
  Chumbys are on.
- **A Chumby and its neochumbyd talk TLS 1.3,** and each side checks the
  other's pinned key on every connection before it carries anything else.
  Pairing pins the keys after someone types the code from the Chumby's
  screen into `neochumbyd approve`. The code is 128 bits from the
  connection and both keys, too long for anyone in the middle to make both
  ends show alike. Until a connection is known to be a paired Chumby's,
  neochumbyd holds it to a 30-second deadline and caps how many it takes
  (overall, and from one address), and at most eight pairing requests wait
  at once.
- **A Chumby pairing trusts whoever answers.** It can't tell neochumbyd's
  approval from an impostor's, so someone in the middle while it pairs
  can have it pin their key, even though neochumbyd never approves them.
  Pair on a network you trust.
- **Apps' programs on the server are sandboxed.** Companions (one per
  Chumby) and server programs (one per server, `server=`) run as
  neochumbyd's user, but confined. Under neochumbyd's state directory each
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
  | anything else, or Linux without Landlock | none: neochumbyd starts no companion or server program |

  On a system with no sandbox, `neochumbyd serve --unsandboxed` runs them
  anyway, with all of neochumbyd's access, and neochumbyd says so in its
  log at every start. Where there's a sandbox, the flag does nothing. Run
  as root with no sandbox, neochumbyd refuses even with the flag. Some
  kernels leave Landlock out (some hypervisor distributions, such as
  Proxmox's, do), and a container shares its host's kernel: if your
  server runs in one, check neochumbyd's log as it starts.

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

  Separate users per app (`--app-uids`) would close those Landlock leaves
  open, at the cost of neochumbyd keeping the right to change user.
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
  alone.
- **neochumby never starts SSH.** The Chumby's stock SSH, with its
  well-known root password, stays off unless its owner turns it on.
- **A neochumby stick runs code on any Chumby it's plugged into at boot,**
  as the stock firmware's USB hooks allow.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
