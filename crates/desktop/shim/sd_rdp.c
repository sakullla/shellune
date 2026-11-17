/* RDP backend: FreeRDP 3.x behind the shim's opaque handle.
 *
 * Built only into builds that link the vendored FreeRDP (SD_WITH_RDP). Everything
 * here runs on the worker thread except sd_rdp_abort, which FreeRDP documents as
 * callable from another thread while a connect is in progress.
 *
 * The client is created through FreeRDP's client entry points rather than freerdp_new
 * alone: the entry points install the client-common context, which is what creates the
 * bitmap cache gdi_init asserts on and which loads the channels this client needs.
 *
 * Clipboard: the cliprdr channel calls back on its own thread, not the worker. Its
 * state (the channel interface, the local group offered to the server and the fetch
 * of the remote one) lives behind `clip_lock`, which is never held while an event is
 * dispatched. The worker reaches the same state when the UI pushes a local group.
 */
#ifdef SD_WITH_RDP
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#endif
#include "sd_internal.h"

#ifdef SD_WITH_RDP

#include <freerdp/freerdp.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/input.h>
#include <freerdp/settings.h>
#include <freerdp/error.h>
#include <freerdp/client.h>
#include <freerdp/codec/color.h>
#include <freerdp/client/channels.h>
#include <freerdp/channels/channels.h>
#include <freerdp/client/cliprdr.h>
#include <freerdp/client/cmdline.h>
#include <freerdp/channels/cliprdr.h>
#include <freerdp/event.h>
#include <freerdp/version.h>
#include <winpr/version.h>
#include <winpr/wlog.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* How long after the session is up the clipboard channel may take to say it is ready
 * before a waiting push is reported as refused. */
#define SD_RDP_CLIPBOARD_GRACE_MS 10000u

typedef struct sd_rdp_session {
  sd_client *client;
  freerdp *instance;
  rdpContext *context;
  int32_t connected;
  uint32_t buttons;
  int32_t certificate_rejected;
  int32_t limit_hit;
  /* Clipboard sharing for this attempt, copied from the configuration once. */
  int32_t clipboard;

  /* Everything below is shared with the cliprdr channel thread, under clip_lock. */
  CRITICAL_SECTION clip_lock;
  CliprdrClientContext *cliprdr; /* NULL until the channel is connected */
  int32_t clip_ready;            /* the server sent Monitor Ready */
  int32_t clip_waiting;          /* a local group waits for the channel */
  int32_t clip_refused;          /* SD_NOTICE_CLIPBOARD_REFUSED was reported */
  ULONGLONG connected_at;
  sd_clip_set *local;            /* what this client offers the server */
  uint8_t *local_unicode;        /* its text as CF_UNICODETEXT */
  size_t local_unicode_len;
  sd_clip_fetch fetch;           /* the remote copy being fetched */
} sd_rdp_session;

/* The entry points let FreeRDP allocate a context of this size, so the session can be
 * reached from the library's own callbacks without a global. The client-common code
 * requires the allocation to be at least rdpClientContext, so the embedded type is that
 * one and our field follows it. */
typedef struct sd_rdp_context {
  rdpClientContext common;
  sd_rdp_session *session;
} sd_rdp_context;

static sd_rdp_session *sd_session_of(rdpContext *context) {
  if (context == NULL) return NULL;
  return ((sd_rdp_context *)context)->session;
}

/* An embedded GUI must never fall back to client-common's stdin prompts.
 * Credentials were supplied through settings; empty ones let non-NLA servers
 * show their login desktop. NLA still validates them at the server. */
static BOOL sd_rdp_authenticate(freerdp *instance, char **username, char **password,
                                char **domain, rdp_auth_reason reason) {
  (void)instance;
  if (reason != AUTH_RDP && reason != AUTH_TLS && reason != AUTH_NLA &&
      reason != AUTH_RDSTLS) return FALSE;
  if (username == NULL || password == NULL || domain == NULL) return FALSE;
  if (*username == NULL) *username = sd_strdup("");
  if (*password == NULL) *password = sd_strdup("");
  if (*domain == NULL) *domain = sd_strdup("");
  return *username != NULL && *password != NULL && *domain != NULL;
}

/* ---- certificate questions ------------------------------------------------ */

static DWORD sd_rdp_verify_certificate(freerdp *instance, const char *host, UINT16 port,
                                       const char *common_name, const char *subject,
                                       const char *issuer, const char *fingerprint,
                                       DWORD flags) {
  if (instance == NULL || instance->context == NULL) return 0;
  sd_rdp_session *session = sd_session_of(instance->context);
  if (session == NULL) return 0;
  (void)port;
  const char *name = subject != NULL ? subject : common_name;
  /* FreeRDP has already run its own verification: this callback is the user
   * decision, never an automatic accept. */
  int32_t decision =
      sd_ask_certificate(session->client, host, name, issuer, fingerprint, (uint32_t)flags);
  session->certificate_rejected = decision != 1;
  /* The shim uses 1=accept, 2=reject. FreeRDP uses 2=accept temporarily!
   * Trust persistence belongs to the application, never FreeRDP's global store. */
  return decision == 1 ? 2 : 0;
}

static DWORD sd_rdp_verify_changed_certificate(freerdp *instance, const char *host,
                                               UINT16 port, const char *common_name,
                                               const char *subject, const char *issuer,
                                               const char *new_fingerprint,
                                               const char *old_subject,
                                               const char *old_issuer,
                                               const char *old_fingerprint, DWORD flags) {
  if (instance == NULL || instance->context == NULL) return 0;
  sd_rdp_session *session = sd_session_of(instance->context);
  if (session == NULL) return 0;
  (void)port;
  (void)old_subject;
  (void)old_issuer;
  (void)old_fingerprint;
  const char *name = subject != NULL ? subject : common_name;
  int32_t decision =
      sd_ask_certificate(session->client, host, name, issuer, new_fingerprint, (uint32_t)flags);
  session->certificate_rejected = decision != 1;
  return decision == 1 ? 2 : 0;
}

