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
  `minisign -S -s <key> -m <artifact> -x <artifact>.sig`
- The installer downloads `<artifact>.sig` + this public key and runs
  `minisign -V -p agentos-release.pub -x <artifact>.sig -m <artifact>` before
  installing — it refuses to install on signature failure.
- Until the public key is committed and CI signs, the installer verifies the
  SHA-256 checksum and reports the signature step as skipped.

## Rotating the key

Generate a new keypair, replace `agentos-release.pub`, update the
`MINISIGN_SECRET_KEY` secret, and announce the new key fingerprint in the
release notes. Old releases remain verifiable with the old key from git history.
