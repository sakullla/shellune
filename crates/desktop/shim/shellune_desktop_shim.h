/* Shellune native desktop shim - stable C ABI.
 *
 * This header is the complete surface the Rust crate \`shellune-desktop\` sees.
 * No FreeRDP, WinPR, LibVNCClient or libvncserver type, struct layout or opaque
 * pointer crosses it: the protocol libraries stay behind an opaque handle and a
 * single, flat event struct. Callers own nothing a library allocated.
 *
 * Rules the implementation keeps, and the Rust wrapper relies on:
 *  - every function takes or returns only fixed-width integers, UTF-8 strings or
 *    the opaque \`sd_client\` pointer;
 *  - \`sd_event\` is passed by pointer to a callback and must not be retained: the
 *    strings and the pixel buffer are valid only for the duration of that call;
 *  - a callback never runs after \`sd_client_detach\` returned, and never after
 *    \`sd_client_free\` started;
 *  - a callback must not call back into the same client (it would serialise on the
 *    dispatch lock). Report decisions through \`sd_client_answer_certificate\`.
 */
#ifndef SHELLUNE_DESKTOP_SHIM_H
#define SHELLUNE_DESKTOP_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped when the struct layout or a function signature changes.
 * 2: formatted clipboard entries, notices, clipboard and drive options. */
#define SD_ABI_VERSION 2u

/* Upper bound the shim enforces on a single frame notification, so a hostile or
 * broken server cannot make us allocate without limit. */
#define SD_MAX_FRAME_BYTES (64u * 1024u * 1024u)
/* Clipboard bounds, both directions: text (UTF-8 bytes) and one image entry. A
 * remote entry above its bound is skipped and reported with
 * SD_NOTICE_CLIPBOARD_TOO_LARGE; a local one is refused with SD_ERR_ARGUMENT. */
#define SD_MAX_CLIPBOARD_BYTES (1024u * 1024u)
#define SD_MAX_CLIPBOARD_IMAGE_BYTES (32u * 1024u * 1024u)
/* One entry per kind at most, so a group never holds more than this. */
#define SD_MAX_CLIPBOARD_ENTRIES 3u
#define SD_MAX_TEXT_BYTES 4096u

typedef struct sd_client sd_client;

typedef enum sd_protocol {
  SD_PROTOCOL_RDP = 1,
  SD_PROTOCOL_VNC = 2,
} sd_protocol;

typedef enum sd_status {
  SD_OK = 0,
  SD_ERR_ARGUMENT = 1,
  SD_ERR_UNAVAILABLE = 2, /* built without this protocol's library */
  SD_ERR_STATE = 3,       /* not valid in the client's current state */
  SD_ERR_ABORTED = 4,
  SD_ERR_INTERNAL = 5,
} sd_status;

/* Why an attempt ended. Distinct causes stay distinct: an authentication failure
 * is not reported as a connection failure, and a rejected certificate is not
 * reported as a bad password. */
typedef enum sd_failure {
  SD_FAILURE_NONE = 0,
  SD_FAILURE_CONNECT = 1,             /* TCP/protocol connect failed */
  SD_FAILURE_TLS = 2,                 /* TLS handshake or certificate chain failed */
  SD_FAILURE_AUTHENTICATION = 3,      /* credentials rejected by the server */
  SD_FAILURE_CERTIFICATE_REJECTED = 4,/* the certificate question was answered "no" */
  SD_FAILURE_UNSUPPORTED_SECURITY = 5,/* security type not enabled in this build */
  SD_FAILURE_PROTOCOL = 6,            /* peer spoke something other than the protocol */
  SD_FAILURE_IO = 7,
  SD_FAILURE_CANCELLED = 8,           /* the caller cancelled */
  SD_FAILURE_REMOTE_CLOSED = 9,
  SD_FAILURE_LIMIT = 10,              /* a size bound above was exceeded */
  SD_FAILURE_INTERNAL = 11,
} sd_failure;

