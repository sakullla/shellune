//! The C ABI of \`shim/shellune_desktop_shim.h\`, nothing more.
//!
//! Every type here mirrors the header exactly; \`tests/desktop_native.rs\` checks
//! the layout offsets so a change on either side fails loudly instead of reading
//! the wrong field.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_void};

pub const SD_ABI_VERSION: u32 = 2;

pub const SD_MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
pub const SD_MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const SD_MAX_CLIPBOARD_ENTRIES: usize = 3;

pub const SD_OK: i32 = 0;
pub const SD_ERR_ARGUMENT: i32 = 1;
pub const SD_ERR_UNAVAILABLE: i32 = 2;
pub const SD_ERR_STATE: i32 = 3;
pub const SD_ERR_ABORTED: i32 = 4;
pub const SD_ERR_INTERNAL: i32 = 5;

pub const SD_PROTOCOL_RDP: i32 = 1;
pub const SD_PROTOCOL_VNC: i32 = 2;

pub const SD_BUTTON_LEFT: u32 = 0x01;
pub const SD_BUTTON_MIDDLE: u32 = 0x02;
pub const SD_BUTTON_RIGHT: u32 = 0x04;
pub const SD_BUTTON_WHEEL_UP: u32 = 0x08;
pub const SD_BUTTON_WHEEL_DOWN: u32 = 0x10;
pub const SD_BUTTON_WHEEL_LEFT: u32 = 0x20;
pub const SD_BUTTON_WHEEL_RIGHT: u32 = 0x40;

pub const SD_VNC_SECURITY_NONE: u32 = 0x1;
pub const SD_VNC_SECURITY_VNC_AUTH: u32 = 0x2;
pub const SD_VNC_SECURITY_TLS: u32 = 0x4;
pub const SD_VNC_SECURITY_VENCRYPT: u32 = 0x8;

pub const SD_EVENT_STATE: u32 = 0;
pub const SD_EVENT_FRAME: u32 = 1;
pub const SD_EVENT_CERTIFICATE: u32 = 2;
pub const SD_EVENT_CLIPBOARD: u32 = 3;
pub const SD_EVENT_NOTICE: u32 = 4;

pub const SD_CLIPBOARD_TEXT_UTF8: u32 = 1;
pub const SD_CLIPBOARD_IMAGE_PNG: u32 = 2;
pub const SD_CLIPBOARD_IMAGE_DIB: u32 = 3;

pub const SD_NOTICE_CLIPBOARD_REFUSED: u32 = 1;
pub const SD_NOTICE_CLIPBOARD_LATIN1_ONLY: u32 = 2;
pub const SD_NOTICE_CLIPBOARD_TOO_LARGE: u32 = 3;
pub const SD_NOTICE_DRIVE_UNAVAILABLE: u32 = 4;

pub const SD_PROBE_UTF8_TO_CF_UNICODE: i32 = 1;
pub const SD_PROBE_CF_UNICODE_TO_UTF8: i32 = 2;
pub const SD_PROBE_UTF8_TO_LATIN1: i32 = 3;
pub const SD_PROBE_LATIN1_TO_UTF8: i32 = 4;
pub const SD_PROBE_STEP_FORMAT_LIST: u32 = 1;
pub const SD_PROBE_STEP_RESPONSE: u32 = 2;

pub const SD_STATE_IDLE: u32 = 0;
pub const SD_STATE_CONNECTING: u32 = 1;
pub const SD_STATE_CONNECTED: u32 = 2;
pub const SD_STATE_CLOSING: u32 = 3;
pub const SD_STATE_CLOSED: u32 = 4;
pub const SD_STATE_FAILED: u32 = 5;

#[repr(C)]
pub struct sd_event {
    pub abi_version: u32,
    pub kind: u32,
    pub state: u32,
    pub failure: u32,
    pub detail: i32,
    pub request_id: u32,
    pub cert_flags: u32,
    pub text: *const c_char,
    pub text_len: u32,
    pub host: *const c_char,
    pub subject: *const c_char,
    pub issuer: *const c_char,
    pub fingerprint: *const c_char,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub pixels: *const u8,
    pub entries: *const sd_clipboard_entry,
    pub entry_count: u32,
    pub notice: u32,
}