/* ---- frame delivery ------------------------------------------------------- */

static BOOL sd_rdp_begin_paint(rdpContext *context) {
  rdpGdi *gdi = context->gdi;
  if (gdi != NULL && gdi->primary != NULL && gdi->primary->hdc != NULL &&
      gdi->primary->hdc->hwnd != NULL && gdi->primary->hdc->hwnd->invalid != NULL) {
    gdi->primary->hdc->hwnd->invalid->null = TRUE;
  }
  return TRUE;
}

static BOOL sd_rdp_end_paint(rdpContext *context) {
  sd_rdp_session *session = sd_session_of(context);
  if (session == NULL) return TRUE;
  rdpGdi *gdi = context->gdi;
  if (gdi == NULL || gdi->primary == NULL || gdi->primary->hdc == NULL ||
      gdi->primary->hdc->hwnd == NULL) {
    return TRUE;
  }
  HGDI_RGN invalid = gdi->primary->hdc->hwnd->invalid;
  if (invalid == NULL || invalid->null) return TRUE;
  if (invalid->w <= 0 || invalid->h <= 0) return TRUE;
  if (gdi->primary_buffer == NULL || gdi->width <= 0 || gdi->height <= 0) return TRUE;
  if (gdi->stride < (UINT32)gdi->width * 4u) return TRUE;

  /* Clip the dirty rectangle to the buffer and to the byte bound before handing the
   * pointer out: the callback copies the pixels synchronously and never keeps them. */
  int32_t x = invalid->x < 0 ? 0 : invalid->x;
  int32_t y = invalid->y < 0 ? 0 : invalid->y;
  int32_t w = invalid->w;
  int32_t h = invalid->h;
  if (x >= gdi->width || y >= gdi->height) return TRUE;
  if (x + w > gdi->width) w = gdi->width - x;
  if (y + h > gdi->height) h = gdi->height - y;
  if (w <= 0 || h <= 0) return TRUE;
  if ((uint64_t)w * (uint64_t)h * 4u > SD_MAX_FRAME_BYTES) {
    sd_emit_state(session->client, SD_STATE_CONNECTED, SD_FAILURE_LIMIT, 0,
                  "desktop update exceeded the frame byte bound");
    return TRUE;
  }

  const uint8_t *pixels = gdi->primary_buffer + (size_t)y * gdi->stride + (size_t)x * 4u;
  sd_emit_frame(session->client, x, y, w, h, pixels, gdi->stride, (uint32_t)gdi->width,
                (uint32_t)gdi->height);
  return TRUE;
}

static BOOL sd_rdp_desktop_resize(rdpContext *context) {
  sd_rdp_session *session = sd_session_of(context);
  rdpGdi *gdi = context->gdi;
  rdpSettings *settings = context->settings;
  if (gdi == NULL || settings == NULL) return TRUE;
  uint32_t width = freerdp_settings_get_uint32(settings, FreeRDP_DesktopWidth);
  uint32_t height = freerdp_settings_get_uint32(settings, FreeRDP_DesktopHeight);
  if (width == 0 || height == 0) return TRUE;
  if ((uint64_t)width * (uint64_t)height * 4u > SD_MAX_FRAME_BYTES) {
    if (session != NULL) {
      sd_emit_state(session->client, SD_STATE_CONNECTED, SD_FAILURE_LIMIT, 0,
                    "desktop size exceeded the frame byte bound");
    }
    return FALSE;
  }
  return gdi_resize(gdi, width, height);
}

/* FreeRDP calls this once the connection is established, which is also the first
 * moment the bitmap cache exists: gdi_init asserts on it, so it cannot be called any
 * earlier. */
static BOOL sd_rdp_post_connect(freerdp *instance) {
  if (instance == NULL || instance->context == NULL) return FALSE;
  sd_rdp_session *session = sd_session_of(instance->context);
  if (session == NULL) return FALSE;
  uint32_t width = freerdp_settings_get_uint32(instance->context->settings, FreeRDP_DesktopWidth);
  uint32_t height = freerdp_settings_get_uint32(instance->context->settings, FreeRDP_DesktopHeight);
  if (width == 0 || height == 0 || (uint64_t)width * height * 4u > SD_MAX_FRAME_BYTES) {
    session->limit_hit = 1;
    return FALSE;
  }
  if (!gdi_init(instance, PIXEL_FORMAT_BGRA32)) return FALSE;
  if (instance->context->update == NULL) return FALSE;
  instance->context->update->BeginPaint = sd_rdp_begin_paint;
  instance->context->update->EndPaint = sd_rdp_end_paint;
  instance->context->update->DesktopResize = sd_rdp_desktop_resize;
  return TRUE;
}

/* ---- clipboard channel ---------------------------------------------------- */

static sd_rdp_session *sd_rdp_clip_session(CliprdrClientContext *cliprdr) {
  return cliprdr != NULL ? (sd_rdp_session *)cliprdr->custom : NULL;
}

/* Announces the local group. Called with clip_lock held. */
static void sd_rdp_clip_announce(sd_rdp_session *session) {
  CliprdrClientContext *cliprdr = session->cliprdr;
  if (cliprdr == NULL || cliprdr->ClientFormatList == NULL) return;
  CLIPRDR_FORMAT formats[SD_MAX_CLIPBOARD_ENTRIES];
  UINT32 count = 0;
  memset(formats, 0, sizeof(formats));
  if (session->local != NULL) {
    if (session->local_unicode != NULL &&
        sd_clip_set_find(session->local, SD_CLIPBOARD_TEXT_UTF8) != NULL) {
      formats[count++].formatId = SD_CF_UNICODETEXT;
    }
    if (sd_clip_set_find(session->local, SD_CLIPBOARD_IMAGE_PNG) != NULL) {
      formats[count].formatId = SD_CF_LOCAL_PNG;
      formats[count++].formatName = (char *)"PNG";
    }
    if (sd_clip_set_find(session->local, SD_CLIPBOARD_IMAGE_DIB) != NULL) {
      formats[count++].formatId = SD_CF_DIB;
    }
  }
  CLIPRDR_FORMAT_LIST list;
  memset(&list, 0, sizeof(list));
  list.common.msgType = CB_FORMAT_LIST;
  list.numFormats = count;
  list.formats = count > 0 ? formats : NULL;
  (void)cliprdr->ClientFormatList(cliprdr, &list);
}

