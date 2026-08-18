package com.rzkyif.loca

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * AES-256-GCM sealing whose key lives in the AndroidKeyStore.
 *
 * This is the ONE primitive behind both plaintext-secret stores on Android:
 * the Client's paired-core bearer tokens (`commands/keychain.rs`) and the
 * embedded Core's secrets-vault master key (`secrets/store.rs`). The master key
 * unseals the vault and the vault holds the TLS private key, so it is the
 * pairing trust root. Two independent KeyStore integrations is exactly the
 * shape that has already drifted elsewhere in this repo, so there is one.
 *
 * Called from Rust over JNI. It is Kotlin rather than ~20 hand-written JNI
 * calls for a reason that is about risk rather than line count: there is no
 * device testing in this project until the very end, and JNI mistakes surface
 * only on a device. The KeyStore ceremony belongs in the language whose API it
 * is.
 *
 * **This class deals in raw bytes and owns no file format.** `seal` returns
 * `iv || ciphertext-with-tag` and `unseal` takes the same. The on-disk framing,
 * its version marker and its base64 all live on the Rust side
 * (`secrets/android.rs`), so there is no constant shared across the language
 * boundary that could drift.
 *
 * **Do not delete this file.** `tauri android init` regenerates the tree around
 * it, and a silently-missing SecretBox would drop both stores back to
 * plaintext. Its absence is a COMPILE error (`commands/keychain.rs` pulls it in
 * with `include_str!`) and gutting it fails
 * `the_kotlin_secret_box_still_declares_what_the_jni_layer_calls`.
 */
object SecretBox {
  private const val PROVIDER = "AndroidKeyStore"
  private const val TRANSFORM = "AES/GCM/NoPadding"
  private const val IV_LEN = 12
  private const val TAG_BITS = 128

  private fun store(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

  /** The existing key for this alias, or null when there is none to be had.
   *  An invalidated key reads as absent rather than throwing, because the
   *  caller's answer is the same either way: this envelope cannot be opened. */
  private fun existingKey(alias: String): SecretKey? =
    try {
      (store().getEntry(alias, null) as? KeyStore.SecretKeyEntry)?.secretKey
    } catch (_: Exception) {
      null
    }

  /** The key for this alias, generating one on first use.
   *
   *  `setUserAuthenticationRequired(false)` is deliberate: the Core runs with
   *  the screen locked and a key gated on user presence would wedge every
   *  background turn. `setRandomizedEncryptionRequired(true)` makes the
   *  provider pick the IV, so no caller can reuse one, which under GCM is a
   *  total break rather than a weakness. */
  private fun key(alias: String): SecretKey {
    existingKey(alias)?.let { return it }
    val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
    gen.init(
      KeyGenParameterSpec.Builder(
        alias,
        KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
      )
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .setUserAuthenticationRequired(false)
        .setRandomizedEncryptionRequired(true)
        .build(),
    )
    return gen.generateKey()
  }

  /** Seal, generating the key on first use. Returns `iv || ciphertext+tag`.
   *  Throws on any provider failure; the Rust side turns that into the loud
   *  fallback rather than failing the boot. */
  @JvmStatic
  fun seal(alias: String, plaintext: ByteArray): ByteArray {
    val cipher = Cipher.getInstance(TRANSFORM)
    cipher.init(Cipher.ENCRYPT_MODE, key(alias))
    val ciphertext = cipher.doFinal(plaintext)
    val iv = cipher.iv
    if (iv.size != IV_LEN) throw IllegalStateException("provider returned a ${iv.size}-byte IV")
    return iv + ciphertext
  }

  /** Open `iv || ciphertext+tag` that this device sealed.
   *
   *  Throws when the input is truncated, fails its tag, or when the key is
   *  gone. That last case is reachable without an attacker: a device-wide
   *  credential reset and some restore-to-new-device flows destroy KeyStore
   *  keys. It deliberately does NOT generate a key here, because a freshly
   *  generated one would turn "the key is gone" into a tag failure and lose the
   *  distinction the caller logs. */
  @JvmStatic
  fun unseal(alias: String, sealed: ByteArray): ByteArray {
    if (sealed.size <= IV_LEN) throw IllegalArgumentException("sealed value is too short")
    val secret = existingKey(alias) ?: throw IllegalStateException("no keystore key for $alias")
    val iv = sealed.copyOfRange(0, IV_LEN)
    val ciphertext = sealed.copyOfRange(IV_LEN, sealed.size)
    val cipher = Cipher.getInstance(TRANSFORM)
    cipher.init(Cipher.DECRYPT_MODE, secret, GCMParameterSpec(TAG_BITS, iv))
    return cipher.doFinal(ciphertext)
  }
}