#[repr(C)]
pub struct sd_clipboard_entry {
    pub kind: u32,
    pub len: u32,
    pub bytes: *const u8,
}

#[repr(C)]
pub struct sd_probe_step {
    pub kind: u32,
    pub count: u32,
    pub ids: *const u32,
    pub names: *const *const c_char,
    pub ok: i32,
    pub len: u32,
    pub data: *const u8,
}

/// Opaque in the header; the crate never looks inside.
#[repr(C)]
pub struct sd_client {
    _private: [u8; 0],
}

pub type sd_event_fn = extern "C" fn(user: *mut c_void, event: *const sd_event);

unsafe extern "C" {
    pub fn sd_abi_version() -> u32;
    pub fn sd_status_name(status: i32) -> *const c_char;
    pub fn sd_failure_name(failure: i32) -> *const c_char;
    pub fn sd_state_name(state: i32) -> *const c_char;
    pub fn sd_backend_info(proto: i32, out: *mut c_char, cap: usize) -> i32;
    pub fn sd_client_new(
        proto: i32,
        callback: sd_event_fn,
        user: *mut c_void,
        out: *mut *mut sd_client,
    ) -> i32;
    pub fn sd_client_set_server(client: *mut sd_client, host: *const c_char, port: u16) -> i32;
    pub fn sd_client_set_credentials(
        client: *mut sd_client,
        user: *const c_char,
        password: *const c_char,
        domain: *const c_char,
    ) -> i32;
    pub fn sd_client_set_size(client: *mut sd_client, width: u32, height: u32) -> i32;
    pub fn sd_client_set_vnc_security(client: *mut sd_client, mask: u32) -> i32;
    pub fn sd_client_set_vnc_colour_depth(client: *mut sd_client, bytes_per_pixel: u32) -> i32;
    pub fn sd_client_connect(client: *mut sd_client) -> i32;
    pub fn sd_client_cancel(client: *mut sd_client) -> i32;
    pub fn sd_client_wait(client: *mut sd_client, timeout_ms: u32) -> i32;
    pub fn sd_client_answer_certificate(
        client: *mut sd_client,
        request_id: u32,
        accept: i32,
    ) -> i32;
    pub fn sd_client_key(
        client: *mut sd_client,
        scancode: u32,
        keysym: u32,
        down: i32,
        extended: i32,
    ) -> i32;
    pub fn sd_client_pointer(client: *mut sd_client, x: u16, y: u16, buttons: u32) -> i32;
    pub fn sd_client_set_clipboard(client: *mut sd_client, enabled: i32) -> i32;
    pub fn sd_client_set_drive(
        client: *mut sd_client,
        name: *const c_char,
        path: *const c_char,
    ) -> i32;
    pub fn sd_client_clipboard_set(
        client: *mut sd_client,
        entries: *const sd_clipboard_entry,
        count: u32,
    ) -> i32;
    pub fn sd_client_detach(client: *mut sd_client);
    pub fn sd_client_free(client: *mut sd_client) -> i32;

    pub fn sd_probe_rdp_select(
        ids: *const u32,
        names: *const *const c_char,
        count: u32,
        out_ids: *mut u32,
        out_kinds: *mut u32,
        out_count: *mut u32,
    ) -> i32;
    pub fn sd_probe_rdp_fetch(
        steps: *const sd_probe_step,
        step_count: u32,
        requested: *mut u32,
        requested_cap: u32,
        requested_count: *mut u32,
        callback: sd_event_fn,
        user: *mut c_void,
    ) -> i32;
    pub fn sd_probe_text(
        conversion: i32,
        input: *const u8,
        len: usize,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> i32;
    pub fn sd_probe_rdp_settings(client: *mut sd_client, out: *mut c_char, cap: usize) -> i32;
}
