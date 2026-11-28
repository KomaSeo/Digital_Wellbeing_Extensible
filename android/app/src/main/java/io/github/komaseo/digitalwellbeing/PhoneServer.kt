package io.github.komaseo.digitalwellbeing

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL

/**
 * Responsibility: Talk to the sync server's phone API (`POST <server>/phone/api`) for the to-do
 * page, and read the server address the user types. Plain JVM code, so it is tested off Android.
 */
object PhoneServer {
    /** What the server answered: the HTTP status and the body (0 and the error when unreachable). */
    data class Reply(val status: Int, val body: String)

    /** The sync server's port when an address has none (the PC's rule, `clean_server_url`). */
    const val DEFAULT_PORT = 47820

    /**
     * The server address the user typed, as "http://host:port" without a trailing slash, or null
     * when it is not an address. "1.2.3.4" gets "http://" and the default port; the old page
     * address ending in "/phone" (the browser page before the app) is taken as the server's.
     */
    fun normalize(typed: String): String? {
        var address = typed.trim()
        if (address.isEmpty()) return null
        if (!address.contains("://")) address = "http://$address"
        address = address.trimEnd('/')
        if (address.endsWith("/phone")) address = address.removeSuffix("/phone")
        val parsed = try {
            URI(address)
        } catch (error: Exception) {
            return null
        }
        if (parsed.scheme != "http" && parsed.scheme != "https") return null
        if (parsed.host.isNullOrEmpty()) return null
        if (!parsed.path.isNullOrEmpty() || parsed.query != null) return null
        return if (parsed.port == -1) "$address:$DEFAULT_PORT" else address
    }

    /** Send one request of the to-do page (a JSON text) and wait for the answer. */
    fun post(server: String, body: String, timeoutMillis: Int = 20_000): Reply {
        val connection = try {
            URL("$server/phone/api").openConnection() as HttpURLConnection
        } catch (error: Exception) {
            return Reply(0, error.toString())
        }
        return try {
            connection.requestMethod = "POST"
            connection.connectTimeout = timeoutMillis
            connection.readTimeout = timeoutMillis
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json; charset=utf-8")
            connection.outputStream.use { stream -> stream.write(body.toByteArray(Charsets.UTF_8)) }
            val status = connection.responseCode
            val stream = if (status >= 400) connection.errorStream else connection.inputStream
            val text = stream?.use { input -> input.readBytes().toString(Charsets.UTF_8) } ?: ""
            Reply(status, text)
        } catch (error: IOException) {
            Reply(0, error.toString())
        } finally {
            connection.disconnect()
        }
    }
}
