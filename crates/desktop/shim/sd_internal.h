/* Internal glue between the shared handle and the protocol backends.
 *
 * Only sd_shim.c, sd_rdp.c and sd_vnc.c include this. The Rust crate sees
 * shellune_desktop_shim.h alone; nothing here is part of the ABI promise beyond
 * keeping the layout identical in every translation unit of this library.
 */
#ifndef SD_INTERNAL_H
#define SD_INTERNAL_H

#include "shellune_desktop_shim.h"

#include <stdlib.h>
#include <string.h>

/* The CRT's _strdup/_snprintf_s are MSVC-only; keeping one helper makes the
 * backend files free of platform-specific string calls. */
static __inline char *sd_strdup(const char *text) {
  if (text == NULL) return NULL;
  size_t len = strlen(text) + 1;
  char *copy = (char *)malloc(len);
  if (copy != NULL) memcpy(copy, text, len);
  return copy;
}

#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#error "The native desktop shim currently has a verified Windows implementation only."
#endif

#define SD_INPUT_QUEUE 256u

typedef struct sd_input_event {
  uint32_t kind; /* 0 keyboard, 1 pointer */
  uint32_t scancode;
  uint32_t keysym;
  int32_t down;
  int32_t extended;
  uint16_t x;
  uint16_t y;
  uint32_t buttons;
} sd_input_event;

struct sd_client {
  uint32_t abi;
  int32_t proto;
  int32_t reserved;

  /* One lock serialises event dispatch with detach and with state changes, so
   * "no event after detach returned" holds without a second flag.
   *
   * It is a plain CRITICAL_SECTION, not a recursive one: the thread already
   * holding it must not enter it again. Event dispatch calls the consumer's
   * callback while holding it, so a callback must not call back into the same
   * client (sd_client_cancel, sd_client_key, ... would deadlock). */
  CRITICAL_SECTION lock;
  sd_event_fn fn;
  void *user;
  int32_t state;

  /* Configuration, written before connect and read by the worker. */
  char *host;
  uint16_t port;
  char *username;
  char *password;
  char *domain;
  uint32_t width;
  uint32_t height;
  uint32_t vnc_mask;
  uint32_t vnc_bytes_per_pixel;

  /* Worker lifetime. `cancel` is set by any thread; the backend polls it. */
  HANDLE thread;
  volatile LONG cancel;
  volatile LONG running;

  /* The backend session pointer, guarded separately from `lock`.
   *
   * Readers (abort, from any thread) take a reference under `backend_lock` and
   * release it after their last use; the worker clears the pointer and waits for
   * the reference count to reach zero before it frees the session. A separate
   * lock is needed because releasing a session calls back into event dispatch,
   * which takes `lock`, and `lock` is not recursive. */
  CRITICAL_SECTION backend_lock;
  HANDLE backend_idle;
  volatile LONG backend_refs;
  void *backend;

  /* Certificate question: the worker waits here, another thread answers. */
  HANDLE cert_signal;
  volatile LONG cert_request;
  volatile LONG cert_answer;
  char *cert_host;
  char *cert_subject;
  char *cert_issuer;
  char *cert_fingerprint;
  uint32_t cert_flags;

  /* Input queued by the UI thread and drained by the worker, because the protocol
   * libraries are only touched from their own thread. */
  CRITICAL_SECTION input_lock;
  sd_input_event inputs[SD_INPUT_QUEUE];
  uint32_t input_head;
  uint32_t input_count;
  char *clipboard_text;
  uint32_t clipboard_len;
};

/* --- helpers implemented in sd_shim.c, used by the backends --- */

/* Emits a state event. Takes the dispatch lock, so a concurrent detach wins. */
void sd_emit_state(sd_client *client, int32_t state, int32_t failure, int32_t detail,
                   const char *text);
/* Copies the pixels into a frame event for the duration of the callback. */
void sd_emit_frame(sd_client *client, int32_t x, int32_t y, int32_t w, int32_t h,
                   const uint8_t *pixels, uint32_t stride, uint32_t width,
                   uint32_t height);
/* Asks the user; blocks the worker thread until answered, cancelled or timed out.
 * Returns 1 (accept and remember), 2 (accept for this session) or 0 (reject). */
int32_t sd_ask_certificate(sd_client *client, const char *host, const char *subject,
                           const char *issuer, const char *fingerprint,
                           uint32_t flags);
void sd_emit_clipboard(sd_client *client, const char *text, uint32_t len);

/* --- backend pointer lifetime ---
 *
 * The worker publishes its session once the object exists and retires it before
 * freeing. Anything that may run on another thread while the worker is alive
 * (abort) reads the pointer through sd_backend_acquire and must call
 * sd_backend_release after its last use of the session. */

/* Publishes `backend` so other threads can reach it. */
void sd_backend_publish(sd_client *client, void *backend);
/* Returns the current backend with one reference held, or NULL. */
void *sd_backend_acquire(sd_client *client);
/* Drops one reference taken by sd_backend_acquire. */
void sd_backend_release(sd_client *client);
/* Clears the published pointer and waits until no reader holds a reference, so the
 * caller may free the session. Called by the worker only, before the free. */
void sd_backend_retire(sd_client *client);

int32_t sd_is_cancelled(sd_client *client);
/* Pops one queued input event. Returns 0 when the queue is empty. */
int32_t sd_pop_input(sd_client *client, sd_input_event *out);
/* Pops the queued clipboard text, transferring ownership to the caller. */
char *sd_take_clipboard(sd_client *client, uint32_t *len);
/* Drains the queue into the backend. Called from the worker loop only. */
void sd_flush_input(sd_client *client);


/* --- backends --- */

/* Each protocol exposes the same five entry points under its own name; sd_shim.c
 * dispatches on the client's protocol so the two libraries never alias each other's
 * symbols in one link unit. */
#if defined(SD_WITH_RDP)
sd_status sd_rdp_available(char *out, size_t cap);
void sd_rdp_run(sd_client *client);
void sd_rdp_abort(sd_client *client);
void sd_rdp_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                int32_t extended);
void sd_rdp_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_rdp_clipboard(sd_client *client, const char *utf8, uint32_t len);
#endif

#if defined(SD_WITH_VNC)
sd_status sd_vnc_available(char *out, size_t cap);
void sd_vnc_run(sd_client *client);
void sd_vnc_abort(sd_client *client);
void sd_vnc_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                int32_t extended);
void sd_vnc_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_vnc_clipboard(sd_client *client, const char *utf8, uint32_t len);
#endif

/* Implemented in sd_stub.c when no protocol library is linked. */
#if !defined(SD_WITH_RDP) && !defined(SD_WITH_VNC)
sd_status sd_backend_available(int32_t proto, char *out, size_t cap);
void sd_backend_run(sd_client *client);
void sd_backend_abort(sd_client *client);
void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended);
void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_backend_clipboard(sd_client *client, const char *utf8, uint32_t len);
#endif

#if defined(SD_WITH_RDP) || defined(SD_WITH_VNC)
/* The protocol-neutral façade the rest of the shim calls. */
sd_status sd_backend_available(int32_t proto, char *out, size_t cap);
void sd_backend_run(sd_client *client);
void sd_backend_abort(sd_client *client);
void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended);
void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_backend_clipboard(sd_client *client, const char *utf8, uint32_t len);
#endif


#endif /* SD_INTERNAL_H */