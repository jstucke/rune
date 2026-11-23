//! Printing a PDF through the desktop's own print dialog: the XDG desktop portal's Print
//! interface on Linux (`print/portal.rs`), nothing yet elsewhere (`PrintPrepared::Unavailable`).
//!
//! Two steps, each a `Tx` call whose answer runs as an action on a later frame:
//! [`Tx::prepare_print`] shows the dialog and answers with the reader's settings and pages;
//! [`Tx::print_pdf`] then hands the printer the PDF the app made for them. The portal leaves page
//! ranges and scale to the app (the GTK and KDE backends ignore them), so an app cuts the
//! document to [`PrintSetup::pages`] before sending it. Tests answer both steps with
//! [`RuntimeCore::resolve_print_dialog`] and [`RuntimeCore::resolve_print_job`].
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::ops::RangeInclusive;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::Tx;
#[cfg(doc)]
use crate::runtime::RuntimeCore;

#[cfg(all(target_os = "linux", feature = "dbus"))]
mod parent;
#[cfg(all(target_os = "linux", feature = "dbus"))]
mod portal;

/// The print dialog's settings, GTK's `GtkPrintSettings` keys to string values (`printer`,
/// `n-copies`, `duplex`, `print-pages`, `page-ranges`, …), as the portal hands them back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrintSettings(pub BTreeMap<String, String>);

impl PrintSettings {
    /// These settings with the page choice cleared, so a dialog started from them offers all
    /// pages again while keeping the printer, copies, duplex and the rest.
    pub fn without_pages(&self) -> Self {
        let mut settings = self.clone();
        settings.0.remove("print-pages");
        settings.0.remove("page-ranges");
        settings
    }
}

/// One page setup value: the portal's are strings (`PPDName`, `Name`, `DisplayName`,
/// `Orientation`) and numbers (`Width`, `Height` and the margins, in millimetres).
#[derive(Clone, Debug, PartialEq)]
pub enum PageSetupValue {
    Text(String),
    Number(f64),
}

/// The dialog's paper and margins, kept to start the next dialog with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageSetup(pub BTreeMap<String, PageSetupValue>);

/// What [`Tx::prepare_print`] starts the dialog with: its title, and the settings and page
/// setup a last print returned (empty the first time).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrintOptions {
    pub title: String,
    pub settings: PrintSettings,
    pub page_setup: PageSetup,
}

/// Which pages the reader asked for, 0-based and in the order typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintPages {
    All,
    Ranges(Vec<RangeInclusive<u32>>),
}

impl PrintPages {
    /// From `print-pages` and `page-ranges` (`0-2,4,9-11`, 0-based). `print-pages` other than
    /// `ranges` (`all`, `current`, `selection`) is all pages, as is a range list with nothing
    /// readable in it.
    pub fn from_settings(settings: &PrintSettings) -> Self {
        let get = |key: &str| settings.0.get(key).map(String::as_str);
        if get("print-pages") != Some("ranges") { return PrintPages::All; }
        let page = |s: &str| s.trim().parse::<u32>().ok();
        let ranges: Vec<_> = get("page-ranges").unwrap_or_default().split(',').filter_map(|part| {
            match part.split_once('-') {
                Some((a, b)) => { let (a, b) = (page(a)?, page(b)?); Some(a.min(b)..=a.max(b)) }
                None => page(part).map(|p| p..=p),
            }
        }).collect();
        if ranges.is_empty() { PrintPages::All } else { PrintPages::Ranges(ranges) }
    }
}

/// What the reader chose in the dialog, for [`Tx::print_pdf`].
#[derive(Clone, Debug, PartialEq)]
pub struct PrintSetup {
    pub settings: PrintSettings,
    pub page_setup: PageSetup,
    /// Read from `settings`.
    pub pages: PrintPages,
    /// The portal's handle on this dialog's choice; 0 for none.
    pub(crate) token: u32,
}

impl PrintSetup {
    /// A choice of `settings` and `page_setup`, as a test answers the dialog with.
    pub fn new(settings: PrintSettings, page_setup: PageSetup) -> Self {
        let pages = PrintPages::from_settings(&settings);
        Self { settings, page_setup, pages, token: 0 }
    }
}

/// How the print dialog ended.
#[derive(Clone, Debug, PartialEq)]
pub enum PrintPrepared {
    /// The reader chose to print.
    Ready(PrintSetup),
    /// The reader cancelled, or the window hid or closed.
    Cancelled,
    /// The desktop has no print service: no XDG desktop portal, or one without a Print backend.
    NoService,
    /// No dialog could show for another reason, and why: another platform, a print already in
    /// progress, a hidden window, an overlay, or an error from the print service.
    Unavailable(String),
}

/// What [`Tx::print_pdf`] sends: the PDF, the job's title, and the dialog's choice.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintJob {
    pub title: String,
    pub setup: PrintSetup,
    pub bytes: Vec<u8>,
}

