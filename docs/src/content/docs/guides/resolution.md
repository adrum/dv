---
title: How a version is chosen
description: The resolution order, and why each step is there.
---

Every `dotnet` that runs through a shim is resolved afresh, from the directory
it runs in. Nothing is cached and nothing changes on `cd`. First match wins:

1. **`DV_DOTNET_VERSION`** — an explicit override for one command or one
   shell: `DV_DOTNET_VERSION=8 dotnet build`.
2. **The nearest `global.json`**, when it names an SDK version — read with the
   `dotnet` host's own roll-forward rules.
3. **The default** — `dv default 10.0`.
4. **The newest installed release.** A preview is chosen only when nothing
   else is installed.

```sh
dv resolve --source     # 9.0.319 (/Users/you/code/legacy/global.json)
```

## The rules that hold it together

**Resolution is read-only.** It never installs, never writes and never
touches the working directory. That is what makes shims cheap, and what lets
`dv which` and `dv resolve` answer "what would run here?" for scripts.

**Falling back beats failing.** A directory with no pin resolves to the newest
installed SDK rather than an error. A version manager that refuses to run
`dotnet --info` in an unpinned directory is experienced as broken.

**An explicit pin that cannot be honored is an error.** If `global.json` or
`DV_DOTNET_VERSION` asks for something that is not installed, the command
fails with a message naming the fix — never a silent fallback to a different
SDK:

```
dv: /path/global.json requires .NET SDK 9.0.100 (rollForward: latestFeature)
    and nothing installed satisfies it — run `dv install` here to install
    what it asks for
```

## global.json, the way dotnet reads it

dv does not invent a version file. It follows the `dotnet` host's resolver —
ported from its source rather than from the documentation, because the two
disagree in the details that decide which SDK runs:

- **Only the nearest `global.json` counts.** One without an `sdk.version`
  still ends the search; dv does not keep walking up to a parent's pin.
- **A version with no `rollForward` means `patch`**: that exact SDK if it is
  installed, else the newest patch in the same feature band.
- **An invalid file is ignored**, as the host ignores it — with a warning
  from `dv doctor` rather than a failure.

The [global.json reference](/dv/reference/global-json/) has every policy and
what `dv pin` writes.

## Why the newest *release*, not the newest SDK

Left alone, `dotnet` picks the newest SDK it can see — previews included. dv's
fallback prefers releases, so installing an 11.0 preview to try it does not
quietly move every unpinned project onto it. To use the preview, pin it or
make it the default.

## Switching on `cd`

Optional, and only ever a convenience:

```sh
eval "$(dv init zsh --hook)"
```

The hook exports `DV_DOTNET_VERSION` when you enter a directory that pins a
version, and clears it when you leave. Correctness does not depend on it — the
shims resolve every process, interactive or not — so if it misbehaves, drop
`--hook` and nothing else changes.

An explicit `DV_DOTNET_VERSION` you set yourself is left alone: the hook only
ever manages the value it set.

:::caution
An override the nearest `global.json` does not accept makes `dotnet` refuse to
start, because dv runs an install root that holds only the overriding SDK.
`dv doctor` reports exactly this case.
:::
