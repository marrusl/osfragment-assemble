# Fragment input invariants

Two values arrive from a fragment image and flow straight into paths: the
fragment's **name** and its **layer member paths**. Both are checked in exactly
one place. If you add a new filesystem join keyed on a fragment name, or a new
matcher over layer paths, read this first, because the invariant you need is
already established upstream and re-deriving it locally is how the two bugs
below happened.

## Fragment names: validated once, at the parse boundary

**The grammar** (`src/fragment.rs`):

```text
[a-z0-9]([a-z0-9._-]*[a-z0-9])?     1 to 64 characters
```

Lowercase ASCII letters and digits, optionally separated by `.`, `-`, or `_`,
starting and ending with a letter or digit.

**Where it is enforced:** `FragmentName::new`, in `src/fragment.rs`. The inner
`String` is private to that module, so `FragmentName` cannot be constructed
anywhere else. There are exactly two construction sites:

- `parse_fragment_toml` — the authoritative path, returns `Err`.
- `fragment_from_annotations` in `src/loader.rs` — the OCI annotation fast
  path, returns `None` so the caller falls back to layer extraction, where the
  in-layer `fragment.toml` is parsed and validated properly. Annotations are a
  cache of the TOML, so falling back to the authoritative value is correct;
  failing outright would reject a fragment whose real name is fine.

**Why it matters:** `Fragment.name` reaches a filesystem join in
`src/self_contained.rs` (`fragments/<name>/`) and is interpolated into
Containerfile `COPY` sources, bind-mount `source=` options, and `FROM ... AS
frag-<name>` stage names in `src/generator.rs`. Before the newtype, a fragment
named `../../escape` materialized into a sibling of the output directory,
outside the tree the tool had authorized itself to write.

**The rule when you add a join:** take a `FragmentName`, not a `&str`. It
implements `AsRef<Path>` precisely so `dir.join(&fragment.name)` is safe
without a local check. Do not add a second validation at the use site, and do
not accept a raw string there. The grammar is deliberately strict enough that
one validated name is always exactly one path component, which
`a_validated_name_is_a_single_path_component` pins.

**Reject, never sanitize.** Rewriting a name into something safe produces a
build that does not match what the fragment author wrote. Uppercase is rejected
rather than lowercased for the same reason, and separately because a
Containerfile stage name is case-insensitive to the builder while the
`--from=frag-<name>` reference the tool emits is generated verbatim.

## Layer paths: normalized once, in `validate_tar_entry`

**The rule:** a tar archive can carry the same member as any of

```text
fragment/hooks/entrypoint
./fragment/hooks/entrypoint
/fragment/hooks/entrypoint
```

depending on which builder produced the layer. `validate_tar_entry` in
`src/loader.rs` returns the entry's path in one canonical form (relative, with
a leading `/` and any `.` component removed). **Match against that return
value, never against `entry.path()`.** That is why the function returns a
`PathBuf` rather than `()`: the normalized path is the only thing a caller has
convenient access to, so reaching for the raw one is a visible act rather than
the default.

Ordering matters and is easy to get backwards: the traversal and
absolute-path-outside-`/fragment/` checks run on the **raw** path, before
normalization. Stripping the leading `/` first would defeat the absolute-path
check.

**What went wrong before this existed:** every matcher compared against the
literal `fragment/` prefix, so only the unprefixed form matched, even though
`validate_tar_entry` had always explicitly permitted `/fragment/...`. A
fragment whose hooks arrived `./`- or `/`-prefixed had them silently dropped
from `hook_paths`, which meant the `hooks/entrypoint` contract was never
evaluated for it, while the hook files still landed in the built image via the
generator's bind mount. The same blind spot made a `/`-prefixed layer report
`fragment.toml` as missing, and made `extract_fragment_payload_to_disk` write
nothing at all for `--self-contained`.

## Testing these: the fixture builder is not path-transparent

`tar::Header::set_path` **normalizes a leading `./` away** and rejects both
`..` components and absolute paths. A fixture built through it cannot express
any of the three forms above, so a test that names `./fragment/hooks/entrypoint`
silently asserts about `fragment/hooks/entrypoint` instead and passes for the
wrong reason. This cost a full debugging cycle: the first reproduction attempt
"passed", proving nothing.

`create_test_tarball_with_modes` in `src/loader.rs` therefore writes every
entry's path verbatim into the raw header name field and never calls
`set_path`. Keep it that way, and if you add another fixture builder, do the
same. When writing a test for a path-matching bug, assert on what the archive
actually carries (read the entries back and print them) before trusting that
the fixture says what you wrote.
