/* Clipboard groups, text conversions and the RDP fetch sequence.
 *
 * Nothing here touches a protocol library or a session: the functions are pure, so
 * the probes at the end of this file can run them in every build, including the
 * stub one, and the tests prove the same code the backends call.
 */
#include "sd_internal.h"

#include <stdlib.h>
#include <string.h>

/* ------------------------------------------------------------------- UTF-8 */

/* Decodes one code point at `*at`. Returns 1 for a valid sequence; for an invalid
 * one it consumes a single byte, sets U+FFFD and returns 0. */
static int32_t sd_utf8_next(const uint8_t *s, size_t len, size_t *at, uint32_t *cp) {
  size_t i = *at;
  uint8_t b0 = s[i];
  if (b0 < 0x80) {
    *cp = b0;
    *at = i + 1;
    return 1;
  }
  uint32_t need = 0;
  uint32_t value = 0;
  uint32_t min = 0;
  if ((b0 & 0xE0) == 0xC0) {
    need = 1;
    value = b0 & 0x1F;
    min = 0x80;
  } else if ((b0 & 0xF0) == 0xE0) {
    need = 2;
    value = b0 & 0x0F;
    min = 0x800;
  } else if ((b0 & 0xF8) == 0xF0) {
    need = 3;
    value = b0 & 0x07;
    min = 0x10000;
  } else {
    goto invalid;
  }
  if (len - i <= need) goto invalid;
  for (uint32_t k = 1; k <= need; k++) {
    uint8_t b = s[i + k];
    if ((b & 0xC0) != 0x80) goto invalid;
    value = (value << 6) | (b & 0x3F);
  }
  if (value < min || value > 0x10FFFF || (value >= 0xD800 && value <= 0xDFFF)) goto invalid;
  *cp = value;
  *at = i + need + 1;
  return 1;
invalid:
  *cp = 0xFFFD;
  *at = i + 1;
  return 0;
}

