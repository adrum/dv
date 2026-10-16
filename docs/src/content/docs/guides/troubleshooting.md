---
title: Troubleshooting
description: The failures that are silent, and what to do about them.
---

Start with `dv doctor`. Every check it runs exists because the corresponding
failure is silent — none of them announce themselves, and all of them look
like "dv doesn't work".

## `dotnet` still points somewhere else

Two causes, in order of likelihood.

**A stale shell hash.** Shells cache command locations, so `dotnet` may
resolve to a path recorded before the shims existed — or to a deleted one, in
which case `which dotnet` prints nothing at all.

```sh
hash -r     # or open a new shell
```

Windows has no `hash -r`: open a new terminal. A program started before the
shims were on the user `PATH` keeps the `PATH` it started with.

**PATH order.** Something earlier on `PATH` owns the name `dotnet` — most
often a system-wide install, which macOS adds through `/etc/paths.d/dotnet`.
Doctor names the file:

```
problem another dotnet comes before dv's shims: /usr/local/share/dotnet/dotnet
        move ~/.local/share/dv/shims earlier in PATH — `eval "$(dv init zsh)"`
        at the end of your profile does that
```

`dv init` prepends, so the usual cause is a later line in your profile putting
another prefix in front.

## dotnetup is installed too

[dotnetup](https://learn.microsoft.com/dotnet/core/tools/dotnetup/) is
Microsoft's own SDK manager. It and dv can both be installed; what matters is
which one's `dotnet` comes first on `PATH`. When dotnetup's does, dv's pins,
default and overrides do not apply, and doctor says so:

```
problem dotnetup's dotnet comes before dv's shims (…/dotnetup/dotnet/dotnet),
        so dv's pins, default and overrides do not apply
        `dotnetup env set none` takes dotnetup's dotnet off PATH (it stays
        usable as `dotnetup dotnet`) — or move dv's shims earlier in PATH
```

dotnetup can also set `DOTNET_ROOT` to its own install, which sends global
tools to its runtimes; doctor reports that separately.

## Global tools use the wrong .NET

```
warn  DOTNET_ROOT is /usr/local/share/dotnet, so global tools and app hosts
      use that install, not dv's
```

App hosts — global tools, and apps built with one — find their runtime through
`DOTNET_ROOT`. `dv init` sets it to dv's combined root; something later in
your profile is setting it back.

## An SDK is pinned but not installed

```
dv: /path/global.json requires .NET SDK 9.0.100 (rollForward: latestFeature)
    and nothing installed satisfies it — run `dv install` here
```

Deliberate: a pin that cannot be honored is an error rather than a silent
fallback to a different SDK. `dv install` with no version installs the newest
published SDK the file accepts.

## `global.json` is ignored

```
warn  /path/global.json is ignored by dotnet: `sdk.version` is not a valid
      version: `10.0`
```

The host reads only full SDK versions (`10.0.100`, not `10.0`) and ignores a
file it cannot parse, falling back to the newest SDK. dv does the same, and
doctor says why. `dv pin 10.0` rewrites the `sdk` section correctly.

## `sdk.paths` sends dotnet elsewhere

A `global.json` with `sdk.paths` tells the host to look for SDKs in those
directories — commonly a repository-local `.dotnet`. Include `"$host$"` in
the list to let it find the SDK dv resolved.

## An install root is incomplete

```
problem install roots are incomplete: 10.0.402 (missing sdk/10.0.402)
        run `dv rehash` to rebuild them
```

Something deleted part of a root. `dv rehash` relinks it from dv's pristine
copy in seconds; nothing is downloaded.

## An install seems stuck behind another

```
dv: another dv process is already installing .NET SDK 10.0.402 — wait for it
    to finish, then try again
```

Two dv processes cannot install the same SDK at once — an editor and a
terminal can easily both try. The lock is released when the other process
exits, however it exits.

## Reclaiming disk

```sh
dv cache status     # what the downloads take up
dv cache prune      # drop archives nothing is installed from
dv cache clear      # drop all of them
```

Cached archives are only ever a download shortcut; clearing them never affects
an installed SDK. Install roots are hard links, so they cost almost nothing
beyond the SDKs themselves.
