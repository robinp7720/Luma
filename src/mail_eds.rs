use crate::mail_eds_ffi::{
    CAMEL_FOLDER_NOSELECT, CAMEL_FOLDER_VIRTUAL, CAMEL_STORE_FOLDER_INFO_RECURSIVE, CamelFolder,
    CamelFolderInfo, CamelInternetAddress, CamelMimeMessage, CamelService, CamelStore,
    CamelStoreGetFolderFlags, E_SOURCE_EXTENSION_MAIL_ACCOUNT, EMailSession, ESource,
    ESourceRegistry, camel_data_wrapper_write_to_stream_sync, camel_folder_get_folder_summary,
    camel_folder_get_message_sync, camel_folder_info_free, camel_folder_search_body_sync,
    camel_folder_search_header_sync, camel_folder_summary_get, camel_internet_address_get,
    camel_message_info_get_date_received, camel_message_info_get_date_sent,
    camel_message_info_get_from, camel_message_info_get_preview, camel_message_info_get_subject,
    camel_mime_message_get_from, camel_mime_message_get_subject, camel_session_ref_service,
    camel_store_get_folder_info_sync, camel_store_get_folder_sync, e_mail_folder_uri_build,
    e_mail_session_get_local_store, e_mail_session_new, e_mail_session_uri_to_folder_sync,
    e_source_backend_get_backend_name, e_source_get_extension, e_source_get_uid,
    e_source_registry_list_enabled, e_source_registry_new_sync, g_free, g_list_free,
    g_object_unref, g_ptr_array_add, g_ptr_array_free, g_ptr_array_new,
};
use crate::mail_eds_protocol::{
    MailEdsActionRequest, MailEdsActionResponse, MailEdsMessageSummary, MailEdsSearchRequest,
    MailEdsSearchResponse, MailEdsStatus,
};
use anyhow::{Context, Result, bail};
use gtk4::gio;
use gtk4::glib;
use gtk4::glib::ffi::{GError, GList, GPtrArray, gboolean, gpointer};
use gtk4::glib::translate::from_glib_full;
use gtk4::prelude::FileExt;
use libc::{c_char, c_void, mode_t};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailEdsFolder {
    pub folder_uri: String,
}

pub trait MailEdsAdapter {
    fn list_searchable_folders(&self) -> Result<Vec<MailEdsFolder>>;
    fn search_folder(&self, folder_uri: &str, tokens: &[String]) -> Result<Vec<String>>;
    fn message_summary(&self, folder_uri: &str, uid: &str) -> Result<MailEdsMessageSummary>;
}

pub fn search_mail_with_adapter<A: MailEdsAdapter>(
    adapter: &A,
    query: &str,
    limit: usize,
) -> Result<Vec<MailEdsMessageSummary>> {
    let tokens = tokenize_query(query);
    let mut rows = Vec::new();

    for folder in adapter.list_searchable_folders()? {
        let Ok(uids) = adapter.search_folder(&folder.folder_uri, &tokens) else {
            continue;
        };
        for uid in uids {
            if rows.len() >= limit {
                return Ok(rows);
            }

            match adapter.message_summary(&folder.folder_uri, &uid) {
                Ok(summary) => rows.push(summary),
                Err(_) => continue,
            }
        }
    }

    Ok(rows)
}

pub fn search_mail(query: &str, limit: usize) -> Result<Vec<MailEdsMessageSummary>> {
    let backend = EdsMailBackend::new()?;
    search_cached_mail(&backend, query, limit)
}

pub fn open_message(message_id: &str) -> Result<MailEdsStatus> {
    let backend = EdsMailBackend::new()?;
    backend.open_message(message_id)
}

pub fn reply_to_message(message_id: &str) -> Result<MailEdsStatus> {
    let backend = EdsMailBackend::new()?;
    backend.reply_to_message(message_id)
}

pub fn compose_to_message(message_id: &str) -> Result<MailEdsStatus> {
    let backend = EdsMailBackend::new()?;
    backend.compose_to_message(message_id)
}

pub fn copy_sender(message_id: &str) -> Result<MailEdsStatus> {
    let backend = EdsMailBackend::new()?;
    backend.copy_sender(message_id)
}

