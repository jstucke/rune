//! xdg-desktop-portal Print over zbus. Like the FileChooser client (`file_dialog/portal.rs`),
//! each step runs on the shared portal worker (`platform::portal::spawn`), and dropping its
//! future closes the portal request.
use super::{PageSetup, PageSetupValue, PrintAnswer, PrintPrepared, PrintSent, PrintSettings, PrintSetup, Step};
use crate::platform::portal::{self, DEST, PATH, RequestError, RequestTimeouts};
use super::parent::ParentWindow;
use futures_lite::FutureExt;
use std::collections::HashMap;
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::Duration;
use zbus::{Connection, Proxy, zvariant::{Fd, OwnedValue, Value}};

const INTERFACE: &str = "org.freedesktop.portal.Print";
/// Connecting, D-Bus activation of the portal and the method call. The reader's choice, and the
/// print service taking the job, have no timeout.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Reading the portal's `version` property.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `Print` waits, so it doesn't follow `PreparePrint`'s answer at once. GTK's backend
/// (xdg-desktop-portal-gtk 1.15.1) given a `Print` 2 ms after that answer reads no file from its
/// fd on the "Print to File" printer, writes nothing, and still answers success; 50 ms later
/// it writes the file. Five times that, as it costs the reader nothing.
const PRINT_AFTER_PREPARE: Duration = Duration::from_millis(250);

/// The options for `PreparePrint` (`print_token` 0) or `Print`. Version 3 portals take
/// `supported_output_file_formats`; only PDF, as the bytes are PDF (else GTK's Print to File
/// would offer PostScript and SVG and write PDF bytes under those names).
fn request_options(handle_token: &str, print_token: u32, version: u32) -> HashMap<&'static str, Value<'_>> {
    let mut map = HashMap::from([("handle_token", Value::from(handle_token)), ("modal", Value::from(true))]);
    if print_token != 0 { map.insert("token", Value::from(print_token)); }
    if version >= 3 { map.insert("supported_output_file_formats", Value::from(vec!["pdf"])); }
    map
}

fn settings_map(settings: &PrintSettings) -> HashMap<&str, Value<'_>> {
    settings.0.iter().map(|(k, v)| (k.as_str(), Value::from(v.as_str()))).collect()
}

fn page_setup_map(page_setup: &PageSetup) -> HashMap<&str, Value<'_>> {
    page_setup.0.iter().map(|(k, v)| (k.as_str(), match v {
        PageSetupValue::Text(text) => Value::from(text.as_str()),
        PageSetupValue::Number(n) => Value::from(*n),
    })).collect()
}

/// A reply's `a{sv}` entry, empty when absent or mistyped.
fn dict(results: &HashMap<String, OwnedValue>, key: &str) -> HashMap<String, OwnedValue> {
    results.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| HashMap::<String, OwnedValue>::try_from(v).ok()).unwrap_or_default()
}

fn page_setup_value(v: &Value<'_>) -> Option<PageSetupValue> {
    Some(match v {
        Value::Str(s) => PageSetupValue::Text(s.to_string()),
        Value::F64(n) => PageSetupValue::Number(*n),
        Value::I32(n) => PageSetupValue::Number(f64::from(*n)),
        Value::U32(n) => PageSetupValue::Number(f64::from(*n)),
        Value::I16(n) => PageSetupValue::Number(f64::from(*n)),
        Value::U16(n) => PageSetupValue::Number(f64::from(*n)),
        Value::U8(n) => PageSetupValue::Number(f64::from(*n)),
        Value::I64(n) => PageSetupValue::Number(*n as f64),
        Value::U64(n) => PageSetupValue::Number(*n as f64),
        _ => return None,
    })
}

/// `PreparePrint`'s `Response`: 0 is the reader's choice; 1 a cancel, and 2 a dialog that ended
/// another way (GTK's answer to its Cancel button and to Escape), are no choice.
fn prepared(status: u32, results: &HashMap<String, OwnedValue>) -> PrintPrepared {
    match status {
        0 => {
            let settings = dict(results, "settings").into_iter()
                .filter_map(|(k, v)| match &*v { Value::Str(s) => Some((k, s.to_string())), _ => None })
                .collect();
            let page_setup = dict(results, "page-setup").into_iter().filter_map(|(k, v)| Some((k, page_setup_value(&v)?))).collect();
            let mut setup = PrintSetup::new(PrintSettings(settings), PageSetup(page_setup));
            setup.token = results.get("token").and_then(|v| match &**v { Value::U32(t) => Some(*t), _ => None }).unwrap_or(0);
            PrintPrepared::Ready(setup)
        }
        1 | 2 => PrintPrepared::Cancelled,
        n => PrintPrepared::Unavailable(format!("the print dialog ended with an error (portal response {n})")),
    }
}

/// `Print`'s `Response`, whose results are always empty.
fn sent(status: u32) -> PrintSent { if status == 0 { PrintSent::Sent } else { PrintSent::NotConfirmed } }