/// How handing a job to the print service went. The portal reports nothing after the hand-off:
/// no progress, no completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintSent {
    /// Handed off.
    Sent,
    /// The print service answered with an error, or couldn't be asked. KDE's backend answers
    /// so even when the job went through, so this doesn't mean it failed.
    NotConfirmed,
}

/// A print step that isn't open, from [`RuntimeCore::resolve_print_dialog`] and
/// [`RuntimeCore::resolve_print_job`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintError(pub String);
impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for PrintError {}

/// The print step showing, or about to show, as tests see it ([`RuntimeCore::print_request`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrintRequest<'a> {
    Prepare(&'a PrintOptions),
    Send(&'a PrintJob),
}

/// A step's answer, to the callback of the step that asked.
#[derive(Debug)]
pub(crate) enum PrintAnswer {
    Prepared(PrintPrepared),
    Sent(PrintSent),
}

#[derive(Clone)]
pub(crate) enum Step {
    Prepare(Arc<PrintOptions>),
    Send(Arc<PrintJob>),
}

impl Step {
    /// The answer when the desktop has no print service: for a job, not confirmed.
    pub(crate) fn no_service(&self) -> PrintAnswer {
        match self {
            Step::Prepare(_) => PrintAnswer::Prepared(PrintPrepared::NoService),
            Step::Send(_) => PrintAnswer::Sent(PrintSent::NotConfirmed),
        }
    }

    /// The answer when this step can't happen, or must end now: `why` for a dialog, and not
    /// confirmed for a job.
    pub(crate) fn refused(&self, why: &str) -> PrintAnswer {
        match self {
            Step::Prepare(_) => PrintAnswer::Prepared(PrintPrepared::Unavailable(why.into())),
            Step::Send(_) => PrintAnswer::Sent(PrintSent::NotConfirmed),
        }
    }

    pub(crate) fn cancelled(&self) -> PrintAnswer {
        match self {
            Step::Prepare(_) => PrintAnswer::Prepared(PrintPrepared::Cancelled),
            Step::Send(_) => PrintAnswer::Sent(PrintSent::NotConfirmed),
        }
    }

    fn request(&self) -> PrintRequest<'_> {
        match self {
            Step::Prepare(options) => PrintRequest::Prepare(options),
            Step::Send(job) => PrintRequest::Send(job),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            Step::Prepare(options) => serde_json::json!({ "step": "dialog", "title": options.title }),
            Step::Send(job) => serde_json::json!({ "step": "job", "title": job.title, "bytes": job.bytes.len() }),
        }
    }
}

pub(crate) type AnswerFn = Box<dyn FnOnce(crate::states::States<'_>, PrintAnswer, &mut Tx)>;

pub(crate) struct Request {
    pub step: Step,
    pub on_answer: AnswerFn,
}

/// A `Tx`'s print request. `Tx` is `Clone`; clones share the request and the runtime takes it once.
#[derive(Clone)]
pub(crate) struct Call(Rc<RefCell<Option<Request>>>);

impl Call {
    pub(crate) fn take(&self) -> Option<Request> { self.0.borrow_mut().take() }
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let request = self.0.borrow();
        f.debug_tuple("PrintCall").field(&request.as_ref().map(|r| r.step.request())).finish()
    }
}

/// The runtime's print step, from the request until its answer is queued.
pub(crate) struct Pending {
    pub id: u64,
    pub step: Step,
}

/// The runtime's one print step at a time. As with file dialogs (`DialogState`), its answer
/// carries its `id`, so an answer to a step that has since ended changes nothing.
#[derive(Default)]
pub(crate) enum StepState {
    /// None asked for, or the last one's answer is queued.
    #[default]
    Closed,
    /// Asked for; a host starts it next.
    Requested(Pending),
    /// A host has it on screen, or with the print service.
    Showing(Pending),
}

impl StepState {
    fn pending(&self) -> Option<&Pending> {
        match self {
            StepState::Closed => None,
            StepState::Requested(p) | StepState::Showing(p) => Some(p),
        }
    }

    pub(crate) fn request(&self) -> Option<PrintRequest<'_>> { self.pending().map(|p| p.step.request()) }
    pub(crate) fn is_open(&self) -> bool { !matches!(self, StepState::Closed) }
    pub(crate) fn id(&self) -> Option<u64> { self.pending().map(|p| p.id) }

    /// The host takes a requested step to start; nothing while one shows.
    pub(crate) fn show(&mut self) -> Option<(u64, Step)> {
        let StepState::Requested(_) = self else { return None };
        let StepState::Requested(p) = std::mem::take(self) else { unreachable!() };
        let started = (p.id, p.step.clone());
        *self = StepState::Showing(p);
        Some(started)
    }

