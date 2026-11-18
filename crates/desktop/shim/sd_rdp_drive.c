/* RDP drive: checking the mapped folder, adding it to the settings, and probes.
 *
 * Keeping the server inside the folder is FreeRDP's job here, through the patch in
 * packaging/native/patches/freerdp-drive-confinement.patch: every open, create,
 * listing, rename target and delete is resolved (GetFinalPathNameByHandleW) and
 * refused with access denied when it lands outside the folder, which is what a
 * symbolic link, junction or mount point inside it would do. The probes below drive
 * that patched drive service the way the rdpdr channel does, so the tests prove the
 * code that ships rather than a copy of it.
 */
#ifdef SD_WITH_RDP
#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#else
#include <winpr/winsock.h>
#endif
#endif
#include "sd_internal.h"

#ifdef SD_WITH_RDP

#include <freerdp/freerdp.h>
#include <freerdp/settings.h>
#include <freerdp/client.h>
#include <freerdp/client/channels.h>
#include <freerdp/channels/rdpdr.h>
#include <winpr/stream.h>
#include <winpr/wlog.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if !defined(_WIN32)
#include <dirent.h>
#include <errno.h>
#include <sys/stat.h>
#include <winpr/string.h>
#endif

#if defined(_WIN32)
static int32_t sd_drive_is_sep(WCHAR c) { return c == L'/' || c == L'\\'; }
#endif

int32_t sd_rdp_drive_check(const char *path) {
  if (path == NULL || path[0] == '\0') return SD_DRIVE_MISSING;
  /* The drive service bounds the UTF-8 path with MAX_PATH bytes (strnlen) and the
   * UTF-16 base path with MAX_PATH units: a longer one would be cut short and name
   * another folder. Three units stay free for the separator it may add. */
  if (strlen(path) >= MAX_PATH) return SD_DRIVE_TOO_LONG;
  WCHAR wide[MAX_PATH];
#if defined(_WIN32)
  int units = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, MAX_PATH);
#else
  /* WinPR's converter, which counts without the terminator and fails (-1) on
   * invalid UTF-8. Sized first: it does not bound a short buffer. */
  int units = 0;
  SSIZE_T needed = ConvertUtf8ToWChar(path, NULL, 0);
  if (needed >= 0 && needed < MAX_PATH) {
    SSIZE_T converted = ConvertUtf8ToWChar(path, wide, MAX_PATH);
    if (converted >= 0) units = (int)converted + 1;
  }
#endif
  if (units <= 1) return SD_DRIVE_NOT_ABSOLUTE;
  size_t length = (size_t)units - 1;
  if (length >= MAX_PATH - 3) return SD_DRIVE_TOO_LONG;
#if !defined(_WIN32)
  /* macOS and Linux: an absolute path. FreeRDP's own special values ("*" every
   * mount, "%" home, "DynamicDrives") never start with '/'. */
  if (path[0] != '/') return SD_DRIVE_NOT_ABSOLUTE;
  struct stat info;
  if (stat(path, &info) != 0) {
    return errno == EACCES ? SD_DRIVE_NO_ACCESS : SD_DRIVE_MISSING;
  }
  if (!S_ISDIR(info.st_mode)) return SD_DRIVE_NOT_A_FOLDER;
  /* Listing proves read access, as on Windows. */
  DIR *listing = opendir(path);
  if (listing == NULL) return SD_DRIVE_NO_ACCESS;
  closedir(listing);
  return 0;
