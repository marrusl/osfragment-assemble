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
