/* Shellune native desktop shim: handle lifecycle, event dispatch, input queue.
 *
 * The two protocol backends (sd_rdp.c, sd_vnc.c) only ever speak the helpers
 * declared in sd_internal.h, so the rules about locking, cancellation and event
 * ordering live here once instead of twice.
 */
#include "sd_internal.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if !defined(_WIN64)
#error "sd_event offsets are asserted for 64-bit builds; update them with the type."
#endif

/* MSVC only has _Static_assert in C mode with /std:c11, which this library does not
 * require, so the portable typedef form is used. The Rust side asserts the same
 * numbers in tests/desktop_native.rs: changing a field without changing both fails
 * the build here rather than at run time. */
#define SD_ASSERT_LAYOUT(condition, name) \
  typedef char sd_layout_##name[(condition) ? 1 : -1]

SD_ASSERT_LAYOUT(sizeof(sd_event) == 120, event_size);
SD_ASSERT_LAYOUT(offsetof(sd_event, abi_version) == 0, event_abi_version);
SD_ASSERT_LAYOUT(offsetof(sd_event, text) == 32, event_text);
SD_ASSERT_LAYOUT(offsetof(sd_event, host) == 48, event_host);
SD_ASSERT_LAYOUT(offsetof(sd_event, pixels) == 112, event_pixels);

/* How long a certificate question may stay unanswered before the worker gives up
 * and rejects. The UI is expected to answer or cancel; this only bounds a leak. */
#define SD_CERT_WAIT_MS 120000
/* How long free() waits for a worker that was asked to stop. */
#define SD_FREE_WAIT_MS 15000

uint32_t sd_abi_version(void) { return SD_ABI_VERSION; }

const char *sd_status_name(int32_t status) {
  switch (status) {
    case SD_OK: return "ok";
    case SD_ERR_ARGUMENT: return "argument";
    case SD_ERR_UNAVAILABLE: return "unavailable";
    case SD_ERR_STATE: return "state";
    case SD_ERR_ABORTED: return "aborted";
    case SD_ERR_INTERNAL: return "internal";
    default: return "unknown";
  }
}

const char *sd_failure_name(int32_t failure) {
  switch (failure) {
    case SD_FAILURE_NONE: return "none";
    case SD_FAILURE_CONNECT: return "connect";
    case SD_FAILURE_TLS: return "tls";
    case SD_FAILURE_AUTHENTICATION: return "authentication";
    case SD_FAILURE_CERTIFICATE_REJECTED: return "certificate-rejected";
    case SD_FAILURE_UNSUPPORTED_SECURITY: return "unsupported-security";
    case SD_FAILURE_PROTOCOL: return "protocol";
    case SD_FAILURE_IO: return "io";
    case SD_FAILURE_CANCELLED: return "cancelled";
    case SD_FAILURE_REMOTE_CLOSED: return "remote-closed";
    case SD_FAILURE_LIMIT: return "limit";
    case SD_FAILURE_INTERNAL: return "internal";
    default: return "unknown";
  }
}

const char *sd_state_name(int32_t state) {
  switch (state) {
    case SD_STATE_IDLE: return "idle";
    case SD_STATE_CONNECTING: return "connecting";
    case SD_STATE_CONNECTED: return "connected";
    case SD_STATE_CLOSING: return "closing";
    case SD_STATE_CLOSED: return "closed";
    case SD_STATE_FAILED: return "failed";
    default: return "unknown";
  }
}

#if defined(SD_WITH_RDP) || defined(SD_WITH_VNC)
/* One façade, two backends. Linking both libraries into one binary means their
 * shared entry point names would otherwise collapse onto whichever object the linker
 * saw first, and an RDP session could quietly run the VNC code. */

sd_status sd_backend_available(int32_t proto, char *out, size_t cap) {
#if defined(SD_WITH_RDP)
  if (proto == SD_PROTOCOL_RDP) return sd_rdp_available(out, cap);
#endif
#if defined(SD_WITH_VNC)
  if (proto == SD_PROTOCOL_VNC) return sd_vnc_available(out, cap);
#endif
  snprintf(out, cap, "this build has no %s backend",
           proto == SD_PROTOCOL_RDP ? "RDP" : "VNC");
  return SD_ERR_UNAVAILABLE;
}

