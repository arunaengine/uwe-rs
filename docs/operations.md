<!-- Where uwe keeps its state, and how to back it up, restore it and upgrade uwe safely. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Operating uwe

This page is for people who run uwe for a team or on a shared machine. It covers where uwe keeps its state, how to back it up and restore it, and what to expect from an upgrade.

## Where the state lives

Configuration is read from `/etc/uwe/config.toml` (or the file named by `UWE_SITE_CONFIG`) and then from `~/.config/uwe/config.toml`. Run state lives in the cache folder, `$XDG_CACHE_HOME/uwe` or `~/.cache/uwe`:

| Path | What it holds | Needed after a restore |
| --- | --- | --- |
| `runs.sqlite` | The run database: runs, tasks, attempts, events and the cache index | Yes |
| `store/` | The content-addressed store with every result, unless the config moves it elsewhere | Yes |
| `digests.sqlite` | Remembered digests of local files, so that unchanged inputs are not hashed again | No, it is rebuilt |
| `containers/` | Ownership records of local containers that a run started | Only while runs are active |
| `scratch/` | Working folders and journals of task attempts, also on remote executors | Only while runs are active |
| `downloads/`, `conda/`, `cwl-images/`, `tools/` | Copies of remote inputs, solved environments, saved images and helper tools | No, they are made again |
| `repeats/`, `replays/` | Folders that `uwe repeat` and `uwe replay` restored | No |

`-database FILE` and the `store` setting of the config file can move the database and the store. A remote store, for example on S3, is not inside the cache folder at all. Back it up with the tools of its provider, for example bucket versioning or replication.

## Backup

The run database uses SQLite in WAL mode. Copying `runs.sqlite` alone while uwe writes to it can give a broken copy. Use one of these:

- Stop all runs, `uwe page` and `uwe clean`, then copy the files.
- Or take an online copy with the SQLite shell, which is consistent even while uwe runs:

  ```sh
  sqlite3 ~/.cache/uwe/runs.sqlite ".backup '/backup/uwe/runs.sqlite'"
  ```

Back up the database first and the store second. Stored files are written once and never change, so a store copied after the database holds every file that the database copy refers to. Do not run `uwe clean -remove` during a backup, as it may delete stored files that the database copy still refers to.

Stored files are read-only. Copy them with a tool that keeps the folder structure, for example `rsync -a`.

## Restore

1. Put `runs.sqlite` and `store/` back where they were, or point `-database` and the `store` setting at them.
2. Leave out `containers/` and `scratch/` from an old backup. They describe runs that are no longer active. A run that was active at backup time shows as unfinished; record what happened to it with `uwe reconcile RUN -reason TEXT`.
3. Check the result with `uwe check-machine` and `uwe runs`.

A stored file that was damaged in the backup is found when it is read. uwe then stores it again on the next run that makes it.

## Upgrades

- A newer uwe upgrades `runs.sqlite` on first open, in place. Keep a backup from before the upgrade.
- An older uwe refuses a database that a newer one has upgraded, with an error that names both schema versions. To roll back, restore the database from before the upgrade. Runs recorded since then are lost from the database, but their files stay in the store.
- `uwe --version` prints the version you are running. Write it down with each backup.

## Disk space

The store, the cache and the images of the container engine grow over time. Preview what `uwe clean` would remove, then remove it with `uwe clean -remove`. `uwe clean -keep-days N` and `-keep-run RUN` keep more. A layered store also takes `uwe clean -front`, which only empties its local front. Destructive clean-up of a remote store is not supported yet, so use a lifecycle rule of the bucket for staging data.
