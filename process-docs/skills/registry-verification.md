# Verifying registry paths end to end against a local registry

How to prove a change to fragment metadata handling actually works against a
real registry, without publishing anything or editing the developer's config.

## The tool never passes `--tls-verify=false`

Every skopeo invocation in `loader.rs` (`inspect --raw`, `copy`, the digest
lookup) is built without TLS options. Against a plain-HTTP local registry the
run dies with:

```
Error parsing image name "docker://localhost:5050/...":
  pinging container registry localhost:5050:
  Get "https://localhost:5050/v2/": http: server gave HTTP response to HTTPS client
```

Do not "fix" this by adding a TLS flag to the tool, and do not edit
`~/.config/containers/registries.conf`. Point skopeo at a throwaway config for
the length of the test instead; the child skopeo process inherits the variable:

```bash
cat > /tmp/scratch/registries.conf <<'EOF'
[[registry]]
location = "localhost:5050"
insecure = true
EOF
export CONTAINERS_REGISTRIES_CONF=/tmp/scratch/registries.conf
```

`podman push` to the same registry takes `--tls-verify=false` directly and does
not need the file.

## Only `list` exercises the annotation fast path

`load_registry_fragment_metadata_only` tries annotations first and falls back to
a full layer pull. Command mapping:

- `list` — metadata-only, so annotations are used when present.
- `inspect` — always the full load, because it prints tree and hook paths that
  the annotations do not carry.

Testing an annotation change with `inspect` therefore proves nothing about the
annotation path. Use `list` with a manifest pointing at the test image.

## The observable signal is skopeo's own chatter

The fast path pulls no layers, so a successful annotation read prints no
`Getting image source signatures` / `Copying blob` lines. Their presence means
the fast path was skipped and the fragment was read from its layers, which is
also what a fragment carrying unrecognized annotation keys should do. A rename
of the annotation keys is fully verified by building the fragment twice, once
with each key set, and confirming the old-key image falls back while the new-key
image does not.

Annotations are set at build time and live on the image manifest:

```bash
podman build --annotation com.github.marrusl.osfragment.name=tailscale \
             --annotation com.github.marrusl.osfragment.phase=config \
             -f Containerfile.fragment -t localhost:5050/fragments/tailscale:1.82.0 .
```

`skopeo inspect --raw` shows the manifest's `annotations` map and is the quickest
way to confirm they survived the push.

## Regenerating docs that must show published refs

A doc example is supposed to be real tool output, but the fragments it names
(`quay.io/marrusl2/fragments/...`) are the ones a breaking change has not been
applied to yet — they are exactly what the change stops loading. Pointing the
manifest at `localhost:5050` produces a genuine run with the wrong refs in it,
and hand-editing the host back afterwards produces output that is no longer
genuine.

Mirror the published location to the local registry instead. Only the pull is
redirected; the tool still resolves and emits the manifest's declared refs:

```bash
cat > /tmp/scratch/registries-mirror.conf <<'EOF'
[[registry]]
location = "quay.io/marrusl2/fragments"

[[registry.mirror]]
location = "localhost:5050/fragments"
insecure = true
EOF
export CONTAINERS_REGISTRIES_CONF=/tmp/scratch/registries-mirror.conf
```

The manifest keeps its `quay.io/...` entries and the emitted Containerfile
carries them, while the bytes come from the locally rebuilt fragments. Diff the
doc's code block against the generated file rather than eyeballing it; that is
what catches unrelated drift, such as a stale `# Fragments:` comment still
carrying parentheticals from a field that was deleted two changes ago.

Note the mirror is consulted, not enforced: if the local registry is missing
the image, skopeo falls through to the real location and silently pulls the
published (pre-change) fragment. Build and push before generating.

## A stale local image silently shadows the published fragment

`COPY --from=<ref>` and `RUN --mount=from=<ref>` both resolve against podman's
local storage first. If a tag like `quay.io/marrusl2/fragments/grafana:11.0` is
already present locally — which it always is after a rebuild-and-push script,
because the script builds under the published name — podman never contacts the
registry, and the build tests whatever that local copy happens to contain.