static size_t sd_utf8_put(uint8_t *out, uint32_t cp) {
  if (cp < 0x80) {
    out[0] = (uint8_t)cp;
    return 1;
  }
  if (cp < 0x800) {
    out[0] = (uint8_t)(0xC0 | (cp >> 6));
    out[1] = (uint8_t)(0x80 | (cp & 0x3F));
    return 2;
  }
  if (cp < 0x10000) {
    out[0] = (uint8_t)(0xE0 | (cp >> 12));
    out[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
    out[2] = (uint8_t)(0x80 | (cp & 0x3F));
    return 3;
  }
  out[0] = (uint8_t)(0xF0 | (cp >> 18));
  out[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F));
  out[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
  out[3] = (uint8_t)(0x80 | (cp & 0x3F));
  return 4;
}

int32_t sd_clip_utf8_valid(const uint8_t *utf8, size_t len) {
  size_t at = 0;
  uint32_t cp = 0;
  while (at < len) {
    if (!sd_utf8_next(utf8, len, &at, &cp)) return 0;
  }
  return 1;
}

/* UTF-8 to the CF_UNICODETEXT layout: UTF-16LE, CRLF line ends, NUL terminated.
 * Stops at an embedded NUL, which would end the text for every reader anyway. */
uint8_t *sd_clip_utf8_to_cf_unicode(const uint8_t *utf8, size_t len, size_t *out_len) {
  if (out_len != NULL) *out_len = 0;
  if (utf8 == NULL && len > 0) return NULL;
  /* Worst case: every byte is a lone LF (two units) plus the terminator. */
  if (len > (SIZE_MAX / 4) - 1) return NULL;
  uint8_t *out = (uint8_t *)malloc((len * 2 + 1) * 2);
  if (out == NULL) return NULL;
  size_t units = 0;
  size_t at = 0;
  uint32_t previous = 0;
  while (at < len) {
    uint32_t cp = 0;
    sd_utf8_next(utf8, len, &at, &cp);
    if (cp == 0) break;
    if (cp == '\n' && previous != '\r') {
      out[units * 2] = '\r';
      out[units * 2 + 1] = 0;
      units++;
    }
    if (cp >= 0x10000) {
      uint32_t v = cp - 0x10000;
      uint16_t hi = (uint16_t)(0xD800 | (v >> 10));
      uint16_t lo = (uint16_t)(0xDC00 | (v & 0x3FF));
      out[units * 2] = (uint8_t)(hi & 0xFF);
      out[units * 2 + 1] = (uint8_t)(hi >> 8);
      units++;
      out[units * 2] = (uint8_t)(lo & 0xFF);
      out[units * 2 + 1] = (uint8_t)(lo >> 8);
      units++;
    } else {
      out[units * 2] = (uint8_t)(cp & 0xFF);
      out[units * 2 + 1] = (uint8_t)(cp >> 8);
      units++;
    }
    previous = cp;
  }
  out[units * 2] = 0;
  out[units * 2 + 1] = 0;
  units++;
  if (out_len != NULL) *out_len = units * 2;
  return out;
}

/* CF_UNICODETEXT to UTF-8, up to the first NUL. An odd trailing byte is ignored
 * and an unpaired surrogate becomes U+FFFD. Line ends are kept as sent. */
uint8_t *sd_clip_cf_unicode_to_utf8(const uint8_t *utf16, size_t len, size_t *out_len) {
  if (out_len != NULL) *out_len = 0;
  if (utf16 == NULL && len > 0) return NULL;
  size_t units = len / 2;
  if (units > (SIZE_MAX - 1) / 3) return NULL;
  uint8_t *out = (uint8_t *)malloc(units * 3 + 1);
  if (out == NULL) return NULL;
  size_t written = 0;
  for (size_t i = 0; i < units; i++) {
    uint32_t unit = (uint32_t)utf16[i * 2] | ((uint32_t)utf16[i * 2 + 1] << 8);
    if (unit == 0) break;
    uint32_t cp = unit;
    if (unit >= 0xD800 && unit <= 0xDBFF) {
      cp = 0xFFFD;
      if (i + 1 < units) {
        uint32_t next = (uint32_t)utf16[i * 2 + 2] | ((uint32_t)utf16[i * 2 + 3] << 8);
        if (next >= 0xDC00 && next <= 0xDFFF) {
          cp = 0x10000 + (((unit - 0xD800) << 10) | (next - 0xDC00));
          i++;
        }
      }
    } else if (unit >= 0xDC00 && unit <= 0xDFFF) {
      cp = 0xFFFD;
    }
    written += sd_utf8_put(out + written, cp);
  }
  out[written] = 0;
  if (out_len != NULL) *out_len = written;
  return out;
}

uint8_t *sd_clip_utf8_to_latin1(const uint8_t *utf8, size_t len, size_t *out_len) {
  if (out_len != NULL) *out_len = 0;
  if (utf8 == NULL && len > 0) return NULL;
  if (len == SIZE_MAX) return NULL;
  uint8_t *out = (uint8_t *)malloc(len + 1);
  if (out == NULL) return NULL;
  size_t written = 0;
  size_t at = 0;
  while (at < len) {
    uint32_t cp = 0;
    if (!sd_utf8_next(utf8, len, &at, &cp) || cp > 0xFF) {
      free(out);
      return NULL;
    }
    out[written++] = (uint8_t)cp;
  }
  out[written] = 0;
  if (out_len != NULL) *out_len = written;
  return out;
}

uint8_t *sd_clip_latin1_to_utf8(const uint8_t *latin1, size_t len, size_t *out_len) {
  if (out_len != NULL) *out_len = 0;
  if (latin1 == NULL && len > 0) return NULL;
  if (len > (SIZE_MAX - 1) / 2) return NULL;
  uint8_t *out = (uint8_t *)malloc(len * 2 + 1);
  if (out == NULL) return NULL;
  size_t written = 0;
  for (size_t i = 0; i < len; i++) written += sd_utf8_put(out + written, latin1[i]);
  out[written] = 0;
  if (out_len != NULL) *out_len = written;
  return out;
}

/* ------------------------------------------------------------------ groups */

static int32_t sd_clip_kind_valid(uint32_t kind) {
  return kind == SD_CLIPBOARD_TEXT_UTF8 || kind == SD_CLIPBOARD_IMAGE_PNG ||
         kind == SD_CLIPBOARD_IMAGE_DIB;
}

sd_status sd_clip_validate(const sd_clipboard_entry *entries, uint32_t count) {
  if (count > SD_MAX_CLIPBOARD_ENTRIES) return SD_ERR_ARGUMENT;
  if (count > 0 && entries == NULL) return SD_ERR_ARGUMENT;
  uint32_t seen = 0;
  for (uint32_t i = 0; i < count; i++) {
    const sd_clipboard_entry *entry = &entries[i];
    if (!sd_clip_kind_valid(entry->kind)) return SD_ERR_ARGUMENT;
    uint32_t bit = 1u << entry->kind;
    if ((seen & bit) != 0) return SD_ERR_ARGUMENT;
    seen |= bit;
    if (entry->len > 0 && entry->bytes == NULL) return SD_ERR_ARGUMENT;
    if (entry->kind == SD_CLIPBOARD_TEXT_UTF8) {
      if (entry->len > SD_MAX_CLIPBOARD_BYTES) return SD_ERR_ARGUMENT;
      if (!sd_clip_utf8_valid(entry->bytes, entry->len)) return SD_ERR_ARGUMENT;
    } else if (entry->len == 0 || entry->len > SD_MAX_CLIPBOARD_IMAGE_BYTES) {
      return SD_ERR_ARGUMENT;
    }
  }
  return SD_OK;
}

void sd_clip_set_clear(sd_clip_set *set) {
  if (set == NULL) return;
  for (uint32_t i = 0; i < set->count; i++) free(set->items[i].bytes);
  memset(set, 0, sizeof(*set));
}

void sd_clip_set_free(sd_clip_set *set) {
  if (set == NULL) return;
  sd_clip_set_clear(set);
  free(set);
}

int32_t sd_clip_set_add(sd_clip_set *set, uint32_t kind, uint8_t *bytes, uint32_t len) {
  if (set == NULL || set->count >= SD_MAX_CLIPBOARD_ENTRIES ||
      sd_clip_set_find(set, kind) != NULL) {
    free(bytes);
    return 0;
  }
  sd_clip_item *item = &set->items[set->count++];
  item->kind = kind;
  item->len = len;
  item->bytes = bytes;
  return 1;
}

sd_clip_set *sd_clip_set_copy(const sd_clipboard_entry *entries, uint32_t count) {
  sd_clip_set *set = (sd_clip_set *)calloc(1, sizeof(sd_clip_set));
  if (set == NULL) return NULL;
  for (uint32_t i = 0; i < count && i < SD_MAX_CLIPBOARD_ENTRIES; i++) {
    /* One spare byte keeps a zero length entry a real allocation. */
    uint8_t *copy = (uint8_t *)malloc((size_t)entries[i].len + 1);
    if (copy == NULL) {
      sd_clip_set_free(set);
      return NULL;
    }
    if (entries[i].len > 0) memcpy(copy, entries[i].bytes, entries[i].len);
    copy[entries[i].len] = 0;
    sd_clip_set_add(set, entries[i].kind, copy, entries[i].len);
  }
  return set;
}

const sd_clip_item *sd_clip_set_find(const sd_clip_set *set, uint32_t kind) {
  if (set == NULL) return NULL;
  for (uint32_t i = 0; i < set->count; i++) {
    if (set->items[i].kind == kind) return &set->items[i];
  }
  return NULL;
}

/* ------------------------------------------------------------- RDP formats */

static int32_t sd_ascii_equal_nocase(const char *a, const char *b) {
  if (a == NULL || b == NULL) return 0;
  while (*a != '\0' && *b != '\0') {
    char x = *a >= 'A' && *a <= 'Z' ? (char)(*a + 32) : *a;
    char y = *b >= 'A' && *b <= 'Z' ? (char)(*b + 32) : *b;
    if (x != y) return 0;
    a++;
    b++;
  }
  return *a == *b;
}

uint32_t sd_clip_select(const uint32_t *ids, const char *const *names, uint32_t count,
                        uint32_t out_ids[2], uint32_t out_kinds[2]) {
  uint32_t text = 0;
  uint32_t png = 0;
  uint32_t dibv5 = 0;
  uint32_t dib = 0;
  for (uint32_t i = 0; ids != NULL && i < count; i++) {
    uint32_t id = ids[i];
    const char *name = names != NULL ? names[i] : NULL;
    if (id == 0) continue;
    if (name != NULL && name[0] != '\0') {
      /* Registered formats are matched by name; the server picks their ids. */
      if (png == 0 && (sd_ascii_equal_nocase(name, "PNG") ||
                       sd_ascii_equal_nocase(name, "image/png"))) {
        png = id;
      }
      continue;
    }
    if (id == SD_CF_UNICODETEXT && text == 0) text = id;
    if (id == SD_CF_DIBV5 && dibv5 == 0) dibv5 = id;
    if (id == SD_CF_DIB && dib == 0) dib = id;
  }
  uint32_t planned = 0;
  if (text != 0) {
    out_ids[planned] = text;
    out_kinds[planned++] = SD_CLIPBOARD_TEXT_UTF8;
  }
  if (png != 0) {
    out_ids[planned] = png;
    out_kinds[planned++] = SD_CLIPBOARD_IMAGE_PNG;
  } else if (dibv5 != 0 || dib != 0) {
    out_ids[planned] = dibv5 != 0 ? dibv5 : dib;
    out_kinds[planned++] = SD_CLIPBOARD_IMAGE_DIB;
  }
  return planned;
}

void sd_clip_fetch_reset(sd_clip_fetch *fetch) {
  if (fetch == NULL) return;
  sd_clip_set_clear(&fetch->collected);
  memset(fetch, 0, sizeof(*fetch));
}

static uint32_t sd_clip_fetch_start(sd_clip_fetch *fetch, const uint32_t ids[2],
                                    const uint32_t kinds[2], uint32_t count) {
  sd_clip_set_clear(&fetch->collected);
  fetch->too_large = 0;
  fetch->plan_count = count > 2 ? 2 : count;
  for (uint32_t i = 0; i < fetch->plan_count; i++) {
    fetch->plan_ids[i] = ids[i];
    fetch->plan_kinds[i] = kinds[i];
  }
  fetch->next = 0;
  fetch->in_flight = fetch->plan_count > 0 ? 1 : 0;
  return fetch->in_flight ? fetch->plan_ids[0] : 0;
}

uint32_t sd_clip_fetch_on_list(sd_clip_fetch *fetch, const uint32_t *ids,
                               const char *const *names, uint32_t count) {
  uint32_t plan_ids[2] = {0, 0};
  uint32_t plan_kinds[2] = {0, 0};
  uint32_t planned = sd_clip_select(ids, names, count, plan_ids, plan_kinds);
  if (fetch->in_flight) {
    /* The copy being fetched is no longer the remote clipboard. Its outstanding
     * answer still arrives and is dropped; only then does this list's fetch start. */
    fetch->abandoned = 1;
    fetch->queued = 1;
    fetch->queued_count = planned;
    for (uint32_t i = 0; i < planned; i++) {
      fetch->queued_ids[i] = plan_ids[i];
      fetch->queued_kinds[i] = plan_kinds[i];
    }
    return 0;
  }
  return sd_clip_fetch_start(fetch, plan_ids, plan_kinds, planned);
}

static void sd_clip_fetch_store(sd_clip_fetch *fetch, uint32_t kind, const uint8_t *data,
                                uint32_t len) {
  if (data == NULL || len == 0) return;
  if (kind == SD_CLIPBOARD_TEXT_UTF8) {
    /* Each UTF-16 unit is at least one UTF-8 byte, so the unit count before the
     * terminator already proves an oversized text without converting it. */
    size_t units = 0;
    while (units < len / 2 && (data[units * 2] != 0 || data[units * 2 + 1] != 0)) units++;
    if (units > SD_MAX_CLIPBOARD_BYTES) {
      fetch->too_large = 1;
      return;
    }
    size_t out_len = 0;
    uint8_t *text = sd_clip_cf_unicode_to_utf8(data, units * 2, &out_len);
    if (text == NULL) return;
    if (out_len > SD_MAX_CLIPBOARD_BYTES) {
      free(text);
      fetch->too_large = 1;
      return;
    }
    if (out_len == 0) {
      free(text);
      return;
    }
    sd_clip_set_add(&fetch->collected, kind, text, (uint32_t)out_len);
    return;
  }
  if (len > SD_MAX_CLIPBOARD_IMAGE_BYTES) {
    fetch->too_large = 1;
    return;
  }
  uint8_t *copy = (uint8_t *)malloc(len);
  if (copy == NULL) return;
  memcpy(copy, data, len);
  sd_clip_set_add(&fetch->collected, kind, copy, len);
}

uint32_t sd_clip_fetch_on_response(sd_clip_fetch *fetch, int32_t ok, const uint8_t *data,
                                   uint32_t len, sd_clip_set *finished,
                                   int32_t *too_large, int32_t *done) {
  *done = 0;
  *too_large = 0;
  if (!fetch->in_flight || fetch->next >= fetch->plan_count) return 0;
  if (fetch->abandoned) {
    fetch->abandoned = 0;
    fetch->in_flight = 0;
    sd_clip_set_clear(&fetch->collected);
    if (!fetch->queued) return 0;
    fetch->queued = 0;
    return sd_clip_fetch_start(fetch, fetch->queued_ids, fetch->queued_kinds,
                               fetch->queued_count);
  }
  if (ok) sd_clip_fetch_store(fetch, fetch->plan_kinds[fetch->next], data, len);
  fetch->next++;
  if (fetch->next < fetch->plan_count) return fetch->plan_ids[fetch->next];
  fetch->in_flight = 0;
  *done = 1;
  *finished = fetch->collected;
  memset(&fetch->collected, 0, sizeof(fetch->collected));
  *too_large = fetch->too_large;
  fetch->too_large = 0;
  return 0;
}

uint32_t sd_clip_entries(const sd_clip_set *set, sd_clipboard_entry out[SD_MAX_CLIPBOARD_ENTRIES]) {
  uint32_t count = 0;
  for (uint32_t i = 0; set != NULL && i < set->count && i < SD_MAX_CLIPBOARD_ENTRIES; i++) {
    const sd_clip_item *item = &set->items[i];
    uint32_t bound = item->kind == SD_CLIPBOARD_TEXT_UTF8 ? SD_MAX_CLIPBOARD_BYTES
                                                          : SD_MAX_CLIPBOARD_IMAGE_BYTES;
    if (item->len > bound || (item->len > 0 && item->bytes == NULL)) continue;
    out[count].kind = item->kind;
    out[count].len = item->len;
    out[count].bytes = item->bytes;
    count++;
  }
  return count;
}

/* ------------------------------------------------------------------ probes */

static void sd_probe_deliver(const sd_clip_set *set, int32_t too_large, sd_event_fn fn,
                             void *user) {
  if (fn == NULL) return;
  sd_clipboard_entry entries[SD_MAX_CLIPBOARD_ENTRIES];
  uint32_t count = sd_clip_entries(set, entries);
  if (count > 0) {
    sd_event event;
    memset(&event, 0, sizeof(event));
    event.abi_version = SD_ABI_VERSION;
    event.kind = SD_EVENT_CLIPBOARD;
    event.state = SD_STATE_CONNECTED;
    event.entries = entries;
    event.entry_count = count;
    fn(user, &event);
  }
  if (too_large) {
    sd_event event;
    memset(&event, 0, sizeof(event));
    event.abi_version = SD_ABI_VERSION;
    event.kind = SD_EVENT_NOTICE;
    event.state = SD_STATE_CONNECTED;
    event.notice = SD_NOTICE_CLIPBOARD_TOO_LARGE;
    fn(user, &event);
  }
}

sd_status sd_probe_rdp_select(const uint32_t *ids, const char *const *names, uint32_t count,
                              uint32_t *out_ids, uint32_t *out_kinds, uint32_t *out_count) {
  if ((ids == NULL && count > 0) || out_ids == NULL || out_kinds == NULL ||
      out_count == NULL) {
    return SD_ERR_ARGUMENT;
  }
  uint32_t plan_ids[2] = {0, 0};
  uint32_t plan_kinds[2] = {0, 0};
  uint32_t planned = sd_clip_select(ids, names, count, plan_ids, plan_kinds);
  for (uint32_t i = 0; i < planned; i++) {
    out_ids[i] = plan_ids[i];
    out_kinds[i] = plan_kinds[i];
  }
  *out_count = planned;
  return SD_OK;
}

sd_status sd_probe_rdp_fetch(const sd_probe_step *steps, uint32_t step_count,
                             uint32_t *requested, uint32_t requested_cap,
                             uint32_t *requested_count, sd_event_fn fn, void *user) {
  if ((steps == NULL && step_count > 0) || requested_count == NULL ||
      (requested == NULL && requested_cap > 0)) {
    return SD_ERR_ARGUMENT;
  }
  for (uint32_t i = 0; i < step_count; i++) {
    const sd_probe_step *step = &steps[i];
    if (step->kind == SD_PROBE_STEP_FORMAT_LIST) {
      if (step->ids == NULL && step->count > 0) return SD_ERR_ARGUMENT;
    } else if (step->kind == SD_PROBE_STEP_RESPONSE) {
      if (step->data == NULL && step->len > 0) return SD_ERR_ARGUMENT;
    } else {
      return SD_ERR_ARGUMENT;
    }
  }
  /* The same calls, in the same order, as the RDP backend's channel callbacks. */
  sd_clip_fetch fetch;
  memset(&fetch, 0, sizeof(fetch));
  uint32_t asked = 0;
  for (uint32_t i = 0; i < step_count; i++) {
    const sd_probe_step *step = &steps[i];
    uint32_t id = 0;
    if (step->kind == SD_PROBE_STEP_FORMAT_LIST) {
      id = sd_clip_fetch_on_list(&fetch, step->ids, step->names, step->count);
    } else {
      sd_clip_set finished;
      memset(&finished, 0, sizeof(finished));
      int32_t too_large = 0;
      int32_t done = 0;
      id = sd_clip_fetch_on_response(&fetch, step->ok, step->data, step->len, &finished,
                                     &too_large, &done);
      if (done) sd_probe_deliver(&finished, too_large, fn, user);
      sd_clip_set_clear(&finished);
    }
    if (id != 0) {
      if (asked < requested_cap) requested[asked] = id;
      asked++;
    }
  }
  *requested_count = asked;
  sd_clip_fetch_reset(&fetch);
  return SD_OK;
}

sd_status sd_probe_text(int32_t conversion, const uint8_t *in, size_t len, uint8_t *out,
                        size_t cap, size_t *written) {
  if ((in == NULL && len > 0) || written == NULL || (out == NULL && cap > 0)) {
    return SD_ERR_ARGUMENT;
  }
  size_t produced = 0;
  uint8_t *result = NULL;
  switch (conversion) {
    case SD_PROBE_UTF8_TO_CF_UNICODE:
      result = sd_clip_utf8_to_cf_unicode(in, len, &produced);
      break;
    case SD_PROBE_CF_UNICODE_TO_UTF8:
      result = sd_clip_cf_unicode_to_utf8(in, len, &produced);
      break;
    case SD_PROBE_UTF8_TO_LATIN1:
      result = sd_clip_utf8_to_latin1(in, len, &produced);
      break;
    case SD_PROBE_LATIN1_TO_UTF8:
      result = sd_clip_latin1_to_utf8(in, len, &produced);
      break;
    default:
      return SD_ERR_ARGUMENT;
  }
  if (result == NULL) {
    *written = 0;
    return SD_ERR_ARGUMENT;
  }
  *written = produced;
  if (cap > 0) memcpy(out, result, produced < cap ? produced : cap);
  free(result);
  return SD_OK;
}
