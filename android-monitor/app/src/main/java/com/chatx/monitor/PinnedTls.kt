package com.chatx.monitor

import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager
import okhttp3.OkHttpClient
import java.util.concurrent.TimeUnit

class PinnedTrustManager(
    private val expectedFingerprint: String,
) : X509TrustManager {
    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) = Unit

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) {
        val certificate = chain?.firstOrNull()
            ?: throw java.security.cert.CertificateException(
                "服务器未提供 TLS 证书。",
            )
        val digest = MessageDigest.getInstance("SHA-256")
            .digest(certificate.encoded)
        val actual = digest.joinToString("") { byte -> "%02x".format(byte) }
        if (!MessageDigest.isEqual(
                actual.toByteArray(Charsets.US_ASCII),
                expectedFingerprint.toByteArray(Charsets.US_ASCII),
            )
        ) {
            throw java.security.cert.CertificateException(
                "ChatX TLS 指纹不匹配。",
            )
        }
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

object WssClients {
    fun direct(fingerprintSha256: String): OkHttpClient {
        val trustManager = PinnedTrustManager(fingerprintSha256)
        val context = SSLContext.getInstance("TLS").apply {
            init(null, arrayOf(trustManager), SecureRandom())
        }
        return OkHttpClient.Builder()
            .sslSocketFactory(context.socketFactory, trustManager)
            .hostnameVerifier { _, _ -> true }
            .connectTimeout(4, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS)
            .pingInterval(15, TimeUnit.SECONDS)
            .build()
    }

    fun relay(): OkHttpClient =
        OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS)
            .pingInterval(15, TimeUnit.SECONDS)
            .build()
}
