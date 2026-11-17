/* Internal glue between the shared handle and the protocol backends.
 *
 * Only the shim's own sources (sd_*.c) include this. The Rust crate sees
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
#elif !defined(SD_WITH_RDP) && !defined(SD_WITH_VNC)
#include <errno.h>
#include <pthread.h>
#include <time.h>

typedef pthread_mutex_t CRITICAL_SECTION;
typedef int32_t LONG;
typedef uint32_t DWORD;
typedef void *LPVOID;
#define WINAPI
#ifndef FALSE
#define FALSE 0
#endif
#ifndef TRUE
#define TRUE 1
#endif
#define WAIT_OBJECT_0 0u
#define WAIT_TIMEOUT 258u

typedef struct sd_posix_handle {
  int kind; /* 1 = auto-reset event, 2 = thread */
  pthread_mutex_t mutex;
  pthread_cond_t cond;
  int signaled;
  int exited;
  pthread_t thread;
  DWORD (*entry)(LPVOID);
  LPVOID arg;
} *HANDLE;

static __inline void InitializeCriticalSection(CRITICAL_SECTION *cs) {
  pthread_mutex_init(cs, NULL);
}
static __inline void DeleteCriticalSection(CRITICAL_SECTION *cs) {
  pthread_mutex_destroy(cs);
}
static __inline void EnterCriticalSection(CRITICAL_SECTION *cs) {
  pthread_mutex_lock(cs);
}
static __inline void LeaveCriticalSection(CRITICAL_SECTION *cs) {
  pthread_mutex_unlock(cs);
}
static __inline LONG InterlockedExchange(volatile LONG *target, LONG value) {
  return __atomic_exchange_n(target, value, __ATOMIC_SEQ_CST);
}
static __inline LONG InterlockedIncrement(volatile LONG *target) {
  return __atomic_add_fetch(target, 1, __ATOMIC_SEQ_CST);
}
static __inline LONG InterlockedDecrement(volatile LONG *target) {
  return __atomic_sub_fetch(target, 1, __ATOMIC_SEQ_CST);
}
static __inline LONG InterlockedCompareExchange(volatile LONG *dest, LONG exch,
                                                LONG comp) {
  LONG expected = comp;
  __atomic_compare_exchange_n(dest, &expected, exch, 0, __ATOMIC_SEQ_CST,
                              __ATOMIC_SEQ_CST);
  return expected;
}
static __inline HANDLE CreateEventW(void *attr, int manual, int initial,
                                    const void *name) {
  (void)attr;
  (void)manual;
  (void)name;
  HANDLE h = (HANDLE)calloc(1, sizeof(struct sd_posix_handle));
  if (h == NULL) return NULL;
  h->kind = 1;
  h->signaled = initial ? 1 : 0;
  pthread_mutex_init(&h->mutex, NULL);
  pthread_cond_init(&h->cond, NULL);
  return h;
}
static __inline void SetEvent(HANDLE h) {
  if (h == NULL || h->kind != 1) return;
  pthread_mutex_lock(&h->mutex);
  h->signaled = 1;
  pthread_cond_signal(&h->cond);
  pthread_mutex_unlock(&h->mutex);
}
static __inline void *sd_posix_thread_trampoline(void *raw) {
  HANDLE h = (HANDLE)raw;
  h->entry(h->arg);
  pthread_mutex_lock(&h->mutex);
  h->exited = 1;
  pthread_cond_broadcast(&h->cond);
  pthread_mutex_unlock(&h->mutex);
  return NULL;
}
static __inline HANDLE CreateThread(void *attr, size_t stack,
                                    DWORD (*start)(LPVOID), LPVOID arg,
                                    DWORD flags, DWORD *id) {
  (void)attr;
  (void)stack;
  (void)flags;
  (void)id;
  HANDLE h = (HANDLE)calloc(1, sizeof(struct sd_posix_handle));
  if (h == NULL) return NULL;
  h->kind = 2;
  h->entry = start;
  h->arg = arg;
  pthread_mutex_init(&h->mutex, NULL);
  pthread_cond_init(&h->cond, NULL);
  if (pthread_create(&h->thread, NULL, sd_posix_thread_trampoline, h) != 0) {
    pthread_cond_destroy(&h->cond);
    pthread_mutex_destroy(&h->mutex);
    free(h);
    return NULL;
  }
  return h;
}
static __inline DWORD WaitForSingleObject(HANDLE h, DWORD timeout_ms) {
  if (h == NULL) return WAIT_TIMEOUT;
  struct timespec ts;
  clock_gettime(CLOCK_REALTIME, &ts);
  ts.tv_sec += (time_t)(timeout_ms / 1000u);
  ts.tv_nsec += (long)((timeout_ms % 1000u) * 1000000u);
  if (ts.tv_nsec >= 1000000000L) {
    ts.tv_sec += 1;
    ts.tv_nsec -= 1000000000L;
  }
  pthread_mutex_lock(&h->mutex);
  DWORD result = WAIT_TIMEOUT;
  if (h->kind == 1) {
    while (!h->signaled) {
      if (timeout_ms == 0 ||
          pthread_cond_timedwait(&h->cond, &h->mutex, &ts) == ETIMEDOUT) {
        break;
      }
    }
    if (h->signaled) {
      h->signaled = 0;
      result = WAIT_OBJECT_0;
    }
  } else if (h->kind == 2) {
    while (!h->exited) {
      if (timeout_ms == 0 ||
          pthread_cond_timedwait(&h->cond, &h->mutex, &ts) == ETIMEDOUT) {
        break;
      }
    }
    if (h->exited) result = WAIT_OBJECT_0;
  }
  pthread_mutex_unlock(&h->mutex);
  return result;
}
static __inline void CloseHandle(HANDLE h) {
  if (h == NULL) return;
  if (h->kind == 2) pthread_join(h->thread, NULL);
  pthread_cond_destroy(&h->cond);
  pthread_mutex_destroy(&h->mutex);
  free(h);
}
static __inline DWORD GetLastError(void) { return (DWORD)errno; }
#else
#error "The native desktop shim currently has a verified Windows implementation only."
#endif

