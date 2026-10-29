# Security

## What neochumby protects, and what it doesn't

- **Everything on a Chumby runs as root,** as the stock firmware does. An
  app is trusted code. Install apps only from sources you trust.
- **The app tree is served over NFSv3,** read-only and without
  authentication. Anyone who can reach a neochumbyd's NFS port can read the
  tree (our binaries and your game data). Limit it to the network your
  Chumbys are on.
- **A Chumby and its neochumbyd talk TLS 1.3** (or Noise, if built that
  way), and each side checks the other's pinned key on every connection.
  Pairing pins the keys after someone confirms that the 6-digit code
  matches on both screens.
- **Companions run on the server.** Started as root with `--plugin-uids
  FIRST-LAST`, neochumbyd runs as the user owning its state directory, and
  each app's companions under a uid of the app's own, with no other groups
  and no capabilities, in a directory only that uid can reach. They can't
  read neochumbyd's key (`identity`, 0600), its pairings or settings
  (directories closed to other users), change the tree, or reach another
  app's files. They can still do what any local user can: use the network,
  reach neochumbyd's ports, and read world-readable files, the tree
  included. Without `--plugin-uids` (or without root), companions run as
  neochumbyd's user, with what that user can reach, and neochumbyd warns
  as it starts. Either way, install apps only from sources you trust.
- **neochumby never starts SSH.** The Chumby's stock SSH, with its
  well-known root password, stays off unless its owner turns it on.
- **A neochumby stick runs code on any Chumby it's plugged into at boot,**
  as the stock firmware's USB hooks allow.

## Reporting a vulnerability

Please report security problems privately, through GitHub's **Report a
vulnerability** on this repository's Security tab, rather than in an issue.