void sd_backend_run(sd_client *client) {
#if defined(SD_WITH_RDP)
  if (client->proto == SD_PROTOCOL_RDP) {
    sd_rdp_run(client);
    return;
  }
#endif
#if defined(SD_WITH_VNC)
  if (client->proto == SD_PROTOCOL_VNC) {
    sd_vnc_run(client);
    return;
  }
#endif
  sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0,
                "no backend for this protocol in this build");
}

void sd_backend_abort(sd_client *client) {
#if defined(SD_WITH_RDP)
  if (client->proto == SD_PROTOCOL_RDP) {
    sd_rdp_abort(client);
    return;
  }
#endif
#if defined(SD_WITH_VNC)
  if (client->proto == SD_PROTOCOL_VNC) {
    sd_vnc_abort(client);
    return;
  }
#endif
}

void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended) {
#if defined(SD_WITH_RDP)
  if (client->proto == SD_PROTOCOL_RDP) {
    sd_rdp_key(client, scancode, keysym, down, extended);
    return;
  }
#endif
#if defined(SD_WITH_VNC)
  if (client->proto == SD_PROTOCOL_VNC) {
    sd_vnc_key(client, scancode, keysym, down, extended);
    return;
  }
#endif
}

void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons) {
#if defined(SD_WITH_RDP)
  if (client->proto == SD_PROTOCOL_RDP) {
    sd_rdp_pointer(client, x, y, buttons);
    return;
  }
#endif
#if defined(SD_WITH_VNC)
  if (client->proto == SD_PROTOCOL_VNC) {
    sd_vnc_pointer(client, x, y, buttons);
    return;
  }
#endif
}

void sd_backend_clipboard(sd_client *client, const char *utf8, uint32_t len) {
#if defined(SD_WITH_RDP)
  if (client->proto == SD_PROTOCOL_RDP) {
    sd_rdp_clipboard(client, utf8, len);
    return;
  }
#endif
#if defined(SD_WITH_VNC)
  if (client->proto == SD_PROTOCOL_VNC) {
    sd_vnc_clipboard(client, utf8, len);
    return;
  }
#endif
}
#endif /* any backend */

sd_status sd_backend_info(int32_t proto, char *out, size_t cap) {
  if (out == NULL || cap == 0) return SD_ERR_ARGUMENT;
  out[0] = '\0';
  return sd_backend_available(proto, out, cap);
}

/* ------------------------------------------------------------------ dispatch */

static void sd_dispatch(sd_client *client, sd_event *event) {
  event->abi_version = SD_ABI_VERSION;
  EnterCriticalSection(&client->lock);
  sd_event_fn fn = client->fn;
  void *user = client->user;
  if (fn != NULL) fn(user, event);
  LeaveCriticalSection(&client->lock);
}

void sd_emit_state(sd_client *client, int32_t state, int32_t failure, int32_t detail,
                   const char *text) {
  sd_event event;
  memset(&event, 0, sizeof(event));
  event.kind = SD_EVENT_STATE;
  event.state = (uint32_t)state;
  event.failure = (uint32_t)failure;
  event.detail = detail;
  if (text != NULL) {
    size_t len = strlen(text);
    if (len > SD_MAX_TEXT_BYTES) len = SD_MAX_TEXT_BYTES;
    event.text = text;
    event.text_len = (uint32_t)len;
  }
  EnterCriticalSection(&client->lock);
  client->state = state;
  LeaveCriticalSection(&client->lock);
  sd_dispatch(client, &event);
}

void sd_emit_frame(sd_client *client, int32_t x, int32_t y, int32_t w, int32_t h,
                   const uint8_t *pixels, uint32_t stride, uint32_t width,
                   uint32_t height) {
  if (pixels == NULL || w <= 0 || h <= 0) return;
  sd_event event;
  memset(&event, 0, sizeof(event));
  event.kind = SD_EVENT_FRAME;
  event.state = SD_STATE_CONNECTED;
  event.x = x;
  event.y = y;
  event.w = w;
  event.h = h;
  event.pixels = pixels;
  event.stride = stride;
  event.width = width;
  event.height = height;
  sd_dispatch(client, &event);
}

void sd_emit_clipboard(sd_client *client, const char *text, uint32_t len) {
  if (text == NULL) return;
  if (len > SD_MAX_CLIPBOARD_BYTES) len = SD_MAX_CLIPBOARD_BYTES;
  sd_event event;
  memset(&event, 0, sizeof(event));
  event.kind = SD_EVENT_CLIPBOARD;
  event.state = SD_STATE_CONNECTED;
  event.text = text;
  event.text_len = len;
  sd_dispatch(client, &event);
}

