# Security

## What neochumby protects, and what it doesn't

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
  trusted code: install apps only from sources you trust.**
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
- **Companions run on the server** as neochumbyd's user, with what that user
  can reach. They are trusted code too.
- **neochumby never starts SSH.** The Chumby's stock SSH, with its
  well-known root password, stays off unless its owner turns it on.
- **A neochumby stick runs code on any Chumby it's plugged into at boot,**
  as the stock firmware's USB hooks allow.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
