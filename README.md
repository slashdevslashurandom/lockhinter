# lockhinter, a standalone tool for setting and clearing LockedHint

This program is a simple tool designed to manage the `LockedHint` state of a
`logind` session (whether when used as part of `systemd` or a fork, such as
`elogind`).

Normally, the desktop environment (such as GNOME or Plasma) would set LockedHint
whenever a session is locked or unlocked in order to indicate the session's
state to the login manager. In addition, certain utilities query that state
themselves, usually to make sure that sensitive operations can only be performed
while the session is unlocked.

More lightweight desktop environments, window managers and Wayland compositors,
however, do not do that, and the session is always treated as if it is
unlocked. The decision to not add this functionality is usually justified by a
desire to not add extra functionality or not add a dependency on `systemd`.

To solve this problem, `lockhinter` functions as an in-between utility. When
launched with command-line arguments including a locker program's name and its
arguments, `lockhinter` launches the locker, then sets `LockedHint` to true,
waiting until the locker terminates. If it terminates gracefully and returns a
code of `0` (no errors), `LockedHint` is then set to false, otherwise it is not
reset. (This is to make sure that `lockhinter` or the locker program crashing
doesn't cause the login manager or programs reading `LockedHint` to think a
session was unlocked even though it wasn't.)

## Installation

Right now, the best way to install the program is via `cargo`:

`$ cargo install lockhinter`

You will likely also need to add `$HOME/.cargo/bin` to your `PATH` environment
variable, for example by editing the `.profile` file in your home directory to
say:

```
export PATH=$HOME/.cargo/bin:$PATH
```

If any Linux distro maintainers decide to add this utility to their
repositories, then installing via package manager will be the preferred way, as
it would provide a system-wide installation that doesn't require editing
`$PATH`, as well as automatic updates.

## Usage

`lockhinter` has two modes of operation. The primary one is using it to run
another locker utility (such as `swaylock`), in which case it will keep the
`LockedHint` property set for as long as the locker is running. Parameters for
running the locker should be specified immediately, or after the `--` separator.

```
$ lockhinter -- swaylock -f -c 3d3846
```

or

```
$ lockhinter swaylock -f -c 3d3846
```

By default, if the `LockedHint` property is already set on a session,
`lockhinter` will terminate immediately and not change it, but this can be
overridden with the `-f` or `--force`` parameter:

```
$ lockhinter -f -- swaylock -f -c 3d3846
```

That way, the program will start the locker again and clear the `LockedHint`
property once the locker closes properly.

If the `-c` parameter is specified, then `lockhinter` will simply check the
`LockedHint` property's state, output `FALSE` or `TRUE` to standard output and
terminate with a code of 0 or 1 respectively.

```
$ lockhinter -c
FALSE
```