fn tokenize_query(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

#[derive(Debug, Deserialize)]
struct CacheFolderRow {
    folder_id: i64,
    folder_name: String,
}

#[derive(Debug, Deserialize)]
struct CacheMessageRow {
    uid: String,
    subject: Option<String>,
    mail_from: Option<String>,
    dsent: Option<i64>,
    dreceived: Option<i64>,
    preview: Option<String>,
    mail_to: Option<String>,
    mail_cc: Option<String>,
    userheaders: Option<String>,
}

fn search_cached_mail(
    backend: &EdsMailBackend,
    query: &str,
    limit: usize,
) -> Result<Vec<MailEdsMessageSummary>> {
    let tokens = tokenize_query(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    let mut seen = BTreeSet::new();

    for store in backend.iter_stores()? {
        let Some(cache_db_path) = store.cache_db_path.as_ref() else {
            continue;
        };
        if !cache_db_path.exists() {
            continue;
        }

        let folders: Vec<CacheFolderRow> = match cache_sqlite_json(
            cache_db_path,
            "SELECT folder_id, folder_name FROM folders ORDER BY folder_id",
        ) {
            Ok(folders) => folders,
            Err(_) => continue,
        };

        for folder in folders {
            if results.len() >= limit {
                return Ok(results);
            }

            let message_sql = build_cache_message_query(&tokens, folder.folder_id);
            let rows: Vec<CacheMessageRow> = match cache_sqlite_json(cache_db_path, &message_sql) {
                Ok(rows) => rows,
                Err(_) => continue,
            };

            for row in rows {
                if results.len() >= limit {
                    return Ok(results);
                }

                let folder_uri = match build_folder_uri(store.store, &folder.folder_name) {
                    Ok(uri) => uri,
                    Err(_) => continue,
                };
                let message_id = encode_message_id(&folder_uri, &row.uid);
                if !seen.insert(message_id.clone()) {
                    continue;
                }

                let subject = row
                    .subject
                    .clone()
                    .filter(|subject| !subject.trim().is_empty())
                    .unwrap_or_else(|| "(no subject)".to_string());
                let sender_raw = row
                    .mail_from
                    .clone()
                    .filter(|sender| !sender.trim().is_empty())
                    .unwrap_or_else(|| "Unknown sender".to_string());
                let sender_email = extract_email_address(&sender_raw);
                let date_label = email_date_label(
                    row.dreceived
                        .unwrap_or_default()
                        .max(row.dsent.unwrap_or_default()),
                    current_unix_seconds(),
                );
                let snippet = row.preview.clone().unwrap_or_default();

                results.push(MailEdsMessageSummary {
                    message_id,
                    folder_uri,
                    subject,
                    sender: sender_raw,
                    sender_email: sender_email.clone(),
                    date_label,
                    snippet,
                    openable: true,
                    replyable: sender_email.is_some(),
                    composable: sender_email.is_some(),
                });
            }
        }
    }

    Ok(results)
}

fn build_cache_message_query(tokens: &[String], folder_id: i64) -> String {
    let mut clauses = Vec::new();
    for token in tokens {
        let token = sqlite_like_pattern(token);
        let clause = format!(
            "(lower(coalesce(subject, '')) LIKE '{token}' OR lower(coalesce(mail_from, '')) LIKE '{token}' OR lower(coalesce(mail_to, '')) LIKE '{token}' OR lower(coalesce(mail_cc, '')) LIKE '{token}' OR lower(coalesce(preview, '')) LIKE '{token}' OR lower(coalesce(userheaders, '')) LIKE '{token}')"
        );
        clauses.push(clause);
    }

    let where_clause = clauses.join(" AND ");
    format!(
        "SELECT uid, subject, mail_from, dsent, dreceived, preview, mail_to, mail_cc, userheaders FROM messages_{folder_id} WHERE {where_clause}"
    )
}

fn sqlite_like_pattern(token: &str) -> String {
    format!("%{}%", token.replace('\'', "''"))
}

fn cache_sqlite_json<T>(db_path: &Path, sql: &str) -> Result<Vec<T>>
where
    for<'de> T: Deserialize<'de>,
{
    let output = Command::new("sqlite3")
        .args([
            "-json",
            db_path.to_str().context("invalid cache db path")?,
            sql,
        ])
        .output()
        .with_context(|| format!("failed to query cache database {}", db_path.display()))?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "sqlite3 failed for {}: {}",
            db_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8(output.stdout).context("cache query output was not utf-8")?;
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&stdout).context("failed to parse cache query output")
}

fn local_store_cache_db_path() -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join("evolution")
            .join("mail")
            .join("local")
            .join("folders.db"),
    )
}

