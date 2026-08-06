# Documentation authority order

When `README.md` and `docs/fragment-format.md` disagree, `README.md` is authoritative — it's
what a 2026-08 GPT-5 review panel caught: `docs/fragment-format.md` had drifted stale on
`baseType` classification and the `mirror` field while `README.md` stayed accurate. If you're
updating one, check the other for the same fact.

`process-docs/specs/implemented/2026-07-24-bootc-assemble-poc-design.md` and
`process-docs/plans/2026-07-24-bootc-assemble-poc.md` are **historical POC records, not current
reference**. They're marked with a banner rather than kept in sync, and known-drifted on at
least: hooks, `phase`, `packages.required`, the annotation namespace, and hook
package-management rules. Don't cite them for current behavior; use `README.md` and
`docs/fragment-format.md` instead.

The design spec sits under `implemented/` because the work it describes shipped, which is all
that directory claims. It is not a statement that the document still matches the code. Every
spec under `implemented/` should be read that way.
