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
- **A Chumby and its neochumbyd talk TLS 1.3** (or Noise, if built that
  way), and each side checks the other's pinned key on every connection.
  Pairing pins the keys after someone confirms that the 6-digit code
  matches on both screens.
- **Companions and server programs run on the server, in a sandbox.**
  neochumbyd starts none without one, and logs which each gets and what it
  leaves open. Either of two is enough, and both may apply at once:
  - **Landlock** (#9), with neochumbyd running as its own user: each
    program is kept to its own files. What Landlock can't restrict at the
    kernel's ABI stays open (`chmod` and `chown`; signals before ABI 6;
    truncation before ABI 3).
  - **A uid per app** (`--app-uids FIRST-LAST`, neochumbyd started as
    root): neochumbyd runs as the user owning its state directory, and each
    app's companions and server program under one uid of the app's own,
    with no other groups and no capabilities, in a directory only that uid
    can reach. They can't read neochumbyd's key (`identity`, 0600), its
    pairings or settings (directories closed to other users), change the
    tree, or reach another app's files.

  Under either, they can still use the network, reach neochumbyd's ports,
  and (under uids alone) read world-readable files, the tree included.
  With neither, `serve` refuses to start unless its owner passes
  `--unsandboxed`, which runs them as neochumbyd's user, able to read its
  key and pairings and change the tree. As root it refuses even then.
  Either way, install apps only from sources you trust.
- **Which sandbox: the trade-off.** To start programs as other uids,
  neochumbyd keeps `CAP_SETUID` (with `CAP_SETGID` and `CAP_CHOWN`). A
  neochumbyd that keeps `CAP_SETUID` can make itself root on its host, so
  a flaw in neochumbyd itself is worse under `--app-uids` than in an
  unprivileged neochumbyd. On a host whose kernel has Landlock, run
  neochumbyd unprivileged, with Landlock. Per-app uids are for hosts and
  containers without Landlock: kernels built without it (for people who run
  a hypervisor like Proxmox, the host's kernel may be one), and neochumbyd
  in LXC or Docker, where it's root in the container anyway. Both together close what
  Landlock leaves open.
- **neochumby never starts SSH.** The Chumby's stock SSH, with its
  well-known root password, stays off unless its owner turns it on.
- **A neochumby stick runs code on any Chumby it's plugged into at boot,**
  as the stock firmware's USB hooks allow.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
