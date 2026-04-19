---
title: Phase 9 — Distribution & Release Engineering
tags:
  - release
  - distribution
  - signing
  - docker
  - homebrew
  - phase-9
date: 2026-04-18
status: planned
effort: 1.5d
priority: high
---

# Phase 9 — Distribution & Release Engineering

> Signed binaries for five platform triples, published to GitHub Releases. Homebrew tap and Docker image on top. SBOM and reproducible builds. This is what users actually download.

---

## Why this phase

A production daemon is distributed software. Users must trust what they install came from us, wasn't modified in transit, and is reproducibly built from the public source. Phase 9 is where we commit to that standard.

---

## Deliverables

- Per-release signed artifacts in GitHub Releases for 5 target triples:
  - `x86_64-unknown-linux-musl`
  - `aarch64-unknown-linux-musl`
  - `x86_64-apple-darwin`
  - `aarch64-apple-darwin`
  - `x86_64-pc-windows-msvc`
- `cosign`-signed checksums + SLSA provenance for every artifact.
- SBOM (`cyclonedx` or SPDX) per release.
- Homebrew tap: `<org>/homebrew-tap/gmail-mcp.rb`.
- Docker image: `ghcr.io/<org>/gmail-mcp:<version>` (for HTTP transport deployments from Phase 8).
- `cargo install gmail-mcp` works from crates.io.
- Changelog + release notes generation (via `git-cliff` or similar).

---

## GitHub Actions release workflow

Fleshed-out version of the Phase 1 skeleton:

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags: ["v*"]

