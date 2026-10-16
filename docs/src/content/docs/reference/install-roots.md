---
title: How dv runs an SDK
description: Install roots, hard links, and why dv does not just hand everything to the dotnet host.
---

## The constraint

A .NET install root holds one `dotnet` host beside side-by-side SDKs,
runtimes and packs:

```
dotnet
host/fxr/10.0.13/                     the resolver
sdk/8.0.426/  sdk/10.0.402/           SDKs
shared/Microsoft.NETCore.App/8.0.32/  runtimes
packs/Microsoft.NETCore.App.Ref/…     targeting packs
```

The host chooses among them per command. That is right for runtimes — an app
built for `net8.0` needs the 8.0 runtime even when SDK 10 built it — and
limiting for SDKs, because the host's choice only listens to `global.json`.
"Use 8.0 unless something says otherwise", "use 9.0 for this one command",
and "not the preview" cannot be said to it at all.

## Two kinds of root

dv keeps each SDK archive extracted, untouched, in `versions/`, and composes
install roots out of them:

- **`roots/<sdk>/`** exposes **exactly one SDK**, and every host, runtime and
  targeting pack from every installed SDK — and every runtime installed on its
  own with `dv runtime install`, kept in `runtimes/`. The shim runs this root's
  `dotnet`, so the SDK dv resolved is the only one the host can choose — and
  any installed runtime can run what it builds.
- **`dotnet/`** exposes everything, like a system-wide install. `DOTNET_ROOT`
  names it, for editors and global tools that start outside dv.

Every root uses the newest installed host: a newer host understands every
older SDK, and the reverse is not true. Each SDK keeps its own templates and
workload manifests.

## Why hard links

The roots are built from **hard links**, file by file — not copies, and not
symlinks.

Symlinks were tried first and fail quietly: the SDK locates its install root
from its own *resolved* path, so a symlinked `sdk/10.0.402` leads it back
into `versions/10.0.402`, where only its own targeting packs exist. It then
downloads the rest from NuGet and hands the apps it launches the wrong
`DOTNET_ROOT`. A hard link has no "real" path to escape to.

Hard links also cost no disk — a root is directory entries pointing at the
same files — and need no special privilege on Windows, where symlinks do.

Each entry is linked beside its final location and renamed into place, so a
`dotnet` that starts mid-update never sees a half-linked SDK.

## What dv owns in a root

Each root records the entries dv placed in it (`.dv-root.json`). dv only ever
adds or removes recorded entries, and removes only the files the SDK archive
shipped — never anything else written there. The SDK writes into its root:
workloads add packs, metadata and new manifest versions, some of them inside
directories dv linked. Those survive `dv rehash`, other installs, and
`dv install --force`. See [tools and workloads](/dv/guides/tools-and-workloads/).

## What runs, exactly

`dotnet` on `PATH` is a shim:

```sh
#!/bin/sh
# managed-by: dv
exec '/path/to/dv' run dotnet "$@"
```

`dv run dotnet` resolves the version, sets `DOTNET_ROOT` (and
`DOTNET_ROOT_<ARCH>`) to that SDK's root, and replaces itself with the root's
`dotnet` — `exec`, so signals, exit codes and job control belong to `dotnet`.
On Windows, which has no `exec`, dv waits and exits with `dotnet`'s code.

Every shim carries the `managed-by: dv` marker, and dv never removes a file
without it.