#define SD_INPUT_QUEUE 256u

/* ---- clipboard groups (sd_clip.c) ----------------------------------------
 *
 * One group is what one copy put on a clipboard: each kind at most once, every
 * entry owned by the group. The helpers are pure and protocol free so the probes in
 * the public header can test them without a server. */

typedef struct sd_clip_item {
  uint32_t kind;
  uint32_t len;
  uint8_t *bytes;
} sd_clip_item;

typedef struct sd_clip_set {
  uint32_t count;
  sd_clip_item items[SD_MAX_CLIPBOARD_ENTRIES];
  /* Kinds (bit 1 << kind) that were over their bound: text cut, images left out. */
  uint32_t too_large;
} sd_clip_set;

/* Checks a caller's entries against the bounds and rules of the header. */
sd_status sd_clip_validate(const sd_clipboard_entry *entries, uint32_t count);
/* Deep copy of validated entries; NULL on allocation failure. Text over its bound
 * is cut to it and marked in `too_large`. */
sd_clip_set *sd_clip_set_copy(const sd_clipboard_entry *entries, uint32_t count);
void sd_clip_set_free(sd_clip_set *set);
/* Releases the entries, leaving an empty group. */
void sd_clip_set_clear(sd_clip_set *set);
const sd_clip_item *sd_clip_set_find(const sd_clip_set *set, uint32_t kind);
/* Adds an entry, taking `bytes`; frees it and returns 0 when the group is full or
 * already has that kind. */
int32_t sd_clip_set_add(sd_clip_set *set, uint32_t kind, uint8_t *bytes, uint32_t len);

/* Text conversions. Each returns a malloc'd buffer (NUL terminated for UTF-8 and
 * Latin-1 results, so a zero length result is never NULL) or NULL on failure. */
uint8_t *sd_clip_utf8_to_cf_unicode(const uint8_t *utf8, size_t len, size_t *out_len);
uint8_t *sd_clip_cf_unicode_to_utf8(const uint8_t *utf16, size_t len, size_t *out_len);
/* NULL when a character is outside Latin-1 or the input is not UTF-8. */
uint8_t *sd_clip_utf8_to_latin1(const uint8_t *utf8, size_t len, size_t *out_len);
uint8_t *sd_clip_latin1_to_utf8(const uint8_t *latin1, size_t len, size_t *out_len);
int32_t sd_clip_utf8_valid(const uint8_t *utf8, size_t len);
/* The length of the longest prefix of valid UTF-8 `utf8` that fits in `bound` bytes
 * and ends on a whole character. */
size_t sd_clip_utf8_cut(const uint8_t *utf8, size_t len, size_t bound);

/* RDP clipboard format ids used on the wire. */
#define SD_CF_DIB 8u
#define SD_CF_UNICODETEXT 13u
#define SD_CF_DIBV5 17u
/* The id this client gives its registered "PNG" format in its own format lists. */
#define SD_CF_LOCAL_PNG 0xC0F0u

/* Picks the formats one remote copy is fetched as (see sd_probe_rdp_select). */
uint32_t sd_clip_select(const uint32_t *ids, const char *const *names, uint32_t count,
                        uint32_t out_ids[2], uint32_t out_kinds[2]);

/* The fetch of one remote copy: one request in flight at a time, text before image,
 * and the whole group delivered once. A format list that arrives while a request is
 * in flight voids the unfinished group; its own fetch starts once the outstanding
 * answer has arrived (and been dropped), so an answer is never credited to the
 * wrong request. */