fn account_cache_db_path(source_uid: &str) -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("evolution")
            .join("mail")
            .join(source_uid)
            .join("folders.db"),
    )
}

struct EdsMailBackend {
    registry: *mut ESourceRegistry,
    session: *mut EMailSession,
}

impl EdsMailBackend {
    fn new() -> Result<Self> {
        let mut error = ptr::null_mut();
        let registry = unsafe { e_source_registry_new_sync(ptr::null_mut(), &mut error) };
        if registry.is_null() {
            return Err(
                error_to_anyhow(error).context("failed to create Evolution source registry")
            );
        }

        let session = unsafe { e_mail_session_new(registry) };
        if session.is_null() {
            unsafe {
                g_object_unref(registry as *mut c_void);
            }
            bail!("failed to create Evolution mail session");
        }

        Ok(Self { registry, session })
    }

    fn list_mail_account_uids(&self) -> Result<Vec<String>> {
        let mut seen = BTreeSet::new();
        for extension_name in [E_SOURCE_EXTENSION_MAIL_ACCOUNT, "mail-account"] {
            let extension_name = cstring(extension_name)?;
            let list =
                unsafe { e_source_registry_list_enabled(self.registry, extension_name.as_ptr()) };
            if list.is_null() {
                continue;
            }

            unsafe {
                let mut cursor = list;
                while !cursor.is_null() {
                    let source = (*cursor).data as *mut ESource;
                    if !source.is_null() {
                        if !is_searchable_mail_source(source) {
                            g_object_unref(source as *mut c_void);
                            cursor = (*cursor).next;
                            continue;
                        }

                        let uid_ptr = e_source_get_uid(source);
                        if !uid_ptr.is_null() {
                            let uid = CStr::from_ptr(uid_ptr).to_string_lossy().into_owned();
                            if !uid.trim().is_empty() {
                                seen.insert(uid);
                            }
                        }
                        g_object_unref(source as *mut c_void);
                    }
                    cursor = (*cursor).next;
                }
                g_list_free(list);
            }
        }

        Ok(seen.into_iter().collect())
    }

    fn iter_stores(&self) -> Result<Vec<StoreHandle>> {
        let mut stores = Vec::new();
        let local_store = unsafe { e_mail_session_get_local_store(self.session) };
        if !local_store.is_null() {
            stores.push(StoreHandle::borrowed(
                local_store,
                local_store_cache_db_path(),
            ));
        }

        for uid in self.list_mail_account_uids()? {
            let uid = cstring(&uid)?;
            let service =
                unsafe { camel_session_ref_service(self.session as *mut c_void, uid.as_ptr()) };
            if service.is_null() {
                continue;
            }

            stores.push(StoreHandle::owned(
                service as *mut CamelStore,
                account_cache_db_path(&uid.to_string_lossy()),
            ));
        }

        Ok(stores)
    }

    fn list_folder_uris_for_store(&self, store: *mut CamelStore) -> Result<Vec<String>> {
        let mut error = ptr::null_mut();
        let root = unsafe {
            camel_store_get_folder_info_sync(
                store,
                ptr::null(),
                CAMEL_STORE_FOLDER_INFO_RECURSIVE,
                ptr::null_mut(),
                &mut error,
            )
        };
        if root.is_null() {
            return Err(error_to_anyhow(error).context("failed to enumerate Evolution folders"));
        }

        let mut folders = Vec::new();
        unsafe {
            collect_folder_uris(store, root, &mut folders)?;
            camel_folder_info_free(root);
        }
        Ok(folders)
    }

    fn search_folder_uids(
        &self,
        folder: *mut CamelFolder,
        tokens: &[String],
    ) -> Result<Vec<String>> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let words = make_words_array(tokens)?;
        let mut candidates = BTreeSet::new();

