# Release artifact signing

AgentOS release binaries are signed with [minisign](https://jedisct1.github.io/minisign/).
This directory holds the **public** verification key; the secret key lives only
in CI as the `MINISIGN_SECRET_KEY` GitHub Actions secret (never committed).

Owned by Phase 08 (security sign-off & supply chain); consumed by Phase 06
(`scripts/install.sh`, the Homebrew formula) and Phase 09 (release pipeline).

## One-time key generation (maintainer)

```bash
# Generate a PASSWORDLESS release keypair (-W) so CI can sign non-interactively.
# (The secret key is protected by being a CI secret, not by a password.)
minisign -G -W -p packaging/signing/agentos-release.pub -s /tmp/agentos-release.key

# Commit ONLY the public key:
git add packaging/signing/agentos-release.pub

# Add the SECRET key to CI (paste the file contents):
gh secret set MINISIGN_SECRET_KEY < /tmp/agentos-release.key
# Then securely destroy the local copy:
shred -u /tmp/agentos-release.key   # or: rm -P on macOS
```

## How verification works

- CI signs each artifact in `.github/workflows/release.yml`:
  `minisign -S -s <key> -m <artifact> -x <artifact>.sig`. A missing
  `MINISIGN_SECRET_KEY` fails the release leg; unsigned artifacts are never published.
- `scripts/install.sh` pins the public key inline (it must equal line 2 of
  `agentos-release.pub`), requires the `<artifact>.sig` asset, and verifies with
  `minisign -V -P <key>` or `rsign verify -P <key>` before installing. It refuses
  to install on a missing or bad signature. If neither verifier is installed it
  warns and falls back to the mandatory SHA-256 check.
- `minisign` is not packaged for Ubuntu 22.04; `cargo install rsign2` produces and
  verifies the same key and signature format.

## Rotating the key

Generate a new keypair, replace `agentos-release.pub` and the pinned `PUBKEY` in
`scripts/install.sh`, update the
`MINISIGN_SECRET_KEY` secret, and announce the new key fingerprint in the
release notes. Old releases remain verifiable with the old key from git history.