typedef struct sd_clip_fetch {
  uint32_t plan_ids[2];
  uint32_t plan_kinds[2];
  uint32_t plan_count;
  uint32_t next;      /* index of the request in flight */
  int32_t in_flight;
  int32_t abandoned;  /* the answer in flight belongs to a voided group */
  int32_t queued;     /* a newer plan waits for that answer */
  uint32_t queued_ids[2];
  uint32_t queued_kinds[2];
  uint32_t queued_count;
  uint32_t too_large; /* kinds of this group over their bound (bit 1 << kind) */
  sd_clip_set collected;
} sd_clip_fetch;

/* A format list arrived. Returns the format id to request now, or 0 when there is
 * nothing to request yet (nothing wanted, or an answer is still outstanding). */
uint32_t sd_clip_fetch_on_list(sd_clip_fetch *fetch, const uint32_t *ids,
                               const char *const *names, uint32_t count);
/* An answer arrived. Returns the next format id to request, or 0. When the group is
 * complete `*done` is 1, the group is moved to `finished` (caller clears it) and
 * `*too_large` holds the kinds that were over their bound (bit 1 << kind): a text
 * is cut to the bound and kept, an image is left out. An answer while nothing is
 * in flight is ignored. */
uint32_t sd_clip_fetch_on_response(sd_clip_fetch *fetch, int32_t ok, const uint8_t *data,
                                   uint32_t len, sd_clip_set *finished,
                                   uint32_t *too_large, int32_t *done);
void sd_clip_fetch_reset(sd_clip_fetch *fetch);
/* Fills `out` with views of the group's entries; returns how many. */
uint32_t sd_clip_entries(const sd_clip_set *set, sd_clipboard_entry out[SD_MAX_CLIPBOARD_ENTRIES]);

typedef struct sd_input_event {
  uint32_t kind; /* 0 keyboard, 1 pointer transition, 2 pointer motion */
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
  int32_t clipboard_enabled;
  char *drive_name;
  char *drive_path;

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
  uint32_t queued_buttons;
  /* The newest local clipboard group not yet handed to the backend, or NULL. */
  sd_clip_set *clipboard_pending;
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
 * Returns 1 (accept), 2 (reject) or 0 (cancel/timeout). */
int32_t sd_ask_certificate(sd_client *client, const char *host, const char *subject,
                           const char *issuer, const char *fingerprint,
                           uint32_t flags);
/* Delivers one remote clipboard group; an empty group is not delivered. */
void sd_emit_clipboard(sd_client *client, const sd_clip_set *set);
void sd_emit_notice(sd_client *client, uint32_t notice, int32_t detail);
/* One SD_NOTICE_CLIPBOARD_TOO_LARGE per kind set in `kinds` (bit 1 << kind). */
void sd_emit_too_large(sd_client *client, uint32_t kinds);

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
/* Pops the queued clipboard group, transferring ownership to the caller. */
sd_clip_set *sd_take_clipboard(sd_client *client);
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
/* Takes ownership of `set`. */
void sd_rdp_clipboard(sd_client *client, sd_clip_set *set);
sd_status sd_rdp_probe_settings(sd_client *client, char *out, size_t cap);
sd_status sd_rdp_probe_session(sd_client *client, const sd_probe_session_step *steps,
                               uint32_t count);
#endif

/* --- RDP drive (sd_rdp_drive.c) --- */
#if defined(SD_WITH_RDP)
struct rdp_settings;
/* 0 when `path` can be offered as a drive, else an sd_drive_problem. */
int32_t sd_rdp_drive_check(const char *path);
/* Adds `path` (already checked) as drive `name`. Returns 0 on success, else an
 * sd_drive_problem. */
int32_t sd_rdp_drive_add(struct rdp_settings *settings, const char *name, const char *path);
/* The size of the context sd_rdp_run allocates, for probes that build one. */
size_t sd_rdp_context_size(void);
#endif

#if defined(SD_WITH_VNC)
sd_status sd_vnc_available(char *out, size_t cap);
void sd_vnc_run(sd_client *client);
void sd_vnc_abort(sd_client *client);
void sd_vnc_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                int32_t extended);
void sd_vnc_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_vnc_clipboard(sd_client *client, sd_clip_set *set);
#endif

/* Implemented in sd_stub.c when no protocol library is linked. */
#if !defined(SD_WITH_RDP) && !defined(SD_WITH_VNC)
sd_status sd_backend_available(int32_t proto, char *out, size_t cap);
void sd_backend_run(sd_client *client);
void sd_backend_abort(sd_client *client);
void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended);
void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_backend_clipboard(sd_client *client, sd_clip_set *set);
#endif

#if defined(SD_WITH_RDP) || defined(SD_WITH_VNC)
/* The protocol-neutral façade the rest of the shim calls. */
sd_status sd_backend_available(int32_t proto, char *out, size_t cap);
void sd_backend_run(sd_client *client);
void sd_backend_abort(sd_client *client);
void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended);
void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons);
void sd_backend_clipboard(sd_client *client, sd_clip_set *set);
#endif


#endif /* SD_INTERNAL_H */