        unsafe {
            for header in ["subject", "from"] {
                let header = cstring(header)?;
                let mut out_uids: *mut GPtrArray = ptr::null_mut();
                let mut error = ptr::null_mut();
                let ok = camel_folder_search_header_sync(
                    folder,
                    header.as_ptr(),
                    words.array as *const GPtrArray,
                    &mut out_uids,
                    ptr::null_mut(),
                    &mut error,
                );
                if ok == 0 {
                    if !out_uids.is_null() {
                        g_ptr_array_free(out_uids, 0);
                    }
                    return Err(error_to_anyhow(error).context("Evolution header search failed"));
                }

                if !out_uids.is_null() {
                    extend_uids_from_ptr_array(out_uids, &mut candidates);
                    g_ptr_array_free(out_uids, 0);
                }
            }

            let mut out_uids: *mut GPtrArray = ptr::null_mut();
            let mut error = ptr::null_mut();
            let ok = camel_folder_search_body_sync(
                folder,
                words.array as *const GPtrArray,
                &mut out_uids,
                ptr::null_mut(),
                &mut error,
            );
            if ok == 0 {
                if !out_uids.is_null() {
                    g_ptr_array_free(out_uids, 0);
                }
                return Err(error_to_anyhow(error).context("Evolution body search failed"));
            }
            if !out_uids.is_null() {
                extend_uids_from_ptr_array(out_uids, &mut candidates);
                g_ptr_array_free(out_uids, 0);
            }
        }

        Ok(candidates.into_iter().collect())
    }

    fn message_summary_for_folder(
        &self,
        folder_uri: &str,
        uid: &str,
    ) -> Result<MailEdsMessageSummary> {
        let folder = self.open_folder(folder_uri)?;
        let summary = unsafe { camel_folder_get_folder_summary(folder) };
        if summary.is_null() {
            unsafe {
                g_object_unref(folder as *mut c_void);
            }
            bail!("Evolution folder summary unavailable");
        }

        let uid = cstring(uid)?;
        let info = unsafe { camel_folder_summary_get(summary, uid.as_ptr()) };
        if info.is_null() {
            unsafe {
                g_object_unref(folder as *mut c_void);
            }
            bail!("Evolution message summary unavailable");
        }

        let subject = unsafe { c_string_from_ptr(camel_message_info_get_subject(info)) }
            .unwrap_or_else(|| "(no subject)".to_string());
        let sender_raw = unsafe { c_string_from_ptr(camel_message_info_get_from(info)) }
            .unwrap_or_else(|| "Unknown sender".to_string());
        let sender_email = extract_email_address(&sender_raw);
        let date_sent = unsafe { camel_message_info_get_date_sent(info) };
        let date_received = unsafe { camel_message_info_get_date_received(info) };
        let date_label = email_date_label(date_received.max(date_sent), current_unix_seconds());
        let snippet =
            unsafe { c_string_from_ptr(camel_message_info_get_preview(info)) }.unwrap_or_default();

        let message_id = encode_message_id(folder_uri, uid.to_str().unwrap_or_default());
        let replyable = sender_email.is_some();
        let summary = MailEdsMessageSummary {
            message_id,
            folder_uri: folder_uri.to_string(),
            subject,
            sender: sender_raw,
            sender_email,
            date_label,
            snippet,
            openable: true,
            replyable,
            composable: replyable,
        };

        unsafe {
            g_object_unref(folder as *mut c_void);
        }

        Ok(summary)
    }

    fn open_folder(&self, folder_uri: &str) -> Result<*mut CamelFolder> {
        let folder_uri = cstring(folder_uri)?;
        let mut error = ptr::null_mut();
        let folder = unsafe {
            e_mail_session_uri_to_folder_sync(
                self.session,
                folder_uri.as_ptr(),
                0 as CamelStoreGetFolderFlags,
                ptr::null_mut(),
                &mut error,
            )
        };
        if folder.is_null() {
            Err(error_to_anyhow(error).context("failed to open Evolution folder"))
        } else {
            Ok(folder)
        }
    }

    fn fetch_message(&self, folder_uri: &str, uid: &str) -> Result<*mut CamelMimeMessage> {
        let folder = self.open_folder(folder_uri)?;
        let uid = cstring(uid)?;
        let mut error = ptr::null_mut();
        let message = unsafe {
            camel_folder_get_message_sync(folder, uid.as_ptr(), ptr::null_mut(), &mut error)
        };
        unsafe {
            g_object_unref(folder as *mut c_void);
        }
        if message.is_null() {
            Err(error_to_anyhow(error).context("failed to load Evolution message"))
        } else {
            Ok(message)
        }
    }

    fn open_message(&self, message_id: &str) -> Result<MailEdsStatus> {
        let message_ref = parse_message_id(message_id)?;
        let message = self.fetch_message(&message_ref.folder_uri, &message_ref.uid)?;
        let path = save_message_to_temp_file(message)?;
        unsafe {
            g_object_unref(message as *mut c_void);
        }

        if spawn_evolution_viewer(&path).is_err() {
            let file = gio::File::for_path(&path);
            gio::AppInfo::launch_default_for_uri(&file.uri(), gio::AppLaunchContext::NONE)
                .context("failed to open Evolution message")?;
        }

        Ok(MailEdsStatus {
            ok: true,
            message: "opened message".to_string(),
        })
    }

    fn reply_to_message(&self, message_id: &str) -> Result<MailEdsStatus> {
        let summary = self.message_summary_from_message_id(message_id)?;
        let Some(sender_email) = summary.sender_email else {
            bail!("no sender address available for reply");
        };
        let subject = if summary.subject.trim().is_empty() {
            "Re:".to_string()
        } else {
            format!("Re: {}", summary.subject)
        };
        let mailto = mailto_reply_url(&sender_email, &subject);
        launch_mailto(&mailto)?;
        Ok(MailEdsStatus {
            ok: true,
            message: "opened reply composer".to_string(),
        })
    }

    fn compose_to_message(&self, message_id: &str) -> Result<MailEdsStatus> {
        let summary = self.message_summary_from_message_id(message_id)?;
        let Some(sender_email) = summary.sender_email else {
            bail!("no sender address available for compose");
        };
        let mailto = mailto_compose_url(&sender_email);
        launch_mailto(&mailto)?;
        Ok(MailEdsStatus {
            ok: true,
            message: "opened compose window".to_string(),
        })
    }

    fn copy_sender(&self, message_id: &str) -> Result<MailEdsStatus> {
        let summary = self.message_summary_from_message_id(message_id)?;
        let Some(sender_email) = summary.sender_email else {
            bail!("no sender address available to copy");
        };
        Ok(MailEdsStatus {
            ok: true,
            message: sender_email,
        })
    }

    fn message_summary_from_message_id(&self, message_id: &str) -> Result<MailEdsMessageSummary> {
        let message_ref = parse_message_id(message_id)?;
        self.message_summary_for_folder(&message_ref.folder_uri, &message_ref.uid)
    }
}

