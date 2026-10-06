package com.chatx.monitor

import java.util.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.fail
import org.junit.Test

class MonitorCryptoTest {
    private fun config(
        deviceId: String = "dev_0123456789abcdef",
    ): PairingConfig {
        val key = ByteArray(32) { index -> (index + 1).toByte() }
        return PairingConfig(
            desktopId = "d_0123456789abcdef0123456789abcdef",
            port = 18432,
            fingerprintSha256 = "00".repeat(32),
            deviceId = deviceId,
            directToken = "11".repeat(32),
            deviceKey = Base64.getUrlEncoder()
                .withoutPadding()
                .encodeToString(key),
            directEndpoints = emptyList(),
            relay = null,
        )
    }

    private fun expectDecryptFailure(block: () -> Unit) {
        try {
            block()
            fail("expected authenticated decrypt to fail")
        } catch (_: Exception) {
            // AES-GCM authentication failure is the expected result.
        }
    }

    @Test
    fun controlRoundTripBindsDeviceDirectionAndTimeWindow() {
        val config = config()
        val clear = """{"action":"refresh_snapshot"}"""
            .toByteArray(Charsets.UTF_8)
        val frame = MonitorCrypto.encryptControlBytes(
            config = config,
            requestId = "c_0123456789abcdef",
            direction = "request",
            issuedAt = 1_000L,
            expiresAt = 31_000L,
            plaintext = clear,
        )

        assertArrayEquals(
            clear,
            MonitorCrypto.decryptControlBytes(config, "request", frame),
        )
        expectDecryptFailure {
            MonitorCrypto.decryptControlBytes(config, "response", frame)
        }
        expectDecryptFailure {
            MonitorCrypto.decryptControlBytes(
                config(deviceId = "dev_fedcba9876543210"),
                "request",
                frame,
            )
        }
        expectDecryptFailure {
            MonitorCrypto.decryptControlBytes(
                config,
                "request",
                frame.copy(expiresAt = frame.expiresAt + 1L),
            )
        }
    }
}
