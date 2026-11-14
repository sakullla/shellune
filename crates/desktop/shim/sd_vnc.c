/* VNC backend: LibVNCClient behind the shim's opaque handle.
 *
 * Built only into builds that link the vendored libvncserver (SD_WITH_VNC).
 * The socket is created here rather than by the library so a cancel can break a
 * blocking connect or handshake without racing the library's own state.
 */
#include <winsock2.h>
#include <ws2tcpip.h>

#include "sd_internal.h"

#ifdef SD_WITH_VNC

#include <rfb/rfbclient.h>
/* rfbLog/rfbLogProc live in the server header; the client header only includes the
 * protocol one. Redirecting the log is the only way LibVNCClient reports why a
 * handshake was refused. */
#include <rfb/rfb.h>

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define SD_CONNECT_TIMEOUT_MS 10000u
#define SD_POLL_SLICE_MS 20u
/* Bounds a single blocking recv during a handshake or the security probe.
 *
 * Abort normally ends such a read by shutting the socket down, which makes recv
 * return 0 at once. This timeout is the backstop for the case where it does not
 * reach that socket, so a cancel can never leave the worker parked in recv: the
 * read fails within this bound, the handshake reports failure, and the failure
 * path reads the cancel flag. It is cleared again once the peer has answered, so
 * a healthy session keeps its unlimited reads.
 *
 * Sized for the slowest legitimate handshake read (a TLS handshake step on a slow
 * link), not for promptness: promptness comes from the shutdown, which is what
 * makes a cancel land in milliseconds. */
#define SD_HANDSHAKE_RECV_TIMEOUT_MS 2000u

typedef struct sd_vnc_session {
  sd_client *client;
  rfbClient *rfb;
  SOCKET sock;
  int32_t connected;
  int32_t limit_hit;
  char reason[256];
} sd_vnc_session;

static char sd_vnc_tag;
static INIT_ONCE sd_vnc_wsa_once = INIT_ONCE_STATIC_INIT;
static int32_t sd_vnc_wsa_ready;

static BOOL CALLBACK sd_vnc_wsa_init(PINIT_ONCE once, PVOID param, PVOID *context) {
  (void)once;
  (void)param;
  (void)context;
  WSADATA data;
  sd_vnc_wsa_ready = WSAStartup(MAKEWORD(2, 2), &data) == 0 ? 1 : 0;
  return TRUE;
}

static sd_vnc_session *sd_vnc_of(rfbClient *rfb) {
  if (rfb == NULL) return NULL;
  return (sd_vnc_session *)rfbClientGetClientData(rfb, &sd_vnc_tag);
}

/* LibVNCClient routes its messages through these two globals. The build links the
 * client half only, so the shim provides them: a silent default means a log call can
 * neither crash nor print, and the shim does not try to parse library log text.
 *
 * A failed handshake is classified from the protocol itself (see
 * sd_vnc_probe_security_types) instead of from the library's messages, because the
 * log path is not a documented interface. */
static void sd_vnc_quiet(const char *format, ...) { (void)format; }
rfbLogProc rfbLog = sd_vnc_quiet;
rfbLogProc rfbErr = sd_vnc_quiet;

/* ---- callbacks ------------------------------------------------------------ */

static void sd_vnc_frame(rfbClient *rfb, int x, int y, int w, int h) {
  sd_vnc_session *session = sd_vnc_of(rfb);
  if (session == NULL || rfb->frameBuffer == NULL || w <= 0 || h <= 0) return;
  uint32_t bytes_per_pixel = rfb->format.bitsPerPixel / 8u;
  if (bytes_per_pixel == 0) return;
  uint32_t width = (uint32_t)rfb->width;
  uint32_t height = (uint32_t)rfb->height;
  if (width == 0 || height == 0) return;
  uint32_t stride = width * bytes_per_pixel;
  if (stride == 0) return;

  int32_t left = x < 0 ? 0 : x;
  int32_t top = y < 0 ? 0 : y;
  int32_t right = x + w;
  int32_t bottom = y + h;
  if (right > (int32_t)width) right = (int32_t)width;
  if (bottom > (int32_t)height) bottom = (int32_t)height;
  if (right <= left || bottom <= top) return;
  if ((uint64_t)(right - left) * (uint64_t)(bottom - top) * bytes_per_pixel >
      SD_MAX_FRAME_BYTES) {
    session->limit_hit = 1;
    return;
  }
  const uint8_t *pixels =
      rfb->frameBuffer + (size_t)top * stride + (size_t)left * bytes_per_pixel;
  sd_emit_frame(session->client, left, top, right - left, bottom - top, pixels, stride,
                width, height);
}

