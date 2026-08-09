# Secret-Bearing Mount Fragments

Implementation spec for the design at
`process-docs/specs/proposed/2026-08-08-secret-bearing-mount-fragments-design.md`.
The design is settled: seven decisions, each recorded with rationale
and cost, grounded in the empirical runs recorded in
`process-docs/skills/entitlement-build-mounts.md`. This spec states
what must be true when the work lands. It does not restate the
decisions or re-derive the evidence; for why anything below is the way
it is, read the design, which carries the citations.

## Scope

This is a documentation spec, and it is small. Saying so plainly is
part of its job.

The tool does not change. No flag, no validation, no new machinery of
any kind: the mechanism shipped with build mounts
(`process-docs/specs/proposed/2026-08-07-build-mounts.md`), and the
design's acceptance test (decision 7) is precisely that the convention
needs zero tool changes. The convention's two central artifacts also
already exist: `examples/fragments/rhel-entitlement-example/` and
`examples/manifests/rhel-entitlement.yaml` landed with the evidence
run and are prerequisites this spec records, not work it creates.

What remains is landing text on three surfaces:

1. The format documentation. The convention text quoted in the design
   lands in `docs/fragment-format.md`.
2. The real-path sweep. The shipped docs stop presenting, as worked
   examples, the mount targets the design's decisive experiment showed
   a subscribed build host silently overriding.
3. The README example index. The committed example fragment is
   currently invisible from `README.md`, whose example list still says
   the directory contains ten fragments.

That is the whole surface.

## The convention, normatively

These statements are what the landed documentation must preserve.
Rationale and evidence labels live in the design at the cited
decisions.

