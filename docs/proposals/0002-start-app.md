# Proposal 0002: no shells, a start app, and the session opens apps

Status: **agreed by John and Matt**, replacing the "shells" part of
proposal 0001 (#1).

## What neochumby is for

neochumby doesn't set out to recreate the original Chumby experience. It's
a common set of conventions and practices for writing new software for the
Chumby: how apps are packaged, delivered, started and opened, and a library
of standard methods for the hardware and the patterns apps share. An app can
provide a Chumby-style experience, and the library should make that easy,
but the session itself holds no UI policy.

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
   - **Only apps that declare `start = true` can be start apps,** so that
     `start=doom` can't trap someone in Doom. The session refuses any other
     name, logs why, and opens `home`.
   - Declaring it is a promise: the app opens other apps through the
     session, offers CHUMBY MODE and a way to the owner's settings, handles
     the top button (a `button` line on its pipe, or
     `NEOCHUMBY_REASON=button`) by showing its own main screen, and doesn't
     take the button (`button = true`).
   - **The server's settings suggest such an app as the start app** when it
     is added to the tree (in `neochumbyd set` and the web page).
   - `neochumby-home` and wx declare it.
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

## Always-on behaviour belongs to the app's service

Some of what an app offers has to keep working while another app is in
front: a button that brings the app back, idle timeouts, dimming the
backlight at night, alarms. That is what the `service` role is for: the
session runs every app's service for as long as neochumby runs.

- **The session supervises services:** one that exits is restarted, with
  backoff.
- **A service gets the same request pipe,** so it can `open <app>`. If
  another app is in front, the session ends it first. To keep services from
  fighting over the screen:
  - only the **start app's service** may open an app while another one is
    in front;
  - an app can declare `interrupt = false` (games, AirPlay) so that it
    isn't ended this way;
  - after the escape hatch, such requests are ignored until someone opens
    an app by hand.
- **The session tells services what's in front:** a `front <app>` line on
  the pipe. The pipe is line-based, and both ends ignore lines they don't
  know, so lines can be added later.
- **The touchscreen is shared:** every reader gets every event, so a
  service can watch it.
- The library provides the shared pieces (the request pipe, the backlight,
  idle time, sunrise and sunset), so each app that wants this behaviour
  doesn't write its own.

## The top button belongs to the session

`/dev/switch` (the top button) can only be opened by one process at a
time, so it can't be shared like the touchscreen. The session opens it once
and decides who gets each press:

- **An app that declares `button = true`** (Doom and Quake use it as FIRE)
  inherits the open file while it's in front, and the session stops reading
  it. `Squeeze::open` picks it up from the environment, so those apps don't
  change.
- **For any other app, a press returns to the start app,** which is told why
  (`NEOCHUMBY_REASON=button`), so it can show its own main screen. This is
  the stock Chumby's behaviour.
- **With the start app itself in front, a press is a `button` line** on its
  pipe, so it can show its main screen without starting again (the session
  writes to the front app's pipe, not only to services'). The write never
  waits; if it fails (the pipe is closed or full), the session ends the
  start app and opens it again with `NEOCHUMBY_REASON=button`, so a wedged
  start app still recovers. An app that doesn't read its pipe, such as
  `neochumby-home`, simply ignores the press.
- **There's no hold gesture on the button,** since games hold it to fire.
  The corner hold is the way out of anything.
- **On `chumby-mode`, the session closes it,** so the stock software can open
  it.

## The speaker

The speaker is usually driven by a service (AirPlay, music), not the app in
front, so there's no manifest key for it.

- **The library holds the speaker handling** (#13): `audio::Unmuted` saves
  the volume, sets a level and unmutes, restoring both when dropped, for a
  process that has another program play (wx's music, through the stock
  `btplay`). `audio::Speaker` adds `/dev/dsp`, for one that writes PCM
  itself (AirPlay).
- **`/dev/dsp` can be opened by only one process at a time,** so the
  newest player takes the speaker. The library asks whoever holds it to
  yield; the holder stops its player and drops its guard, and may start
  again once the speaker is free. So AirPlay doesn't wait while the radio
  plays.
- **When a supervised service dies,** the session mutes the speaker and
  restores the volume it had at boot, so a crash can't leave it on.

This is deliberately not a resident shell. The always-on behaviour is split
into small programs that each do one thing and can be replaced, restarted
or reused on their own, and the UI is an ordinary app that talks to them.
The session treats no app specially. If returning to an app turns out to be
too slow, the fix should be a mechanism any app can use (for example,
pausing the foreground app's process group as job control does), not a
role.

## Names

| Term | What it names |
| --- | --- |
| **app name** | `<name>` from the crate `neochumby-<name>`, also its directory in the tree. Every reference to an app uses it: `start=`, `open <app>`, `Msg::Open { app }`. |
| **binary name** | Used only by the manifest's role keys: `exec`, `service`, `companion`, `daemon`. |

## What changes

| PR | Change |
| --- | --- |
| #4 (`shells=`) | Closed. |
| #5 (`Msg::Config`) | The setting is **`start`**. |
| The manifest keys | `home` and `shells` go; `start`, `button` and `interrupt` come. `neochumbyd` stays explicit, and a companion without it is refused when the tree is built. |
| #13 (the speaker) | `audio::Unmuted` and `audio::Speaker` in the library. |
| The speaker handover | A follow-up PR: the newest player takes the speaker. |
| The session (Matt, three PRs) | 1. The request pipe and the start app (`start = true`); `neochumby-home` on the pipe as a plain `exec` app. 2. Services supervised, with the interruption rules. 3. The top button handed over. `front` notices where they fit. |