permissions:
  contents: write
  id-token: write     # cosign keyless OIDC

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        include:
          - { os: ubuntu-latest,  target: x86_64-unknown-linux-musl,   cross: true  }
          - { os: ubuntu-latest,  target: aarch64-unknown-linux-musl,  cross: true  }
          - { os: macos-14,       target: x86_64-apple-darwin,         cross: false }
          - { os: macos-14,       target: aarch64-apple-darwin,        cross: false }
          - { os: windows-latest, target: x86_64-pc-windows-msvc,      cross: false }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: ${{ matrix.target }} }
      - uses: Swatinem/rust-cache@v2
      - name: Install musl-cross if needed
        if: matrix.cross
        run: cargo install cross --locked
      - name: Build
        run: |
          if ${{ matrix.cross }}; then
            cross build --release --target ${{ matrix.target }}
          else
            cargo build --release --target ${{ matrix.target }}
          fi
      - name: Package
        shell: bash
        run: |
          name=gmail-mcp-${{ github.ref_name }}-${{ matrix.target }}
          mkdir -p dist/$name
          bin=gmail-mcp${{ startsWith(matrix.target, 'x86_64-pc-windows') && '.exe' || '' }}
          cp target/${{ matrix.target }}/release/$bin dist/$name/
          cp LICENSE-APACHE LICENSE-MIT README.md SECURITY.md dist/$name/
          cd dist
          if [[ "${{ matrix.target }}" == *"windows"* ]]; then
            7z a $name.zip $name
          else
            tar czf $name.tar.gz $name
          fi
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: dist/*.tar.gz dist/*.zip

  sbom:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-cyclonedx --locked
      - run: cargo cyclonedx -f json
      - uses: actions/upload-artifact@v4
        with: { name: sbom, path: "**/*.cdx.json" }

  sign-and-publish:
    needs: [build, sbom]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with: { path: artifacts/ }
      - uses: sigstore/cosign-installer@v3
      - name: Compute checksums
        run: |
          find artifacts -type f \( -name "*.tar.gz" -o -name "*.zip" \) -exec sha256sum {} \; > SHA256SUMS
      - name: Sign checksums (keyless OIDC)
        run: cosign sign-blob --yes SHA256SUMS > SHA256SUMS.sig
      - name: SLSA provenance
        uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v2.0.0
        with:
          base64-subjects: "${{ steps.digest.outputs.base64 }}"
      - uses: softprops/action-gh-release@v2
        with:
          files: |
            artifacts/**/*.tar.gz
            artifacts/**/*.zip
            SHA256SUMS
            SHA256SUMS.sig
            artifacts/sbom/*.cdx.json
          generate_release_notes: true

  crates-io:
    needs: sign-and-publish
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: |
          cargo publish -p gmail-mcp-core --token ${{ secrets.CRATES_IO_TOKEN }}
          sleep 30  # let the index settle
          cargo publish -p gmail-mcp --token ${{ secrets.CRATES_IO_TOKEN }}

  docker:
    needs: sign-and-publish
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-qemu-action@v3
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v6
        with:
          context: .
          platforms: linux/amd64,linux/arm64
          push: true
          tags: |
            ghcr.io/${{ github.repository }}:${{ github.ref_name }}
            ghcr.io/${{ github.repository }}:latest

  homebrew:
    needs: sign-and-publish
    runs-on: ubuntu-latest
    steps:
      - uses: mislav/bump-homebrew-formula-action@v3
        with:
          formula-name: gmail-mcp
          homebrew-tap: <org>/homebrew-tap
          download-url: https://github.com/<org>/gmail-mcp/releases/download/${{ github.ref_name }}/gmail-mcp-${{ github.ref_name }}-x86_64-apple-darwin.tar.gz
        env:
          COMMITTER_TOKEN: ${{ secrets.HOMEBREW_PAT }}
```

---

## Dockerfile

```dockerfile
# syntax=docker/dockerfile:1.7

FROM --platform=$BUILDPLATFORM rust:1.85 AS build
WORKDIR /src
COPY . .
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
    esac && \
    rustup target add $target && \
    apt-get update && apt-get install -y musl-tools && \
    cargo build --release --target $target && \
    cp target/$target/release/gmail-mcp /out/gmail-mcp

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/gmail-mcp /usr/local/bin/gmail-mcp
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/gmail-mcp"]
CMD ["serve", "--transport", "http", "--listen", "0.0.0.0:8443"]
```

Uses `distroless/static` — no shell, no libc, minimal attack surface. User is `nonroot` (UID 65532).

---

## Homebrew formula

```ruby
# <org>/homebrew-tap/Formula/gmail-mcp.rb
class GmailMcp < Formula
  desc "Production-grade Gmail MCP server"
  homepage "https://github.com/<org>/gmail-mcp"
  version "X.Y.Z"
  license any_of: ["Apache-2.0", "MIT"]

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/<org>/gmail-mcp/releases/download/v#{version}/gmail-mcp-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "..."
    else
      url "https://github.com/<org>/gmail-mcp/releases/download/v#{version}/gmail-mcp-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "..."
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/<org>/gmail-mcp/releases/download/v#{version}/gmail-mcp-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "..."
    else
      url "https://github.com/<org>/gmail-mcp/releases/download/v#{version}/gmail-mcp-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "..."
    end
  end

  def install
    bin.install "gmail-mcp"
  end

  test do
    assert_match "gmail-mcp #{version}", shell_output("#{bin}/gmail-mcp --version")
  end
end
```

The formula is auto-bumped per release by the `homebrew` job above.

---

## Reproducibility

```toml
# .cargo/config.toml (committed)
[profile.release]
debug = false
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

Combined with `--locked` on the release build, `SOURCE_DATE_EPOCH` set by CI from the tag, and a fixed Rust toolchain in `rust-toolchain.toml`, bit-identical builds across runs are achievable for Linux musl targets. macOS and Windows are harder — note as known limitations.

---

## Verification

```bash
# After a v0.1.0 tag:
gh release view v0.1.0
# → 5 platform archives + SHA256SUMS + SHA256SUMS.sig + SBOM

# Verify signature locally:
cosign verify-blob \
  --certificate-identity-regexp 'https://github.com/<org>/gmail-mcp' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature SHA256SUMS.sig SHA256SUMS

# Install paths:
brew install <org>/tap/gmail-mcp
cargo install gmail-mcp
docker pull ghcr.io/<org>/gmail-mcp:v0.1.0
curl -L https://github.com/<org>/gmail-mcp/releases/download/v0.1.0/gmail-mcp-v0.1.0-x86_64-unknown-linux-musl.tar.gz | tar xz
```

---

## Release checklist (one-page, referenced from CONTRIBUTING.md)

1. Bump version in `Cargo.toml` + `Cargo.lock`.
2. Update `CHANGELOG.md` via `git-cliff --tag v<version>`.
3. PR, review, merge to main.
4. Tag: `git tag -s v<version> -m "v<version>"` (signed tag).
5. Push tag: `git push origin v<version>`.
6. Watch release workflow to green.
7. Verify signatures on at least one artifact (the `verify-blob` command above).
8. Announce in `DISCUSSIONS.md`.
9. If behavior change or security-relevant fix: post in `SECURITY.md` advisories section.

---

## Files changed

| File | Change |
|------|--------|
| `.github/workflows/release.yml` | Full pipeline (replaces Phase 1 skeleton) |
| `Dockerfile` | New |
| `.dockerignore` | New |
| `.cargo/config.toml` | Reproducibility profile |
| `CHANGELOG.md` | New, managed by git-cliff |
| `cliff.toml` | git-cliff config |
| `docs/releasing.md` | Release checklist + verification commands |
| `README.md` | Installation methods (brew, cargo, docker, direct download) |
| External: `<org>/homebrew-tap` repo | Initial formula |

---

## Dependencies

- **Requires:** Phases 1–7 at minimum. Phase 8 is optional and its absence just means the Docker image runs in stdio mode over a volume-mounted socket (or the image is less useful — acceptable tradeoff).
- **Blocks:** Nothing; this is the delivery phase.

---

## Related

- [[Gmail MCP Server Plan]]
- [[01-repo-bootstrap-and-ci]]
- [[07-production-hardening]]