typedef enum sd_event_kind {
  SD_EVENT_STATE = 0,       /* state/failure/detail */
  SD_EVENT_FRAME = 1,       /* pixels/x/y/w/h/width/height/stride */
  SD_EVENT_CERTIFICATE = 2, /* request_id/host/subject/issuer/fingerprint/cert_flags */
  SD_EVENT_CLIPBOARD = 3,   /* entries/entry_count: one remote copy, every format fetched */
  SD_EVENT_NOTICE = 4,      /* notice; never changes the session state */
} sd_event_kind;

/* What one clipboard entry holds. A group carries each kind at most once. */
typedef enum sd_clipboard_kind {
  SD_CLIPBOARD_TEXT_UTF8 = 1, /* UTF-8 text, no terminating NUL */
  SD_CLIPBOARD_IMAGE_PNG = 2, /* a complete PNG file */
  SD_CLIPBOARD_IMAGE_DIB = 3, /* a packed DIB: BITMAPINFOHEADER (or V5) + bits */
} sd_clipboard_kind;

/* Something the user should know that is not a state change. */
typedef enum sd_notice {
  SD_NOTICE_CLIPBOARD_REFUSED = 1,     /* the server did not open the clipboard channel */
  SD_NOTICE_CLIPBOARD_LATIN1_ONLY = 2, /* VNC server without extended clipboard; text
                                          outside Latin-1 was not sent */
  SD_NOTICE_CLIPBOARD_TOO_LARGE = 3,   /* a remote entry exceeded its bound and was skipped */
  SD_NOTICE_DRIVE_UNAVAILABLE = 4,     /* the mapped folder is missing or unreadable; the
                                          session connects without it */
} sd_notice;

typedef struct sd_clipboard_entry {
  uint32_t kind; /* sd_clipboard_kind */
  uint32_t len;
  const uint8_t *bytes; /* valid only during the call or callback that carries it */
} sd_clipboard_entry;

typedef enum sd_state {
  SD_STATE_IDLE = 0,
  SD_STATE_CONNECTING = 1,
  SD_STATE_CONNECTED = 2,
  SD_STATE_CLOSING = 3,
  SD_STATE_CLOSED = 4,
  SD_STATE_FAILED = 5,
} sd_state;

/* Client-side pointer buttons, protocol independent. */
#define SD_BUTTON_LEFT 0x01u
#define SD_BUTTON_MIDDLE 0x02u
#define SD_BUTTON_RIGHT 0x04u
#define SD_BUTTON_WHEEL_UP 0x08u
#define SD_BUTTON_WHEEL_DOWN 0x10u
#define SD_BUTTON_WHEEL_LEFT 0x20u
#define SD_BUTTON_WHEEL_RIGHT 0x40u

/* VNC security types a build may negotiate. Anything outside the mask is refused
 * before it is attempted, so a server offering only an unsupported type fails
 * with SD_FAILURE_UNSUPPORTED_SECURITY instead of silently downgrading. */
#define SD_VNC_SECURITY_NONE 0x1u
#define SD_VNC_SECURITY_VNC_AUTH 0x2u
#define SD_VNC_SECURITY_TLS 0x4u      /* only when the build includes a TLS backend */
#define SD_VNC_SECURITY_VENCRYPT 0x8u /* only when the build includes a TLS backend */
#define SD_VNC_SECURITY_DEFAULT (SD_VNC_SECURITY_NONE | SD_VNC_SECURITY_VNC_AUTH)