impl Drop for EdsMailBackend {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                g_object_unref(self.session as *mut c_void);
            }
            if !self.registry.is_null() {
                g_object_unref(self.registry as *mut c_void);
            }
        }
    }
}

impl MailEdsAdapter for EdsMailBackend {
    fn list_searchable_folders(&self) -> Result<Vec<MailEdsFolder>> {
        let mut folders = Vec::new();
        for store in self.iter_stores()? {
            let Ok(folder_uris) = self.list_folder_uris_for_store(store.store) else {
                continue;
            };
            folders.extend(
                folder_uris
                    .into_iter()
                    .map(|folder_uri| MailEdsFolder { folder_uri }),
            );
        }
        Ok(folders)
    }

    fn search_folder(&self, folder_uri: &str, tokens: &[String]) -> Result<Vec<String>> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let folder = self.open_folder(folder_uri)?;
        let uids = self.search_folder_uids(folder, tokens)?;
        unsafe {
            g_object_unref(folder as *mut c_void);
        }
        Ok(uids)
    }

    fn message_summary(&self, folder_uri: &str, uid: &str) -> Result<MailEdsMessageSummary> {
        self.message_summary_for_folder(folder_uri, uid)
    }
}

fn collect_folder_uris(
    store: *mut CamelStore,
    info: *mut CamelFolderInfo,
    out: &mut Vec<String>,
) -> Result<()> {
    let mut cursor = info;
    while !cursor.is_null() {
        unsafe {
            let flags = (*cursor).flags;
            if flags & (CAMEL_FOLDER_NOSELECT | CAMEL_FOLDER_VIRTUAL) == 0 {
                if let Some(full_name) = c_string_from_ptr((*cursor).full_name) {
                    let folder_uri = build_folder_uri(store, &full_name)?;
                    out.push(folder_uri);
                }
            }

            if !(*cursor).child.is_null() {
                collect_folder_uris(store, (*cursor).child, out)?;
            }

            cursor = (*cursor).next;
        }
    }

    Ok(())
}