- A secret-bearing mount fragment is an ordinary fragment. Everything
  the build-mounts mechanism enforces applies to it unchanged:
  presence-based derivation, the mandatory digest pin, collision
  checks, `ro,z` emission, never committed. (Design, "The convention,
  stated generically.")
- It is published as a pair: a public example carrying placeholder
  files, and a live counterpart built privately, wherever the real
  material is available. (Decision 1.)
- The example ships real files at exactly the paths the live fragment
  uses. Derivation is presence-based, so a declaration-only example is
  structurally impossible, and identical paths make the pair derive
  identical mount targets, which is what lets a consumer rehearse the
  composition with the example half. (Decision 1; "The convention,
  stated generically.")
- The example appends `-example` to both the `fragment.toml` `name`
  and the image repository. Both are required because a digest-pinned
  manifest entry shows no tag, and of the fragment's identifying
  fields only the repository reaches the generated Containerfile's
  load-bearing text: the emitted `--mount=...,from=` carries the image
  reference, while the name reaches only the `# Fragments:` header
  comment, which OCP output omits. A fragment name ending in
  `-example` is reserved by this convention to assert that its
  `mount/` material is placeholder. (Decision 5.)
- Placeholders are throwaway self-signed certificates, never real
  material in any state, with any explanatory preamble kept free of
  the colon character. The colon rule is a hard requirement, not
  advice: a colon anywhere in a PEM body crashes
  subscription-manager's certificate parser with a bare SIGSEGV that
  names nothing. Placeholder filenames use an obviously fake serial
  such as `0000000000000000`; no real entitlement serial appears in
  the example or in any public document. (Decision 1; "The minimum
  viable set.")
- Repo definitions ride `tree/`; credentials ride `mount/`. The two
  halves of one acquisition scheme ship in one pinnable fragment.
  There is no flag for persisting `mount/` material; an author who
  wants credential material persisted places it under `tree/` and
  takes custody of the consequence by that placement. (Decision 3.)
- The live half is pushed only to a registry its consumer controls,
  because pull access to it equals possession of the credential.
  (Decision 4.)
- Both halves derive mount points, so both are subject to the
  mandatory digest pin. (Decision 5; build-mounts spec.)

## Deliverable 1: the format documentation

The subsection "Example fragments for mounts that carry credentials",
quoted in the design under "Proposed addition to
`docs/fragment-format.md`", lands verbatim, with the blockquote
markers removed, as the closing subsection of the `## mount/ Directory
Layout` section of `docs/fragment-format.md`.

Verbatim means verbatim: the text went through the design's review
with the rest of the document, and this spec does not reopen its
wording. Once landed, `docs/fragment-format.md` is the living
statement of the convention under the repository's documentation
authority order (`process-docs/skills/documentation-authority.md`),
and the design's quoted copy becomes a historical record.

## Deliverable 2: the real-path sweep

The design's section "The real-path model is baked into the shipped
surfaces" records where the shipped documentation teaches the
`/etc/pki/entitlement` and `/etc/rhsm` mount targets as worked
examples. Landing deliverable 1 without this sweep would leave the
same page warning against the pattern its own examples teach.

In scope, the shipped documentation:

- `docs/fragment-format.md`, four instances: the `mount/` line of the
  anatomy block, the derivation example in the `mount/` section, the
  emitted-form example, and the mounts-annotation example value.
- `docs/design.md`, the `mount/` paragraph's worked path.
- `docs/design-overview.md` is named as a surface by the design; at
  spec time its `mount/` paragraph describes the model without a
  worked path, so the obligation there is verification, not editing.

Replacement rule: a worked example that stays entitlement-flavored
uses the `/run/secrets/` form the example fragment ships
(`/run/secrets/etc-pki-entitlement`, `/run/secrets/rhsm`); a worked
example that needs no entitlement flavor may use a scheme-neutral path
such as a mirror client-certificate location. No worked example uses a
target the design's boundaries name as a masking hazard, which rules
out `/etc/rhsm` specifically. The derivation example must keep its
nesting-prune pedagogy, two files in nested directories deriving one
mount; the pair `mount/run/secrets/rhsm/rhsm.conf` plus
`mount/run/secrets/rhsm/ca/redhat-uep.pem` deriving
`/run/secrets/rhsm` preserves it exactly and matches the shipped
example fragment.

Deliberately excluded from the sweep:

- The four user-facing strings in source (`src/mount.rs:80`,
  `src/mount.rs:169`, `src/mount.rs:214`, `src/validate.rs:232`).
  They are tool surface, carried on the public roadmap
  (`docs/roadmap.md`) as their own change with its own review. The
  design recorded a single sweep covering docs and error text
  together; that scope was since split, and this spec carries the
  documentation half only.
- `process-docs/` content (the build-mounts spec, skills files). Specs
  are point-in-time records under the documentation authority order,
  not teaching surfaces, and are not edited to track later knowledge.
- Test code in `src/`, which exercises real-path fixtures as data.
  Tests are not a teaching surface.

One adjacent drift fix rides the same edit to
`docs/fragment-format.md`: line 49 claims the `description` field is
"Displayed by `inspect` and `list`", and neither command prints it
(verified against `src/inspect.rs` and `src/list.rs`; the field
appears only in test fixtures). The sweep replaces the claim with what
is true of the field. It is folded in because the same file is being
edited and the drift has been recorded before without landing.

## Deliverable 3: the README example index

The example-fragments list in `README.md` gains an entry for
`rhel-entitlement-example`, and the list's fragment count is
corrected. The entry's one-line description must carry the placeholder
assertion rather than presenting the fragment as ready to use: it
ships placeholder material at the live paths and authenticates
nothing. The sentence carrying the count currently reads "contains 10
ready-to-use fragments"; the same edit drops the ready-to-use framing
(for example, "contains 11 example fragments"), because a corrected
count under the old framing would introduce, as ready to use, an entry
whose description says it is not. Per the documentation authority order, whoever edits the
README checks `docs/fragment-format.md` for the same facts, and this
spec's deliverables 1 and 2 are the counterpart of that check.

## Already landed, recorded as standing criteria

`examples/fragments/rhel-entitlement-example/` and
`examples/manifests/rhel-entitlement.yaml` landed with the evidence
run. They are listed here so the acceptance criteria below can hold
them, and any future edit to them, to the convention:

- The `fragment.toml` name is `rhel-entitlement-example` and the
  manifest's `image:` reference names a repository ending in
  `-example`, pinned by digest.
- Four placeholder files sit at the exact live paths under
  `mount/run/secrets/`, with the obviously fake serial
  `0000000000000000` and no colon character anywhere in the
  placeholder PEM files.
- The example's README states the pairing and suffix rule, the
  registry custody rule, the failure signature (no repository file is
  generated, and the error text varies by composition without naming
  the placeholder), and the colon rule.
- The example's README states the fourth required file by role, as
  whatever `repo_ca_cert` in the shipped `rhsm.conf` resolves to, and
  names Satellite and proxied configurations as cases that resolve it
  elsewhere. `redhat-uep.pem` appears only as the stock CDN-direct
  resolution, never as the rule. The design states the minimum set by
  role rather than by filename, and this criterion is what keeps a
  future edit from collapsing the role back into a filename.
- The manifest's comments record why the base must be a RHEL base
  (the product certificate) and how to build, publish, and re-pin the
  live half.

## Acceptance criteria

1. `docs/fragment-format.md` § `mount/` Directory Layout closes with a
   subsection titled "Example fragments for mounts that carry
   credentials" whose text is identical to the design's quoted block
   with blockquote markers removed.
2. `grep -rn "etc/pki/entitlement\|etc/rhsm" docs/` returns no hits.
   (At spec time it returns five, across `docs/fragment-format.md` and
   `docs/design.md`.)
3. The derivation example in `docs/fragment-format.md` still
   demonstrates the nesting-prune rule: two files in nested
   directories deriving a single mount.
4. The example-fragments list in `README.md` includes
   `rhel-entitlement-example`, its count matches the number of entries
   in `examples/fragments/`, its description states that the material
   is placeholder and authenticates nothing, and the sentence carrying
   the count no longer describes the set as ready to use.
5. `./target/release/osfragment-assemble inspect
   examples/fragments/rhel-entitlement-example/`, with the binary
   built from current HEAD, derives exactly two mount targets,
   `/run/secrets/etc-pki-entitlement` and `/run/secrets/rhsm`.
6. `! grep -q ":" <file>` succeeds for each `.pem` file under the
   example's `mount/`. (Stated in this polarity so the check exits
   zero exactly when the criterion is met.)
7. A reader holding only `docs/fragment-format.md` and the example
   directory can author both halves of a pair: the landed subsection
   states the pairing, the exact-paths rule, the suffix rule on both
   fields, the placeholder construction rule, the pin requirement on
   both halves, and the registry custody rule, and the example
   exhibits each of them.
8. `docs/fragment-format.md` no longer claims the `description` field
   is displayed by `inspect` or `list`, and what it says about the
   field instead is true of the current commands.
9. The three tool-side exclusions named under Out of scope (the four
   source strings, generation-time mount-target reporting, and local
   container storage as a fragment source) each appear as an entry on
   `docs/roadmap.md`.

## Out of scope

Named so nothing here reads as forgotten. Each item is deliberate, and
each names its home: a design section for the settled matters, and the
public roadmap (`docs/roadmap.md`) for the work that continues
elsewhere.

- **Any tool change.** Structural validation of `mount/` contents was
  considered in the design (decision 1) and not taken. There is no
  flag, no new subcommand, no annotation work.
- **The four real-path source strings** (`src/mount.rs:80`, `:169`,
  `:214`, `src/validate.rs:232`). Carried on the roadmap as their own
  change with its own review; see deliverable 2.
- **Generation-time reporting of derived mount targets.** A mitigation
  for the design's silent-failure item 1, the moved mount root. The
  design itself proposes no mechanism changes for the silent-failure
  cluster; the selection of this fix postdates it and is recorded on
  the roadmap. It is a tool change and is not part of this spec.
- **Local container storage as a fragment source.** Carried on the
  roadmap; it revisits how the digest pin is computed, and this
  convention takes the pin rule exactly as the build-mounts spec
  states it.
- **Hook-phase mounts.** Left open by the design ("Where mounts
  attach"); nothing in this spec depends on how it resolves.
- **Unproven scheme instances.** SUSE SCC, mirror mTLS, and proxy CA
  bundles are pattern claims; the design's "What was not verified"
  section is the record. The landed documentation stays scheme-generic
  and claims nothing measured about them.
- **The design's "Observed in passing" items** (a stale comment in
  `src/inspect.rs`, the collision error's unreachable rationale, the
  three-versus-four count in `docs/design.md`). Pre-existing, recorded
  there, not part of this spec.