static void sd_vnc_cut_text(rfbClient *rfb, const char *text, int len) {
  sd_vnc_session *session = sd_vnc_of(rfb);
  if (session == NULL || text == NULL || len <= 0) return;
  if ((uint32_t)len > SD_MAX_CLIPBOARD_BYTES) len = (int)SD_MAX_CLIPBOARD_BYTES;
  sd_emit_clipboard(session->client, text, (uint32_t)len);
}

static rfbBool sd_vnc_malloc_framebuffer(rfbClient *rfb) {
  sd_vnc_session *session = sd_vnc_of(rfb);
  uint32_t bytes_per_pixel = rfb->format.bitsPerPixel / 8u;
  uint64_t bytes = (uint64_t)rfb->width * (uint64_t)rfb->height * bytes_per_pixel;
  if (bytes == 0 || bytes > SD_MAX_FRAME_BYTES) {
    if (session != NULL) {
      session->limit_hit = 1;
      snprintf(session->reason, sizeof(session->reason),
               "remote framebuffer of %ux%u exceeds the %u byte bound", rfb->width,
               rfb->height, (unsigned)SD_MAX_FRAME_BYTES);
    }
    return FALSE;
  }
  rfb->frameBuffer = (uint8_t *)calloc(1, (size_t)bytes);
  return rfb->frameBuffer != NULL ? TRUE : FALSE;
}

static rfbCredential *sd_vnc_credential(rfbClient *rfb, int credential_type) {
  sd_vnc_session *session = sd_vnc_of(rfb);
  if (session == NULL || credential_type != rfbCredentialTypeUser) return NULL;
  char *password = NULL;
  char *username = NULL;
  EnterCriticalSection(&session->client->lock);
  if (session->client->password != NULL) password = sd_strdup(session->client->password);
  if (session->client->username != NULL) username = sd_strdup(session->client->username);
  LeaveCriticalSection(&session->client->lock);
  if (password == NULL) return NULL;
  rfbCredential *credential = (rfbCredential *)calloc(1, sizeof(rfbCredential));
  if (credential == NULL) {
    free(password);
    free(username);
    return NULL;
  }
  credential->userCredential.username = username;
  credential->userCredential.password = password;
  return credential;
}

/* ---- socket --------------------------------------------------------------- */

/* Connects with our own timeout and cancel checkpoints, so an unreachable or
 * silent server cannot hold the worker past the deadline or a cancel. */
static SOCKET sd_vnc_connect(sd_client *client, const char *host, uint16_t port,
                             sd_vnc_session *session) {
  struct addrinfo hints;
  memset(&hints, 0, sizeof(hints));
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  hints.ai_protocol = IPPROTO_TCP;

  char service[8];
  snprintf(service, sizeof(service), "%u", (unsigned)port);
  struct addrinfo *resolved = NULL;
  int rc = getaddrinfo(host, service, &hints, &resolved);
  if (rc != 0 || resolved == NULL) {
    snprintf(session->reason, sizeof(session->reason), "could not resolve %s (%d)", host, rc);
    return INVALID_SOCKET;
  }

  SOCKET result = INVALID_SOCKET;
  for (struct addrinfo *it = resolved; it != NULL && result == INVALID_SOCKET; it = it->ai_next) {
    SOCKET sock = socket(it->ai_family, it->ai_socktype, it->ai_protocol);
    if (sock == INVALID_SOCKET) continue;
    u_long nonblocking = 1;
    ioctlsocket(sock, FIONBIO, &nonblocking);

    int32_t connected = connect(sock, it->ai_addr, (int)it->ai_addrlen) == 0 ? 1 : 0;
    int32_t last_error = WSAGetLastError();
    if (!connected && last_error == WSAEWOULDBLOCK) {
      DWORD elapsed = 0;
      while (elapsed < SD_CONNECT_TIMEOUT_MS) {
        if (sd_is_cancelled(client)) {
          last_error = WSAEINTR;
          break;
        }
        fd_set writable;
        fd_set failed;
        FD_ZERO(&writable);
        FD_ZERO(&failed);
        FD_SET(sock, &writable);
        FD_SET(sock, &failed);
        struct timeval wait;
        wait.tv_sec = 0;
        wait.tv_usec = 100 * 1000;
        int ready = select(0, NULL, &writable, &failed, &wait);
        elapsed += 100;
        if (ready <= 0) continue;
        int32_t so_error = 0;
        int length = sizeof(so_error);
        getsockopt(sock, SOL_SOCKET, SO_ERROR, (char *)&so_error, &length);
        last_error = so_error;
        connected = so_error == 0 ? 1 : 0;
        break;
      }
      if (!connected && last_error == 0) last_error = WSAETIMEDOUT;
    }

    if (connected) {
      nonblocking = 0;
      ioctlsocket(sock, FIONBIO, &nonblocking);
      result = sock;
      break;
    }
    closesocket(sock);
    snprintf(session->reason, sizeof(session->reason), "connect to %s:%u failed (%d)", host,
             (unsigned)port, last_error);
  }
  freeaddrinfo(resolved);
  return result;
}

