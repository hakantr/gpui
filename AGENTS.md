# Zed upstream parity

This repository is a standalone extraction of GPUI from the Zed repository. The retained GPUI
implementation must preserve the behavior, public API, and feature semantics of the Zed commit
recorded in `UPSTREAM.md`.

Parity is the default, not an absolute. A consumer project may hit a limit that upstream does not
address and that cannot reasonably be worked around on its own side; the exception process below
exists for exactly that case and for nothing else.

## Default rule

- Do not originate features, behavior changes, API changes, platform fixes, workarounds, or
  performance patches in this repository.
- Every library code change must already exist in an identifiable Zed commit and must be copied
  from that commit verbatim wherever the standalone layout permits.
- If a requested library change is not present in Zed, prefer making the change in Zed first and
  importing it through the upstream sync process.
- During an upstream sync, never preserve a repo-local implementation difference unless it is a
  recorded deliberate divergence (see below). Resolve every other source conflict in favor of the
  recorded Zed revision.
- The `gpui` package version is repo-local and is deliberately ahead of the version Zed publishes.
  It is recorded in `SAPMALAR.md`, is preserved across every upstream sync, and is never resolved
  in favor of the Zed revision. Prereleases (`-alpha`, `-beta`, `-rc`) are not used.
- Repo-local changes are limited to standalone workspace wiring, dependency-path adaptations,
  the `gpui` package version, test-only adaptations, explicitly documented omissions of Zed-only
  integrations, provenance and extraction documentation, verification tooling, and lockfile
  updates required to build outside the Zed workspace. These changes must not alter GPUI runtime
  behavior, public API, or retained GPUI feature semantics.
- Keep every unavoidable standalone adaptation explicitly documented in `EXTRACTION.md`. Before
  completing a change, compare retained source files with the recorded Zed revision and run the
  relevant checks.

## Deliberate divergence

A change that departs from the recorded Zed revision is allowed only when all of the following
hold. The burden of proof is on the change.

1. **A real limit exists.** The current upstream state blocks or fails to support a capability or
   a measured performance need of a consumer project. A preference, a tidier API, or an
   anticipated future need does not qualify.
2. **The consumer side cannot solve it, or solving it there costs too much.** Either the limit is
   unreachable from the consumer's own code, or working around it there would require a cost
   clearly out of proportion to the change here. Establish this by trying the consumer-side route
   first and recording why it failed or what it would cost.
3. **The gain is stated in observable terms.** Say what becomes possible or how much faster it
   gets, in numbers where numbers apply.
4. **It is recorded before it lands.** Add an entry to `SAPMALAR.md` naming the divergence, the
   limit it removes, the consumer-side alternatives that were ruled out and why, the files
   touched, and what would let the divergence be dropped again.

Keep divergences as small as the limit requires, and prefer additive changes (a new feature flag,
a new accessor) to edits of existing upstream behavior — an addition is far easier to carry across
a sync than a rewrite. Send the change upstream as well whenever it is something Zed would take;
a divergence is a bridge, not a destination.

During an upstream sync, reapply each recorded divergence and re-check whether the limit still
exists. Drop the entry from `SAPMALAR.md` the moment upstream covers it.