typedef struct sd_event {
  uint32_t abi_version;
  uint32_t kind;
  uint32_t state;
  uint32_t failure;
  int32_t detail;      /* protocol/library code, 0 when unknown */
  uint32_t request_id; /* SD_EVENT_CERTIFICATE only */
  uint32_t cert_flags; /* VERIFY_CERT_FLAG_* as reported by FreeRDP */
  const char *text;    /* reason or clipboard text, not NUL terminated */
  uint32_t text_len;
  const char *host;
  const char *subject;
  const char *issuer;
  const char *fingerprint;
  uint32_t width;
  uint32_t height;
  uint32_t stride; /* bytes per row of `pixels` */
  int32_t x;
  int32_t y;
  int32_t w;
  int32_t h;
  const uint8_t *pixels; /* BGRA, valid only during the callback */
  const sd_clipboard_entry *entries; /* SD_EVENT_CLIPBOARD only */
  uint32_t entry_count;
  uint32_t notice; /* SD_EVENT_NOTICE only: sd_notice */
} sd_event;

typedef void (*sd_event_fn)(void *user, const sd_event *event);

/* Returns SD_ABI_VERSION; the Rust side refuses a different value. */
uint32_t sd_abi_version(void);

const char *sd_status_name(int32_t status);
const char *sd_failure_name(int32_t failure);
const char *sd_state_name(int32_t state);

/* Fills `out` with the linked library versions and the security types that were
 * actually compiled in. Returns SD_ERR_UNAVAILABLE when this build has no backend
 * for `proto`, and SD_ERR_ARGUMENT when the buffer is too small. */
sd_status sd_backend_info(int32_t proto, char *out, size_t cap);

sd_status sd_client_new(int32_t proto, sd_event_fn fn, void *user, sd_client **out);
sd_status sd_client_set_server(sd_client *client, const char *host, uint16_t port);
sd_status sd_client_set_credentials(sd_client *client, const char *user,
                                   const char *password, const char *domain);
/* Requested desktop size for RDP, and the initial framebuffer request for VNC. */
sd_status sd_client_set_size(sd_client *client, uint32_t width, uint32_t height);
sd_status sd_client_set_vnc_security(sd_client *client, uint32_t allowed_mask);
/* The only accepted value is 4: the VNC backend pins a 32bpp pixel format, so every
 * frame it emits is BGRA with stride == width * 4. Any other value returns
 * SD_ERR_ARGUMENT instead of configuring a depth that could never arrive. */
sd_status sd_client_set_vnc_colour_depth(sd_client *client, uint32_t bytes_per_pixel);
/* Clipboard sharing for the next attempt, on by default. Off means nothing is
 * received and nothing is sent: RDP does not load the clipboard channel, VNC
 * registers no cut-text callback, and a push answers SD_ERR_UNAVAILABLE. */
sd_status sd_client_set_clipboard(sd_client *client, int32_t enabled);
/* A local folder the RDP server sees as a drive named `name`. NULL or empty `path`
 * clears it. The folder is checked when the attempt starts: when it is missing or
 * unreadable the session connects without it and reports
 * SD_NOTICE_DRIVE_UNAVAILABLE. VNC accepts and ignores it. */
sd_status sd_client_set_drive(sd_client *client, const char *name, const char *path);

/* Starts the worker thread. Returns once the attempt is queued, not connected. */
sd_status sd_client_connect(sd_client *client);
/* Asks the attempt to stop: aborts a connect in progress, closes a live session,
 * and wakes a pending certificate question. Safe to call from any thread. */
sd_status sd_client_cancel(sd_client *client);
/* Waits for the worker to exit. SD_ERR_STATE means it is still running after
 * `timeout_ms`, i.e. the resources are not released yet. */
sd_status sd_client_wait(sd_client *client, uint32_t timeout_ms);
sd_status sd_client_answer_certificate(sd_client *client, uint32_t request_id,
                                       int32_t accept);

/* A key event carries both forms: RDP needs the PC scancode, VNC needs the X11
 * keysym. The backend uses the one its protocol speaks and ignores the other, so
 * the UI's layout-aware mapping stays above this ABI. */
sd_status sd_client_key(sd_client *client, uint32_t scancode, uint32_t keysym,
                        int32_t down, int32_t extended);
sd_status sd_client_pointer(sd_client *client, uint16_t x, uint16_t y,
                            uint32_t buttons);