fn build_folder_uri(store: *mut CamelStore, folder_name: &str) -> Result<String> {
    let folder_name = cstring(folder_name)?;
    let uri = unsafe { e_mail_folder_uri_build(store, folder_name.as_ptr()) };
    if uri.is_null() {
        bail!("failed to build Evolution folder URI");
    }

    let value = c_string_from_ptr(uri).unwrap_or_default();
    unsafe {
        g_free(uri as *mut c_void);
    }
    Ok(value)
}

fn extend_uids_from_ptr_array(array: *mut GPtrArray, out: &mut BTreeSet<String>) {
    unsafe {
        let slice =
            std::slice::from_raw_parts((*array).pdata as *const *mut c_void, (*array).len as usize);
        for &item in slice {
            if item.is_null() {
                continue;
            }
            let uid = CStr::from_ptr(item as *const c_char)
                .to_string_lossy()
                .into_owned();
            if !uid.trim().is_empty() {
                out.insert(uid);
            }
        }
    }
}

struct WordsArray {
    array: *mut GPtrArray,
    storage: Vec<CString>,
}

impl Drop for WordsArray {
    fn drop(&mut self) {
        unsafe {
            if !self.array.is_null() {
                g_ptr_array_free(self.array, 0);
            }
        }
    }
}

fn make_words_array(tokens: &[String]) -> Result<WordsArray> {
    let array = unsafe { g_ptr_array_new() };
    if array.is_null() {
        bail!("failed to allocate Evolution search token array");
    }

    let mut storage = Vec::with_capacity(tokens.len());
    for token in tokens {
        let token = cstring(token)?;
        unsafe {
            g_ptr_array_add(array, token.as_ptr() as *mut c_void);
        }
        storage.push(token);
    }

    Ok(WordsArray { array, storage })
}

fn parse_message_id(message_id: &str) -> Result<MessageRef> {
    let mut parts = message_id.splitn(3, '|');
    let backend = parts.next().unwrap_or_default();
    if backend != "evolution" {
        bail!("unsupported Evolution message identifier");
    }

    let folder_uri = parts
        .next()
        .and_then(|value| urlencoding::decode(value).ok())
        .map(|value| value.into_owned())
        .context("missing folder URI in Evolution message identifier")?;
    let uid = parts
        .next()
        .and_then(|value| urlencoding::decode(value).ok())
        .map(|value| value.into_owned())
        .context("missing UID in Evolution message identifier")?;

    Ok(MessageRef { folder_uri, uid })
}

fn encode_message_id(folder_uri: &str, uid: &str) -> String {
    format!(
        "evolution|{}|{}",
        urlencoding::encode(folder_uri),
        urlencoding::encode(uid)
    )
}

fn save_message_to_temp_file(message: *mut CamelMimeMessage) -> Result<PathBuf> {
    let file_name = format!(
        "luma-mail-eds-{}-{}.eml",
        std::process::id(),
        current_unix_nanos()
    );
    let path = std::env::temp_dir().join(file_name);
    let path_str = path.to_string_lossy();
    let c_path = cstring(path_str.as_ref())?;
    let mut error = ptr::null_mut();
    let stream = unsafe {
        crate::mail_eds_ffi::camel_stream_fs_new_with_name(
            c_path.as_ptr(),
            libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
            0o600,
            &mut error,
        )
    };
    if stream.is_null() {
        return Err(error_to_anyhow(error).context("failed to create temporary mail file"));
    }

    let written = unsafe {
        camel_data_wrapper_write_to_stream_sync(
            message as *mut crate::mail_eds_ffi::CamelDataWrapper,
            stream,
            ptr::null_mut(),
            &mut error,
        )
    };
    unsafe {
        g_object_unref(stream as *mut c_void);
    }
    if written < 0 {
        return Err(error_to_anyhow(error).context("failed to write message to temporary file"));
    }

    Ok(path)
}