#else
  /* A drive letter path or a UNC share; FreeRDP's own special values ("*" every
   * drive, "%" home, "DynamicDrives") and the device namespaces ("\\?\", "\\.\")
   * are never accepted from here. */
  int32_t drive_absolute = length >= 3 &&
                           ((wide[0] >= L'A' && wide[0] <= L'Z') ||
                            (wide[0] >= L'a' && wide[0] <= L'z')) &&
                           wide[1] == L':' && sd_drive_is_sep(wide[2]);
  int32_t unc = length >= 3 && sd_drive_is_sep(wide[0]) && sd_drive_is_sep(wide[1]) &&
                !sd_drive_is_sep(wide[2]) &&
                !((wide[2] == L'?' || wide[2] == L'.') && (length == 3 || sd_drive_is_sep(wide[3])));
  if (!drive_absolute && !unc) return SD_DRIVE_NOT_ABSOLUTE;
  DWORD attributes = GetFileAttributesW(wide);
  if (attributes == INVALID_FILE_ATTRIBUTES) {
    return GetLastError() == ERROR_ACCESS_DENIED ? SD_DRIVE_NO_ACCESS : SD_DRIVE_MISSING;
  }
  if ((attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) return SD_DRIVE_NOT_A_FOLDER;
  /* Listing proves read access; a folder the user cannot open would only fail later,
   * inside the remote session, with no explanation. */
  WCHAR pattern[MAX_PATH + 2];
  memcpy(pattern, wide, length * sizeof(WCHAR));
  size_t at = length;
  if (!sd_drive_is_sep(wide[length - 1])) pattern[at++] = L'\\';
  pattern[at++] = L'*';
  pattern[at] = 0;
  WIN32_FIND_DATAW found;
  HANDLE listing = FindFirstFileW(pattern, &found);
  if (listing == INVALID_HANDLE_VALUE) {
    return GetLastError() == ERROR_FILE_NOT_FOUND ? 0 : SD_DRIVE_NO_ACCESS;
  }
  FindClose(listing);
  return 0;
#endif
}

int32_t sd_rdp_drive_add(rdpSettings *settings, const char *name, const char *path) {
  if (settings == NULL || name == NULL || path == NULL) return SD_DRIVE_NOT_ADDED;
  const char *const params[3] = {"drive", name, path};
  if (!freerdp_client_add_device_channel(settings, 3, params) ||
      freerdp_device_collection_find(settings, name) == NULL) {
    return SD_DRIVE_NOT_ADDED;
  }
  return 0;
}

/* ---- probe: requests through FreeRDP's drive service ------------------------ */

typedef struct sd_probe_devman {
  DEVMAN devman; /* first: the drive service reads id_sequence through it */
  DEVICE *device;
} sd_probe_devman;

static UINT sd_probe_register(DEVMAN *devman, DEVICE *device) {
  sd_probe_devman *probe = (sd_probe_devman *)(void *)devman;
  if (probe->device != NULL) return ERROR_INVALID_PARAMETER;
  device->id = probe->devman.id_sequence++;
  probe->device = device;
  return CHANNEL_RC_OK;
}

typedef struct sd_probe_answer {
  HANDLE done;
  NTSTATUS status;
  uint32_t file_id;
} sd_probe_answer;

/* An IRP that knows where its answer goes; the drive thread completes it. */
typedef struct sd_probe_irp {
  IRP irp; /* first: the device sees only this */
  sd_probe_answer *answer;
} sd_probe_irp;

static UINT sd_probe_answered(IRP *irp) {
  sd_probe_answer *answer = ((sd_probe_irp *)irp)->answer;
  answer->status = irp->IoStatus;
  answer->file_id = 0;
  if (irp->MajorFunction == IRP_MJ_CREATE && Stream_GetPosition(irp->output) >= 4) {
    const BYTE *out = Stream_Buffer(irp->output);
    answer->file_id = (uint32_t)out[0] | ((uint32_t)out[1] << 8) | ((uint32_t)out[2] << 16) |
                      ((uint32_t)out[3] << 24);
  }
  Stream_Free(irp->input, TRUE);
  Stream_Free(irp->output, TRUE);
  free(irp);
  SetEvent(answer->done);
  return CHANNEL_RC_OK;
}

/* Hands one request to the device and waits for its answer. */
static int32_t sd_probe_send(DEVICE *device, DEVMAN *devman, const sd_probe_drive_request *request,
                             uint32_t file_id, sd_probe_answer *answer) {
  sd_probe_irp *wrapper = (sd_probe_irp *)calloc(1, sizeof(sd_probe_irp));
  wStream *input = Stream_New(NULL, (size_t)request->len + 1);
  wStream *output = Stream_New(NULL, 256);
  if (wrapper == NULL || input == NULL || output == NULL) {
    free(wrapper);
    if (input != NULL) Stream_Free(input, TRUE);
    if (output != NULL) Stream_Free(output, TRUE);
    return 0;
  }
  if (request->len > 0) Stream_Write(input, request->body, request->len);
  Stream_SealLength(input);
  Stream_SetPosition(input, 0);
  IRP *irp = &wrapper->irp;
  wrapper->answer = answer;
  irp->input = input;
  irp->output = output;
  irp->device = device;
  irp->devman = devman;
  irp->FileId = request->use_file ? file_id : 0;
  irp->MajorFunction = request->major;
  irp->MinorFunction = request->minor;
  irp->Complete = sd_probe_answered;
  irp->Discard = sd_probe_answered;
  ResetEvent(answer->done);
  if (device->IRPRequest(device, irp) != CHANNEL_RC_OK) return 0;
  return WaitForSingleObject(answer->done, 10000) == WAIT_OBJECT_0;
}

