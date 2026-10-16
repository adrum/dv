---
title: Integrity
description: What is verified, what that buys, and what dv checks after installation.
---

## Verification

dv installs Microsoft's archives and nothing else, and checks two things
before it trusts one.

**The release metadata is Microsoft's.** Every metadata file dv reads —
`releases-index.json` and each channel's `releases.json` — has a detached
CMS (PKCS #7) signature published beside it. dv verifies it before using or
caching the file, in its own code, on every platform:

- the signature covers exactly the bytes dv is about to use — one changed
  byte fails it
- the signer chains, signature by signature, to **DigiCert Trusted Root G4**,
  which is built into dv rather than read from the system
- the signer is Microsoft Corporation, with a code-signing certificate
- the expiration Microsoft signs into each file has not passed. Signed
  metadata is good for about 90 days, and a server handing out a file past
  that is refused: correctly signed but stale is what a replay of old
  metadata, hiding newer releases, would look like

**The archive is the one that metadata describes.** Each archive is checked
against the sha512 in the verified metadata while it downloads, before
anything is extracted.

Both fail closed. Unsigned metadata, a signature that does not verify, a
missing, empty or malformed hash, or a mismatch all stop the install with exit
code `14` — never a reason to skip the check.

Cached metadata is verified again every time it is read, so a cache edited by
hand is ignored rather than believed. Offline, a cached copy past its
expiration is still used, with a warning that newer releases may exist.

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

## What is not checked

**Revocation.** dv does not consult certificate revocation lists or OCSP,
which would mean extra network requests on every listing. A revoked Microsoft
signing certificate would still verify until a dv release stops trusting it.

**Certificate dates.** The signed expiration is the freshness bound instead;
Microsoft signs within its certificates' lifetimes.

**Mirrors must carry the signatures.** `DV_RELEASES_URL` can point at a
mirror, but it has to serve the `.p7s` files beside the metadata — dv has no
option to skip verification.

If Microsoft ever signs with a certificate that does not chain to the root
dv carries, verification fails with exit code `14` and says so; `dv self
update` brings a dv that knows the new root.

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
