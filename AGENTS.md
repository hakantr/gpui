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

## Known exception

One repo-local API addition currently contradicts the rules above: the retained scaled-path methods
on `Window`, documented in `EXTRACTION.md` under "Repo-local API addition: retained scaled paths".
It is tracked, not accidental.

A sync will remove it, which is the policy working as written. When that happens, either re-apply
`feat/retained-path-transforms` on top of the sync, or land the API in Zed and drop the exception.
Do not silently drop it, and do not treat its removal by a sync as a bug.