sd_status sd_probe_rdp_drive_requests(const char *base, const sd_probe_drive_request *requests,
                                      uint32_t count, uint32_t *statuses) {
  if (base == NULL || (requests == NULL && count > 0) || (statuses == NULL && count > 0)) {
    return SD_ERR_ARGUMENT;
  }
  for (uint32_t i = 0; i < count; i++) {
    if (requests[i].body == NULL && requests[i].len > 0) return SD_ERR_ARGUMENT;
  }
  WSADATA sockets;
  if (WSAStartup(MAKEWORD(2, 2), &sockets) != 0) return SD_ERR_INTERNAL;
  WLog_SetLogLevel(WLog_GetRoot(), WLOG_FATAL);
  sd_status result = SD_ERR_INTERNAL;
  RDP_CLIENT_ENTRY_POINTS entry = {0};
  entry.Size = sizeof(entry);
  entry.Version = RDP_CLIENT_INTERFACE_VERSION;
  entry.ContextSize = (DWORD)sd_rdp_context_size();
  rdpContext *context = freerdp_client_context_new(&entry);
  sd_probe_devman devman;
  memset(&devman, 0, sizeof(devman));
  devman.devman.id_sequence = 1;
  sd_probe_answer answer;
  memset(&answer, 0, sizeof(answer));
  answer.done = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (context == NULL || context->settings == NULL || answer.done == NULL ||
      sd_rdp_drive_add(context->settings, "Shellune", base) != 0) {
    goto out;
  }
  {
    /* The settings entry an attempt adds, loaded the way rdpdr loads it. */
    RDPDR_DEVICE *device = freerdp_device_collection_find(context->settings, "Shellune");
    PDEVICE_SERVICE_ENTRY service = WINPR_FUNC_PTR_CAST(
        freerdp_load_channel_addin_entry("drive", NULL, "DeviceServiceEntry", 0),
        PDEVICE_SERVICE_ENTRY);
    if (device == NULL || service == NULL) goto out;
    DEVICE_SERVICE_ENTRY_POINTS points;
    memset(&points, 0, sizeof(points));
    points.devman = &devman.devman;
    points.RegisterDevice = sd_probe_register;
    points.device = device;
    points.rdpcontext = context;
    if (service(&points) != CHANNEL_RC_OK || devman.device == NULL) goto out;
    uint32_t file_id = 0;
    result = SD_OK;
    for (uint32_t i = 0; i < count; i++) {
      if (!sd_probe_send(devman.device, &devman.devman, &requests[i], file_id, &answer)) {
        result = SD_ERR_INTERNAL;
        break;
      }
      statuses[i] = (uint32_t)answer.status;
      if (requests[i].major == IRP_MJ_CREATE && answer.status == 0) file_id = answer.file_id;
    }
  }
out:
  /* Joins the drive thread and closes (and deletes on close) what it opened. */
  if (devman.device != NULL && devman.device->Free != NULL) devman.device->Free(devman.device);
  if (answer.done != NULL) CloseHandle(answer.done);
  if (context != NULL) freerdp_client_context_free(context);
  WSACleanup();
  return result;
}

sd_status sd_probe_rdp_drive_folder(const char *path, int32_t *problem) {
  if (path == NULL || problem == NULL) return SD_ERR_ARGUMENT;
  *problem = sd_rdp_drive_check(path);
  return SD_OK;
}

#else /* !SD_WITH_RDP */

sd_status sd_probe_rdp_drive_requests(const char *base, const sd_probe_drive_request *requests,
                                      uint32_t count, uint32_t *statuses) {
  (void)base;
  (void)requests;
  (void)count;
  (void)statuses;
  return SD_ERR_UNAVAILABLE;
}

sd_status sd_probe_rdp_drive_folder(const char *path, int32_t *problem) {
  (void)path;
  if (problem != NULL) *problem = 0;
  return SD_ERR_UNAVAILABLE;
}

#endif /* SD_WITH_RDP */
