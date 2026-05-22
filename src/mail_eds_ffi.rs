use gtk4::gio::ffi::GCancellable;
use gtk4::glib::ffi::{GError, GList, GPtrArray, gboolean, gpointer};
use libc::{c_char, c_int, c_void, mode_t};

#[repr(C)]
pub struct ESource {
    _private: [u8; 0],
}

#[repr(C)]
pub struct ESourceRegistry {
    _private: [u8; 0],
}

#[repr(C)]
pub struct ESourceBackend {
    _private: [u8; 0],
}

#[repr(C)]
pub struct EMailSession {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelService {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelStore {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelFolder {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelFolderSummary {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelFolderInfo {
    pub next: *mut CamelFolderInfo,
    pub parent: *mut CamelFolderInfo,
    pub child: *mut CamelFolderInfo,
    pub full_name: *mut c_char,
    pub display_name: *mut c_char,
    pub flags: u32,
    pub unread: i32,
    pub total: i32,
}

#[repr(C)]
pub struct CamelMessageInfo {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelMimeMessage {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelInternetAddress {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelStream {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CamelDataWrapper {
    _private: [u8; 0],
}

pub type CamelStoreGetFolderInfoFlags = u32;
pub type CamelStoreGetFolderFlags = u32;
pub type CamelFolderInfoFlags = u32;

pub const CAMEL_STORE_FOLDER_INFO_FAST: CamelStoreGetFolderInfoFlags = 1 << 0;
pub const CAMEL_STORE_FOLDER_INFO_RECURSIVE: CamelStoreGetFolderInfoFlags = 1 << 1;
pub const CAMEL_STORE_FOLDER_INFO_SUBSCRIBED: CamelStoreGetFolderInfoFlags = 1 << 2;

pub const CAMEL_FOLDER_NOSELECT: CamelFolderInfoFlags = 1 << 0;
pub const CAMEL_FOLDER_NOINFERIORS: CamelFolderInfoFlags = 1 << 1;
pub const CAMEL_FOLDER_CHILDREN: CamelFolderInfoFlags = 1 << 2;
pub const CAMEL_FOLDER_NOCHILDREN: CamelFolderInfoFlags = 1 << 3;
pub const CAMEL_FOLDER_SUBSCRIBED: CamelFolderInfoFlags = 1 << 4;
pub const CAMEL_FOLDER_VIRTUAL: CamelFolderInfoFlags = 1 << 5;
pub const E_SOURCE_EXTENSION_MAIL_ACCOUNT: &str = "Mail Account";

#[link(name = "gobject-2.0")]
unsafe extern "C" {
    pub fn g_object_unref(object: *mut c_void);
}

#[link(name = "glib-2.0")]
unsafe extern "C" {
    pub fn g_free(mem: *mut c_void);
    pub fn g_list_free(list: *mut GList);
    pub fn g_ptr_array_new() -> *mut GPtrArray;
    pub fn g_ptr_array_add(array: *mut GPtrArray, data: gpointer);
    pub fn g_ptr_array_free(array: *mut GPtrArray, free_segment: gboolean) -> *mut gpointer;
}

#[link(name = "edataserver-1.2")]
unsafe extern "C" {
    pub fn e_source_get_uid(source: *mut ESource) -> *const c_char;
    pub fn e_source_get_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> *mut c_void;
    pub fn e_source_backend_get_backend_name(extension: *mut ESourceBackend) -> *const c_char;
    pub fn e_source_registry_new_sync(
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> *mut ESourceRegistry;
    pub fn e_source_registry_list_enabled(
        registry: *mut ESourceRegistry,
        extension_name: *const c_char,
    ) -> *mut GList;
}

#[link(name = "email-engine")]
unsafe extern "C" {
    pub fn e_mail_session_new(registry: *mut ESourceRegistry) -> *mut EMailSession;
    pub fn e_mail_session_get_registry(session: *mut EMailSession) -> *mut ESourceRegistry;
    pub fn e_mail_session_get_local_store(session: *mut EMailSession) -> *mut CamelStore;
    pub fn e_mail_session_uri_to_folder_sync(
        session: *mut EMailSession,
        folder_uri: *const c_char,
        flags: CamelStoreGetFolderFlags,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> *mut CamelFolder;
}

#[link(name = "camel-1.2")]
unsafe extern "C" {
    pub fn camel_session_ref_service(session: *mut c_void, uid: *const c_char)
    -> *mut CamelService;
    pub fn camel_store_get_folder_info_sync(
        store: *mut CamelStore,
        top: *const c_char,
        flags: CamelStoreGetFolderInfoFlags,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> *mut CamelFolderInfo;
    pub fn camel_folder_info_free(fi: *mut CamelFolderInfo);
    pub fn camel_store_get_folder_sync(
        store: *mut CamelStore,
        folder_name: *const c_char,
        flags: CamelStoreGetFolderFlags,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> *mut CamelFolder;
    pub fn camel_folder_get_folder_summary(folder: *mut CamelFolder) -> *mut CamelFolderSummary;
    pub fn camel_folder_search_header_sync(
        folder: *mut CamelFolder,
        header_name: *const c_char,
        words: *const GPtrArray,
        out_uids: *mut *mut GPtrArray,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn camel_folder_search_body_sync(
        folder: *mut CamelFolder,
        words: *const GPtrArray,
        out_uids: *mut *mut GPtrArray,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn camel_folder_get_message_sync(
        folder: *mut CamelFolder,
        message_uid: *const c_char,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> *mut CamelMimeMessage;
    pub fn camel_folder_summary_get(
        summary: *mut CamelFolderSummary,
        uid: *const c_char,
    ) -> *mut CamelMessageInfo;
    pub fn camel_message_info_get_subject(info: *const CamelMessageInfo) -> *const c_char;
    pub fn camel_message_info_get_from(info: *const CamelMessageInfo) -> *const c_char;
    pub fn camel_message_info_get_preview(info: *const CamelMessageInfo) -> *const c_char;
    pub fn camel_message_info_get_date_received(info: *const CamelMessageInfo) -> i64;
    pub fn camel_message_info_get_date_sent(info: *const CamelMessageInfo) -> i64;
    pub fn camel_mime_message_get_subject(message: *mut CamelMimeMessage) -> *const c_char;
    pub fn camel_mime_message_get_from(message: *mut CamelMimeMessage)
    -> *mut CamelInternetAddress;
    pub fn camel_internet_address_get(
        addr: *mut CamelInternetAddress,
        index: c_int,
        namep: *mut *const c_char,
        addressp: *mut *const c_char,
    ) -> gboolean;
    pub fn camel_stream_fs_new_with_name(
        name: *const c_char,
        flags: c_int,
        mode: mode_t,
        error: *mut *mut GError,
    ) -> *mut CamelStream;
    pub fn camel_data_wrapper_write_to_stream_sync(
        data_wrapper: *mut CamelDataWrapper,
        stream: *mut CamelStream,
        cancellable: *mut GCancellable,
        error: *mut *mut GError,
    ) -> isize;
}

#[link(name = "email-engine")]
unsafe extern "C" {
    pub fn e_mail_folder_uri_build(
        store: *mut CamelStore,
        folder_name: *const c_char,
    ) -> *mut c_char;
}
