# Proposal 0002: no shells, a start app, and the session opens apps

Status: **proposed by John**, replacing the "shells" part of proposal 0001
(#1). For discussion with Matt before #4 and #5 go further.

## The mismatch

Proposal 0001 brought in a **shell**: an app that is the Chumby's main
screen, and that other apps are listed in or shown inside (`shells=` in
#4). But every Chumby app is full-screen, one at a time, so nothing is ever
shown inside anything. The shell concept caused real confusion:

- **One word, three meanings.** `home` named a manifest role (a binary),
  the built-in app (`apps/home`), and a setting (an app name).
- **Content tied to particular shells by name.** `shells = ["wx"]` makes an
  app name the shells that may show it.

What a Chumby actually needs is smaller.

## The proposal

1. **Every app is just an app:** a full-screen program (`exec`), plus the
   `service`, `companion` and `daemon` roles as now. There is no `home` or
   `shell` role, and no `shells=` key.
2. **The start app: one setting.** `start=<app name>` says which app the
   Chumby opens at boot and returns to after any other app exits. It
   defaults to `home`. It's set on the server and reaches the Chumby
   through `Msg::Config` (#5), kept in `/psp` for boots without the server.
3. **The session opens every app.** An app never launches another one
   itself. It asks the session, which runs the requested app full-screen
   and, when that exits, goes back to the start app.
   - The session gives the foreground app a request pipe:
     `NEOCHUMBY_SESSION=<fd>`.
   - An app writes a line to it and then exits:

     | Request | What the session does |
     | --- | --- |
     | `open <app>` | Runs that app next |
     | `chumby-mode` | Ends neochumby and hands back to the stock software |
   - So wx, or any app, can offer Doom without being special.
4. **`neochumby-home` is just the app list:** an ordinary app with `exec`.
   It lists the apps with a `title`, and opens them and CHUMBY MODE through
   the session. It's the default start app.
5. **The escape hatch stays the same.** Holding the top-left corner for 3 s
   ends whatever is in front, and the app's whole process group goes with
   it. The session then opens `home`, even if the start app is another one.
6. **Matt's channel (wx) is an app that a Chumby can start with.** Its
   widgets are its own content, rendered by its own companion or daemon, and
   neochumby's core doesn't need to know about them. If a second app ever
   wants the same widgets, a shared widget concept can be added then.

## Names

| Term | What it names |
| --- | --- |
| **app name** | `<name>` from the crate `neochumby-<name>`, also its directory in the tree. Every reference to an app uses it: `start=`, `open <app>`, `Msg::Open { app }`. |
| **binary name** | Used only by the manifest's role keys: `exec`, `service`, `companion`, `daemon`. |

## What changes

| PR | Change |
| --- | --- |
| #4 (`shells=`) | **Closed, or cut down** to documenting that apps open apps through the session. |
| #5 (`Msg::Config`) | The setting is **`start`**, not `home`. Otherwise unchanged. |
| #6 (installer, manifest in `Cargo.toml`) | `home` and `shells` are removed from the manifest keys. |
| The session and `neochumby-home` | The request pipe; the session runs apps and returns to the start app; the app list uses the pipe; `apps/home` becomes a plain `exec` app. |