/* Which of our security bits a server type corresponds to, or 0 when it is a type
 * this build cannot negotiate at all. */
static uint32_t sd_vnc_security_bit(uint8_t type) {
  switch (type) {
    case rfbNoAuth:
      return SD_VNC_SECURITY_NONE;
    case rfbVncAuth:
      return SD_VNC_SECURITY_VNC_AUTH;
#if defined(LIBVNCSERVER_HAVE_LIBSSL) || defined(LIBVNCSERVER_HAVE_GNUTLS)
    case rfbTLS:
      return SD_VNC_SECURITY_TLS;
    case rfbVeNCrypt:
      return SD_VNC_SECURITY_VENCRYPT;
#endif
    default:
      return 0;
  }
}

/* Asks a peer that just refused our handshake which security types it offers, on its
 * own short lived connection. This runs only after a failure, and answers exactly one
 * question: is every type it offers one this build refuses? Guessing from the
 * library's log text is not an interface; this is the protocol itself.
 *
 * The reads are bounded and the cancel flag is checked between them, because this
 * runs on the worker thread: a peer that accepts and then says nothing must not be
 * able to keep the worker alive past a cancel. */
static int32_t sd_vnc_probe_rejects_our_security(sd_client *client, const char *host,
                                                 uint16_t port, uint32_t mask, char *offered,
                                                 size_t cap) {
  if (host == NULL) return 0;
  sd_vnc_session scratch;
  memset(&scratch, 0, sizeof(scratch));
  scratch.sock = INVALID_SOCKET;

  SOCKET sock = sd_vnc_connect(client, host, port, &scratch);
  if (sock == INVALID_SOCKET) return 0;

  DWORD recv_timeout = SD_HANDSHAKE_RECV_TIMEOUT_MS;
  setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&recv_timeout,
             (int)sizeof(recv_timeout));

  int32_t rejected = 0;
  char version[12];
  if (!sd_is_cancelled(client) &&
      recv(sock, version, sizeof(version), MSG_WAITALL) == (int)sizeof(version)) {
    static const char ours[] = "RFB 003.008\n";
    if (send(sock, ours, (int)sizeof(ours) - 1, 0) == (int)sizeof(ours) - 1) {
      uint8_t count = 0;
      int got_count = recv(sock, (char *)&count, 1, MSG_WAITALL);
          if (got_count == 1 && count > 0) {
        uint8_t types[64];
        int32_t wanted = count < (uint8_t)sizeof(types) ? count : (int32_t)sizeof(types);
        if (recv(sock, (char *)types, wanted, MSG_WAITALL) == wanted) {
          uint32_t supported = 0;
          size_t used = 0;
          for (int32_t i = 0; i < wanted; i++) {
            uint32_t bit = sd_vnc_security_bit(types[i]);
            if (bit != 0 && (mask & bit) != 0) supported = bit;
            used += (size_t)snprintf(offered + used, cap > used ? cap - used : 0, i == 0 ? "%u" : ",%u",
                                     (unsigned)types[i]);
            if (used >= cap) break;
          }
                  /* Only a peer that offers nothing we accept is certain to be a security
           * mismatch; anything else stays a plain handshake failure. */
          rejected = supported == 0 ? 1 : 0;
        }
      }
    }
  }
  shutdown(sock, SD_BOTH);
  closesocket(sock);
  return rejected;
}

/* ---- backend entry points ------------------------------------------------- */

