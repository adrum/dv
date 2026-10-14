---
title: global.json
description: What dv reads from global.json, how rollForward picks an SDK, and what dv pin writes.
---

`global.json` is .NET's own way to choose an SDK, read by the `dotnet` host,
Visual Studio, Rider and CI images. dv reads it the same way and writes it
with `dv pin`; there is no other version file.

```json
{
  "sdk": {
    "version": "10.0.100",
    "rollForward": "latestFeature",
    "allowPrerelease": false
  }
}
```

## Finding it

The host reads the **nearest** `global.json`, walking up from the current
directory. A nearer file without `sdk.version` still ends the search — it does
not defer to a parent's pin — and dv behaves the same.

## rollForward

An SDK version is `major.minor.patch`, where the hundreds digit of the patch
is the **feature band**: 10.0.402 is patch 2 of the 10.0.4xx band. Policies
are written in those terms. Every candidate must be at least `sdk.version`.

| Policy | Accepts | Picks |
|---|---|---|
| `disable` | exactly `sdk.version` | that |
| `patch` (the default) | the same feature band | `sdk.version` if installed, else the newest patch in the band |
| `latestPatch` | the same feature band | the newest patch |
| `feature` | the same major.minor | the lowest band, then its newest patch |
| `latestFeature` | the same major.minor | the newest |
| `minor` | the same major | the lowest minor and band, then its newest patch |
| `latestMinor` | the same major | the newest |
| `major` | anything newer | the lowest major, minor and band, then its newest patch |
| `latestMajor` | anything newer | the newest |

The non-`latest` policies prefer the *lowest* matching band — closest to what
was asked for — and the newest patch within it. This is how the host's
resolver behaves; it is not what the policy names suggest, and dv follows the
resolver.

`allowPrerelease` defaults to `true`, and a prerelease `sdk.version` always
allows prereleases whatever the file says.

## What dv pin writes

| You run | `sdk.version` | `rollForward` | Meaning |
|---|---|---|---|
| `dv pin 10.0.402` | `10.0.402` | `disable` | exactly this SDK |
| `dv pin 10.0.4xx` | `10.0.400` | `latestPatch` | any patch in the 10.0.400 band |
| `dv pin 10.0` | `10.0.100` | `latestFeature` | any 10.0 SDK |
| `dv pin 10` | `10.0.100` | `latestMinor` | any 10.x SDK |
| `dv pin lts` | the newest installed LTS major | `latestMinor` | that major |

`dv pin latest` is refused: without a pin, dv already uses the newest SDK.

When only a preview of a line is installed — `dv pin 11.0` before 11 ships —
the pin is written from the preview's version, because a preview sorts below
its line's first release and `11.0.100` would exclude it.

Pinning keeps everything else in the file — `msbuild-sdks`, `test`, other
`sdk` keys — and its key order. Comments, which `global.json` allows, cannot
survive a rewrite; `dv pin` says so when it drops one. Pinning in a directory
below another `global.json` creates a new file that shadows the parent's, and
`dv pin` says that too.

## What dv does not read

- **`sdk.paths`** — custom SDK search directories. The host honors them and
  may find an SDK there instead of the one dv resolved; `dv doctor` warns.
- **Project files.** `<TargetFramework>` decides what a project compiles for,
  not which SDK builds it, and the host does not read it to choose an SDK.
