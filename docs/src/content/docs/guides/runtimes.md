---
title: Runtimes
description: Install a .NET or ASP.NET Core runtime without an SDK.
---

Most machines never need this. Every SDK brings the runtimes it was built
with, and dv links **every installed runtime into every install root** — so
SDK 10 builds and runs an app targeting `net8.0` as soon as SDK 8 is
installed too.

Runtime installs are for the rest: an app that targets a runtime no installed
SDK carries, a server that runs apps and builds none, a test matrix across
runtimes.

```sh
dv runtime install 8.0                  # the .NET runtime, newest 8.0
dv runtime install aspnetcore@8.0       # ASP.NET Core (includes the .NET runtime)
dv runtime install windowsdesktop@10    # Windows only
dv runtime install aspnetcore@8.0.12    # exactly this one
```

The spelling is `component@version`, as in Microsoft's dotnetup; a bare
version means the base .NET runtime. Components are `runtime`, `aspnetcore`
(or `aspnet`) and `windowsdesktop` (or `desktop`). Versions are lines,
`latest`, `lts`, or exact — runtimes have no feature bands.

## What you get

The archive Microsoft publishes, verified the same way as an SDK, extracted
untouched into `runtimes/aspnetcore-8.0.12/`, and linked into every install
root. An app built for `net8.0` by SDK 10 then runs on it:

```sh
$ dv runtime install aspnetcore@8.0
installed ASP.NET Core runtime 8.0.32
$ dotnet bin/Debug/net8.0/web.dll      # .NET 8.0.32 / ASP.NET Core 8
```

## Listing

```sh
dv runtime list            # installed on their own, and those that came with SDKs
dv runtime list --remote   # …and what can be installed (--all for EOL channels)
```

## Updating and removing

`dv runtime install` tracks what it was asked for, so `dv update` moves
`aspnetcore@8.0` to the next 8.0 patch and removes the one it replaced —
exactly as it does for SDK lines. An exact version is a pin.

```sh
dv runtime uninstall aspnetcore@8.0
```

## A machine with no SDK

With only runtimes installed, there is no SDK to resolve, so the `dotnet` shim
runs the combined install root: `dotnet app.dll` works, `dotnet build` says
there is no SDK. Apps started directly find their runtime through
`DOTNET_ROOT`, which `dv init` points at that same root.
