/* The build without the vendored protocol libraries.
 *
 * Keeping the same symbols means the Rust crate, the tests and the application
 * compile and link everywhere, while sd_backend_available reports the truth: the
 * protocol is not built here. Nothing in this file pretends to connect.
 */
#include "sd_internal.h"

#if !defined(SD_WITH_RDP) && !defined(SD_WITH_VNC)

#include <stdio.h>

sd_status sd_backend_available(int32_t proto, char *out, size_t cap) {
  if (proto != SD_PROTOCOL_RDP && proto != SD_PROTOCOL_VNC) return SD_ERR_ARGUMENT;
  int written = snprintf(out, cap,
                         "not built: run packaging/native/build_native.py to link the "
                         "vendored FreeRDP and LibVNCClient");
  if (written < 0) return SD_ERR_INTERNAL;
  if ((size_t)written >= cap) return SD_ERR_ARGUMENT;
  return SD_ERR_UNAVAILABLE;
}

void sd_backend_run(sd_client *client) {
  sd_emit_state(client, SD_STATE_FAILED, SD_FAILURE_INTERNAL, 0,
                "this build has no native desktop backend");
}

void sd_backend_abort(sd_client *client) { (void)client; }

void sd_backend_key(sd_client *client, uint32_t scancode, uint32_t keysym, int32_t down,
                    int32_t extended) {
  (void)client;
  (void)scancode;
  (void)keysym;
  (void)down;
  (void)extended;
}

void sd_backend_pointer(sd_client *client, uint16_t x, uint16_t y, uint32_t buttons) {
  (void)client;
  (void)x;
  (void)y;
  (void)buttons;
}

void sd_backend_clipboard(sd_client *client, sd_clip_set *set) {
  (void)client;
  sd_clip_set_free(set);
}

#endif /* no native backend */
