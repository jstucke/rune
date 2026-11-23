use super::*;

fn settings(pairs: &[(&str, &str)]) -> PrintSettings {
    PrintSettings(pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect())
}

#[test]
fn page_ranges_are_read_only_when_the_reader_chose_ranges() {
    let ranges = |s: &[(&str, &str)]| PrintPages::from_settings(&settings(s));
    assert_eq!(ranges(&[("print-pages", "ranges"), ("page-ranges", "0-2,4,9-11")]), PrintPages::Ranges(vec![0..=2, 4..=4, 9..=11]));
    // KDE answers with one range; spaces and a reversed range are read as meant.
    assert_eq!(ranges(&[("print-pages", "ranges"), ("page-ranges", " 5 - 3 , 7 ")]), PrintPages::Ranges(vec![3..=5, 7..=7]));
    assert_eq!(ranges(&[("print-pages", "ranges"), ("page-ranges", "x,2,-1,")]), PrintPages::Ranges(vec![2..=2]));
    assert_eq!(ranges(&[("print-pages", "ranges"), ("page-ranges", "abc")]), PrintPages::All, "nothing readable");
    assert_eq!(ranges(&[("print-pages", "all"), ("page-ranges", "0-2")]), PrintPages::All, "ranges left over from before");
    assert_eq!(ranges(&[("print-pages", "current")]), PrintPages::All);
    assert_eq!(ranges(&[]), PrintPages::All);
}

use crate::{Cx, El, Env, Runtime, Tx, el};

#[derive(Default)]
struct Doc { prepared: Vec<String>, sent: Vec<PrintSent> }

fn options() -> PrintOptions {
    PrintOptions { title: "Print report.pdf".into(), settings: settings(&[("duplex", "vertical")]), ..Default::default() }
}

/// A print button: Ready sends three bytes under the file's name.
fn doc(_: &Doc, _: &mut Cx<Doc>) -> El {
    el().key("print").size(80.0, 20.0).on_press_tx(|_: &mut Doc, tx: &mut Tx| {
        tx.prepare_print(options(), |s: &mut Doc, prepared, tx| {
            s.prepared.push(format!("{prepared:?}"));
            if let PrintPrepared::Ready(setup) = prepared {
                tx.print_pdf(PrintJob { title: "report.pdf".into(), setup, bytes: vec![1, 2, 3] }, |s: &mut Doc, sent, _| s.sent.push(sent));
            }
        });
    })
}

fn press(rt: &mut Runtime<Doc>, t: f64) {
    rt.pointer_down(10.0, 10.0);
    rt.pointer_up(10.0, 10.0);
    rt.frame(t);
}

#[test]
fn the_dialogs_answer_runs_the_callback_which_sends_the_bytes_and_hears_back() {
    let mut rt = Runtime::new(Doc::default(), doc, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert!(rt.print_request().is_none());
    press(&mut rt, 1.0 / 60.0);
    assert_eq!(rt.print_request(), Some(PrintRequest::Prepare(&options())));
    assert!(rt.resolve_print_job(PrintSent::Sent).is_err(), "the dialog isn't a job");

    let chosen = settings(&[("print-pages", "ranges"), ("page-ranges", "1-2"), ("printer", "Office")]);
    rt.resolve_print_dialog(PrintPrepared::Ready(PrintSetup::new(chosen.clone(), PageSetup::default()))).unwrap();
    assert!(rt.print_request().is_none() && rt.state.prepared.is_empty(), "the answer waits for a frame");
    rt.frame(2.0 / 60.0);
    assert_eq!(rt.state.prepared.len(), 1);
    let Some(PrintRequest::Send(job)) = rt.print_request() else { panic!("the callback sent a job") };
    assert_eq!((job.title.as_str(), &job.setup.settings, job.setup.pages.clone(), job.bytes.as_slice()),
        ("report.pdf", &chosen, PrintPages::Ranges(vec![1..=2]), &[1u8, 2, 3][..]));

    rt.resolve_print_job(PrintSent::NotConfirmed).unwrap();
    rt.frame(3.0 / 60.0);
    assert_eq!(rt.state.sent, [PrintSent::NotConfirmed]);
    assert!(rt.print_request().is_none());
    assert!(rt.resolve_print_dialog(PrintPrepared::Cancelled).is_err(), "nothing is open");
}

#[test]
fn a_cancel_or_no_service_sends_nothing_and_a_second_print_waits_for_none() {
    let mut rt = Runtime::new(Doc::default(), doc, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    press(&mut rt, 1.0 / 60.0);
    press(&mut rt, 2.0 / 60.0);
    rt.frame(2.5 / 60.0);
    assert_eq!(rt.state.prepared, ["Unavailable(\"a print dialog is already open\")"], "a double click opens one dialog");
    assert!(matches!(rt.print_request(), Some(PrintRequest::Prepare(_))), "the first stays open");
    rt.resolve_print_dialog(PrintPrepared::Cancelled).unwrap();
    rt.frame(3.0 / 60.0);
    press(&mut rt, 4.0 / 60.0);
    rt.resolve_print_dialog(PrintPrepared::NoService).unwrap();
    rt.frame(5.0 / 60.0);
    assert_eq!(rt.state.prepared[1..], ["Cancelled", "NoService"]);
    assert!(rt.print_request().is_none() && rt.state.sent.is_empty());
}

#[test]
fn a_window_going_away_answers_what_is_open() {
    let mut rt = Runtime::new(Doc::default(), doc, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    press(&mut rt, 1.0 / 60.0);
    rt.cancel_file_dialogs();
    assert_eq!(rt.state.prepared, ["Cancelled"]);
    press(&mut rt, 2.0 / 60.0);
    rt.resolve_print_dialog(PrintPrepared::Ready(PrintSetup::new(PrintSettings::default(), PageSetup::default()))).unwrap();
    rt.frame(3.0 / 60.0);
    rt.cancel_file_dialogs();
    assert_eq!(rt.state.sent, [PrintSent::NotConfirmed]);
    assert!(rt.print_request().is_none());
}

#[test]
fn settings_without_pages_print_all_pages_next_time() {
    let chosen = settings(&[("print-pages", "ranges"), ("page-ranges", "1-2"), ("printer", "Office")]);
    assert_eq!(chosen.without_pages(), settings(&[("printer", "Office")]));
}

/// "Open and print": a file dialog's answer starts a print. The window going away with that
/// answer still queued ends the print it starts too.
#[test]
fn a_print_started_by_a_file_dialogs_answer_ends_with_the_window() {
    fn view(_: &Doc, _: &mut Cx<Doc>) -> El {
        el().key("open").size(80.0, 20.0).on_press_tx(|_: &mut Doc, tx: &mut Tx| {
            tx.open_file(crate::FileDialogOptions::default(), |_: &mut Doc, _, tx| {
                tx.prepare_print(options(), |s: &mut Doc, prepared, _| s.prepared.push(format!("{prepared:?}")));
            });
        })
    }
    let mut rt = Runtime::new(Doc::default(), view, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    press(&mut rt, 1.0 / 60.0);
    rt.resolve_file_dialog(Some("/docs/a.pdf".into())).unwrap();
    rt.cancel_file_dialogs();
    assert_eq!(rt.state.prepared, ["Cancelled"]);
    assert!(rt.print_request().is_none());
}