int32_t sd_is_cancelled(sd_client *client) {
  return InterlockedCompareExchange(&client->cancel, 0, 0) != 0 ? 1 : 0;
}

int32_t sd_ask_certificate(sd_client *client, const char *host, const char *subject,
                           const char *issuer, const char *fingerprint,
                           uint32_t flags) {
  EnterCriticalSection(&client->lock);
  InterlockedIncrement(&client->cert_request);
  uint32_t request = (uint32_t)InterlockedCompareExchange(&client->cert_request, 0, 0);
  free(client->cert_host);
  free(client->cert_subject);
  free(client->cert_issuer);
  free(client->cert_fingerprint);
  client->cert_host = host != NULL ? sd_strdup(host) : NULL;
  client->cert_subject = subject != NULL ? sd_strdup(subject) : NULL;
  client->cert_issuer = issuer != NULL ? sd_strdup(issuer) : NULL;
  client->cert_fingerprint = fingerprint != NULL ? sd_strdup(fingerprint) : NULL;
  client->cert_flags = flags;
  InterlockedExchange(&client->cert_answer, 0);
  LeaveCriticalSection(&client->lock);

  sd_event event;
  memset(&event, 0, sizeof(event));
  event.kind = SD_EVENT_CERTIFICATE;
  event.state = SD_STATE_CONNECTING;
  event.request_id = request;
  event.cert_flags = flags;
  event.host = client->cert_host;
  event.subject = client->cert_subject;
  event.issuer = client->cert_issuer;
  event.fingerprint = client->cert_fingerprint;
  event.text = "certificate requires a decision";
  event.text_len = (uint32_t)strlen(event.text);
  sd_dispatch(client, &event);

  /* Waiting happens without the dispatch lock: the answer arrives from another
   * thread and must not be serialised behind this one. */
  DWORD elapsed = 0;
  int32_t answer = 0;
  while (elapsed < SD_CERT_WAIT_MS) {
    if (sd_is_cancelled(client)) break;
    DWORD step = 100;
    if (elapsed + step > SD_CERT_WAIT_MS) step = SD_CERT_WAIT_MS - elapsed;
    DWORD waited = WaitForSingleObject(client->cert_signal, step);
    elapsed += step;
    if (waited == WAIT_OBJECT_0) {
      answer = (int32_t)InterlockedCompareExchange(&client->cert_answer, 0, 0);
      break;
    }
  }
  InterlockedExchange(&client->cert_answer, 0);
  return answer;
}

/* ------------------------------------------------------------------- inputs */

int32_t sd_pop_input(sd_client *client, sd_input_event *out) {
  int32_t have = 0;
  EnterCriticalSection(&client->input_lock);
  if (client->input_count > 0) {
    *out = client->inputs[client->input_head];
    client->input_head = (client->input_head + 1) % SD_INPUT_QUEUE;
    client->input_count--;
    have = 1;
  }
  LeaveCriticalSection(&client->input_lock);
  return have;
}

char *sd_take_clipboard(sd_client *client, uint32_t *len) {
  char *text = NULL;
  EnterCriticalSection(&client->input_lock);
  text = client->clipboard_text;
  client->clipboard_text = NULL;
  *len = client->clipboard_len;
  client->clipboard_len = 0;
  LeaveCriticalSection(&client->input_lock);
  return text;
}

void sd_flush_input(sd_client *client) {
  sd_input_event event;
  while (sd_pop_input(client, &event)) {
    if (event.kind == 0) {
      sd_backend_key(client, event.scancode, event.keysym, event.down, event.extended);
    } else {
      sd_backend_pointer(client, event.x, event.y, event.buttons);
    }
  }
  uint32_t len = 0;
  char *text = sd_take_clipboard(client, &len);
  if (text != NULL) {
    sd_backend_clipboard(client, text, len);
    free(text);
  }
}

/* Releasing a pressed key when the picture loses focus is a UI decision, but the
 * queue must never keep a stale press after a close: it dies with the worker. */

/* ----------------------------------------------------------------- lifecycle */

static DWORD WINAPI sd_worker(LPVOID param) {
  sd_client *client = (sd_client *)param;
  sd_backend_run(client);
  InterlockedExchange(&client->running, 0);
  return 0;
}

