# Zed upstream parity

This repository is a standalone extraction of GPUI from the Zed repository. The retained GPUI
implementation must preserve the behavior, public API, and feature semantics of the Zed commit
recorded in `UPSTREAM.md`.

- Do not originate features, behavior changes, API changes, platform fixes, workarounds, or
  performance patches in this repository.
- Every library code change must already exist in an identifiable Zed commit and must be copied
  from that commit verbatim wherever the standalone layout permits.
- If a requested library change is not present in Zed, do not implement it here. Make the change
  in Zed first, then import it through the upstream sync process.
- During an upstream sync, never preserve a repo-local implementation difference. Resolve source
  conflicts in favor of the recorded Zed revision.
- Repo-local changes are limited to standalone workspace wiring, dependency-path adaptations,
  test-only adaptations, explicitly documented omissions of Zed-only integrations, provenance and
  extraction documentation, verification tooling, and lockfile updates required to build outside
  the Zed workspace. These changes must not alter GPUI runtime behavior, public API, or retained
  GPUI feature semantics.
- Keep every unavoidable standalone adaptation explicitly documented in `EXTRACTION.md`. Before
  completing a change, compare retained source files with the recorded Zed revision and run the
  relevant checks.
