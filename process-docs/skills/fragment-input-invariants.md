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
`String` is private, so `FragmentName` cannot be constructed outside
`src/fragment.rs`. Rust field privacy is module-scoped, not type-scoped: every
use site (`loader.rs`, `generator.rs`, `self_contained.rs`, `validate.rs`,
`inspect.rs`) is in another module and so holding one is proof the grammar ran,
but code added inside `src/fragment.rs`, including its `mod tests`, can write
`FragmentName("../../escape".to_string())` and the compiler will accept it. Do
not add a bypass constructor there. There are exactly two construction sites:

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

A tar member name is arbitrary bytes on Unix, so `validate_tar_entry` takes a
`&Path` and rejects a name that is not valid UTF-8 rather than converting it
lossily. The returned path has to be a faithful rendering of the entry name,
not merely a plausible one, because `extract_fragment_payload_to_disk` derives
the file it writes from it: under a lossy conversion two entries differing only
in invalid bytes collapse onto one destination with last-write-wins, and every
other such name materializes with replacement characters. Both are silent.
Rejecting is the same reject-never-sanitize posture the rest of this module
takes.

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
`set_path`. It is the module's **only** fixture builder: `create_test_tarball`
is a thin wrapper over it for the regular-file-at-0o644 case. When writing a
test for a path-matching bug, assert on what the archive actually carries (read
the entries back and print them) before trusting that the fixture says what you
wrote.

**Do not add a second fixture builder, and do not let this one start
normalizing.** This codebase has produced that defect twice. A builder that
silently rewrites its input yields tests that pass while proving nothing, and
the failure is invisible: the test is green, names the case it means to cover,
and does not cover it. The second occurrence was worse than the first, because
that builder called `set_path` and fell back to a raw write only when
`set_path` **errored**. `set_path` errors on `..` and on absolute paths but
succeeds on `./x` and `x/./y`, quietly normalizing both. So the traversal tests
looked honest while any assertion about a `./` prefix was vacuous. Partial
transparency is worse than none, because it survives a spot check.

Two ways a fixture lies, both now closed:

- **Normalization.** Closed by never calling `set_path`.
- **Truncation.** The old-style ustar header name field is 100 bytes
  (`USTAR_NAME_FIELD_BYTES`). A raw write past it is truncated with no error
  from the header, so the builder asserts instead. Nothing in the suite is
  close today, but a realistic path is not far off:
  `fragment/tree/usr/lib/systemd/system/some.service.d/override.conf` is
  already most of the budget. A test that crossed the line would assert about
  the truncated path, most likely asserting a rejection that fired for the
  wrong reason.
