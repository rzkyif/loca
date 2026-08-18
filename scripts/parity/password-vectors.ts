// Generates the admin-password fixtures the Rust port must still verify.
//
// `.admin-password` is written once, at install, and read forever after. If the
// Rust argon2id parameters, the version, or the on-disk format drift by one
// field, every install that set a password loses the remote-pairing path with no
// diagnosis beyond "the password is wrong". So the fixtures are produced by the
// real `hashAdminPassword`, not by a second reading of its format.
//
// Deterministic where it can be: the salt inside `hashAdminPassword` comes from
// the CSPRNG, so re-running this produces different (equally valid) strings.
// Regenerating is therefore safe but not reproducible byte for byte, which is
// why the file records what each case is FOR.
//
// Usage:
//   deno run --allow-read --allow-write \
//     scripts/parity/password-vectors.ts \
//     packages/loca-core-rs/src/auth/testdata/password-vectors.json

// Fully specified rather than bare: this script sits outside the workspace
// members, so no import map applies to its own imports. The versions match
// `packages/loca-core/deno.json`, which is what produced every stored hash.
import { argon2id } from "npm:@noble/hashes@^2.2.0/argon2.js";
import { encodeBase64 } from "jsr:@std/encoding@^1.0.0/base64";

import { hashAdminPassword } from "../../packages/loca-core/src/shared/password.ts";

interface Vector {
  note: string;
  password: string;
  stored: string;
}

const vectors: Vector[] = [
  { note: "the shipping profile, a memorable passphrase", password: "correct horse battery" },
  { note: "the shipping profile, the 8-character minimum", password: "hunter22" },
  { note: "the shipping profile, punctuation and spaces", password: "a b!c#d$e%f^g&h*i(j)" },
  { note: "the shipping profile, two-byte code points", password: "pässwörd-señor" },
  { note: "the shipping profile, three- and four-byte code points", password: "合言葉\u{1f512}" },
  { note: "the shipping profile, the 256-character maximum", password: "x".repeat(256) },
].map((c) => ({ ...c, stored: hashAdminPassword(c.password) }));

// A hash written under a DIFFERENT cost profile, proving the verifier reads
// m/t/p from the stored string rather than from its own constants. This is what
// keeps an existing install working if the profile is ever raised.
{
  const password = "written-under-an-older-profile";
  const salt = new Uint8Array(16).fill(0x5a);
  const hash = argon2id(new TextEncoder().encode(password), salt, { m: 8, t: 1, p: 1, dkLen: 32 });
  vectors.push({
    note: "a lower cost profile: parameters must come from the stored string",
    password,
    stored: `argon2id$m=8,t=1,p=1$${encodeBase64(salt)}$${encodeBase64(hash)}`,
  });
}

const out = Deno.args[0];
if (!out) throw new Error("usage: password-vectors.ts <output.json>");
await Deno.writeTextFile(
  out,
  JSON.stringify(
    {
      source: "packages/loca-core/src/shared/password.ts",
      generator: "scripts/parity/password-vectors.ts",
      note: "argon2id, version 0x13, m=19456 KiB, t=2, p=1, 32-byte output, 16-byte salt, no secret key and no associated data. The format is not PHC: padded standard base64, no v= field, no leading $.",
      vectors,
    },
    null,
    2,
  ) + "\n",
);
console.log(`wrote ${vectors.length} vectors to ${out}`);
