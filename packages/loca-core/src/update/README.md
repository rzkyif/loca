# Self-update

How a running core replaces its own binary and recovers when the replacement is
broken. Two halves: [`self-updater.ts`](self-updater.ts) stages the update and
restarts core, [`rollback.ts`](rollback.ts) decides on boot whether to keep it.

## Update flow ([`self-updater.ts`](self-updater.ts))

1. Fetch the core manifest and verify its Ed25519 signature against the public
   key committed in `data/signing-keys.json`. The signed payload is the whole
   manifest minus the `signature` field, canonicalized, so the `workers[]` and
   `helpers[]` entries (downloaded and executed) are covered too. Downgrades are
   always refused.
2. Pick the entry for the current platform triple, download it to `staging/`,
   and verify its sha256. Worker `.ts` files and per-triple helper binaries are
   downloaded and verified into staging first, then renamed into the live
   directories only once the whole set is good, so a mid-update network failure
   never half-installs a manifest.
3. Swap in the new binary **in place, while core is still running**, and restart
   **through the OS supervisor** so exactly one owner relaunches core and nothing
   races it over the port. A running binary can be swapped on both platforms:
   Unix renames the staged file over the running inode; Windows renames the
   running `.exe` aside to `<bin>.old` (a running `.exe` can be renamed, just not
   overwritten) then renames the new one in. Worker/helper renames tolerate a
   locked (running) Windows target the same way. The supervisor is detected from
   the definition actually installed (launchd plist / systemd unit / scheduled
   task); no definition means background mode:
   - **launchd (macOS):** write the rollback marker and exit. launchd's
     `KeepAlive` relaunches the swapped binary. The updater is NOT involved: a
     second relauncher would fight launchd for the port, which is exactly the
     failure this design removes.
   - **systemd (Linux):** `systemctl --user restart --no-block` (a clean exit
     doesn't trip `Restart=on-failure`, and the enqueued job is owned by the user
     manager, so it outlives our exit).
   - **schtask (Windows):** two layered triggers. Fast path: spawn the
     `loca-core-updater` helper (via `Start-Process`) which, after core exits,
     starts the scheduled task so Task Scheduler owns the new instance. Backstop:
     core exits non-zero, arming the task's own restart-on-failure
     (`RestartCount`/`RestartInterval`, with `MultipleInstances=IgnoreNew`
     deduping the two) so core still comes back even if the helper was torn down
     with core's job. The updater only performs the swap itself in the rare case
     the in-core swap failed (AV lock, cross-volume staging).
   - **background (no supervisor):** spawn the updater as the sole relauncher.

   The marker is always written before the restart, and the previous binary is
   always preserved as `<bin>.old`. See
   [the updater README](../../../loca-core-updater/README.md) for the updater's
   swap/restart contract.

## Boot-time rollback ([`rollback.ts`](rollback.ts))

`handleUpdateMarkerOnBoot()` runs before any service boots and reads the marker
file:

- First boot after a swap (marker present, `attempts` 0, version matches the
  running binary): bump `attempts` to 1 and schedule marker deletion after 30 s
  of uptime. The delete also removes the `<bin>.old` anchor; past that point the
  update is committed.
- Marker still present on a later boot (`attempts` >= 1): the new binary crashed
  inside the commit window. Swap `<bin>.old` back over `<bin>` and exit; the OS
  supervisor re-launches the restored previous version.
- Marker version unrelated to the running binary (already rolled back, or a
  confused state): log, delete the marker, and continue.

The trust root is the same keypair that signs `binaries.json`; the release-side
signing lives in [the website README](../../../loca-website/README.md).