/* Asks for one format of the remote copy. Called with clip_lock held. */
static void sd_rdp_clip_request(sd_rdp_session *session, uint32_t format_id) {
  CliprdrClientContext *cliprdr = session->cliprdr;
  if (format_id == 0 || cliprdr == NULL || cliprdr->ClientFormatDataRequest == NULL) return;
  CLIPRDR_FORMAT_DATA_REQUEST request;
  memset(&request, 0, sizeof(request));
  request.common.msgType = CB_FORMAT_DATA_REQUEST;
  request.requestedFormatId = format_id;
  if (cliprdr->ClientFormatDataRequest(cliprdr, &request) != CHANNEL_RC_OK) {
    /* No answer will come for a request that was never sent. */
    sd_clip_fetch_reset(&session->fetch);
  }
}

static UINT sd_rdp_clip_server_capabilities(CliprdrClientContext *cliprdr,
                                            const CLIPRDR_CAPABILITIES *capabilities) {
  (void)cliprdr;
  (void)capabilities;
  return CHANNEL_RC_OK;
}

static UINT sd_rdp_clip_monitor_ready(CliprdrClientContext *cliprdr,
                                      const CLIPRDR_MONITOR_READY *ready) {
  (void)ready;
  sd_rdp_session *session = sd_rdp_clip_session(cliprdr);
  if (session == NULL) return CHANNEL_RC_OK;
  CLIPRDR_GENERAL_CAPABILITY_SET general;
  memset(&general, 0, sizeof(general));
  general.capabilitySetType = CB_CAPSTYPE_GENERAL;
  general.capabilitySetLength = CB_CAPSTYPE_GENERAL_LEN;
  general.version = CB_CAPS_VERSION_2;
  general.generalFlags = CB_USE_LONG_FORMAT_NAMES;
  CLIPRDR_CAPABILITIES capabilities;
  memset(&capabilities, 0, sizeof(capabilities));
  capabilities.common.msgType = CB_CLIP_CAPS;
  capabilities.cCapabilitiesSets = 1;
  capabilities.capabilitySets = (CLIPRDR_CAPABILITY_SET *)&general;

  EnterCriticalSection(&session->clip_lock);
  if (cliprdr->ClientCapabilities != NULL) {
    (void)cliprdr->ClientCapabilities(cliprdr, &capabilities);
  }
  session->clip_ready = 1;
  session->clip_waiting = 0;
  /* Always announce, even an empty group: the protocol expects a format list after
   * Monitor Ready, and an empty one tells the server this side has nothing yet. */
  sd_rdp_clip_announce(session);
  LeaveCriticalSection(&session->clip_lock);
  return CHANNEL_RC_OK;
}

static UINT sd_rdp_clip_server_format_list(CliprdrClientContext *cliprdr,
                                           const CLIPRDR_FORMAT_LIST *list) {
  sd_rdp_session *session = sd_rdp_clip_session(cliprdr);
  if (session == NULL || list == NULL) return CHANNEL_RC_OK;
  uint32_t count = list->formats != NULL ? list->numFormats : 0;
  uint32_t *ids = NULL;
  const char **names = NULL;
  if (count > 0) {
    ids = (uint32_t *)calloc(count, sizeof(uint32_t));
    names = (const char **)calloc(count, sizeof(const char *));
    if (ids == NULL || names == NULL) count = 0;
    for (uint32_t i = 0; i < count; i++) {
      ids[i] = list->formats[i].formatId;
      names[i] = list->formats[i].formatName;
    }
  }
  EnterCriticalSection(&session->clip_lock);
  if (cliprdr->ClientFormatListResponse != NULL) {
    CLIPRDR_FORMAT_LIST_RESPONSE response;
    memset(&response, 0, sizeof(response));
    response.common.msgType = CB_FORMAT_LIST_RESPONSE;
    response.common.msgFlags = CB_RESPONSE_OK;
    (void)cliprdr->ClientFormatListResponse(cliprdr, &response);
  }
  uint32_t first = sd_clip_fetch_on_list(&session->fetch, ids, names, count);
  sd_rdp_clip_request(session, first);
  LeaveCriticalSection(&session->clip_lock);
  free(ids);
  free(names);
  return CHANNEL_RC_OK;
}

static UINT sd_rdp_clip_server_format_list_response(
    CliprdrClientContext *cliprdr, const CLIPRDR_FORMAT_LIST_RESPONSE *response) {
  sd_rdp_session *session = sd_rdp_clip_session(cliprdr);
  if (session == NULL || response == NULL) return CHANNEL_RC_OK;
  if ((response->common.msgFlags & CB_RESPONSE_FAIL) == 0) return CHANNEL_RC_OK;
  int32_t notify = 0;
  EnterCriticalSection(&session->clip_lock);
  if (!session->clip_refused) {
    session->clip_refused = 1;
    notify = 1;
  }
  LeaveCriticalSection(&session->clip_lock);
  if (notify) sd_emit_notice(session->client, SD_NOTICE_CLIPBOARD_REFUSED);
  return CHANNEL_RC_OK;
}