/* Replaces the whole local clipboard group offered to the peer; the newest call
 * wins. The entries are copied before this returns. `count` may be 0 (an empty
 * clipboard). SD_ERR_ARGUMENT: a bound above is exceeded, a kind repeats or is
 * unknown, or text is not UTF-8; nothing already queued is changed then. VNC
 * forwards only the text entry. */
sd_status sd_client_clipboard_set(sd_client *client, const sd_clipboard_entry *entries,
                                  uint32_t count);
/* Shorthand for a group holding one NUL-terminated UTF-8 text entry. */
sd_status sd_client_clipboard(sd_client *client, const char *utf8);

/* Unregisters the callback. When it returns no further event is delivered and no
 * callback is in flight, so the caller may then release everything it captured. */
void sd_client_detach(sd_client *client);
/* Cancels, waits, detaches and releases. SD_ERR_STATE means the worker did not
 * exit in time; the handle and its thread keep running rather than being freed
 * underneath the protocol library. */
sd_status sd_client_free(sd_client *client);

/* --- diagnostic probes ---
 *
 * Not session calls: each runs one pure piece of the clipboard handling (the same
 * code the backends run) so it can be tested without a server. Every probe is
 * available in the stub build too, except sd_probe_rdp_settings. */

/* The formats an RDP format list would be fetched as, in request order: text
 * (CF_UNICODETEXT), then one image (registered "PNG", else CF_DIBV5, else CF_DIB).
 * `names[i]` may be NULL. Writes up to 2 ids and sd_clipboard_kind values. */
sd_status sd_probe_rdp_select(const uint32_t *ids, const char *const *names, uint32_t count,
                              uint32_t *out_ids, uint32_t *out_kinds, uint32_t *out_count);
/* One message of a scripted RDP clipboard exchange, server to client. */
typedef struct sd_probe_step {
  uint32_t kind;  /* SD_PROBE_STEP_* */
  uint32_t count; /* format list: number of formats */
  const uint32_t *ids;
  const char *const *names; /* may be NULL, as may each name */
  int32_t ok;               /* answer: 0 means CB_RESPONSE_FAIL */
  uint32_t len;
  const uint8_t *data;
} sd_probe_step;
#define SD_PROBE_STEP_FORMAT_LIST 1u
#define SD_PROBE_STEP_RESPONSE 2u
/* Feeds the steps through the fetch sequence the RDP backend runs on its channel
 * callbacks. Every format id the client would request is written to `requested` (up
 * to `requested_cap`; `*requested_count` is the total), and each finished group and
 * notice is delivered to `fn` as a session would deliver it. */
sd_status sd_probe_rdp_fetch(const sd_probe_step *steps, uint32_t step_count,
                             uint32_t *requested, uint32_t requested_cap,
                             uint32_t *requested_count, sd_event_fn fn, void *user);
/* Text conversions. `out` receives at most `cap` bytes; `*written` is the full size
 * needed. SD_ERR_ARGUMENT when the input cannot be represented (latin1 only). */
sd_status sd_probe_text(int32_t conversion, const uint8_t *in, size_t len, uint8_t *out,
                        size_t cap, size_t *written);
#define SD_PROBE_UTF8_TO_CF_UNICODE 1 /* CRLF line ends, NUL terminated UTF-16LE */
#define SD_PROBE_CF_UNICODE_TO_UTF8 2 /* up to the first NUL */
#define SD_PROBE_UTF8_TO_LATIN1 3
#define SD_PROBE_LATIN1_TO_UTF8 4
/* Builds the FreeRDP settings an attempt with this client's configuration would use
 * and describes the clipboard and drive part as "key=value" words. Emits the drive
 * notice like a real attempt. SD_ERR_UNAVAILABLE without the RDP backend. */
sd_status sd_probe_rdp_settings(sd_client *client, char *out, size_t cap);

#ifdef __cplusplus
}
#endif

#endif /* SHELLUNE_DESKTOP_SHIM_H */