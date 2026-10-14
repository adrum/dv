# dv

A fast .NET SDK version manager. `dv` installs Microsoft's official .NET SDKs
in seconds — verified, side by side, no admin rights — and resolves which one
a command runs under from the directory you are standing in.

```sh
dv install 10.0         # newest 10.0 SDK, or 10, 10.0.4xx, 10.0.402, lts
dv pin 8.0              # writes global.json here
dotnet --version        # 8.0.426
```

## dv uses global.json

There is no new version file. `global.json` is what the `dotnet` host,
Visual Studio, Rider and CI images already read, so it is what `dv pin` writes
and what dv resolves from — applying `rollForward` with the same rules as the
host's own resolver, ported from its source.

dv does not replace NuGet or `dotnet tool`. It is the SDK half: getting the
right SDKs onto the machine and pointing `dotnet` at the right one.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/adrum/dv/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/adrum/dv/main/install.ps1 | iex"
```

Either downloads the build for your platform, verifies it against the
published sha256, and puts `dv` in `~/.local/bin` (override with `DV_BIN_DIR`).
Neither touches your shell profile — that line is yours to paste, below.

Or from source:

```sh
cargo build --release
install -m 0755 target/release/dv /usr/local/bin/dv
```

Then put the shims in front of everything else on `PATH`:

```sh
eval "$(dv init zsh)"   # or bash, sh, fish, powershell — add this to your shell profile
```

`dv doctor` confirms it worked, and says plainly what to fix when it did not.

## Commands

| | |
|---|---|
| `dv install [version]` | install `10`, `10.0`, `10.0.4xx`, `10.0.402`, `latest` or `lts` — or, with no version, what `global.json` here asks for |
| `dv uninstall <version>` | remove an installed SDK |
| `dv list` | installed SDKs, active one marked |
| `dv list --remote` | what can be installed, beside what is (`--all` for EOL channels and every patch) |
| `dv pin <version>` | write `global.json` here |
| `dv default <version>` | SDK used when nothing pins one |
| `dv which [command]` | the binary that would run here, and nothing else |
| `dv run <command> …` | run `dotnet`, `dnx`, a global tool or anything on `PATH` under the resolved SDK |
| `dv resolve` | the SDK that would be used here (`--source` says why) |
| `dv init <shell>` | shell setup for `PATH` and `DOTNET_ROOT` (`--hook` adds `cd`-time switching) |
| `dv rehash` | rebuild the shims and install roots |
| `dv cache status\|prune\|clear` | inspect or clear downloaded archives |
| `dv doctor` | check the installation |
| `--format json` | machine-readable output for `install`, `list`, `pin`, `resolve`, `which`, `doctor`, … |
| `dv self update` | replace this binary with the newest build |

## How a version is chosen

First match wins:

1. `DV_DOTNET_VERSION`
2. the nearest `global.json`, when it names an SDK version — read with the
   `dotnet` host's own `rollForward` rules
3. the version set with `dv default`
4. the newest installed release (a preview only when nothing else is
   installed)

`dv which` and `dv resolve` are read-only: they resolve and print, and never
install, write or touch the working directory. When nothing pins a version, dv
falls back to the newest installed SDK rather than failing.

## Every runtime, for every SDK

An app built for `net8.0` needs the 8.0 runtime even when SDK 10 builds it.
dv runs each SDK from an install root holding **that SDK and every installed
runtime and targeting pack**, so a project targeting `net8.0;net10.0` builds
and runs on both with nothing extra to install. The roots are hard links, so
they cost no extra disk.

```
~/.local/share/dv/        (DV_HOME; %LOCALAPPDATA%\dv on Windows)
  versions/10.0.402/      SDK archives, extracted exactly as published
  roots/10.0.402/         this SDK + every runtime — what the shim runs
  dotnet/                 everything — what DOTNET_ROOT names
  shims/dotnet            real files on PATH
  cache/                  downloaded archives
  config.toml             default version, release index location
```

Shims are real files rather than a shell hook because `dotnet` has to resolve
correctly *outside* an interactive shell — editors, language servers, CI
runners and git hooks inherit `PATH` and nothing else. Each shim carries a
`# managed-by: dv` marker, and dv never removes a file it did not write.

## New releases

dv has no built-in list of versions. It reads Microsoft's release metadata
when you install or ask for `--remote`, so every SDK, preview and patch
Microsoft publishes is installable the day it ships, with no dv update.

## Integrity

Every archive is checked against the sha512 Microsoft publishes before it is
extracted, and verification **fails closed** — a missing or malformed hash is
an error, not a reason to skip the check. Extraction refuses absolute paths,
`..` and links. The host, `hostfxr` and SDK entry point are hashed after
install, so `dv doctor` can report a tree that changed. Microsoft's metadata
signatures are not verified yet.

## Platforms

macOS, Linux (glibc and musl) and Windows, on x64 and Arm64 — anywhere
Microsoft publishes an SDK. The client is a single Rust binary with no runtime
dependencies.

## Documentation

Full docs at **https://adrum.github.io/dv** — guides, the command reference,
`global.json` and `rollForward` in detail, and how install roots work.

The site lives in `docs/` (Astro + Starlight):

```sh
cd docs && npm install && npm run dev
```

## License

MIT — see [`LICENSE`](LICENSE). The SDKs dv installs are Microsoft's, under
Microsoft's licenses, which ship inside each SDK.

## No telemetry

`dv` reports nothing anywhere. It talks to the network when you install,
update, or ask for `--remote`, and not otherwise. The SDKs it installs have
their own telemetry, which `DOTNET_CLI_TELEMETRY_OPTOUT=1` turns off.