static UINT sd_rdp_clip_server_data_request(CliprdrClientContext *cliprdr,
                                            const CLIPRDR_FORMAT_DATA_REQUEST *request) {
  sd_rdp_session *session = sd_rdp_clip_session(cliprdr);
  if (session == NULL || request == NULL) return CHANNEL_RC_OK;
  EnterCriticalSection(&session->clip_lock);
  const uint8_t *data = NULL;
  size_t len = 0;
  const sd_clip_item *item = NULL;
  switch (request->requestedFormatId) {
    case SD_CF_UNICODETEXT:
      if (session->local_unicode != NULL) {
        data = session->local_unicode;
        len = session->local_unicode_len;
      }
      break;
    case SD_CF_LOCAL_PNG:
      item = sd_clip_set_find(session->local, SD_CLIPBOARD_IMAGE_PNG);
      break;
    case SD_CF_DIB:
      item = sd_clip_set_find(session->local, SD_CLIPBOARD_IMAGE_DIB);
      break;
    default:
      break;
  }
  if (item != NULL) {
    data = item->bytes;
    len = item->len;
  }
  CLIPRDR_FORMAT_DATA_RESPONSE response;
  memset(&response, 0, sizeof(response));
  response.common.msgType = CB_FORMAT_DATA_RESPONSE;
  if (data != NULL && len > 0 && len <= UINT32_MAX) {
    response.common.msgFlags = CB_RESPONSE_OK;
    response.common.dataLen = (UINT32)len;
    response.requestedFormatData = data;
  } else {
    /* A format this client does not hold, or no longer holds. */
    response.common.msgFlags = CB_RESPONSE_FAIL;
  }
  /* Sent under the lock: the bytes belong to the local group, which a push on the
   * worker could otherwise replace while the channel copies them. */
  if (cliprdr->ClientFormatDataResponse != NULL) {
    (void)cliprdr->ClientFormatDataResponse(cliprdr, &response);
  }
  LeaveCriticalSection(&session->clip_lock);
  return CHANNEL_RC_OK;
}

static UINT sd_rdp_clip_server_data_response(CliprdrClientContext *cliprdr,
                                             const CLIPRDR_FORMAT_DATA_RESPONSE *response) {
  sd_rdp_session *session = sd_rdp_clip_session(cliprdr);
  if (session == NULL || response == NULL) return CHANNEL_RC_OK;
  sd_clip_set finished;
  memset(&finished, 0, sizeof(finished));
  int32_t too_large = 0;
  int32_t done = 0;
  EnterCriticalSection(&session->clip_lock);
  int32_t ok = (response->common.msgFlags & CB_RESPONSE_FAIL) == 0 ? 1 : 0;
  uint32_t next = sd_clip_fetch_on_response(&session->fetch, ok, response->requestedFormatData,
                                            response->common.dataLen, &finished,
                                            &too_large, &done);
  sd_rdp_clip_request(session, next);
  LeaveCriticalSection(&session->clip_lock);
  /* Dispatched without clip_lock: the consumer's callback must never sit between the
   * channel thread and a worker push. */
  if (done) {
    sd_emit_clipboard(session->client, &finished);
    if (too_large) sd_emit_notice(session->client, SD_NOTICE_CLIPBOARD_TOO_LARGE);
  }
  sd_clip_set_clear(&finished);
  return CHANNEL_RC_OK;
}

static void sd_rdp_channel_connected(void *context, const ChannelConnectedEventArgs *e) {
  sd_rdp_session *session = sd_session_of((rdpContext *)context);
  if (session == NULL || e == NULL || e->name == NULL) return;
  if (strcmp(e->name, CLIPRDR_SVC_CHANNEL_NAME) != 0 || e->pInterface == NULL) return;
  CliprdrClientContext *cliprdr = (CliprdrClientContext *)e->pInterface;
  EnterCriticalSection(&session->clip_lock);
  cliprdr->custom = session;
  cliprdr->ServerCapabilities = sd_rdp_clip_server_capabilities;
  cliprdr->MonitorReady = sd_rdp_clip_monitor_ready;
  cliprdr->ServerFormatList = sd_rdp_clip_server_format_list;
  cliprdr->ServerFormatListResponse = sd_rdp_clip_server_format_list_response;
  cliprdr->ServerFormatDataRequest = sd_rdp_clip_server_data_request;
  cliprdr->ServerFormatDataResponse = sd_rdp_clip_server_data_response;
  session->cliprdr = cliprdr;
  LeaveCriticalSection(&session->clip_lock);
}

static void sd_rdp_channel_disconnected(void *context, const ChannelDisconnectedEventArgs *e) {
  sd_rdp_session *session = sd_session_of((rdpContext *)context);
  if (session == NULL || e == NULL || e->name == NULL) return;
  if (strcmp(e->name, CLIPRDR_SVC_CHANNEL_NAME) != 0) return;
  EnterCriticalSection(&session->clip_lock);
  if (session->cliprdr == (CliprdrClientContext *)e->pInterface) {
    session->cliprdr->custom = NULL;
    session->cliprdr = NULL;
    session->clip_ready = 0;
    sd_clip_fetch_reset(&session->fetch);
  }
  LeaveCriticalSection(&session->clip_lock);
}

/* Reports a push that has waited past the grace period for a channel the server
 * never made ready. Worker thread. */
static void sd_rdp_clip_tick(sd_rdp_session *session) {
  if (!session->clipboard) return;
  int32_t notify = 0;
  EnterCriticalSection(&session->clip_lock);
  if (session->clip_waiting && !session->clip_ready && !session->clip_refused &&
      GetTickCount64() - session->connected_at > SD_RDP_CLIPBOARD_GRACE_MS) {
    session->clip_refused = 1;
    notify = 1;
  }
  LeaveCriticalSection(&session->clip_lock);
  if (notify) sd_emit_notice(session->client, SD_NOTICE_CLIPBOARD_REFUSED);
}

/* ---- connection options ---------------------------------------------------- */

/* Whether a folder can be offered as a drive: an absolute, existing, listable
 * directory short enough for the drive channel, which measures its base path with
 * a MAX_PATH bound. FreeRDP's own special values ("*" all drives, "%" home) are
 * never accepted from here. */
