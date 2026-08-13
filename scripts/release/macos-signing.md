# macOS Developer ID signing + notarization

The release pipeline signs + notarizes the macOS Client bundle with an Apple
Developer ID Application certificate, signs + notarizes the macOS `.pkg`
installer with a Developer ID Installer certificate, and ships the iOS Client
through App Store Connect. Everything is **secret-driven**: with the `APPLE_*`
vars in `.env` empty, the macOS build keeps ad-hoc signing (`tauri.conf.json`
`bundle.macOS.signingIdentity` is `"-"`), the `.pkg` ships unsigned (Gatekeeper
warns until the user clears it once), and iOS is skipped. Filling the vars is
the only switch; a partially-filled set downgrades
only the artifacts that miss their secret, never failing the run. This doc is
the checklist for the account-side setup and the verification steps.

## How the plumbing fits together

- `DeployEnv` carries the optional Apple fields, loaded from `.env`
  (`loadOrSeedEnv`) and from the process env on a CI runner (`envFromProcess`) in
  [lib.ts](lib.ts).
- [client.ts](client.ts) `buildClient` injects the `APPLE_*` env into the Tauri
  build subprocess **only for an `apple-darwin` target and only for non-empty
  fields** (`appleSigningEnv`), so a blank cert never reaches Tauri and no other
  platform is affected.