    /// Step `id` was answered: it ends, if it's the one open.
    pub(crate) fn answered(&mut self, id: u64) {
        if self.id() == Some(id) { *self = StepState::Closed; }
    }

    /// Ends whatever is open, for its cancelled answer.
    pub(crate) fn take(&mut self) -> Option<Pending> {
        match std::mem::take(self) {
            StepState::Closed => None,
            StepState::Requested(p) | StepState::Showing(p) => Some(p),
        }
    }

    pub(crate) fn to_json(&self) -> Option<serde_json::Value> { self.pending().map(|p| p.step.to_json()) }
}

/// The runtime's printing: its one step, and the callbacks of steps asked for, by id, until
/// their answer runs.
#[derive(Default)]
pub(crate) struct PrintState {
    pub(crate) step: StepState,
    pub(crate) answers: Vec<(u64, AnswerFn)>,
    pub(crate) next: u64,
}

impl Tx {
    /// Show the desktop's print dialog once this handler's frame is done, started from
    /// `options`. `on_answer` runs, as an action on a later frame, with the reader's choice
    /// (`Ready`), `Cancelled` (also when the window hides or closes), `NoService` on a desktop
    /// without a print service, or `Unavailable` when no dialog could show for another reason:
    /// a platform other than Linux, or a print already in progress. Send the job from `on_answer` with [`Tx::print_pdf`].
    pub fn prepare_print<S: 'static>(&mut self, options: PrintOptions, on_answer: impl FnOnce(&mut S, PrintPrepared, &mut Tx) + 'static) {
        self.print_step(Step::Prepare(Arc::new(options)), move |s: &mut S, answer, tx| {
            if let PrintAnswer::Prepared(prepared) = answer { on_answer(s, prepared, tx); }
        });
    }

    /// Hand `job`'s PDF to the print service the reader chose in [`Tx::prepare_print`]'s
    /// dialog, without showing it again. `on_answer` runs, as an action on a later frame, with
    /// how the hand-off went.
    pub fn print_pdf<S: 'static>(&mut self, job: PrintJob, on_answer: impl FnOnce(&mut S, PrintSent, &mut Tx) + 'static) {
        self.print_step(Step::Send(Arc::new(job)), move |s: &mut S, answer, tx| {
            if let PrintAnswer::Sent(sent) = answer { on_answer(s, sent, tx); }
        });
    }

    fn print_step<S: 'static>(&mut self, step: Step, on_answer: impl FnOnce(&mut S, PrintAnswer, &mut Tx) + 'static) {
        let on_answer: AnswerFn = Box::new(move |s, answer, tx| {
            // None once the playground has dropped the stage that asked.
            if let Some(s) = s.cast::<S>() { on_answer(s, answer, tx); }
        });
        self.prints.push(Call(Rc::new(RefCell::new(Some(Request { step, on_answer })))));
    }
}

/// One window's print steps with the print service: shows what its runtime asks for, and queues
/// the answer after `wake` brings the host back to poll it. Held by the window's file dialog
/// presenter, which the host already drives at the same moments.
pub(crate) struct Presenter {
    showing: Option<(u64, Pin<Box<dyn Future<Output = PrintAnswer>>>)>,
}

impl Presenter {
    pub(crate) fn new() -> Self { Self { showing: None } }

    /// Queues the answer of a step that ended. True when one did.
    pub(crate) fn answer_core(&mut self, rt: &mut crate::runtime::RuntimeCore, wake: &std::task::Waker) -> bool {
        let Some((id, future)) = &mut self.showing else { return false };
        let Poll::Ready(answer) = future.as_mut().poll(&mut Context::from_waker(wake)) else { return false };
        let id = *id;
        self.showing = None;
        rt.answer_print(id, answer);
        true
    }

    /// Starts the runtime's step over `window`. True when an answer was queued.
    pub(crate) fn open_core(&mut self, rt: &mut crate::runtime::RuntimeCore, window: &winit::window::Window, wake: &std::task::Waker) -> bool {
        if self.showing.is_some() { return false; }
        let Some((id, step)) = rt.take_print() else { return false };
        self.showing = Some((id, show_over(step, window)));
        self.answer_core(rt, wake)
    }

    /// The window is going away: dropping the step's future closes the portal request.
    pub(crate) fn stop(&mut self) { self.showing = None; }
}

fn show_over(step: Step, window: &winit::window::Window) -> Pin<Box<dyn Future<Output = PrintAnswer>>> {
    #[cfg(all(target_os = "linux", feature = "dbus"))]
    return Box::pin(portal::show(step, window));
    #[cfg(not(all(target_os = "linux", feature = "dbus")))]
    {
        let _ = window;
        Box::pin(std::future::ready(step.refused("printing isn't available on this platform")))
    }
}

#[cfg(test)]
mod tests;
