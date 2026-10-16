---
title: Commands
description: Every dv command.
---

A version is written `10`, `10.0`, `10.0.4xx`, `10.0.402` (or a preview such
as `11.0.100-rc.2.26504.105`), `latest` or `lts`. Lines prefer releases and
fall back to previews when nothing else matches.

## Versions

| | |
|---|---|
| `dv install <version>` | install an SDK (`--force` reinstalls) |
| `dv install` | install what the nearest `global.json` asks for |
| `dv uninstall <version>` | remove an installed SDK |
| `dv list` | installed SDKs, the active one marked with its source |
| `dv list --remote` | what can be installed, beside what is — newest 2 per feature band |
| `dv list --remote --all` | …including end-of-life channels and every patch |
| `dv list --remote --refresh` | …checking for new releases now rather than using metadata cached in the last 10 minutes |

Older patches stay installable by exact version whether or not the listing
shows them. The trimming is cosmetic, because an exact pin gets committed.

## Choosing

| | |
|---|---|
| `dv pin <version>` | write `global.json` in the current directory |
| `dv default [<version>]` | show or set the SDK used when nothing pins one |
| `dv resolve` | print the SDK version that would be used here |
| `dv resolve --source` | …and what chose it |
| `dv resolve --pinned-only` | print nothing unless `global.json` or the environment pins one |

## Running

| | |
|---|---|
| `dv which [command]` | the binary that would run here, and nothing else |
| `dv run <command> …` | run a command under the resolved SDK |
| `dv exec <command> …` | alias of `run` |

`run` looks in the resolved SDK's install root first (`dotnet`, `dnx`), then
among global tools, then on `PATH`. Whatever it runs gets `DOTNET_ROOT` set to
the resolved root. A command from outside the root also gets the root first on
`PATH` and `DV_DOTNET_VERSION` set, so the tool and anything it starts agree
on one SDK.

## Setup

| | |
|---|---|
| `dv init <shell>` | `PATH` and `DOTNET_ROOT` setup — zsh, bash, sh, fish, powershell |
| `dv init <shell> --hook` | …plus the optional `cd`-time switch |
| `dv rehash` | rebuild the shims and install roots |
| `dv doctor` | check the installation, exit non-zero on a real problem |

## Maintenance

| | |
|---|---|
| `dv cache status` | what the download cache holds |
| `dv cache prune` | remove archives no installed SDK came from |
| `dv cache clear` | remove every cached archive, and the release metadata cache |
| `dv self update` | replace this binary with the newest published build |

## Environment

| | |
|---|---|
| `DV_DOTNET_VERSION` | override resolution entirely |
| `DV_HOME` | where dv keeps state |
| `DV_RELEASES_URL` | where to read the .NET release index — for a mirror |
| `DV_PLATFORM` | override the detected runtime identifier |

## Layout

Everything lives in one directory — `$XDG_DATA_HOME/dv`, else
`~/.local/share/dv`, or `%LOCALAPPDATA%\dv` on Windows — and every file dv
writes is plain text you can read and grep:

```
~/.local/share/dv/
  versions/10.0.402/      SDK archives, extracted exactly as published
  roots/10.0.402/         install root: this SDK, every runtime (hard links)
  dotnet/                 install root holding everything — DOTNET_ROOT
  shims/dotnet            real files on PATH
  cache/                  downloaded archives
  cache/metadata/         Microsoft's release metadata, cached
  config.toml             default version, release index location
```
