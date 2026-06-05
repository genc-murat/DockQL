# Dependency Policy

> **Last updated:** 2026-06-05
> **Applies to:** DockQL v0.6.0+

## Guiding Principles

1. **Minimize risk** — Prefer well-established, actively maintained crates over trendy alternatives.
2. **Stay current** — Regular updates prevent accumulation of technical debt and security issues.
3. **Pin responsibly** — Use `Cargo.lock` for reproducible builds; update deliberately.
4. **Audit every change** — No dependency update reaches `main` without passing the full CI pipeline.

## Update Cadence

| Type | Frequency | Tool | Scope |
|------|-----------|------|-------|
| **Security patches** | As needed (immediately) | `cargo audit` + dependabot | Semver-compatible |
| **Minor updates** | Monthly | `cargo update` | Patch & minor bumps |
| **Major updates** | Per-release cycle | Manual review | Breaking changes (see below) |

## Branch Strategy

- **Security fixes:** Create a dedicated branch, apply the fix, merge via PR.
- **Monthly `cargo update`:** Run on `main` directly. If tests fail, investigate and fix before committing.
- **Major upgrades:** Create a feature branch. Update code, update tests, run full CI, then merge.

## Version Pinning

| Phase | Strategy |
|-------|----------|
| **Pre-1.0 (current)** | Pin major and minor versions in `Cargo.toml`. Accept patch updates from `cargo update`. Example: `bollard = "0.21"` accepts `0.21.x` but not `0.22`. |
| **Post-1.0** | Pin major versions only. Example: `bollard = "0.21"` → only `0.21.x`. |

## Breaking Change Handling

When a dependency releases a new major version:

1. Check the changelog/ migration guide for the new version.
2. Evaluate if the breakage is worth the benefit.
3. If upgrading, update all affected code in a single commit.
4. Run the full test suite and benchmark suite.
5. Document the change in `CHANGELOG.md` under `### Dependencies`.

## Audit Requirements

- **`cargo audit`** runs on every PR (via CI).
- Any vulnerability with `CVSS >= 7.0` must be patched within 7 days.
- Any vulnerability with `CVSS >= 9.0` must be patched within 48 hours.
- Suppressions (`cargo audit --ignore`) require a documented rationale in `audit-suppressions.toml`.

## Adding New Dependencies

Before adding a new crate, answer:

1. **Is it necessary?** Can the functionality be implemented with existing dependencies?
2. **Is it well-maintained?** Last published < 1 year ago? Active GitHub repo?
3. **Is it auditable?** Reasonable code size? No `build.rs` downloading binaries without verification?
4. **Does it add transitive dependencies?** Each new crate pulls in its own tree — keep it shallow.

### Approval

| Dependency Type | Review Required |
|----------------|-----------------|
| New dev-dependency | PR review |
| New runtime dependency | Team lead approval |
| `unsafe`-using crate | Security review |

## Current Direct Dependencies

As of v0.6.0, the project depends on:

```
anyhow, bollard, chrono, clap, clap_complete, crossterm, csv,
dirs, futures-util, ratatui, regex, reqwest, rustyline, serde,
serde_json, serde_yaml, strsim, thiserror, tokio, tokio-stream, toml
```

All dependencies are standard, well-maintained Rust ecosystem crates. No dependencies use procedural macros from untrusted sources.

## Lockfile Policy

- `Cargo.lock` is committed to the repository.
- `cargo check --locked` runs in CI to ensure reproducible builds.
- `cargo update` is run deliberately, never automatically.

## Responsible Disclosure

If you discover a vulnerability in a dependency of this project, please open a [GitHub Security Advisory](https://github.com/genc-murat/DockQL/security/advisories) rather than a public issue.