static int32_t sd_rdp_folder_usable(const char *path) {
  if (path == NULL || path[0] == '\0' || path[0] == '*' || path[0] == '%') return 0;
  WCHAR wide[MAX_PATH];
  int units = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, MAX_PATH - 3);
  if (units <= 1) return 0;
  size_t length = (size_t)units - 1;
  int32_t drive_absolute = length >= 3 &&
                           ((wide[0] >= L'A' && wide[0] <= L'Z') ||
                            (wide[0] >= L'a' && wide[0] <= L'z')) &&
                           wide[1] == L':' && (wide[2] == L'\\' || wide[2] == L'/');
  int32_t unc = length >= 3 && (wide[0] == L'\\' || wide[0] == L'/') &&
                (wide[1] == L'\\' || wide[1] == L'/');
  if (!drive_absolute && !unc) return 0;
  DWORD attributes = GetFileAttributesW(wide);
  if (attributes == INVALID_FILE_ATTRIBUTES || (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
    return 0;
  }
  /* Listing proves read access; a folder the user cannot open would only fail later,
   * inside the remote session, with no explanation. */
  WCHAR pattern[MAX_PATH];
  memcpy(pattern, wide, length * sizeof(WCHAR));
  size_t at = length;
  if (wide[length - 1] != L'\\' && wide[length - 1] != L'/') pattern[at++] = L'\\';
  pattern[at++] = L'*';
  pattern[at] = 0;
  WIN32_FIND_DATAW found;
  HANDLE listing = FindFirstFileW(pattern, &found);
  if (listing == INVALID_HANDLE_VALUE) return GetLastError() == ERROR_FILE_NOT_FOUND ? 1 : 0;
  FindClose(listing);
  return 1;
}

typedef struct sd_rdp_options {
  int32_t clipboard;
  char *drive_name;
  char *drive_path;
} sd_rdp_options;

static void sd_rdp_options_read(sd_client *client, sd_rdp_options *options) {
  memset(options, 0, sizeof(*options));
  EnterCriticalSection(&client->lock);
  options->clipboard = client->clipboard_enabled;
  options->drive_name = sd_strdup(client->drive_name);
  options->drive_path = sd_strdup(client->drive_path);
  LeaveCriticalSection(&client->lock);
}

static void sd_rdp_options_free(sd_rdp_options *options) {
  free(options->drive_name);
  free(options->drive_path);
  memset(options, 0, sizeof(*options));
}

/* Writes the clipboard and drive choices into FreeRDP's settings. Returns FALSE only
 * when FreeRDP refuses a setting; an unusable folder sets `*drive_unavailable` and the
 * session goes ahead without it. */
static BOOL sd_rdp_apply_options(rdpSettings *settings, const sd_rdp_options *options,
                                 int32_t *drive_unavailable) {
  *drive_unavailable = 0;
  /* Explicit both ways: FreeRDP's default is on, and off must mean the channel is
   * never loaded, not merely ignored. */
  if (!freerdp_settings_set_bool(settings, FreeRDP_RedirectClipboard,
                                 options->clipboard ? TRUE : FALSE)) {
    return FALSE;
  }
  if (options->drive_path != NULL && options->drive_path[0] != '\0') {
    const char *name = options->drive_name != NULL && options->drive_name[0] != '\0'
                           ? options->drive_name
                           : "Shellune";
    if (!sd_rdp_folder_usable(options->drive_path)) {
      *drive_unavailable = 1;
    } else {
      const char *const params[3] = {"drive", name, options->drive_path};
      if (!freerdp_client_add_device_channel(settings, 3, params) ||
          freerdp_device_collection_find(settings, name) == NULL) {
        *drive_unavailable = 1;
      }
    }
  }
  return TRUE;
}

/* ---- error classification ------------------------------------------------- */

static int32_t sd_rdp_failure_of(rdpContext *context, int32_t *detail) {
  UINT32 code = freerdp_get_last_error(context);
  *detail = (int32_t)(code & 0x7FFFFFFFu);
  if (code == FREERDP_ERROR_CONNECT_CANCELLED) return SD_FAILURE_CANCELLED;
  if (code == FREERDP_ERROR_AUTHENTICATION_FAILED) return SD_FAILURE_AUTHENTICATION;
  if (code == FREERDP_ERROR_INSUFFICIENT_PRIVILEGES) return SD_FAILURE_AUTHENTICATION;
  if (code == FREERDP_ERROR_TLS_CONNECT_FAILED) return SD_FAILURE_TLS;
  if (code == FREERDP_ERROR_DNS_ERROR || code == FREERDP_ERROR_DNS_NAME_NOT_FOUND) {
    return SD_FAILURE_CONNECT;
  }
  if (code == FREERDP_ERROR_CONNECT_FAILED || code == FREERDP_ERROR_PRE_CONNECT_FAILED ||
      code == FREERDP_ERROR_POST_CONNECT_FAILED) {
    return SD_FAILURE_CONNECT;
  }
  if (code == FREERDP_ERROR_SUCCESS || code == FREERDP_ERROR_NONE) {
    return SD_FAILURE_REMOTE_CLOSED;
  }
  return SD_FAILURE_PROTOCOL;
}

static const char *sd_rdp_error_text(freerdp *instance) {
  if (instance == NULL || instance->context == NULL) return "RDP connect failed";
  UINT32 code = freerdp_get_last_error(instance->context);
  const char *text = freerdp_get_last_error_string(code);
  return text != NULL ? text : "RDP connect failed";
}

/* ---- backend entry points ------------------------------------------------- */

