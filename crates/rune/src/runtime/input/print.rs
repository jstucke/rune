use super::*;
use crate::print::{PrintAnswer, PrintError, PrintPrepared, PrintRequest, PrintSent, Request, Step};

impl RuntimeCore {
    /// The print step showing, or about to show, from [`Tx::prepare_print`] or [`Tx::print_pdf`].
    pub fn print_request(&self) -> Option<PrintRequest<'_>> { self.host.print.step.request() }

    /// Answers the print dialog showing as the reader or the print service would. Its callback
    /// runs on the next frame. For tests and headless drivers; native dialogs answer themselves.
    pub fn resolve_print_dialog(&mut self, prepared: PrintPrepared) -> Result<(), PrintError> {
        let id = self.print_step_id(|r| matches!(r, PrintRequest::Prepare(_)), "no print dialog is open")?;
        self.answer_print(id, PrintAnswer::Prepared(prepared));
        Ok(())
    }

    /// Answers the print job being sent as the print service would; see
    /// [`RuntimeCore::resolve_print_dialog`].
    pub fn resolve_print_job(&mut self, sent: PrintSent) -> Result<(), PrintError> {
        let id = self.print_step_id(|r| matches!(r, PrintRequest::Send(_)), "no print job is being sent")?;
        self.answer_print(id, PrintAnswer::Sent(sent));
        Ok(())
    }

    fn print_step_id(&self, is: impl Fn(PrintRequest<'_>) -> bool, none: &str) -> Result<u64, PrintError> {
        match (self.host.print.step.request(), self.host.print.step.id()) {
            (Some(request), Some(id)) if is(request) => Ok(id),
            _ => Err(PrintError(none.into())),
        }
    }

    /// Takes a handler's request. One step at a time: a request while one is open is refused
    /// rather than queued, so a double click opens one dialog.
    pub(crate) fn request_print(&mut self, request: Request) {
        self.host.print.next += 1;
        let id = self.host.print.next;
        self.host.print.answers.push((id, request.on_answer));
        let refuse = if self.host.native_overlay {
            Some("overlays cannot print")
        } else if self.host.print.step.is_open() {
            Some(match self.host.print.step.request() {
                Some(PrintRequest::Prepare(_)) => "a print dialog is already open",
                _ => "a print job is being sent",
            })
        } else {
            None
        };
        if let Some(why) = refuse {
            self.trace_event(format_args!("print:refused:{why}"));
            self.frame.queue.push(Action::Print(id, request.step.refused(why)));
            return;
        }
        self.trace_event(if matches!(request.step, Step::Prepare(_)) { "print:dialog" } else { "print:job" });
        self.host.print.step = crate::print::StepState::Requested(crate::print::Pending { id, step: request.step });
    }

    /// The step for the host to start, marked shown. Nothing while it shows.
    pub(crate) fn take_print(&mut self) -> Option<(u64, Step)> { self.host.print.step.show() }

    /// Queues step `id`'s answer for the next frame, ending it.
    pub(crate) fn answer_print(&mut self, id: u64, answer: PrintAnswer) {
        self.host.print.step.answered(id);
        self.frame.queue.push(Action::Print(id, answer));
    }

    /// Answers a step this host can't start with `why`.
    pub(crate) fn refuse_print(&mut self, why: &str) {
        if let Some((id, step)) = self.take_print() {
            self.answer_print(id, step.refused(why));
        }
    }

    pub(in crate::runtime) fn run_print(&mut self, app: &mut dyn AppDriver, id: u64, answer: PrintAnswer, one: &mut Tx) -> bool {
        let Some(i) = self.host.print.answers.iter().position(|(step, _)| *step == id) else { return false };
        let (_, f) = self.host.print.answers.remove(i);
        self.trace_event(format_args!("print:answer:{}", match &answer {
            PrintAnswer::Prepared(PrintPrepared::Ready(_)) => "ready",
            PrintAnswer::Prepared(PrintPrepared::Cancelled) => "cancel",
            PrintAnswer::Prepared(PrintPrepared::NoService) => "no-service",
            PrintAnswer::Prepared(PrintPrepared::Unavailable(_)) => "unavailable",
            PrintAnswer::Sent(PrintSent::Sent) => "sent",
            PrintAnswer::Sent(PrintSent::NotConfirmed) => "not-confirmed",
        }));
        f(app.states(), answer, one);
        true
    }

    /// Ends the open or waiting step (a dialog as cancelled, a job as not confirmed), and runs
    /// any queued print callbacks now while the window's state is still alive. A callback that
    /// starts another step is answered too, up to a bound against one that always asks again.
    pub(crate) fn cancel_prints(&mut self, app: &mut dyn AppDriver) {
        for _ in 0..64 {
            if let Some(p) = self.host.print.step.take() {
                self.frame.queue.push(Action::Print(p.id, p.step.cancelled()));
            }
            let Some(pos) = self.frame.queue.iter().position(|a| matches!(a, Action::Print(..))) else { return };
            let Action::Print(id, answer) = self.frame.queue.remove(pos) else { unreachable!() };
            let mut one = self.transaction(app);
            self.run_print(app, id, answer, &mut one);
            self.latch_requests(app, &mut one);
            merge(&mut self.frame.pending, one);
            self.commit.dirty = true;
        }
        eprintln!("rune: print callbacks kept printing while their window went away; dropping {}", self.host.print.answers.len());
        self.host.print.step = Default::default();
        self.host.print.answers.clear();
        self.frame.queue.retain(|a| !matches!(a, Action::Print(..)));
    }

    pub(crate) fn print_json(&self) -> Option<serde_json::Value> { self.host.print.step.to_json() }
}
