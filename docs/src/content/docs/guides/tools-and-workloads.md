---
title: Tools and workloads
description: dnx, local and global tools, and SDK workloads under dv.
---

dv shims two commands: **`dotnet`** and **`dnx`** — the only executables an
SDK archive puts at its top level. Everything else .NET runs goes through
`dotnet`, so those two cover it.

| What | How you run it | Through dv |
|---|---|---|
| `build`, `test`, `run`, `watch`, `format`, … | `dotnet <verb>` | the `dotnet` shim |
| Local tools (`.config/dotnet-tools.json`) | `dotnet tool run x`, `dotnet x` | the `dotnet` shim |
| One-off tools | `dnx <package>` | the `dnx` shim (SDK 10 and later) |
| Global tools | `dotnet-ef`, `csharpier`, … | `DOTNET_ROOT`, or `dv run` |
| Workloads | `dotnet workload install …` | the `dotnet` shim |

## dnx

`dnx` ships with SDK 10 and later. It is a small script that runs the
`dotnet` beside it, so through the shim it runs from the resolved SDK's root
and follows the same pin as `dotnet`. That matters most where `dnx` is most
used: MCP server and editor configurations (`"command": "dnx"`), which are
started by programs that inherit `PATH` and nothing else.

In a directory pinned to an SDK older than 10, `dnx` fails rather than
borrowing a newer SDK's — running a different SDK than the pin asks for is
exactly what a pin exists to prevent.

## Global tools

`dotnet tool install -g` puts tools in `~/.dotnet/tools` (under
`DOTNET_CLI_HOME` if that is set). Each is an app host — a small launcher that
finds a runtime through `DOTNET_ROOT`. `dv init` sets `DOTNET_ROOT` to dv's
combined root, which holds every runtime dv installed, so global tools work
straight from `PATH`. Add `~/.dotnet/tools` to `PATH` if your shell does not
already; `dv doctor` says so when it is missing.

`dv run dotnet-ef …` works too: dv looks among global tools before `PATH`, and
points the tool at the resolved SDK's root rather than the combined one.

Global tools are not shimmed. A global tool belongs to no SDK version, and
shimming each one would mean dv tracking what you install with `dotnet tool`.

## Workloads

Workloads — `wasm-tools`, `maui`, `android`, `ios` and the rest — install
through the shim like any other `dotnet` command:

```sh
dotnet workload install wasm-tools
dotnet workload list
```

They are installed into the **resolved SDK's install root**, the same place a
system-wide .NET would put them, and dv keeps them there:

- dv's pristine copy of the SDK in `versions/` is never written to. Workload
  installs and updates add files beside dv's; they do not change any file dv
  linked.
- `dv rehash`, installing other SDKs, and `dv install <version> --force` all
  leave installed workloads and their updated manifests in place. dv removes
  only what it placed, file by file.
- Uninstalling an SDK removes its install root, and with it that SDK's
  workloads — as uninstalling it system-wide would.

Workloads are per feature band in .NET, and per SDK root in dv: install one
under the SDK that builds the project.

:::caution[Not yet in the combined root]
A workload installed through the shim is visible to that SDK's root only — not
to the combined root `DOTNET_ROOT` names. An editor that builds through
`DOTNET_ROOT` rather than through `dotnet` on `PATH` will not see it. Point the
editor at the `dotnet` shim, or at the SDK's root (`dv resolve --format json`
prints it), until this is addressed.
:::

:::note[Windows]
On Windows, the SDK can install workloads as MSI packages into
`C:\Program Files\dotnet` rather than into the root it runs from. dv's roots
are user-owned and outside Program Files, which should keep workloads
file-based, but this has not been verified yet.
:::
