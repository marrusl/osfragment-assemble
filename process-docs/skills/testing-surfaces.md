# Testing Surfaces

Where tests live, what each surface can and cannot exercise, and the
conventions that keep new tests consistent with the suite.

## Layout

- **Unit tests** are inline `#[cfg(test)]` modules in each `src/*.rs` file.
  The generator module carries most of the suite; its tests build
  `LoadedFragment` values by hand and assert on the generated Containerfile
  string.
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
  registry refs (`COPY --from=<image_ref>`). Pick deliberately — assertions
  on the `--from=` form depend on it.
- The hook invocation line is the `HOOK_INVOCATION` const, indentation
  included; `hook_invocation_lines` / `frag_hook_tokens` exist for
  whole-line and token-level assertions.
- A fragment with both repo and non-repo tree paths gets its repo files
  emitted **twice**: once hoisted into the repo section, again via the
  whole-tree `COPY` in the config section. Same content, harmless, but it
  surprises exact-output and count-based assertions.

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
to an image) has no offline execution pattern — only its structure is
testable, via `inspect`.