fn spawn_evolution_viewer(path: &Path) -> Result<()> {
    let path = path.to_string_lossy().to_string();
    if command_exists("evolution")
        && Command::new("evolution")
            .args(["--component=mail", "--view", &path])
            .spawn()
            .is_ok()
    {
        return Ok(());
    }

    Err(anyhow::anyhow!("evolution command unavailable"))
}

fn launch_mailto(mailto: &str) -> Result<()> {
    if command_exists("evolution") && Command::new("evolution").arg(mailto).spawn().is_ok() {
        return Ok(());
    }

    gio::AppInfo::launch_default_for_uri(mailto, gio::AppLaunchContext::NONE)
        .context("failed to open mailto URI")
}

fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .args(["-lc", &format!("command -v {command} >/dev/null 2>&1")])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn error_to_anyhow(error: *mut GError) -> anyhow::Error {
    if error.is_null() {
        anyhow::anyhow!("Evolution API returned an unknown error")
    } else {
        let error: glib::Error = unsafe { from_glib_full(error) };
        anyhow::anyhow!(error.to_string())
    }
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).context("string contained an interior NUL byte")
}

fn is_searchable_mail_source(source: *mut ESource) -> bool {
    unsafe {
        let extension_name = match cstring(E_SOURCE_EXTENSION_MAIL_ACCOUNT) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let extension = e_source_get_extension(source, extension_name.as_ptr());
        if extension.is_null() {
            return false;
        }

        let backend_name = e_source_backend_get_backend_name(extension as *mut _);
        let Some(backend_name) = c_string_from_ptr(backend_name) else {
            return false;
        };

        is_supported_mail_backend(&backend_name)
    }
}

fn is_supported_mail_backend(backend_name: &str) -> bool {
    matches!(
        backend_name,
        "imapx" | "maildir" | "local" | "pop3" | "google" | "mbox"
    )
}

fn c_string_from_ptr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn folder_label_from_uri(folder_uri: &str) -> String {
    folder_uri
        .rsplit('/')
        .next()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or(folder_uri)
        .to_string()
}

fn current_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn email_date_label(message_date_seconds: i64, now: u64) -> String {
    if message_date_seconds <= 0 {
        return String::new();
    }

    let message_seconds = message_date_seconds as u64;
    let age_seconds = now.saturating_sub(message_seconds);
    if age_seconds < 60 {
        "just now".to_string()
    } else if age_seconds < 3_600 {
        format!("{}m ago", age_seconds / 60)
    } else if age_seconds < 86_400 {
        format!("{}h ago", age_seconds / 3_600)
    } else if age_seconds < 172_800 {
        "yesterday".to_string()
    } else {
        format!("{}d ago", age_seconds / 86_400)
    }
}