/// Why a step couldn't be asked: no portal or no Print backend is no service.
fn dbus_error(step: &Step, e: zbus::Error) -> PrintAnswer {
    if portal::is_unavailable(&e) { step.no_service() } else { step.refused(&e.to_string()) }
}

/// The PDF as a sealed-off memory file, read from its start: the portal's backends read the fd
/// from its offset to the end.
#[allow(unsafe_code)]
fn memfd(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    // SAFETY: a NUL-terminated name and valid flags; the new fd is owned by `file` at once.
    let fd = unsafe { libc::memfd_create(c"rune-print".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error()); }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file.into())
}

/// Starts `step` on the portal worker, modal over `window`. The future holds what makes
/// `window` the dialog's parent (a Wayland export) until the step ends.
pub(super) fn show(step: Step, window: &winit::window::Window) -> impl Future<Output = PrintAnswer> + 'static {
    let parent = ParentWindow::of(window);
    let (reply, result) = async_channel::bounded(1);
    let (handle, ask) = (parent.handle().to_owned(), step.clone());
    let spawned = portal::spawn(move || run(ask, handle, reply));
    async move {
        let _parent = parent;
        if let Err(e) = spawned { return step.refused(&e); }
        result.recv().await.unwrap_or_else(|_| step.refused("print worker stopped"))
    }
}

/// Asks the portal and answers, unless the window's side stops waiting (`reply` closed).
async fn run(step: Step, parent: String, reply: async_channel::Sender<PrintAnswer>) {
    if let Some(answer) = ask(&step, &parent, &reply).await { reply.try_send(answer).ok(); }
}

/// `future`, unless the window's side stops waiting first (`None` then). Only for the steps
/// before the request: once the dialog is asked for, `portal::request` itself watches `reply`,
/// so it can close the dialog on the way out.
async fn until_closed<T>(reply: &async_channel::Sender<PrintAnswer>, future: impl Future<Output = T>) -> Option<T> {
    async { Some(future.await) }.or(async { reply.closed().await; None }).await
}

async fn ask(step: &Step, parent: &str, reply: &async_channel::Sender<PrintAnswer>) -> Option<PrintAnswer> {
    // No session bus means this desktop cannot supply the portal.
    let conn = match until_closed(reply, portal::timeout(RPC_TIMEOUT, Connection::session())).await? {
        Some(Ok(conn)) => conn,
        Some(Err(_)) => return Some(step.no_service()),
        None => return Some(step.refused("the print portal timed out")),
    };
    let answer = ask_on(&conn, step, parent, reply).await;
    conn.close().await.ok();
    answer
}

async fn ask_on(conn: &Connection, step: &Step, parent: &str, reply: &async_channel::Sender<PrintAnswer>) -> Option<PrintAnswer> {
    let proxy = match until_closed(reply, portal::timeout(RPC_TIMEOUT, async {
        portal::register_app_id(conn).await;
        Proxy::new(conn, DEST, PATH, INTERFACE).await
    })).await? {
        Some(Ok(proxy)) => proxy,
        Some(Err(e)) => return Some(dbus_error(step, e)),
        None => return Some(step.refused("the print portal timed out")),
    };
    // A portal without the property is older than version 2; without a Print backend, the call
    // below says so. A portal that doesn't answer fails that call too, so this waits less.
    let version = until_closed(reply, portal::timeout(VERSION_TIMEOUT, proxy.get_property::<u32>("version"))).await?
        .and_then(Result::ok).unwrap_or(1);
    let token = portal::next_token("print");
    let timeouts = || RequestTimeouts { rpc: RPC_TIMEOUT, response: None };
    let response = match step {
        Step::Prepare(options) => {
            let body = (parent, options.title.as_str(), settings_map(&options.settings), page_setup_map(&options.page_setup),
                request_options(&token, 0, version));
            portal::request(conn, &proxy, &token, "PreparePrint", &body, timeouts(), reply.closed()).await
        }
        Step::Send(job) => {
            until_closed(reply, async_io::Timer::after(PRINT_AFTER_PREPARE)).await?;
            let fd = match memfd(&job.bytes) {
                Ok(fd) => fd,
                Err(e) => return Some(step.refused(&format!("couldn't put the PDF in a memory file: {e}"))),
            };
            let body = (parent, job.title.as_str(), Fd::from(&fd), request_options(&token, job.setup.token, version));
            portal::request(conn, &proxy, &token, "Print", &body, timeouts(), reply.closed()).await
        }
    };
    Some(match (response, step) {
        (Ok((status, results)), Step::Prepare(_)) => PrintAnswer::Prepared(prepared(status, &results)),
        (Ok((status, _)), Step::Send(_)) => PrintAnswer::Sent(sent(status)),
        (Err(RequestError::Cancelled), _) => return None,
        (Err(RequestError::Dbus(e)), _) => dbus_error(step, e),
        (Err(RequestError::Unavailable), _) => step.no_service(),
        (Err(RequestError::TimedOut), _) => step.refused("the print portal timed out"),
        (Err(RequestError::Disconnected), _) => step.refused("the print portal disconnected"),
    })
}

#[cfg(test)]
#[path = "portal_tests.rs"]
mod tests;