- [.env.example](../../.env.example) documents every var (the "Apple signing
  (macOS + iOS, optional)" block).
- On CI the App Store Connect `.p8` arrives base64-encoded in the
  `APPLE_API_KEY_B64` secret; `materializeAppleApiKey` in
  [apple-toolchain.ts](apple-toolchain.ts) (called by [ci-build.ts](ci-build.ts))
  writes the key file and fills `APPLE_API_KEY_PATH` itself. Locally you point
  `APPLE_API_KEY_PATH` at the real file instead.
- `hardenedRuntime` is already `true` (Tauri's default for `bundle.macOS`), which
  notarization requires. Do **not** re-add it as if it were missing.

macOS cannot be cross-signed (Apple's tooling + terms require a Mac), so signing
runs on the macOS host/runner that builds the bundle - the same host that already
builds the `apple-darwin` triples.

## Cert-ready checklist

1. **Obtain + install the certificates.** With the Apple Developer Program
   membership ($99/yr), create a **Developer ID Application** certificate (the
   Client `.app`) AND a **Developer ID Installer** certificate (the unified
   `.pkg`; `productsign` rejects an Application cert), and install both into the
   login keychain of the macOS build machine. Note the identity strings, e.g.
   `Developer ID Application: Your Name (TEAMID1234)` /
   `Developer ID Installer: Your Name (TEAMID1234)`, and your Team ID.

2. **Fill `.env`** (see [.env.example](../../.env.example)):
   - `APPLE_SIGNING_IDENTITY` = the Application identity string;
     `APPLE_INSTALLER_SIGNING_IDENTITY` = the Installer identity string. Locally
     you can leave `APPLE_CERTIFICATE` / `APPLE_CERTIFICATE_PASSWORD` and
     `APPLE_INSTALLER_CERTIFICATE` / `APPLE_INSTALLER_CERTIFICATE_PASSWORD` blank
     and rely on the certs already in the login keychain.
   - Notarization: fill **either** the Apple ID trio (`APPLE_ID`,
     `APPLE_PASSWORD` (an app-specific password from appleid.apple.com, not your
     login password), `APPLE_TEAM_ID`) **or** the App Store Connect API key trio
     (`APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH` pointing at the
     `.p8`). The API key is the better fit for CI, and the iOS release requires
     it (with the **Admin** role, for Xcode cloud signing + the upload). Do not
     fill both.

3. **Finalize `tauri.conf.json` `bundle.macOS`** (the one part this plan left
   untouched, because it can only be validated against a real notarization run):
   - Optionally set `signingIdentity` here instead of the env var (the env var
     overrides it, so either works; the env keeps it out of git).
   - Add an `entitlements` file if a notarization run reports one is needed. Start
     with **no** custom entitlements: the WKWebView runs its JIT out-of-process
     (in `com.apple.WebKit.WebContent`), so the host app usually does **not** need
     `com.apple.security.cs.allow-jit` or `allow-unsigned-executable-memory`, and
     `device.microphone` is a sandbox/App-Store entitlement that a non-sandboxed
     Developer ID build does not require. Add entitlements only if a real
     `notarytool` submission or launch failure proves one is missing - do not
     copy a speculative list in.
   - Set `providerShortName` only if the Apple ID belongs to multiple teams (it
     disambiguates which team notarizes).

4. **The user-facing artifact is the unified `.pkg`** (the macOS download on
   the website), built by [unified-installers.ts](unified-installers.ts) from
   the signed `.app` + the Core payload. With signing on, Tauri staples the
   notarization ticket to the bundle after a successful notarization; confirm
   `xcrun stapler validate` passes on the `.app` inside the `.pkg`. Until
   signing is on, the `.pkg` still builds (unsigned) and Gatekeeper warns on
   first open.

5. **Wire CI secrets.** Add the `APPLE_*` vars as repo secrets that reach the
   **macOS build runner only** (mirroring how the Tauri/Android build-signing
   keys are scoped - never the publish job): `APPLE_SIGNING_IDENTITY`,
   `APPLE_CERTIFICATE` (base64 `.p12` of the Application cert),
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_INSTALLER_SIGNING_IDENTITY`,
   `APPLE_INSTALLER_CERTIFICATE` (base64 `.p12` of the Installer cert),
   `APPLE_INSTALLER_CERTIFICATE_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_API_KEY`,
   `APPLE_API_ISSUER`, and `APPLE_API_KEY_B64` (base64 of the `.p8`; the
   CI-side form of `APPLE_API_KEY_PATH`). See the required-secrets list in
   [packages/loca-website/README.md](../../packages/loca-website/README.md).

6. **Verify end to end** on a clean Mac (ideally a fresh user account):
   - Download the unified `.pkg` **via a browser** (so it gets the quarantine
     xattr) and confirm it opens with no Gatekeeper "unidentified developer" /
     "damaged" warning.
   - `spctl -a -vvv /Applications/Loca.app` reports `accepted` with
     `source=Notarized Developer ID`.
   - Grant microphone + screen-recording, trigger a self-update, and confirm the
     grants persist (a stable identity is the reason to sign beyond first-open
     warnings; ad-hoc identities can re-prompt after each update).

## macOS `.pkg` installer (its own Installer identity)

The unified macOS installer ([unified-installers.ts](unified-installers.ts))
wraps the signed client `.app` and the Core payload into a `productbuild`
distribution `.pkg`. When `APPLE_INSTALLER_SIGNING_IDENTITY` is set it is
`productsign`ed and, when the notarization creds exist, `notarytool submit
--wait` + `stapler staple` runs (`notarizePkg` in
[core-installers.ts](core-installers.ts)). Unsigned (with a note) otherwise.

`productsign` requires a **Developer ID Installer** certificate; the
**Developer ID Application** identity that signs the client bundle is a
different cert kind and is rejected, hence the dedicated env var. On CI the
Installer cert (base64 `.p12` in `APPLE_INSTALLER_CERTIFICATE`) is imported into
an ephemeral keychain for the signing run and deleted after
(`materializeInstallerKeychain` in [apple-toolchain.ts](apple-toolchain.ts));
Tauri's own `APPLE_CERTIFICATE` import covers only the client build, not
`productsign`. Locally both certs just live in the login keychain.

## Windows Authenticode (separate identity, still DORMANT)

Independent of Apple. The client NSIS installer and the unified installer ship
unsigned until a Windows code-signing cert exists (SmartScreen warns on first
run). To turn it on, fill `WINDOWS_CERTIFICATE_THUMBPRINT` (a cert in the
runner's store) or `WINDOWS_SIGN_COMMAND` (e.g. an Azure Trusted Signing
invocation with `%1` for the file) in `.env` / repo secrets. `client.ts`
`injectWindowsSigning` patches `tauri.conf.json` for the client bundle;
`signWindows` (in [core-installers.ts](core-installers.ts), used by
[unified-installers.ts](unified-installers.ts)) signs the unified installer.
Both are inert when the env is empty.

## iOS (secret-driven, like the macOS signing above)

The iOS Client is wired end to end and gated purely on the `APPLE_*` env plus a
Mac + Xcode: the dev loop always works, and the build + release paths self-skip
while the env is absent.

- **Always available:** `deno task dev:ios` boots the dev Core and launches the
  app on the iOS Simulator (mirrors `dev:android`, clean Ctrl+C teardown
  included). `deno task --cwd packages/loca-client init:ios` generates
  `gen/apple` (commit it as source, like `gen/android`). `deno task check:ios`
  compiles the shell for `aarch64-apple-ios`; the CI `ios` job runs the same
  check on every `main` push (compile-only, no secrets).
- **Secret-gated:** `build:client:ios` falls back to an unsigned Simulator build
  without signing; `scripts/release/ios.ts` (`iosItem`, registered in `main.ts`)
  is dropped by `appleReleaseConfigured(env)` when the signing + App Store
  Connect env is absent, and `ci-build.ts --ios` (the macOS arm64 release leg)
  skips with a note under the same condition, so an unconfigured repo releases
  exactly what it releases today. To activate iOS: fill `APPLE_TEAM_ID` + a
  certificate (`APPLE_SIGNING_IDENTITY` or the base64 `APPLE_CERTIFICATE`) + the
  `APPLE_API_KEY` / `APPLE_API_ISSUER` / `APPLE_API_KEY_PATH` (locally) or
  `APPLE_API_KEY_B64` (CI) trio; the same paths then build/sign/upload the
  `.ipa` to App Store Connect with no other switch. The API key drives Xcode's
  automatic cloud signing, so it needs the **Admin** role and no distribution
  cert or provisioning profile is exported. **Create the App Store Connect app
  record first** - the upload errors without it; submitting for App Review after
  TestFlight processing is also a manual ASC step.
- **Upload tool watch:** the upload uses `xcrun altool --upload-app`, which Apple
  has deprecated. If it starts failing, switch to `xcrun iTMSTransporter -m
upload` or an `xcodebuild -exportArchive` upload destination (note in
  [ios.ts](ios.ts)); verify the first real upload lands in App Store Connect ->
  TestFlight.
- **Architecture constraint:** iOS cannot run a local Core (no persistent
  background service); the iOS Client pairs to a Core on another device only. The
  mobile platform impl
  ([mobile.ts](../../packages/loca-client/src/ui/lib/platform/mobile.ts)) already
  returns false/null across the local-core surface, and `updater.check` returns
  null on iOS (the App Store owns updates; there is no OTA self-host).
- **Keychain is iOS-ready:** the Apple native keyring store is cfg-gated for
  `macos` + `ios`
  ([keychain.rs:82](../../packages/loca-client/src/tauri/src/commands/keychain.rs#L82),
  and the same in the `loca-core-keychain` helper).
- **`gen/apple` customizations** (hand-applied after `init:ios`, like the Android
  manifest edits): `Info.plist` usage strings (`NSMicrophoneUsageDescription`,
  `NSLocalNetworkUsageDescription`) + an App Transport Security
  `NSAllowsLocalNetworking` exception for the LAN/dev Core, and a channel xcconfig
  that suffixes the bundle id off `LOCA_CHANNEL` (the iOS analogue of gradle's
  `applicationIdSuffix`).
- **Distribution:** App Store only (the DMA alternative-marketplace path still
  needs the membership and is EU-only). `iosItem` mirrors `androidItem` but
  uploads to App Store Connect instead of self-hosting on R2, and requires App
  Review.
