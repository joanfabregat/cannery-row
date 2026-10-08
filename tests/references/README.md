# Frozen references

Many tests compare the server, the runner and the libraries against recorded expected outputs: HTTP exchanges, database effects, runner process layouts, timestamp and OIDC vectors. Those outputs were recorded once, from the original implementation of Cannery Row, and are frozen here. They are the expected behaviour now; a test that disagrees with them is a regression unless the change is deliberate.

`references.tar.zst` holds them (about 170 MB of highly repetitive JSON, 0.6 MB compressed), and `references.tar.zst.sha256` its checksum. `unpack.sh` checks the archive and extracts it into the git-ignored paths the tests read, under `crates/*/tests/fixtures/`. Run it after checkout:

```sh
tests/references/unpack.sh
```

To change a reference deliberately, unpack, edit the JSON, and rebuild the archive reproducibly from the list of files it contains:

```sh
tar --zstd -tf tests/references/references.tar.zst > /tmp/reference-files
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='2026-10-07 00:00Z' \
  --mode='u=rw,go=r' -cf - -T /tmp/reference-files |
  zstd -19 -q -o tests/references/references.tar.zst -f
(cd tests/references && sha256sum references.tar.zst > references.tar.zst.sha256)
```

Notes:

- `crates/tracks/tests/fixtures/tracks_repository_reference.json` was recorded on PostgreSQL with the `en_US.utf8` collation. `tracks_repository_reference.builtin-locale.json` beside it is the same corpus on the builtin `C.UTF-8` locale of the managed database, where one cursor case orders differently.