void sd_vnc_run(sd_client *client) {
  InitOnceExecuteOnce(&sd_vnc_wsa_once, sd_vnc_wsa_init, NULL, NULL);
  if (!sd_vnc_wsa_ready) {
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0,
                  "could not initialise Winsock");
    return;
  }

  sd_vnc_session *session = (sd_vnc_session *)calloc(1, sizeof(sd_vnc_session));
  if (session == NULL) {
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0,
                  "could not allocate the VNC session");
    return;
  }
  session->client = client;
  session->sock = INVALID_SOCKET;

  char *host = NULL;
  uint16_t port = 5900;
  uint32_t mask = SD_VNC_SECURITY_DEFAULT;
  uint32_t bytes_per_pixel = 4;
  uint32_t width = 0;
  uint32_t height = 0;
  EnterCriticalSection(&client->lock);
  host = sd_strdup(client->host);
  port = client->port != 0 ? client->port : 5900;
  mask = client->vnc_mask;
  bytes_per_pixel = client->vnc_bytes_per_pixel;
  width = client->width;
  height = client->height;
  LeaveCriticalSection(&client->lock);

  if (host == NULL) {
    free(session);
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0, "no host configured");
    return;
  }

  int32_t failure = SD_FAILURE_NONE;
  SOCKET sock = sd_vnc_connect(client, host, port, session);
  if (sock == INVALID_SOCKET) {
    failure = sd_is_cancelled(client) ? SD_FAILURE_CANCELLED : SD_FAILURE_CONNECT;
    if (session->reason[0] == '\0') {
      snprintf(session->reason, sizeof(session->reason), "VNC connect failed");
    }
    goto finish;
  }
  session->sock = sock;

  /* Published as soon as the socket exists, before rfbGetClient and before the
   * blocking handshake. This is what removes the window the old code had: there is
   * no moment after the connection is up where client->backend is still NULL, so
   * abort always finds a session whose socket it can shut down.
   *
   * A cancel that arrived before this point set the flag before it called abort
   * (sd_client_cancel writes the flag first), so the check below sees it; a cancel
   * that arrives after it finds the published session. Neither ordering leaves the
   * worker parked in a read. */
  sd_backend_publish(client, session);
  if (sd_is_cancelled(client)) {
    failure = SD_FAILURE_CANCELLED;
    snprintf(session->reason, sizeof(session->reason), "connect cancelled");
    goto finish;
  }

  rfbClient *rfb = rfbGetClient(8, 3, bytes_per_pixel);
  if (rfb == NULL) {
    failure = SD_FAILURE_INTERNAL;
    snprintf(session->reason, sizeof(session->reason), "could not create a LibVNCClient client");
    goto finish;
  }
  session->rfb = rfb;
  rfbClientSetClientData(rfb, &sd_vnc_tag, session);

  /* The library would connect on its own; it is told a connection already
   * exists, so the socket above stays the single owner of the deadline. */
  rfb->sock = sock;
  rfb->listenSpecified = TRUE;
  rfb->serverHost = sd_strdup(host);
  rfb->serverPort = (int)port;
  rfb->connectTimeout = SD_CONNECT_TIMEOUT_MS;
  rfb->canHandleNewFBSize = TRUE;
  rfb->MallocFrameBuffer = sd_vnc_malloc_framebuffer;
  rfb->GotFrameBufferUpdate = sd_vnc_frame;
  rfb->GotXCutText = sd_vnc_cut_text;
  rfb->GetCredential = sd_vnc_credential;
  /* Only encodings this integration actually decodes end to end are requested. */
  rfb->appData.encodingsString = sd_strdup("raw copyrect rre hextile zrle");

  rfb->format.bitsPerPixel = 32;
  rfb->format.depth = 24;
  rfb->format.redShift = 16;
  rfb->format.greenShift = 8;
  rfb->format.blueShift = 0;
  rfb->format.redMax = 255;
  rfb->format.greenMax = 255;
  rfb->format.blueMax = 255;
  rfb->format.bigEndian = FALSE;
  rfb->format.trueColour = TRUE;

  {
    uint32_t schemes[4];
    int count = 0;
    if ((mask & SD_VNC_SECURITY_NONE) != 0) schemes[count++] = rfbNoAuth;
    if ((mask & SD_VNC_SECURITY_VNC_AUTH) != 0) schemes[count++] = rfbVncAuth;
#if defined(LIBVNCSERVER_HAVE_LIBSSL) || defined(LIBVNCSERVER_HAVE_GNUTLS)
    if ((mask & SD_VNC_SECURITY_TLS) != 0) schemes[count++] = rfbTLS;
    if ((mask & SD_VNC_SECURITY_VENCRYPT) != 0) schemes[count++] = rfbVeNCrypt;
#endif
    SetClientAuthSchemes(rfb, schemes, count);
  }

  /* One bounded recv per handshake step, so the read above cannot park the worker
   * forever if the abort's shutdown never reaches this socket. Cleared after the
   * peer has answered, below, because a live session reads whole messages. */
  {
    DWORD recv_timeout = SD_HANDSHAKE_RECV_TIMEOUT_MS;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&recv_timeout,
               (int)sizeof(recv_timeout));
  }

  rfbBool initialised = rfbInitClient(rfb, NULL, NULL);

  if (!initialised) {
    /* LibVNCClient frees the client and closes its socket itself when the handshake
     * fails, so the handle must not be touched or cleaned up again. */
    session->rfb = NULL;
    /* Retire before the socket value is discarded and before the probe below opens
     * a socket of its own: once this returns no thread is inside sd_vnc_abort with
     * this session, so a stale SOCKET value cannot be handed to shutdown() after
     * Windows has recycled the number for the probe. */
    sd_backend_retire(client);
    session->sock = INVALID_SOCKET;
    if (sd_is_cancelled(client)) {
      failure = SD_FAILURE_CANCELLED;
      snprintf(session->reason, sizeof(session->reason), "connect cancelled");
    } else if (session->limit_hit) {
      failure = SD_FAILURE_LIMIT;
    } else {
      char offered[96];
      if (sd_vnc_probe_rejects_our_security(client, host, port, mask, offered,
                                            sizeof(offered))) {
        failure = SD_FAILURE_UNSUPPORTED_SECURITY;
        snprintf(session->reason, sizeof(session->reason),
                 "the server offers only security types this build refuses (%s)", offered);
      } else {
        failure = SD_FAILURE_CONNECT;
        if (session->reason[0] == '\0') {
          snprintf(session->reason, sizeof(session->reason), "VNC handshake failed");
        }
      }
    }
    goto finish;
  }

  {
    DWORD recv_timeout = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&recv_timeout,
               (int)sizeof(recv_timeout));
  }

  /* Ask once for the whole framebuffer at the requested size. */
  if (width != 0 && height != 0) {
    SendFramebufferUpdateRequest(rfb, 0, 0, (int)width, (int)height, FALSE);
  }

  session->connected = 1;
  sd_emit_state(client, SD_STATE_CONNECTED, SD_FAILURE_NONE, 0, NULL);
  failure = SD_FAILURE_NONE;

  while (!sd_is_cancelled(client)) {
    int ready = WaitForMessage(rfb, SD_POLL_SLICE_MS * 1000);
    if (ready < 0) {
      failure = SD_FAILURE_IO;
      snprintf(session->reason, sizeof(session->reason), "reading from the VNC server failed");
      break;
    }
    if (ready > 0) {
      rfbBool handled = HandleRFBServerMessage(rfb);
      if (!handled) {
        if (session->limit_hit) {
          failure = SD_FAILURE_LIMIT;
        } else {
          failure = SD_FAILURE_REMOTE_CLOSED;
          snprintf(session->reason, sizeof(session->reason), "the VNC server closed the session");
        }
        break;
      }
    }
    sd_flush_input(client);
  }
  if (sd_is_cancelled(client)) {
    failure = SD_FAILURE_CANCELLED;
    snprintf(session->reason, sizeof(session->reason), "cancelled");
  }

  /* Retire first: after it returns no abort can be inside shutdown() on this
   * socket, so the cleanup below is free to close it. */
  sd_backend_retire(client);
  session->connected = 0;
  /* rfbClientCleanup closes rfb->sock, which is the socket created above. */
  session->sock = INVALID_SOCKET;
  rfbClientCleanup(rfb);
  session->rfb = NULL;

