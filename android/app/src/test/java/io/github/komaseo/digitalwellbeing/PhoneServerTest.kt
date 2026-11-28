package io.github.komaseo.digitalwellbeing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.InetAddress
import java.net.ServerSocket
import kotlin.concurrent.thread

class PhoneServerTest {
    /** Responsibility: Answer one HTTP request with `status` and `answer`, and keep what it got. */
    private class OneAnswerServer(status: Int, answer: String) : AutoCloseable {
        private val socket = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val address = "http://127.0.0.1:${socket.localPort}"
        @Volatile var requestLine = ""
        @Volatile var contentType = ""
        @Volatile var body = ""
        private val worker = thread {
            socket.accept().use { connection ->
                val reader = BufferedReader(InputStreamReader(connection.getInputStream(), Charsets.UTF_8))
                requestLine = reader.readLine()
                var length = 0
                while (true) {
                    val line = reader.readLine() ?: break
                    if (line.isEmpty()) break
                    val (name, value) = line.split(":", limit = 2).map(String::trim)
                    if (name.equals("Content-Length", ignoreCase = true)) length = value.toInt()
                    if (name.equals("Content-Type", ignoreCase = true)) contentType = value
                }
                // The body is UTF-8; read characters until its bytes are all in.
                val received = StringBuilder()
                while (received.toString().toByteArray(Charsets.UTF_8).size < length) {
                    received.append(reader.read().toChar())
                }
                body = received.toString()
                val bytes = answer.toByteArray(Charsets.UTF_8)
                val head = "HTTP/1.1 $status Answer\r\nContent-Type: application/json\r\n" +
                    "Content-Length: ${bytes.size}\r\nConnection: close\r\n\r\n"
                connection.getOutputStream().apply {
                    write(head.toByteArray(Charsets.UTF_8))
                    write(bytes)
                    flush()
                }
            }
        }

        override fun close() {
            worker.join(5_000)
            socket.close()
        }
    }

    @Test
    fun typedAddressesBecomeTheServerAddress() {
        assertEquals("http://1.2.3.4:47911", PhoneServer.normalize(" 1.2.3.4:47911 "))
        assertEquals("http://sync.example.com:47911", PhoneServer.normalize("http://sync.example.com:47911/"))
        // No port: the sync server's, as on the PC.
        assertEquals("http://3.35.0.1:47820", PhoneServer.normalize("3.35.0.1"))
        assertEquals("https://sync.example.com:47820", PhoneServer.normalize("https://sync.example.com"))
        // The browser page's address from before the app.
        assertEquals("http://1.2.3.4:47911", PhoneServer.normalize("http://1.2.3.4:47911/phone"))
    }

    @Test
    fun whatIsNotAnAddressIsRefused() {
        assertNull(PhoneServer.normalize(""))
        assertNull(PhoneServer.normalize("   "))
        assertNull(PhoneServer.normalize("ftp://1.2.3.4"))
        assertNull(PhoneServer.normalize("http://"))
        assertNull(PhoneServer.normalize("http://1.2.3.4:47911/other"))
        assertNull(PhoneServer.normalize("not an address"))
    }

    @Test
    fun aRequestGoesToThePhoneApiAndTheAnswerComesBack() {
        OneAnswerServer(200, "{\"ok\":\"할 일\"}").use { server ->
            val reply = PhoneServer.post(server.address, "{\"action\":\"list\",\"token\":\"토큰\"}")
            server.close()
            assertEquals(200, reply.status)
            assertEquals("{\"ok\":\"할 일\"}", reply.body)
            assertEquals("POST /phone/api HTTP/1.1", server.requestLine)
            assertEquals("{\"action\":\"list\",\"token\":\"토큰\"}", server.body)
            assertEquals("application/json; charset=utf-8", server.contentType)
        }
    }

    @Test
    fun anErrorStatusKeepsItsBodyAndAnUnreachableServerIsStatusZero() {
        val address: String
        OneAnswerServer(413, "{\"error\":\"too large\"}").use { server ->
            address = server.address
            val reply = PhoneServer.post(server.address, "{}")
            assertEquals(413, reply.status)
            assertEquals("{\"error\":\"too large\"}", reply.body)
        }
        assertEquals(0, PhoneServer.post(address, "{}", timeoutMillis = 2_000).status)
    }
}
