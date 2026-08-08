# Testing Surfaces

Where tests live, what each surface can and cannot exercise, and the
conventions that keep new tests consistent with the suite.

## Layout

- **Unit tests** are inline `#[cfg(test)]` modules in each `src/*.rs` file.
  The generator module carries most of the suite; its tests build
  `LoadedFragment` values by hand and assert on the generated Containerfile
  string. `main.rs` carries none: it is argument parsing and dispatch, and
  anything worth testing belongs in a library module where the normal
  surfaces can reach it.
- **CLI tests** live in `tests/cli.rs` (assert_cmd + predicates). They are
  **offline-only**: `inspect <local-dir>` on `examples/fragments/*` is the
  one subcommand that runs without network, and it is the established
  pattern for exercising example fragments. `generate`, `list`, and
  `inspect <registry-ref>` all shell out to skopeo and cannot be CLI-tested
  offline.

## Generator test conventions

- Use the `make_repos_fragment` / `make_config_fragment` /
  `make_hook_fragment` helpers rather than hand-rolling `LoadedFragment`.
- `resolved_digest: Some(..)` on any fragment switches emission to named
  stages (`COPY --from=frag-<name>`); `None` everywhere emits inline
  registry refs (`COPY --from=<image_ref>`). Pick deliberately: assertions
  on the `--from=` form depend on it.
- The hook invocation line is the `HOOK_INVOCATION` const, indentation
  included; `hook_invocation_lines` / `frag_hook_tokens` exist for
  whole-line and token-level assertions.
- A fragment with both repo and non-repo tree paths gets its repo files
  emitted **twice**: once hoisted into the repo section, again via the
  whole-tree `COPY` in the config section. Same content, harmless, but it
  surprises exact-output and count-based assertions.

## Registry-independent logic behind an injected call

Two functions take the real registry or filesystem call as a parameter so
their decision logic is reachable offline, each with a thin public wrapper
that passes the real one:

- `self_contained::write_output` over `write_output_with(.., materialize)`
- `loader::load_all_fragments` over `load_all_fragments_with(.., load)`

Write tests against the `_with` form. For `load_all_fragments_with` that
covers manifest ordering, `manifest_index` assignment, digest stripping, and
which errors abort the run, none of which needs skopeo. A stub that counts
its own calls also pins *when* the real load is reached: a `dir:` source and
an empty manifest must never reach it, and a failing fragment must stop the
run rather than let it continue.

## Pure helpers behind a printing subcommand

`run_inspect` and `run_list` print straight to stdout, and `list` needs a
registry, so anything they decide has to be lifted out to be reachable:

- `inspect::local_mount_section` / `registry_mount_section` build the section
  from carried evidence, doing no I/O.
- `list::fragment_lines` builds both table widths and appends the mount line
  after whichever it chose; `list::note_line` takes the whole listing and
  returns one value.

The layout of the last two is the coverage. A behavior that lives in where a
`println!` sits (this line comes after both branches, this note fires once
for the run) cannot be asserted at all until the choice is a return value, so
prefer moving the decision into a helper over trying to test the printing.

## Which stream output goes to

Result output goes to stdout, commentary about a fragment goes to stderr:
`inspect` prints the `mount/` section and `MOUNT_SECTION_NOTE` to stdout
while the empty-mount notice and the drift warning go to stderr.

**Unit tests cannot hold that contract, and will not even see it fail.**
libtest captures each test's stdout and stderr and prints them only for
failing tests, so a caller-owned `eprintln!` reached from a `#[cfg(test)]`
module produces no visible output in a normal `cargo test` run and no way to
tell which stream it took. (This also means library `eprintln!` noise in test
output is not a real problem: it appears only under `-- --nocapture`.)

The one surface that separates the streams is `tests/cli.rs`, where
assert_cmd runs a real process and `.stdout(..)` / `.stderr(..)` are distinct
predicates. Assert both halves: that the expected stream carries the text
**and** that the other one does not. Only the negative half makes it a
routing test rather than a content test.

## Mutation-verify ordering and placement guards

Ordering/placement tests can pass identically under the behavior they were
meant to prevent (see commit 91d1cdf). Before trusting one, reintroduce the
plausible regression (a sort, a section gate) locally, confirm the test
fails, revert. Commit the test only after it has been seen to discriminate.

## Known-unpinnable

`run_list` prints directly to stdout and the `list` subcommand needs
registry access, so its `Manifest:` provenance line has no test; the
parse-site test in `manifest.rs` (`parsed_manifest_records_its_source_path`)
and the generator header test are the coverage for that contract. Hook
script *behavior* in `examples/fragments/*/hooks/` (what an entrypoint does
to an image) has no offline execution pattern; only its structure is
testable, via `inspect`.

The annotation drift warning's *stream* is unpinnable for the same reason.
Drift needs a registry annotation to disagree with the layer, so it is always
`None` on the local-directory path that `tests/cli.rs` can reach, and the
registry path needs skopeo. Its content is covered by
`inspect::tests::registry_inspect_surfaces_drift_from_loaded_evidence`; that
it reaches stderr rather than stdout rests on sitting in the same emission
block as the empty-mount notice, whose routing the CLI tests do lock.