finish:
  /* Also reached by the failure paths, which may not have retired yet. Idempotent:
   * it clears the pointer (there is none by now in the normal case) and waits out
   * any reader, so client->backend never outlives the session it points at. */
  sd_backend_retire(client);
  if (session->sock != INVALID_SOCKET) {
    closesocket(session->sock);
    session->sock = INVALID_SOCKET;
  }
  free(host);

  if (failure == SD_FAILURE_CANCELLED) {
    sd_emit_state(client, SD_STATE_CLOSED, SD_FAILURE_CANCELLED, 0,
                  session->reason[0] != '\0' ? session->reason : "cancelled");
  } else if (failure == SD_FAILURE_NONE) {
    sd_emit_state(client, SD_STATE_CLOSED, SD_FAILURE_NONE, 0, "session ended");
  } else {
    sd_emit_state(client, SD_STATE_FAILED, failure, 0,
                  session->reason[0] != '\0' ? session->reason : "VNC session failed");
  }
  free(session);
}

void sd_vnc_abort(sd_client *client) {
  /* The reference is held across shutdown(), so the worker cannot free the session
   * (and close its socket) in the middle of this call. */
  sd_vnc_session *session = (sd_vnc_session *)sd_backend_acquire(client);
  if (session == NULL) return;
  /* shutdown, not closesocket: the worker still owns the handle and will close it
   * after its blocking read returns. */
  if (session->sock != INVALID_SOCKET) shutdown(session->sock, SD_BOTH);
  sd_backend_release(client);
}

