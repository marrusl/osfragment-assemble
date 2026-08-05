# Adapting a package's own layout to image mode

What a fragment is actually for, beyond installing a package: encoding the
adjustments an application needs when its RPM's assumptions meet a bootc image.
The `grafana` example fragment is the worked case (measured 2026-08-05 against
grafana 13.1.1 on `centos-bootc:stream10`).

## `%post` is where packages hide their image-mode problems

An RPM's `%post` runs once, at build time, on a machine it assumes it owns. Two
habits show up constantly and both are invisible in the file list:

- **Creating accounts with `useradd`/`groupadd`** instead of shipping a
  `sysusers.d` fragment.
- **Moving or generating content under `/var`**, which in image mode is a
  first-boot seed rather than part of the image.

`rpm -ql` will not show you either one. Read `rpm -q --scripts <pkg>`. Grafana's
`%post` both creates its account and does
`mv $GRAFANA_HOME/data/plugins-bundled $DATA_DIR`, and neither is visible any
other way.

## Find where the application *actually* reads a path

Grafana's bundled datasource plugins looked like they lived at a path derived
from the configured data directory, so the obvious fix was to leave a symlink
there. That was wrong, and the config files do not say so: `defaults.ini`
documents `plugins = data/plugins` and mentions no bundled path at all, and the
binary carries no `bundled_plugins_path` setting to grep for.

**The server's own startup log is the authority.** Started with the unit file's
arguments — including `cfg:default.paths.data=/var/lib/grafana` — it logs:

```
level=info msg="Path Plugins" path="[/var/lib/grafana/plugins /usr/share/grafana/data/plugins-bundled]"
```

So the bundled path is derived from the **homepath**, not from
`default.paths.data`. Which means the RPM's own `%post` move relocates the
plugins somewhere the server never reads: the directory it does read is left
empty, and grafana quietly re-downloads elasticsearch and zipkin from
grafana.com on first start. The packaging works around
[grafana/grafana#123110](https://github.com/grafana/grafana/issues/123110) and
introduces this in the process.

The fragment's fix is therefore to **undo the `%post` move** rather than invent
a third location — restoring the files where the package ships them, which is
both where the server reads and already under `/usr`.

Boot the service and read its log before designing around a path. Prove the
result with the network cut (`podman run --network=none`): plugins that still
register are being loaded from disk, and a plugin that was silently downloaded
before will announce itself by its absence.

## `bootc container lint`'s `var-tmpfiles` is two checks, not one

Clearing it needs both halves addressed, and they want different things:

| reported as | what it flags | how to clear it |
|---|---|---|
| `Found content in /var missing systemd tmpfiles.d entries` | every path under `/var` — **directories and symlinks included** — with no `tmpfiles.d` entry | ship a `tmpfiles.d` fragment declaring them |
| `Found non-directory/non-symlink files in /var` | regular files only | get the files out of `/var` |

Removing the files therefore shrinks the warning without clearing it: the
directories that held them are still undeclared. Conversely a symlink left in
`/var` is fine for the second check and still needs a `tmpfiles.d` entry for the
first.

Convenient detail: the first check prints its findings **as the tmpfiles.d lines
it wants**, so the fix can be copied out of the warning:

```
d /var/lib/grafana 0755 grafana grafana - -
```

Declaring the directories is not just lint appeasement — it is what makes the
package work on a fresh `/var`, which is the state image mode actually boots
into.

## `sysusers.d` matches on names, not IDs

The `sysusers` check reports `/etc/passwd` and `/etc/group` entries with no
corresponding `sysusers.d` declaration. It compares **names**; the numeric IDs
allocated by `useradd -r` at build time do not have to appear anywhere. So the
declaration stays portable:

```
#Type Name    ID GECOS          Home directory     Shell
u     grafana -  "grafana user" /usr/share/grafana /sbin/nologin
```

A single `u` line covers the matching group too — the base's own `chrony.conf`
is exactly this shape, and its user and group carry different IDs (994 and 992)
without being flagged.

## Check `rpm -V` as a second opinion

`rpm -V <pkg>` reports the 23 relocated plugin files as `missing` in a stock
install, because the rpmdb still records them where the package put them and
`%post` moved them elsewhere. That reading is free and points straight at
content a `%post` relocated. After the fragment undoes the move, the same
command reports nothing — a useful signal that the fragment restored the
package's intended layout rather than inventing its own.
