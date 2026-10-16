---
title: Scripting dv
description: Exit codes and JSON output, for callers that are programs rather than people.
---

dv is meant to be driven by other software — a GUI listing versions, a CI
script, an editor plugin. Two things make that reliable: exit codes that
distinguish *why* something failed, and output that does not change meaning
when a sentence gets reworded.

## Exit codes

```sh
dv resolve >/dev/null 2>&1
case $? in
  0)  ;;                  # fine
  10) dv install ;;       # pinned, not installed: install what global.json asks for
  13) echo "network down, retry later" ;;
  15) sleep 5 ;;          # another dv is installing, retry
  *)  echo "dv failed" ;;
esac
```

| Code | Meaning | What a caller should do |
|---|---|---|
| `0` | Success | — |
| `1` | Any failure without a more specific code | Report it |
| `2` | Bad arguments | Fix the invocation |
| `10` | Resolved a version, but it is not installed | Install it |
| `11` | Nothing installed, and nothing to fall back to | Install anything |
| `12` | No such version is published for this platform | Pick another; `dv list --remote` |
| `13` | Network — connection, DNS, TLS, or a bad response | Retry later |
| `14` | Verification — a checksum mismatched or was missing | **Stop.** Do not retry blindly |
| `15` | Another dv process holds the lock | Retry shortly; nothing is wrong |

These numbers are interface. They are asserted as literals in dv's tests, so
changing one fails the build rather than quietly breaking you. Anything without
a specific code exits `1`; treat `1` as "unknown", not as a stable category.

## JSON output

`--format json` works on `install`, `uninstall`, `list`, `pin`, `default`,
`which`, `resolve` and `doctor`.

```sh
$ dv resolve --format json
{
  "version": "10.0.402",
  "binary": "/Users/you/.local/share/dv/roots/10.0.402/dotnet",
  "root": "/Users/you/.local/share/dv/roots/10.0.402",
  "source": "version-file",
  "source_path": "/Users/you/code/app/global.json",
  "pinned": true
}
```

`source` answers "why am I getting this version?" without a second command.
Its values are stable identifiers rather than the prose a terminal shows:
`environment`, `version-file`, `default`, `newest-installed`.

### Installed and installable, in one call

```sh
$ dv list --remote --format json
{
  "platform": "osx-arm64",
  "available": [
    {
      "version": "10.0.402",
      "installed": true,
      "prerelease": false,
      "channel": "10.0",
      "support_phase": "active",
      "release_type": "lts",
      "eol_date": "2028-11-14",
      "release_date": "2026-10-13",
      "runtime_version": "10.0.13"
    }
  ],
  "installed": [
    { "version": "10.0.402", "active": true, "path": "…/roots/10.0.402" }
  ],
  "active": { "version": "10.0.402", "source": "newest-installed", "source_path": null }
}
```

One call rather than two, because a version picker needs both, and two calls
can disagree about an SDK installed in between. `available` is newest first and
carries **every** patch of the channels fetched — supported channels plus any
channel something installed belongs to; `--all` adds end-of-life ones. The
newest-two-per-band trimming applies only to the terminal listing. `dv list
--format json` gives `installed` and `active` without touching the network.

### How fresh `--remote` is

Release metadata is cached locally. `dv list --remote` uses a copy younger
than **10 minutes** without touching the network, so a picker that calls it on
every visit opens instantly. An older copy is revalidated with a conditional
request — one small round trip when nothing changed. `--refresh` revalidates
now. `dv install` always revalidates, so a patch released a minute ago
installs.

When the network is unreachable, both fall back to the cached copy and say so
on stderr; stdout is unaffected. Only a machine that has never fetched the
metadata fails, with exit code `13`.

`metadata_checked_at` in the output is when the oldest metadata behind the
answer was last confirmed with Microsoft, in Unix seconds — enough to show
"updated 4 minutes ago".

### Showing a picker instantly

A GUI should never wait on the network to draw. `--cached` answers from the
cache, however old, and never makes a request — about 10 ms:

```sh
dv list --remote --cached --format json   # draw this now
dv list --remote --format json            # then, in the background; redraw if it differs
dv list --remote --refresh --format json  # on an explicit refresh
```

That is stale-while-revalidate, with the background work in the process
built for it rather than in a dv process left running after the command
returns. `--cached` exits `13` when something it needs has never been cached
— for example the first run, or `--all` before EOL channels were ever
listed — which is the signal to wait for a normal call instead.

### The rule that makes it usable

**Data on stdout, diagnostics on stderr, always.** Progress, warnings and
errors never land in stdout, so `dv … --format json 2>/dev/null` is always
either valid JSON or nothing. Under `--format json`, dv also suppresses the
human-facing chatter entirely.

`dv resolve --pinned-only --format json` prints `null` rather than nothing when
no file or environment variable pins a version, so a caller can parse
unconditionally.

## Redirecting state

Set `DV_HOME` to keep everything somewhere else — a wrapper's own state
directory, or a CI cache. Nothing dv writes lives outside it, apart from the
binary itself.
