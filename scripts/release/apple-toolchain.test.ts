// Guards the secret-driven Apple gates: with no APPLE_* set (the shipped state)
// every gate stays false, so a release without the secrets behaves exactly as
// before (ad-hoc client, unsigned .pkg installer, iOS skipped). Also pins the two
// forms of the App Store Connect key (a local path or the CI base64) and that
// resolveAppleEnv never leaks blank values into a child process env.

import { assertEquals } from "@std/assert";

import {
  appleReleaseConfigured,
  hasAppleSigning,
  hasAppStoreConnectApi,
  resolveAppleEnv,
} from "./apple-toolchain.ts";
import type { DeployEnv } from "./lib.ts";

/** A fully-empty DeployEnv: mirrors a .env with no signing secrets filled in. */
function blankEnv(overrides: Partial<DeployEnv> = {}): DeployEnv {
  return {
    signingPrivateKey: new Uint8Array(),
    signingPublicKey: new Uint8Array(),
    cloudflareApiToken: "",
    cloudflareAccountId: "",
    r2AccessKeyId: "",
    r2SecretAccessKey: "",
    websiteDomain: "",
    storageDomain: "",
    r2Bucket: "",
    tauriUpdaterPublicKey: "",
    tauriUpdaterPrivateKey: "",
    tauriUpdaterPassword: "",
    androidKeystoreB64: "",
    androidKeystorePassword: "",
    androidKeyAlias: "",
    androidKeyPassword: "",
    appleSigningIdentity: "",
    appleCertificateB64: "",
    appleCertificatePassword: "",
    appleId: "",
    applePassword: "",
    appleTeamId: "",
    appleApiKey: "",
    appleApiIssuer: "",
    appleApiKeyPath: "",
    appleApiKeyB64: "",
    appleInstallerSigningIdentity: "",
    appleInstallerCertificateB64: "",
    appleInstallerCertificatePassword: "",
    windowsCertificateThumbprint: "",
    windowsSignCommand: "",
    windowsTimestampUrl: "",
    ...overrides,
  };
}

const API_TRIO = {
  appleApiKey: "ABC123DEF4",
  appleApiIssuer: "11111111-2222-3333-4444-555555555555",
};

Deno.test("appleReleaseConfigured: false on a blank env (no secrets => iOS stays skipped)", () => {
  assertEquals(appleReleaseConfigured(blankEnv()), false);
});

Deno.test("hasAppleSigning: needs a team id AND a cert (identity or base64 .p12)", () => {
  assertEquals(hasAppleSigning(blankEnv({ appleTeamId: "TEAMID1234" })), false);
  assertEquals(
    hasAppleSigning(blankEnv({ appleSigningIdentity: "Developer ID Application: X" })),
    false,
  );
  assertEquals(
    hasAppleSigning(
      blankEnv({ appleTeamId: "TEAMID1234", appleSigningIdentity: "Developer ID Application: X" }),
    ),
    true,
  );
  assertEquals(
    hasAppleSigning(blankEnv({ appleTeamId: "TEAMID1234", appleCertificateB64: "cDEy" })),
    true,
  );
});

Deno.test("hasAppStoreConnectApi: accepts the .p8 as a path (local) or base64 (CI)", () => {
  assertEquals(hasAppStoreConnectApi(blankEnv(API_TRIO)), false);
  assertEquals(
    hasAppStoreConnectApi(blankEnv({ ...API_TRIO, appleApiKeyPath: "/keys/AuthKey.p8" })),
    true,
  );
  assertEquals(hasAppStoreConnectApi(blankEnv({ ...API_TRIO, appleApiKeyB64: "cDg=" })), true);
  // Key material without the id/issuer is not enough.
  assertEquals(hasAppStoreConnectApi(blankEnv({ appleApiKeyB64: "cDg=" })), false);
});

Deno.test("resolveAppleEnv: blank env yields {} (nothing reaches a child process)", () => {
  assertEquals(resolveAppleEnv(blankEnv()), {});
});

Deno.test("resolveAppleEnv: only non-empty fields are emitted, team id under both names", () => {
  const env = blankEnv({
    appleTeamId: "TEAMID1234",
    appleApiKey: "ABC123DEF4",
    appleApiKeyPath: "/keys/AuthKey_ABC123DEF4.p8",
  });
  assertEquals(resolveAppleEnv(env), {
    APPLE_DEVELOPMENT_TEAM: "TEAMID1234",
    APPLE_TEAM_ID: "TEAMID1234",
    APPLE_API_KEY: "ABC123DEF4",
    APPLE_API_KEY_PATH: "/keys/AuthKey_ABC123DEF4.p8",
  });
});
