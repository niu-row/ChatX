package com.chatx.monitor

import android.util.Base64
import java.security.SecureRandom
import org.json.JSONObject
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

data class EncryptedPairingFrame(
    val nonce: String,
    val ciphertext: String,
) {
    fun toJson(): JSONObject = JSONObject().apply {
        put("nonce", nonce)
        put("ciphertext", ciphertext)
    }

    companion object {
        fun parse(root: JSONObject): EncryptedPairingFrame =
            EncryptedPairingFrame(
                nonce = root.getString("nonce"),
                ciphertext = root.getString("ciphertext"),
            )
    }
}

data class EncryptedSnapshotFrame(
    val sessionId: String,
    val sequence: Long,
    val generatedAt: Long,
    val nonce: String,
    val ciphertext: String,
) {
    companion object {
        fun parse(root: JSONObject): EncryptedSnapshotFrame {
            return EncryptedSnapshotFrame(
                sessionId = root.getString("sessionId"),
                sequence = root.getLong("sequence"),
                generatedAt = root.getLong("generatedAt"),
                nonce = root.getString("nonce"),
                ciphertext = root.getString("ciphertext"),
            )
        }
    }
}

object MonitorCrypto {
    fun decryptSnapshot(
        config: PairingConfig,
        frame: EncryptedSnapshotFrame,
    ): JSONObject {
        require(frame.sequence > 0L) { "Snapshot sequence 无效。" }
        require(frame.sessionId.startsWith("s_")) { "Snapshot session 无效。" }
        val key = decodeUrlBase64(config.deviceKey)
        require(key.size == 32) { "Device E2EE Key 长度无效。" }
        val nonce = decodeUrlBase64(frame.nonce)
        require(nonce.size == 12) { "Snapshot nonce 长度无效。" }
        val encrypted = decodeUrlBase64(frame.ciphertext)
        val aad = buildString {
            append("chatx-monitor-v1|snapshot|")
            append(config.desktopId)
            append('|')
            append(config.deviceId)
            append('|')
            append(frame.sessionId)
            append('|')
            append(frame.sequence)
            append('|')
            append(frame.generatedAt)
        }.toByteArray(Charsets.UTF_8)

        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(aad)
        val clear = cipher.doFinal(encrypted)
        return JSONObject(clear.toString(Charsets.UTF_8))
    }

    fun encryptPairing(
        pairingCode: String,
        pairingId: String,
        direction: String,
        payload: JSONObject,
    ): EncryptedPairingFrame {
        val key = decodeHex(pairingCode)
        require(key.size == 32) { "Pairing secret 长度无效。" }
        val nonce = ByteArray(12).also(SecureRandom()::nextBytes)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(pairingAad(pairingId, direction))
        val encrypted = cipher.doFinal(
            payload.toString().toByteArray(Charsets.UTF_8),
        )
        return EncryptedPairingFrame(
            nonce = encodeUrlBase64(nonce),
            ciphertext = encodeUrlBase64(encrypted),
        )
    }

    fun decryptPairing(
        pairingCode: String,
        pairingId: String,
        direction: String,
        frame: EncryptedPairingFrame,
    ): JSONObject {
        val key = decodeHex(pairingCode)
        require(key.size == 32) { "Pairing secret 长度无效。" }
        val nonce = decodeUrlBase64(frame.nonce)
        require(nonce.size == 12) { "Pairing nonce 长度无效。" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(pairingAad(pairingId, direction))
        val clear = cipher.doFinal(decodeUrlBase64(frame.ciphertext))
        return JSONObject(clear.toString(Charsets.UTF_8))
    }

    private fun pairingAad(pairingId: String, direction: String): ByteArray =
        "chatx-monitor-v1|pairing|$pairingId|$direction"
            .toByteArray(Charsets.UTF_8)

    private fun decodeHex(value: String): ByteArray {
        val normalized = value.trim().lowercase()
        require(normalized.length % 2 == 0) { "Hex 编码无效。" }
        return ByteArray(normalized.length / 2) { index ->
            normalized.substring(index * 2, index * 2 + 2)
                .toInt(16).toByte()
        }
    }

    private fun encodeUrlBase64(value: ByteArray): String =
        Base64.encodeToString(
            value,
            Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
        )

    private fun decodeUrlBase64(value: String): ByteArray =
        Base64.decode(
            value,
            Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
        )
}