void sd_rdp_run(sd_client *client) {
  sd_rdp_session *session = (sd_rdp_session *)calloc(1, sizeof(sd_rdp_session));
  if (session == NULL) {
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0,
                  "could not allocate the RDP session");
    return;
  }
  session->client = client;
  InitializeCriticalSection(&session->clip_lock);
  /* An RDP-only process may never have opened a Rust/VNC socket. FreeRDP's
   * embedded client entry points leave Winsock initialization to the host. */
  WSADATA sockets;
  int socket_error = WSAStartup(MAKEWORD(2, 2), &sockets);
  if (socket_error != 0) {
    DeleteCriticalSection(&session->clip_lock);
    free(session);
    sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_IO, socket_error,
                  "could not initialize Windows sockets");
    return;
  }

  /* FreeRDP logs through WinPR to stderr; errors are enough here, and the reason a
   * connect failed is reported through freerdp_get_last_error_string. */
  WLog_SetLogLevel(WLog_GetRoot(), WLOG_ERROR);

  /* Copy the configuration once, on the worker thread, so the UI may reconfigure for
   * the next attempt without racing this one. */
  char *host = NULL;
  char *username = NULL;
  char *password = NULL;
  char *domain = NULL;
  uint16_t port = 3389;
  uint32_t width = 0;
  uint32_t height = 0;
  EnterCriticalSection(&client->lock);
  host = sd_strdup(client->host);
  username = sd_strdup(client->username);
  password = sd_strdup(client->password);
  domain = sd_strdup(client->domain);
  port = client->port != 0 ? client->port : 3389;
  width = client->width;
  height = client->height;
  LeaveCriticalSection(&client->lock);
  sd_rdp_options options;
  sd_rdp_options_read(client, &options);
  session->clipboard = options.clipboard;

  int32_t status = SD_FAILURE_INTERNAL;
  const char *reason = NULL;
  int32_t detail = 0;
  rdpContext *context = NULL;

  /* FreeRDP's client-common code checks that the allocation is at least
   * rdpClientContext before casting it, so the size is part of the contract. */
  if (sizeof(sd_rdp_context) < sizeof(rdpClientContext)) {
    reason = "the session context is smaller than rdpClientContext";
    goto teardown;
  }

  RDP_CLIENT_ENTRY_POINTS entry = {0};
  entry.Size = sizeof(entry);
  entry.Version = RDP_CLIENT_INTERFACE_VERSION;
  entry.ContextSize = sizeof(sd_rdp_context);
  context = freerdp_client_context_new(&entry);

  if (context == NULL) {
    reason = "could not create a FreeRDP client context";
    goto teardown;
  }
  session->context = context;
  session->instance = context->instance;
  ((sd_rdp_context *)context)->session = session;

  {
    freerdp *instance = context->instance;
    rdpSettings *settings = context->settings;
    if (settings == NULL) {
      reason = "FreeRDP returned no settings";
      goto teardown;
    }
    if (host == NULL) {
      reason = "no host configured";
      goto teardown;
    }
    if (!freerdp_settings_set_string(settings, FreeRDP_ServerHostname, host) ||
        !freerdp_settings_set_uint32(settings, FreeRDP_ServerPort, port) ||
        !freerdp_settings_set_uint32(settings, FreeRDP_DesktopWidth, width) ||
        !freerdp_settings_set_uint32(settings, FreeRDP_DesktopHeight, height)) {
      reason = "could not apply the connection settings";
      goto teardown;
    }
    freerdp_settings_set_uint32(settings, FreeRDP_ColorDepth, 32);
    freerdp_settings_set_uint32(settings, FreeRDP_TcpConnectTimeout, 30000);
    /* Activation needs several round trips; the library's 9s default cannot
     * complete a session on a 6-10s RTT link. Cancellation still wakes its event. */
    freerdp_settings_set_uint32(settings, FreeRDP_TcpAckTimeout, 120000);
    freerdp_settings_set_bool(settings, FreeRDP_NegotiateSecurityLayer, TRUE);
    freerdp_settings_set_bool(settings, FreeRDP_AudioPlayback, FALSE);
    freerdp_settings_set_bool(settings, FreeRDP_AudioCapture, FALSE);
    if (username != NULL) freerdp_settings_set_string(settings, FreeRDP_Username, username);
    if (password != NULL) freerdp_settings_set_string(settings, FreeRDP_Password, password);
    if (domain != NULL) freerdp_settings_set_string(settings, FreeRDP_Domain, domain);

    int32_t drive_unavailable = 0;
    if (!sd_rdp_apply_options(settings, &options, &drive_unavailable)) {
      reason = "could not apply the clipboard and drive settings";
      goto teardown;
    }
    if (drive_unavailable) sd_emit_notice(client, SD_NOTICE_DRIVE_UNAVAILABLE);
    if (options.clipboard &&
        (PubSub_SubscribeChannelConnected(context->pubSub, sd_rdp_channel_connected) < 0 ||
         PubSub_SubscribeChannelDisconnected(context->pubSub, sd_rdp_channel_disconnected) <
             0)) {
      reason = "could not watch the clipboard channel";
      goto teardown;
    }

    instance->VerifyCertificateEx = sd_rdp_verify_certificate;
    instance->AuthenticateEx = sd_rdp_authenticate;
    instance->VerifyChangedCertificateEx = sd_rdp_verify_changed_certificate;
    instance->PostConnect = sd_rdp_post_connect;

    if (!freerdp_client_load_addins(context->channels, settings)) {
      reason = "could not load the RDP channels";
      goto teardown;
    }

    /* Published before the blocking connect, so an abort from another thread finds
     * the context it has to cancel instead of arriving too early. */
    sd_backend_publish(client, session);

    if (!freerdp_connect(instance)) {
      if (sd_is_cancelled(client)) {
        status = SD_FAILURE_CANCELLED;
        reason = "connect cancelled";
      } else if (session->certificate_rejected) {
        status = SD_FAILURE_CERTIFICATE_REJECTED;
        reason = "server certificate was not accepted";
      } else if (session->limit_hit) {
        status = SD_FAILURE_LIMIT;
        reason = "desktop size exceeded the frame byte bound";
      } else {
        status = sd_rdp_failure_of(context, &detail);
        reason = sd_rdp_error_text(instance);
      }
      goto teardown;
    }

    session->connected = 1;
    status = SD_FAILURE_NONE;
    EnterCriticalSection(&session->clip_lock);
    session->connected_at = GetTickCount64();
    LeaveCriticalSection(&session->clip_lock);
    sd_emit_state(client, SD_STATE_CONNECTED, SD_FAILURE_NONE, 0, NULL);

    {
      HANDLE events[64];
      while (!sd_is_cancelled(client)) {
        DWORD count = freerdp_get_event_handles(context, events, 64);
        if (count == 0) {
          status = SD_FAILURE_IO;
          reason = "FreeRDP stopped providing event handles";
          break;
        }
        DWORD waited = WaitForMultipleObjects(count, events, FALSE, 50);
        if (waited == WAIT_FAILED) {
          status = SD_FAILURE_IO;
          detail = (int32_t)GetLastError();
          reason = "waiting for RDP events failed";
          break;
        }
        if (!freerdp_check_event_handles(context)) {
          if (sd_is_cancelled(client)) {
            status = SD_FAILURE_CANCELLED;
            reason = "cancelled";
          } else {
            status = sd_rdp_failure_of(context, &detail);
            reason = sd_rdp_error_text(instance);
          }
          break;
        }
        sd_flush_input(client);
        sd_rdp_clip_tick(session);
      }
      if (sd_is_cancelled(client)) {
        status = SD_FAILURE_CANCELLED;
        reason = "cancelled";
      }
    }
  }