This produced a build failure on 2026-08-04 that looked nothing like its cause:

```
STEP 9/13: RUN --mount=type=bind,from=quay.io/marrusl2/fragments/grafana:11.0,source=/fragment/hooks,...
/bin/sh: line 1: /frag-hooks/entrypoint: No such file or directory
Error: ... while running runtime: exit status 127
```

The published image had a perfectly good `/fragment/hooks/entrypoint`; the local
one did not. Exit 127 with `No such file or directory` reads like a missing
shebang interpreter, which sends you to inspect the script instead of the image
resolution. Removing the tags and re-pulling fixed it with no other change.

**Before any build meant to prove the published fragments work, force a refresh:**

```bash
for r in epel:10 grafana:11.0 awscli-zip:2.36.16; do
    podman rmi -f "quay.io/marrusl2/fragments/$r" 2>/dev/null
    podman pull "quay.io/marrusl2/fragments/$r"
done
```

This is the inverse of the mirror trick above: mirroring makes a local image
stand in for a published one deliberately, and this is the same substitution
happening by accident.

## The published fragment set is mixed-architecture

Measured 2026-08-04: `epel:10`, `awscli-zip:2.36.16`, and
`nvidia-driver-run:610.57.04` are `arm64`, while `grafana:11.0` and
`cis-hardening:2.1` are `amd64` — the originals were pushed from a different
machine than the later ones. Nothing breaks, because fragment payload is
architecture-independent files, but every build on an arm64 host prints

```
WARNING: image platform (linux/amd64) does not match the expected platform (linux/arm64)
```

once per amd64 fragment, including during `COPY --from=`. Check with
`skopeo inspect --format '{{.Architecture}}'` before assuming a fragment set is
uniform, and expect the warning rather than treating it as a new fault.

## Never write non-trivial inline `python3 -c` in this pipeline

Deriving annotation arguments from `fragment.toml`, or checking a manifest's
annotation map, invites a one-liner. Do not. Inside a single-quoted shell string
the shell passes backslashes through literally, so escaped quotes land in the
Python source in places the parser rejects — reliably inside f-string
expressions, which is exactly where JSON and dict literals want them:

```bash
# All three of these died with SyntaxError before running a single statement:
python3 -c 'print(json.dumps(repos, separators=(\",\", \":\")))'
python3 -c 'print(f"{rows(\"full.Containerfile\", n)} rows")'
python3 -c 'print(f".phase present: {any(k.endswith(\".phase\") for k in ks)}")'
```

**It fails silently.** A `SyntaxError` means nothing executes and the error goes
to stderr. When the call sits in a command substitution feeding a shell array,
or under `>/dev/null 2>&1`, the caller sees empty output rather than a failure,
and `set -e` does not necessarily abort a subshell or process substitution.

This caused a real incident on 2026-08-03. The fragment rebuild script derived
its `--annotation` flags from each `fragment.toml` through an inline
`python3 -c`. The Python died on the escaped quotes, the argument array came
back empty, and `podman build` ran with no annotations at all. The script
printed `==> building ...` for each fragment, printed `done: 8 fragments`, and
exited 0. Eight images were built and pushed carrying zero annotations, and it
was only caught later by inspecting a published manifest.

The fix is mechanical: **write the script to a file in scratch and run the
file.** A heredoc into a `.py` file, or a `read -r -d '' VAR <<'PY'` block whose
quoted delimiter stops the shell from touching the contents, both work; passing
the program as an argv string does not.

Then assert on the derived data before acting on it, because the failure mode is
absence rather than error:

```bash
if [ "${#args[@]}" -lt 3 ]; then
    echo "refusing to build $name: only ${#args[@]} annotations derived" >&2
    exit 1
fi
```

That guard is what turns a silent zero-annotation build into a loud refusal. Any
step that derives build arguments from parsed metadata should carry one.
