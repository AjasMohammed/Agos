# Maintainers

| Name | GitHub | Role | Since |
|---|---|---|---|
| Ajas Mohammed | [@AjasMohammed](https://github.com/AjasMohammed) | lead maintainer, release signer | 2026 |

## Release signing key

Releases are signed with minisign. Public key (id `0692DEA1023C9472`, committed at `packaging/signing/agentos-release.pub`, generated 2026-09-17):

```
RWRylDwCod6SBrcNGIz6wZsrWW5Y9o3I+OT/opftcrq4tK/KhgXvtdKl
```

Rotation: a new key is announced in the release notes of the first release signed with it, and the old key stays in git history so older releases remain verifiable.

## The 60-day rule

AgentOS currently has one maintainer. If there is **no commit, release, or issue reply from a maintainer for 60 consecutive days**, treat the project as unmaintained:

- Forks are explicitly encouraged. The Apache-2.0 license already grants that right; this paragraph restates it so nobody has to ask.
- Anyone publishing a maintained fork may say so in an issue here, and that issue will be pinned by whoever returns first.
- Security reports still go through [SECURITY.md](SECURITY.md). After 90 days without a maintainer response, the reporter may publish.

## Becoming a maintainer

Land three non-trivial merged PRs (a test file counts) and ask. There is no other process.