teardown:
  /* sd_backend_retire clears the published pointer and then waits until no reader
   * still holds a reference, so no thread can be inside
   * freerdp_abort_connect_context while the context is freed below. The input path
   * is not part of that protocol because it runs on this same worker thread, which
   * is already past its loop. */
  sd_backend_retire(client);

  if (session->context != NULL) {
    if (session->connected && session->instance != NULL) {
      freerdp_disconnect(session->instance);
    }
    session->connected = 0;
    if (session->instance != NULL) gdi_free(session->instance);
    /* Frees the instance as well; calling freerdp_free afterwards would be a double
     * free of the client entry points. */
    freerdp_client_context_free(session->context);
    session->context = NULL;
    session->instance = NULL;
  }

  /* The channels are gone with the context, so no callback can reach these now. */
  sd_clip_set_free(session->local);
  free(session->local_unicode);
  sd_clip_fetch_reset(&session->fetch);
  DeleteCriticalSection(&session->clip_lock);
  sd_rdp_options_free(&options);

  free(host);
  free(username);
  free(password);
  free(domain);
  free(session);
  WSACleanup();

  if (status == SD_FAILURE_CANCELLED) {
    sd_emit_state(client, SD_STATE_CLOSED, SD_FAILURE_CANCELLED, 0,
                  reason != NULL ? reason : "cancelled");
  } else if (status == SD_FAILURE_NONE) {
    sd_emit_state(client, SD_STATE_CLOSED, SD_FAILURE_NONE, 0, "session ended");
  } else {
    sd_emit_state(client, SD_STATE_FAILED, status, detail,
                  reason != NULL ? reason : "RDP session failed");
  }
}

void sd_rdp_abort(sd_client *client) {
  /* The reference is held across the freerdp call, not just the pointer load:
   * teardown frees the context only after this reference is released. */
  sd_rdp_session *session = (sd_rdp_session *)sd_backend_acquire(client);
  if (session == NULL) return;
  rdpContext *context = session->context;
  if (context != NULL) freerdp_abort_connect_context(context);
  sd_backend_release(client);
}

static rdpInput *sd_rdp_input(sd_client *client, sd_rdp_session **out_session) {
  /* Reached from sd_flush_input only, which runs on the worker thread - the same
   * thread that retires the session - so the returned pointer cannot be freed
   * underneath the caller. It is read under backend_lock so it is never a value
   * published by another thread without that lock. */
  EnterCriticalSection(&client->backend_lock);
  sd_rdp_session *session = (sd_rdp_session *)client->backend;
  LeaveCriticalSection(&client->backend_lock);
  if (out_session != NULL) *out_session = session;
  if (session == NULL || session->context == NULL) return NULL;
  return session->context->input;
}

void sd_rdp_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                int32_t extended) {
  rdpInput *input = sd_rdp_input(client, NULL);
  /* VNC's keysym form is meaningless here: RDP is driven by scancodes. */
  (void)keysym;
  if (input == NULL || scancode > 0xFF) return;
  UINT16 flags = (UINT16)(down ? KBD_FLAGS_DOWN : KBD_FLAGS_RELEASE);
  if (extended) flags |= KBD_FLAGS_EXTENDED;
  freerdp_input_send_keyboard_event(input, flags, (UINT8)scancode);
}

void sd_rdp_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons) {
  sd_rdp_session *session = NULL;
  rdpInput *input = sd_rdp_input(client, &session);
  if (input == NULL || session == NULL) return;

  uint32_t previous = session->buttons;
  session->buttons = buttons;
  static const uint32_t masks[3] = {SD_BUTTON_LEFT, SD_BUTTON_MIDDLE, SD_BUTTON_RIGHT};
  static const UINT16 flags[3] = {PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON3, PTR_FLAGS_BUTTON2};
  for (size_t i = 0; i < 3; i++) {
    uint32_t changed = (previous ^ buttons) & masks[i];
    if (changed == 0) continue;
    UINT16 event = PTR_FLAGS_MOVE | flags[i];
    if ((buttons & masks[i]) != 0) event |= PTR_FLAGS_DOWN;
    freerdp_input_send_mouse_event(input, event, x, y);
  }
  if ((buttons & SD_BUTTON_WHEEL_UP) != 0) {
    freerdp_input_send_mouse_event(input, PTR_FLAGS_WHEEL | 0x0078, x, y);
  }
  if ((buttons & SD_BUTTON_WHEEL_DOWN) != 0) {
    freerdp_input_send_mouse_event(input, PTR_FLAGS_WHEEL | PTR_FLAGS_WHEEL_NEGATIVE | 0x0088,
                                   x, y);
  }
  freerdp_input_send_mouse_event(input, PTR_FLAGS_MOVE, x, y);
}

