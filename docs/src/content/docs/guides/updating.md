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
removed 10.0.402 — nothing tracked needs it; `dv install <version>` gets one back
```

## What is tracked

| Source | From | Moves on update |
|---|---|---|
| `explicit` | `dv install 10.0`, `lts`, `10.0.4xx`, … | yes, within what it names |
| `explicit`, pinned | `dv install 10.0.402` | never |
| `global-json` | `dv install` with no version, in a repository | as that file's `rollForward` allows |
| `default` | `dv default 8.0` | yes, within what it names |
| `existing` | SDKs installed before dv tracked anything | never |

A `global.json` requirement is re-read on every update: edit the pin and the
next update follows it; delete the file and dv stops tracking it, and says so.

`dv list --format json` shows everything tracked. The state lives in
`tracked.json` in dv's home — plain JSON you can read.

## What is removed

After installing, `dv update` removes every installed SDK that no tracked
requirement, the default, or the current directory's `global.json` resolves
to. Each requirement keeps exactly the SDK the `dotnet` host would choose for
it, so a superseded patch goes and a pin stays.

Nothing is removed silently: the update names every version it removed and
how to get it back. A removed SDK's archive stays in the download cache, so
reinstalling it is instant until you run `dv cache prune`.

```sh
dv update --dry-run     # what would change, changing nothing
dv update --no-prune    # install updates, remove nothing
```

:::caution[What dv cannot see]
dv only knows the `global.json` files it was told about. A repository pinned
to an exact SDK that you never ran `dv install` in is protected only while you
run the update from inside it. Run `dv install` once in each repository you
want kept — or `dv update --dry-run` first, which lists every removal.
:::

## Upgrading from an earlier dv

dv 0.1.2 and earlier tracked nothing. The first time a newer dv runs, it
records every SDK already installed as an exact pin, so an update never
removes anything you had. To let one move forward, install its line —
`dv install 10.0` — and uninstall the old pin when you are ready.

## Uninstalling

`dv uninstall 10.0` removes the SDK and stops tracking `10.0` and any exact
pin on that version. If another requirement still accepts it — `lts`, say —
dv tells you the next update would install it again.
