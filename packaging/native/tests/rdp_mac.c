/* Exercise the linked FreeRDP state machine, capturing actual encrypted PDUs.
 * No server, credentials, private keys or network connection are needed. */
#include <freerdp/config.h>
#include <freerdp/freerdp.h>
#include <freerdp/settings.h>
#include <freerdp/transport_io.h>
#include <freerdp/update.h>
#include <freerdp/input.h>
#include <winpr/stream.h>
#include "core/rdp.h"
#include "core/connection.h"
#include <stdio.h>

static unsigned info_flags;
static unsigned last_flags;
static unsigned packets;
static BOOL stop_after_write;

static int capture(rdpTransport* transport, wStream* stream)
{
    (void)transport;
    const BYTE* p = Stream_Buffer(stream);
    const size_t length = Stream_GetPosition(stream);
    /* TPKT + X.224 + MCS SendDataRequest, then the PER length determinant. */
    if (length < 18 || p[0] != 3 || p[7] != 0x64)
        return -1;
    const size_t offset = (p[13] & 0x80) ? 15 : 14;
    last_flags = p[offset] | (p[offset + 1] << 8);
    if (last_flags & SEC_INFO_PKT)
        info_flags = last_flags;
    packets++;
    return stop_after_write ? -1 : (int)length;
}

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #condition); goto out; \
} } while (0)

int main(int argc, char** argv)
{
    int result = 1;
    freerdp* instance = freerdp_new();
    wStream* dummy = NULL;
    CHECK(argc == 2 && instance && freerdp_context_new(instance));
    rdpRdp* rdp = instance->context->rdp;
    rdp->mcs->userId = MCS_BASE_CHANNEL_ID + 1;
    rdpSettings* settings = instance->context->settings;
    rdpTransportIo io = *freerdp_get_io_callbacks(instance->context);
    io.WritePdu = capture;
    CHECK(freerdp_set_io_callbacks(instance->context, &io));
    rdpCertificate* cert = freerdp_certificate_new_from_file(argv[1]);
    CHECK(cert);
    CHECK(freerdp_settings_set_pointer_len(settings, FreeRDP_RdpServerCertificate, cert, 1));
    CHECK(freerdp_settings_set_pointer_len(settings, FreeRDP_ServerRandom, NULL, 32));
    CHECK(freerdp_settings_set_bool(settings, FreeRDP_UseRdpSecurityLayer, TRUE));
    CHECK(freerdp_settings_set_uint32(settings, FreeRDP_EncryptionMethods, ENCRYPTION_METHOD_128BIT));
    CHECK(freerdp_settings_set_string(settings, FreeRDP_ClientHostname, "mac-test"));
    CHECK(freerdp_settings_set_bool(settings, FreeRDP_FrameMarkerCommandEnabled, FALSE));
    CHECK(freerdp_settings_get_bool(settings, FreeRDP_SaltedChecksum));

    /* Include stale state from a previous connection: early packets must reset it. */
    rdp->do_secure_checksum = TRUE;
    CHECK(rdp_client_skip_mcs_channel_join(rdp));
    CHECK(packets == 2);
    CHECK((info_flags & SEC_ENCRYPT) != 0);
    CHECK((info_flags & SEC_SECURE_CHECKSUM) == 0);

    dummy = Stream_New(NULL, 1);
    CHECK(dummy);
    stop_after_write = TRUE; /* Stop after Confirm Active, before unrelated UI setup. */
    const BOOL modes[] = { FALSE, TRUE, FALSE, TRUE };
    for (size_t i = 0; i < sizeof(modes) / sizeof(modes[0]); i++)
    {
        CHECK(freerdp_settings_set_bool(settings, FreeRDP_SaltedChecksum, modes[i]));
        CHECK(rdp_client_transition_to_state(rdp, CONNECTION_STATE_CAPABILITIES_EXCHANGE_CONFIRM_ACTIVE));
        const unsigned before = packets;
        const state_run_t state = rdp_client_connect_confirm_active(rdp, dummy);
        CHECK(state == STATE_RUN_FAILED); /* Deliberate transport stop. */
        CHECK(packets == before + 1);
        CHECK((last_flags & SEC_ENCRYPT) != 0);
        CHECK(((last_flags & SEC_SECURE_CHECKSUM) != 0) == modes[i]);
    }
    puts("PASS: encrypted Client Info uses plain MAC; Confirm Active follows negotiated MAC, including reactivation");
    result = 0;
out:
    Stream_Free(dummy, TRUE);
    if (instance) { freerdp_context_free(instance); freerdp_free(instance); }
    return result;
}
