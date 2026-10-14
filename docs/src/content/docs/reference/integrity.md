---
title: Integrity
description: What is verified, what that buys, and what dv checks after installation.
---

## Verification

dv installs Microsoft's archives and nothing else. Microsoft publishes a
sha512 for every file in its release metadata, and dv checks it while the
download streams, before anything is extracted. Verification **fails
closed**: a file with a missing, empty or malformed hash is an error, not a
reason to skip the check.

A download lands on a `.part` file and is only renamed into place once
complete, so an interrupted download is never mistaken for a finished one. A
cached archive is reused only if it still hashes to the published value.

## Extraction

An archive is untrusted input even when its hash checks out. dv refuses any
entry with an absolute path or `..`, and any link — SDK archives contain only
plain files and directories, and a link could point a later entry outside
the destination. The new tree is swapped into place only after it unpacks
completely and contains the `dotnet` host and the SDK it is named for, so a
failed install leaves the previous one untouched.

## What that actually buys

The hash protects the archive **given trustworthy metadata**, which dv fetches
over TLS from Microsoft. Microsoft also signs its release metadata; dv does not
verify those signatures yet, and saying so is more useful than implying
otherwise.

## After installation

`dv install` records the source URL, the sha512, and the sha256 of the files
that decide what runs — the `dotnet` host, `hostfxr`, and the SDK's
`dotnet.dll` — next to the extracted tree. `dv doctor` reports a tree that
changed since it was installed:

```
problem 10.0.402: changed since install — host/fxr/10.0.13/libhostfxr.dylib
        reinstall with `dv install 10.0.402 --force`
```

Not every file is hashed: an SDK is thousands of them, and a check that takes
minutes is one nobody runs.

## dv itself

`install.sh`, `install.ps1` and `dv self update` verify dv's own release
archive against the `.sha256` published beside it, with the same fail-closed
rule.

## Licensing

dv is MIT. The SDKs it installs are Microsoft's, under Microsoft's licenses,
which ship inside each SDK (`LICENSE.txt`, `ThirdPartyNotices.txt`).