fn extract_email_address(author: &str) -> Option<String> {
    let candidate = author
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(value, _)| value)
        .unwrap_or(author)
        .trim();
    if candidate.contains('@') {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn mailto_compose_url(sender_email: &str) -> String {
    format!("mailto:{}", urlencoding::encode(sender_email))
}

fn mailto_reply_url(sender_email: &str, subject: &str) -> String {
    let mut url = format!("mailto:{}", urlencoding::encode(sender_email));
    let mut params = Vec::new();
    if !subject.trim().is_empty() {
        params.push(format!("subject={}", urlencoding::encode(subject)));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

#[derive(Debug)]
struct MessageRef {
    folder_uri: String,
    uid: String,
}

struct StoreHandle {
    store: *mut CamelStore,
    owned: bool,
    cache_db_path: Option<PathBuf>,
}

impl StoreHandle {
    fn borrowed(store: *mut CamelStore, cache_db_path: Option<PathBuf>) -> Self {
        Self {
            store,
            owned: false,
            cache_db_path,
        }
    }

    fn owned(store: *mut CamelStore, cache_db_path: Option<PathBuf>) -> Self {
        Self {
            store,
            owned: true,
            cache_db_path,
        }
    }
}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        if self.owned && !self.store.is_null() {
            unsafe {
                g_object_unref(self.store as *mut c_void);
            }
        }
    }
}

pub fn action_response(message: String) -> MailEdsActionResponse {
    MailEdsActionResponse { ok: true, message }
}

pub fn status_ok(message: String) -> MailEdsStatus {
    MailEdsStatus { ok: true, message }
}

pub fn search_response(results: Vec<MailEdsMessageSummary>) -> MailEdsSearchResponse {
    MailEdsSearchResponse {
        ok: true,
        message: String::new(),
        results,
    }
}

fn cstring_lossy(value: &str) -> Cow<'_, str> {
    if value.contains('\0') {
        Cow::Owned(value.replace('\0', ""))
    } else {
        Cow::Borrowed(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter;
    struct PartiallyBrokenAdapter;

    impl MailEdsAdapter for FakeAdapter {
        fn list_searchable_folders(&self) -> Result<Vec<MailEdsFolder>> {
            Ok(vec![MailEdsFolder {
                folder_uri: "folder:On This Computer/Inbox".to_string(),
            }])
        }

        fn search_folder(&self, _folder_uri: &str, _tokens: &[String]) -> Result<Vec<String>> {
            Ok(vec!["uid-1".to_string()])
        }

        fn message_summary(&self, _folder_uri: &str, _uid: &str) -> Result<MailEdsMessageSummary> {
            Ok(MailEdsMessageSummary {
                message_id: "folder:On This Computer/Inbox:uid-1".to_string(),
                folder_uri: "folder:On This Computer/Inbox".to_string(),
                subject: "Github digest".to_string(),
                sender: "GitHub".to_string(),
                sender_email: Some("noreply@github.com".to_string()),
                date_label: "1d ago".to_string(),
                snippet: "Your weekly summary".to_string(),
                openable: true,
                replyable: true,
                composable: true,
            })
        }
    }

    impl MailEdsAdapter for PartiallyBrokenAdapter {
        fn list_searchable_folders(&self) -> Result<Vec<MailEdsFolder>> {
            Ok(vec![
                MailEdsFolder {
                    folder_uri: "folder:broken".to_string(),
                },
                MailEdsFolder {
                    folder_uri: "folder:good".to_string(),
                },
            ])
        }

        fn search_folder(&self, folder_uri: &str, _tokens: &[String]) -> Result<Vec<String>> {
            if folder_uri == "folder:broken" {
                anyhow::bail!("Authentication password not available");
            }

            Ok(vec!["uid-2".to_string()])
        }

        fn message_summary(&self, _folder_uri: &str, uid: &str) -> Result<MailEdsMessageSummary> {
            Ok(MailEdsMessageSummary {
                message_id: format!("folder:good:{uid}"),
                folder_uri: "folder:good".to_string(),
                subject: "Github alert".to_string(),
                sender: "GitHub".to_string(),
                sender_email: Some("noreply@github.com".to_string()),
                date_label: "2d ago".to_string(),
                snippet: "A broken folder should not stop the search".to_string(),
                openable: true,
                replyable: true,
                composable: true,
            })
        }
    }

    #[test]
    fn search_normalizes_folder_results_into_message_summaries() {
        let adapter = FakeAdapter;
        let results = search_mail_with_adapter(&adapter, "github", 8).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].subject, "Github digest");
        assert_eq!(
            results[0].sender_email.as_deref(),
            Some("noreply@github.com")
        );
    }

    #[test]
    fn message_ids_round_trip() {
        let folder_uri = "folder://On This Computer/Inbox";
        let uid = "42";
        let encoded = encode_message_id(folder_uri, uid);
        let decoded = parse_message_id(&encoded).unwrap();

        assert_eq!(decoded.folder_uri, folder_uri);
        assert_eq!(decoded.uid, uid);
    }

    #[test]
    fn search_skips_inaccessible_folders() {
        let adapter = PartiallyBrokenAdapter;
        let results = search_mail_with_adapter(&adapter, "github", 8).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].folder_uri, "folder:good");
        assert_eq!(results[0].subject, "Github alert");
    }

    #[test]
    fn supported_mail_backends_are_whitelisted() {
        assert!(is_supported_mail_backend("imapx"));
        assert!(is_supported_mail_backend("maildir"));
        assert!(is_supported_mail_backend("google"));
        assert!(!is_supported_mail_backend("rss"));
        assert!(!is_supported_mail_backend("vfolder"));
        assert!(!is_supported_mail_backend("smtp"));
    }
}