static void sd_clear_config(sd_client *client) {
  free(client->host);
  free(client->username);
  free(client->password);
  free(client->domain);
  client->host = NULL;
  client->username = NULL;
  client->password = NULL;
  client->domain = NULL;
}

sd_status sd_client_new(int32_t proto, sd_event_fn fn, void *user, sd_client **out) {
  if (out == NULL) return SD_ERR_ARGUMENT;
  *out = NULL;
  if (proto != SD_PROTOCOL_RDP && proto != SD_PROTOCOL_VNC) return SD_ERR_ARGUMENT;

  sd_client *client = (sd_client *)calloc(1, sizeof(sd_client));
  if (client == NULL) return SD_ERR_INTERNAL;
  client->abi = SD_ABI_VERSION;
  client->proto = proto;
  client->fn = fn;
  client->user = user;
  client->state = SD_STATE_IDLE;
  client->vnc_mask = SD_VNC_SECURITY_DEFAULT;
  client->vnc_bytes_per_pixel = 4;
  client->width = 1024;
  client->height = 768;
  InitializeCriticalSection(&client->lock);
  InitializeCriticalSection(&client->input_lock);
  client->cert_signal = CreateEventW(NULL, FALSE, FALSE, NULL);
  if (client->cert_signal == NULL) {
    DeleteCriticalSection(&client->input_lock);
    DeleteCriticalSection(&client->lock);
    free(client);
    return SD_ERR_INTERNAL;
  }
  *out = client;
  return SD_OK;
}

sd_status sd_client_set_server(sd_client *client, const char *host, uint16_t port) {
  if (client == NULL || host == NULL || host[0] == '\0' || strlen(host) > 1024) {
    return SD_ERR_ARGUMENT;
  }
  char *copy = sd_strdup(host);
  if (copy == NULL) return SD_ERR_INTERNAL;
  EnterCriticalSection(&client->lock);
  int32_t state = client->state;
  if (state == SD_STATE_CONNECTING || state == SD_STATE_CONNECTED ||
      state == SD_STATE_CLOSING) {
    LeaveCriticalSection(&client->lock);
    free(copy);
    return SD_ERR_STATE;
  }
  free(client->host);
  client->host = copy;
  client->port = port;
  LeaveCriticalSection(&client->lock);
  return SD_OK;
}

sd_status sd_client_set_credentials(sd_client *client, const char *user,
                                    const char *password, const char *domain) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  EnterCriticalSection(&client->lock);
  int32_t state = client->state;
  if (state == SD_STATE_CONNECTING || state == SD_STATE_CONNECTED ||
      state == SD_STATE_CLOSING) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  char *u = user != NULL ? sd_strdup(user) : NULL;
  char *p = password != NULL ? sd_strdup(password) : NULL;
  char *d = domain != NULL ? sd_strdup(domain) : NULL;
  if ((user != NULL && u == NULL) || (password != NULL && p == NULL) ||
      (domain != NULL && d == NULL)) {
    free(u);
    free(p);
    free(d);
    LeaveCriticalSection(&client->lock);
    return SD_ERR_INTERNAL;
  }
  free(client->username);
  free(client->password);
  free(client->domain);
  client->username = u;
  client->password = p;
  client->domain = d;
  LeaveCriticalSection(&client->lock);
  return SD_OK;
}

sd_status sd_client_set_size(sd_client *client, uint32_t width, uint32_t height) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  /* The size is a request: RDP negotiates it and VNC uses it for the first
   * framebuffer request. Only zero and absurd values are refused. */
  if (width == 0 || height == 0 || width > 16384 || height > 16384) {
    return SD_ERR_ARGUMENT;
  }
  EnterCriticalSection(&client->lock);
  int32_t state = client->state;
  if (state == SD_STATE_CONNECTING || state == SD_STATE_CONNECTED ||
      state == SD_STATE_CLOSING) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  client->width = width;
  client->height = height;
  LeaveCriticalSection(&client->lock);
  return SD_OK;
}

sd_status sd_client_set_vnc_security(sd_client *client, uint32_t allowed_mask) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if ((allowed_mask & (SD_VNC_SECURITY_NONE | SD_VNC_SECURITY_VNC_AUTH |
                      SD_VNC_SECURITY_TLS | SD_VNC_SECURITY_VENCRYPT)) != allowed_mask ||
      allowed_mask == 0) {
    return SD_ERR_ARGUMENT;
  }
  EnterCriticalSection(&client->lock);
  if (client->proto != SD_PROTOCOL_VNC) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_ARGUMENT;
  }
  client->vnc_mask = allowed_mask;
  LeaveCriticalSection(&client->lock);
  return SD_OK;
}