static rfbClient *sd_vnc_live(sd_client *client, sd_vnc_session **out_session) {
  /* Reached from sd_flush_input only, which runs on the worker thread - the same
   * thread that retires the session - so the returned pointer cannot be freed
   * underneath the caller. It is read under backend_lock so it is never a value
   * published by another thread without that lock. */
  EnterCriticalSection(&client->backend_lock);
  sd_vnc_session *session = (sd_vnc_session *)client->backend;
  LeaveCriticalSection(&client->backend_lock);
  if (out_session != NULL) *out_session = session;
  if (session == NULL || session->rfb == NULL) return NULL;
  return session->rfb;
}

void sd_vnc_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                int32_t extended) {
  rfbClient *rfb = sd_vnc_live(client, NULL);
  /* RDP's scancode form is meaningless here: RFB carries X11 keysyms. */
  (void)scancode;
  (void)extended;
  if (rfb == NULL || keysym == 0) return;
  SendKeyEvent(rfb, keysym, down ? TRUE : FALSE);
}

void sd_vnc_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons) {
  rfbClient *rfb = sd_vnc_live(client, NULL);
  if (rfb == NULL) return;
  int mask = 0;
  if ((buttons & SD_BUTTON_LEFT) != 0) mask |= 0x1;
  if ((buttons & SD_BUTTON_MIDDLE) != 0) mask |= 0x2;
  if ((buttons & SD_BUTTON_RIGHT) != 0) mask |= 0x4;
  if (!SendPointerEvent(rfb, (int)x, (int)y, mask)) return;
  if ((buttons & SD_BUTTON_WHEEL_UP) != 0) {
    SendPointerEvent(rfb, (int)x, (int)y, mask | 0x8);
    SendPointerEvent(rfb, (int)x, (int)y, mask);
  }
  if ((buttons & SD_BUTTON_WHEEL_DOWN) != 0) {
    SendPointerEvent(rfb, (int)x, (int)y, mask | 0x10);
    SendPointerEvent(rfb, (int)x, (int)y, mask);
  }
}

void sd_vnc_clipboard(sd_client *client, const char *utf8, uint32_t len) {
  rfbClient *rfb = sd_vnc_live(client, NULL);
  if (rfb == NULL || utf8 == NULL || len == 0) return;
  SendClientCutText(rfb, (char *)utf8, (int)len);
}

sd_status sd_vnc_available(char *out, size_t cap) {
  const char *tls =
#if defined(LIBVNCSERVER_HAVE_LIBSSL)
      "openssl";
#elif defined(LIBVNCSERVER_HAVE_GNUTLS)
      "gnutls";
#else
      "none";
#endif
  /* Built from the same compile-time conditions sd_vnc_security_bit() switches on,
   * so the string cannot advertise a type the handshake would refuse. "vencrypt" is
   * the RFB security type name; the old "vencrypt-auth" name does not exist. */
  char security[48];
  int used = snprintf(security, sizeof(security), "none,vncauth");
#if defined(LIBVNCSERVER_HAVE_LIBSSL) || defined(LIBVNCSERVER_HAVE_GNUTLS)
  if (used > 0 && (size_t)used < sizeof(security)) {
    used += snprintf(security + used, sizeof(security) - (size_t)used, ",tls,vencrypt");
  }
#endif
  int written = snprintf(out, cap,
                         "libvncclient=%s tls=%s security=%s "
                         "encodings=raw,copyrect,rre,hextile,zrle",
                         LIBVNCSERVER_VERSION, tls, security);
  if (written < 0) return SD_ERR_INTERNAL;
  if ((size_t)written >= cap) return SD_ERR_ARGUMENT;
  return SD_OK;
}

#endif /* SD_WITH_VNC */