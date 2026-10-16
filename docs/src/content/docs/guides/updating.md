---
title: Keeping SDKs up to date
description: What dv update moves forward, what it removes, and what it cannot see.
---

`dv install` records what you asked for, not only what it installed. That is
what `dv update` works from.

```sh
dv install 10.0         # tracks "the newest 10.0"
dv install lts          # tracks "the newest LTS", across majors
dv install 10.0.402     # an exact version: a pin, never moved
cd ~/code/app && dv install   # tracks this repository's global.json
```

Months later:

```sh
$ dv update
  10.0  10.0.402 → 10.0.405
  lts   10.0.402 → 10.0.405
  /Users/you/code/app/global.json
        9.0.310  up to date
installed 10.0.405
removed 10.0.402 — superseded, and nothing else needs it; `dv install <version>` gets one back
```

## What is tracked

| Source | From | Moves on update |
|---|---|---|
| `explicit` | `dv install 10.0`, `lts`, `10.0.4xx`, … | yes, within what it names |
| `explicit`, pinned | `dv install 10.0.402` | never |
| `global-json` | `dv install` with no version, in a repository | as that file's `rollForward` allows |
| `default` | `dv default 8.0` | yes, within what it names |

A `global.json` requirement is re-read on every update: edit the pin and the
next update follows it; delete the file and dv stops tracking it, and says so.

`dv list --format json` shows everything tracked. The state lives in
`tracked.json` in dv's home — plain JSON you can read.

## What is removed

An update removes only what it **supersedes**: the SDK a requirement resolved
to before the update, once the update has moved that requirement to a newer
one — and only if nothing else still resolves to it. Another requirement, the
default, and the current directory's `global.json` all keep an SDK.

So `10.0` moving from 10.0.402 to 10.0.405 removes 10.0.402, while an exact
pin, or an SDK some other line still uses, stays.

**An SDK no requirement tracks is never removed.** The report lists it, with
the commands to track or remove it:

```
not tracked, left alone: 8.0.426 — `dv install <version>` tracks one,
`dv uninstall <version>` removes it
```

Nothing is removed silently: every removal is named, with how to get it back.
A removed SDK's archive stays in the download cache, so reinstalling it is
instant until you run `dv cache prune`.

```sh
dv update --dry-run     # what would change, changing nothing
dv update --no-prune    # install updates, remove nothing
```

:::caution[What dv cannot see]
dv knows only the `global.json` files it was told about. If a repository you
never ran `dv install` in pins exactly the SDK one of your tracked lines moves
away from — 10.0.402 while `10.0` moves to 10.0.405 — that SDK is removed
unless you run the update from inside the repository. Run `dv install` once in
each repository you want kept, or check `dv update --dry-run` first.
:::

## Uninstalling

`dv uninstall 10.0` removes the SDK and stops tracking `10.0` and any exact
pin on that version. If another requirement still accepts it — `lts`, say —
dv tells you the next update would install it again.
