---
title: Getting started
description: Install dv, get an SDK, and put the shims on PATH.
banner:
  content: 'dv is pre-release: until 1.0, commands and file formats may still change between versions.'
---

## Install dv

```sh
curl -fsSL https://raw.githubusercontent.com/adrum/dv/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/adrum/dv/main/install.ps1 | iex"
```

Either script downloads the build for your platform, verifies it against the
published sha256, and puts `dv` in `~/.local/bin` (override with `DV_BIN_DIR`).
Neither touches your shell profile or `PATH` — that line is yours to paste,
below.

From source instead:

```sh
cargo build --release
install -m 0755 target/release/dv /usr/local/bin/dv
```

## Get an SDK

```sh
dv install lts          # newest long-term-support SDK
dv install 10.0         # newest 10.0.x
dv install 10.0.4xx     # newest in the 10.0.400 feature band
dv install 10.0.402     # exactly this one
dv install              # whatever global.json here asks for
```

Previews install the same way — `dv install 11` installs the newest 11
preview while 11 has not shipped. Once a release exists, lines prefer it.

## Put the shims on PATH

```sh
eval "$(dv init zsh)"   # or bash, sh, fish
```

Add that to your shell profile. It does two things:

- **prepends** dv's shim directory to `PATH`, which matters: a shim directory
  placed after `/usr/local/share/dotnet` or a package manager's prefix is
  silently shadowed by whatever `dotnet` is already there;
- sets `DOTNET_ROOT` to dv's combined install root, so global tools and apps
  built with an app host find a runtime dv installed.

Then open a new shell — or run `hash -r`, because shells cache command
locations and yours may still remember a `dotnet` from before dv existed.

On Windows, add this to your PowerShell `$PROFILE` instead:

```powershell
dv init powershell | Out-String | Invoke-Expression
```

`cmd`, editors and anything started outside PowerShell read the user `PATH`,
so put `%LOCALAPPDATA%\dv\shims` at the front of that as well, and set
`DOTNET_ROOT` to `%LOCALAPPDATA%\dv\dotnet`, then open a new terminal.

```sh
dv doctor
```

Doctor confirms it worked and says plainly what to fix when it did not.

## Pin an SDK for a project

```sh
cd ~/code/legacy-app
dv pin 8.0
dotnet --version        # 8.0.x, here and below
```

`dv pin` writes `global.json` — the file `dotnet`, Visual Studio, Rider and CI
images already read — and keeps anything else in it. Commit it, and everyone
on the project gets the same SDK. [How a version is chosen](/dv/guides/resolution/)
explains what `8.0` turns into.

## Run something under the resolved SDK

```sh
dv run dotnet-ef migrations add Init
```

`dv run` finds the command in the resolved SDK, then among global tools, then
on `PATH`, and points it at the resolved SDK's install root. `dotnet` and
`dnx` need nothing special: their shims already do this.

## Where dv keeps things

Everything lives in one directory — `$XDG_DATA_HOME/dv`, else
`~/.local/share/dv`, or `%LOCALAPPDATA%\dv` on Windows; set `DV_HOME` to put
it elsewhere. Removing dv is deleting that directory and the binary.