sd_status sd_client_set_vnc_colour_depth(sd_client *client, uint32_t bytes_per_pixel) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if (bytes_per_pixel != 3 && bytes_per_pixel != 4) return SD_ERR_ARGUMENT;
  EnterCriticalSection(&client->lock);
  if (client->proto != SD_PROTOCOL_VNC) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_ARGUMENT;
  }
  client->vnc_bytes_per_pixel = bytes_per_pixel;
  LeaveCriticalSection(&client->lock);
  return SD_OK;
}

sd_status sd_client_connect(sd_client *client) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  char info[256];
  sd_status available = sd_backend_available(client->proto, info, sizeof(info));
  if (available != SD_OK) return available;

  EnterCriticalSection(&client->lock);
  if (client->host == NULL) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  int32_t state = client->state;
  if (state == SD_STATE_CONNECTING || state == SD_STATE_CONNECTED ||
      state == SD_STATE_CLOSING) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  /* A previous attempt's thread handle is reused only after it exited. */
  if (client->thread != NULL) {
    if (WaitForSingleObject(client->thread, 0) != WAIT_OBJECT_0) {
      LeaveCriticalSection(&client->lock);
      return SD_ERR_STATE;
    }
    CloseHandle(client->thread);
    client->thread = NULL;
  }
  InterlockedExchange(&client->cancel, 0);
  InterlockedExchange(&client->cert_answer, 0);
  InterlockedExchange(&client->running, 1);
  client->state = SD_STATE_CONNECTING;
  LeaveCriticalSection(&client->lock);

  sd_emit_state(client, SD_STATE_CONNECTING, SD_FAILURE_NONE, 0, NULL);

  client->thread = CreateThread(NULL, 0, sd_worker, client, 0, NULL);
  if (client->thread == NULL) {
    InterlockedExchange(&client->running, 0);
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, (int32_t)GetLastError(),
                  "could not start the connection worker");
    return SD_ERR_INTERNAL;
  }
  return SD_OK;
}

sd_status sd_client_cancel(sd_client *client) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  EnterCriticalSection(&client->lock);
  int32_t state = client->state;
  if (state != SD_STATE_CONNECTING && state != SD_STATE_CONNECTED) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  InterlockedExchange(&client->cancel, 1);
  client->state = SD_STATE_CLOSING;
  LeaveCriticalSection(&client->lock);

  SetEvent(client->cert_signal);
  sd_emit_state(client, SD_STATE_CLOSING, SD_FAILURE_CANCELLED, 0, "cancelled");
  sd_backend_abort(client);
  return SD_OK;
}

sd_status sd_client_wait(sd_client *client, uint32_t timeout_ms) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if (InterlockedCompareExchange(&client->running, 0, 0) == 0) return SD_OK;
  EnterCriticalSection(&client->lock);
  HANDLE thread = client->thread;
  LeaveCriticalSection(&client->lock);
  if (thread == NULL) return SD_OK;
  DWORD waited = WaitForSingleObject(thread, timeout_ms);
  if (waited == WAIT_OBJECT_0) return SD_OK;
  return SD_ERR_STATE;
}

sd_status sd_client_answer_certificate(sd_client *client, uint32_t request_id,
                                       int32_t accept) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  EnterCriticalSection(&client->lock);
  uint32_t pending = (uint32_t)InterlockedCompareExchange(&client->cert_request, 0, 0);
  if (pending != request_id || request_id == 0) {
    LeaveCriticalSection(&client->lock);
    return SD_ERR_STATE;
  }
  InterlockedExchange(&client->cert_answer, accept ? 1 : 2);
  LeaveCriticalSection(&client->lock);
  SetEvent(client->cert_signal);
  return SD_OK;
}