void sd_rdp_clipboard(sd_client *client, sd_clip_set *set) {
  sd_rdp_session *session = NULL;
  sd_rdp_input(client, &session);
  if (session == NULL || !session->clipboard || set == NULL) {
    sd_clip_set_free(set);
    return;
  }
  /* Converted here, on the worker, so the channel thread only copies bytes. */
  uint8_t *unicode = NULL;
  size_t unicode_len = 0;
  const sd_clip_item *text = sd_clip_set_find(set, SD_CLIPBOARD_TEXT_UTF8);
  if (text != NULL) {
    unicode = sd_clip_utf8_to_cf_unicode(text->bytes, text->len, &unicode_len);
    if (unicode == NULL) {
      sd_clip_set_free(set);
      return;
    }
  }
  int32_t notify = 0;
  EnterCriticalSection(&session->clip_lock);
  sd_clip_set *stale = session->local;
  uint8_t *stale_unicode = session->local_unicode;
  session->local = set;
  session->local_unicode = unicode;
  session->local_unicode_len = unicode_len;
  if (session->cliprdr != NULL && session->clip_ready) {
    sd_rdp_clip_announce(session);
  } else {
    /* Announced on Monitor Ready. A channel that never got there within the grace
     * period was refused by the server: said once, then silent. */
    session->clip_waiting = 1;
    if (!session->clip_refused &&
        (session->cliprdr == NULL ||
         GetTickCount64() - session->connected_at > SD_RDP_CLIPBOARD_GRACE_MS)) {
      session->clip_refused = 1;
      notify = 1;
    }
  }
  LeaveCriticalSection(&session->clip_lock);
  sd_clip_set_free(stale);
  free(stale_unicode);
  if (notify) sd_emit_notice(client, SD_NOTICE_CLIPBOARD_REFUSED);
}

/* Whether FreeRDP actually initialised a static channel of this name. */
static int32_t sd_rdp_channel_loaded(rdpChannels *channels, const char *name) {
  StaticClientChannelStats *stats = freerdp_channels_client_stats(channels);
  if (stats == NULL) return 0;
  int32_t found = 0;
  for (size_t i = 0; i < stats->count; i++) {
    if (strcmp(stats->stats[i].channelName, name) == 0) found = 1;
  }
  freerdp_channel_client_stats_free(stats);
  return found;
}

sd_status sd_rdp_probe_settings(sd_client *client, char *out, size_t cap) {
  WSADATA sockets;
  if (WSAStartup(MAKEWORD(2, 2), &sockets) != 0) return SD_ERR_INTERNAL;
  WLog_SetLogLevel(WLog_GetRoot(), WLOG_ERROR);
  sd_rdp_options options;
  sd_rdp_options_read(client, &options);
  sd_status result = SD_ERR_INTERNAL;
  int32_t drive_unavailable = 0;
  RDP_CLIENT_ENTRY_POINTS entry = {0};
  entry.Size = sizeof(entry);
  entry.Version = RDP_CLIENT_INTERFACE_VERSION;
  entry.ContextSize = sizeof(sd_rdp_context);
  rdpContext *context = freerdp_client_context_new(&entry);
  if (context != NULL && context->settings != NULL) {
    rdpSettings *settings = context->settings;
    /* The same calls, in the same order, as sd_rdp_run before it connects. */
    if (sd_rdp_apply_options(settings, &options, &drive_unavailable) &&
        freerdp_client_load_addins(context->channels, settings)) {
      const char *name = options.drive_name != NULL && options.drive_name[0] != '\0'
                             ? options.drive_name
                             : "Shellune";
      RDPDR_DEVICE *device = freerdp_device_collection_find(settings, name);
      const char *drive_path = "";
      if (device != NULL && device->Type == RDPDR_DTYP_FILESYSTEM &&
          ((RDPDR_DRIVE *)device)->Path != NULL) {
        drive_path = ((RDPDR_DRIVE *)device)->Path;
      }
      int written = snprintf(
          out, cap,
          "redirect_clipboard=%d cliprdr=%d device_redirection=%d rdpdr=%d drive=%s "
          "drive_path=%s",
          freerdp_settings_get_bool(settings, FreeRDP_RedirectClipboard) ? 1 : 0,
          sd_rdp_channel_loaded(context->channels, CLIPRDR_SVC_CHANNEL_NAME),
          freerdp_settings_get_bool(settings, FreeRDP_DeviceRedirection) ? 1 : 0,
          sd_rdp_channel_loaded(context->channels, "rdpdr"),
          device != NULL ? name : "", drive_path);
      if (written < 0) {
        result = SD_ERR_INTERNAL;
      } else if ((size_t)written >= cap) {
        result = SD_ERR_ARGUMENT;
      } else {
        result = SD_OK;
      }
    }
  }
  if (context != NULL) freerdp_client_context_free(context);
  sd_rdp_options_free(&options);
  WSACleanup();
  if (drive_unavailable) sd_emit_notice(client, SD_NOTICE_DRIVE_UNAVAILABLE);
  return result;
}

sd_status sd_rdp_available(char *out, size_t cap) {
  int written = snprintf(out, cap, "freerdp=%s winpr=%s tls=openssl clipboard=text,image",
                         FREERDP_VERSION_FULL, WINPR_VERSION_FULL);
  if (written < 0) return SD_ERR_INTERNAL;
  if ((size_t)written >= cap) return SD_ERR_ARGUMENT;
  return SD_OK;
}

#endif /* SD_WITH_RDP */
