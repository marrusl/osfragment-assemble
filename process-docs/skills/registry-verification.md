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