static sd_status sd_queue(sd_client *client, const sd_input_event *event) {
  sd_status status = SD_ERR_STATE;
  EnterCriticalSection(&client->lock);
  if (client->state == SD_STATE_CONNECTED) status = SD_OK;
  LeaveCriticalSection(&client->lock);
  if (status != SD_OK) return status;

  EnterCriticalSection(&client->input_lock);
  if (client->input_count == SD_INPUT_QUEUE) {
    LeaveCriticalSection(&client->input_lock);
    return SD_ERR_STATE;
  }
  uint32_t slot = (client->input_head + client->input_count) % SD_INPUT_QUEUE;
  client->inputs[slot] = *event;
  client->input_count++;
  LeaveCriticalSection(&client->input_lock);
  return SD_OK;
}

sd_status sd_client_key(sd_client *client, uint32_t scancode, uint32_t keysym,
                        int32_t down, int32_t extended) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if (scancode > 0x1FF || keysym > 0x10FFFF) return SD_ERR_ARGUMENT;
  if (scancode == 0 && keysym == 0) return SD_ERR_ARGUMENT;
  sd_input_event event;
  memset(&event, 0, sizeof(event));
  event.kind = 0;
  event.scancode = scancode;
  event.keysym = keysym;
  event.down = down ? 1 : 0;
  event.extended = extended ? 1 : 0;
  return sd_queue(client, &event);
}

sd_status sd_client_pointer(sd_client *client, uint16_t x, uint16_t y,
                            uint32_t buttons) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if ((buttons & ~(SD_BUTTON_LEFT | SD_BUTTON_MIDDLE | SD_BUTTON_RIGHT |
                   SD_BUTTON_WHEEL_UP | SD_BUTTON_WHEEL_DOWN |
                   SD_BUTTON_WHEEL_LEFT | SD_BUTTON_WHEEL_RIGHT)) != 0) {
    return SD_ERR_ARGUMENT;
  }
  sd_input_event event;
  memset(&event, 0, sizeof(event));
  event.kind = 1;
  event.x = x;
  event.y = y;
  event.buttons = buttons;
  return sd_queue(client, &event);
}

sd_status sd_client_clipboard(sd_client *client, const char *utf8) {
  if (client == NULL || utf8 == NULL) return SD_ERR_ARGUMENT;
  size_t len = strlen(utf8);
  if (len > SD_MAX_CLIPBOARD_BYTES) return SD_ERR_ARGUMENT;
  char *copy = (char *)malloc(len + 1);
  if (copy == NULL) return SD_ERR_INTERNAL;
  memcpy(copy, utf8, len + 1);

  EnterCriticalSection(&client->lock);
  int32_t state = client->state;
  LeaveCriticalSection(&client->lock);
  if (state != SD_STATE_CONNECTED) {
    free(copy);
    return SD_ERR_STATE;
  }
  EnterCriticalSection(&client->input_lock);
  free(client->clipboard_text);
  client->clipboard_text = copy;
  client->clipboard_len = (uint32_t)len;
  LeaveCriticalSection(&client->input_lock);
  return SD_OK;
}

void sd_client_detach(sd_client *client) {
  if (client == NULL) return;
  EnterCriticalSection(&client->lock);
  client->fn = NULL;
  client->user = NULL;
  LeaveCriticalSection(&client->lock);
}

sd_status sd_client_free(sd_client *client) {
  if (client == NULL) return SD_ERR_ARGUMENT;
  if (InterlockedCompareExchange(&client->running, 0, 0) != 0) {
    sd_client_cancel(client);
    EnterCriticalSection(&client->lock);
    HANDLE thread = client->thread;
    LeaveCriticalSection(&client->lock);
    if (thread != NULL && WaitForSingleObject(thread, SD_FREE_WAIT_MS) != WAIT_OBJECT_0) {
      /* The library still owns the handle. Give up the Rust callback and leave the
       * allocation alive rather than freeing memory a worker is writing. */
      sd_client_detach(client);
      return SD_ERR_STATE;
    }
  }
  EnterCriticalSection(&client->lock);
  HANDLE thread = client->thread;
  client->thread = NULL;
  client->fn = NULL;
  client->user = NULL;
  LeaveCriticalSection(&client->lock);
  if (thread != NULL) CloseHandle(thread);
  sd_clear_config(client);
  free(client->cert_host);
  free(client->cert_subject);
  free(client->cert_issuer);
  free(client->cert_fingerprint);
  free(client->clipboard_text);
  if (client->cert_signal != NULL) CloseHandle(client->cert_signal);
  DeleteCriticalSection(&client->input_lock);
  DeleteCriticalSection(&client->lock);
  free(client);
  return SD_OK;
}