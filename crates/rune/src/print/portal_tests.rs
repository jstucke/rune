use super::*;
use crate::print::{PageSetupValue, PrintOptions, PrintPages};
use std::sync::Arc;
use zbus::fdo;

fn owned(v: Value<'_>) -> OwnedValue { OwnedValue::try_from(v).unwrap() }

#[test]
fn options_carry_the_token_and_ask_for_pdf_from_version_3() {
    let prepare = request_options("t1", 0, 3);
    assert_eq!(prepare.get("handle_token"), Some(&Value::from("t1")));
    assert_eq!(prepare.get("modal"), Some(&Value::from(true)));
    assert_eq!(prepare.get("supported_output_file_formats"), Some(&Value::from(vec!["pdf"])));
    assert!(!prepare.contains_key("token"), "PreparePrint takes no token");
    let old = request_options("t2", 7, 2);
    assert!(!old.contains_key("supported_output_file_formats"), "version 2 portals reject unknown options' meaning");
    assert_eq!(old.get("token"), Some(&Value::from(7u32)));
    assert_eq!(request_options("t3", 7, 4).get("supported_output_file_formats"), Some(&Value::from(vec!["pdf"])));
}

#[test]
fn settings_and_page_setup_go_as_the_portal_types_them() {
    let settings = PrintSettings([("duplex".to_string(), "vertical".to_string())].into());
    assert_eq!(settings_map(&settings), HashMap::from([("duplex", Value::from("vertical"))]));
    let page_setup = PageSetup([
        ("PPDName".to_string(), PageSetupValue::Text("A4".into())),
        ("Width".to_string(), PageSetupValue::Number(210.0)),
    ].into());
    assert_eq!(page_setup_map(&page_setup), HashMap::from([("PPDName", Value::from("A4")), ("Width", Value::from(210.0))]));
}

#[test]
fn a_choice_comes_back_with_its_settings_page_setup_pages_and_token() {
    let settings = HashMap::from([
        ("print-pages", Value::from("ranges")),
        ("page-ranges", Value::from("0-1,4")),
        ("n-copies", Value::from("2")),
        ("odd", Value::from(3u32)),
    ]);
    let page_setup = HashMap::from([("Name", Value::from("iso_a4")), ("Height", Value::from(297.0)), ("Margin", Value::from(5i32))]);
    let results = HashMap::from([
        ("settings".to_string(), owned(Value::from(settings))),
        ("page-setup".to_string(), owned(Value::from(page_setup))),
        ("token".to_string(), owned(Value::from(41u32))),
    ]);
    let PrintPrepared::Ready(setup) = prepared(0, &results) else { panic!("a choice") };
    assert_eq!(setup.token, 41);
    assert_eq!(setup.pages, PrintPages::Ranges(vec![0..=1, 4..=4]));
    assert_eq!(setup.settings.0.get("n-copies").map(String::as_str), Some("2"));
    assert!(!setup.settings.0.contains_key("odd"), "settings are strings");
    assert_eq!(setup.page_setup.0.get("Name"), Some(&PageSetupValue::Text("iso_a4".into())));
    assert_eq!(setup.page_setup.0.get("Height"), Some(&PageSetupValue::Number(297.0)));
    assert_eq!(setup.page_setup.0.get("Margin"), Some(&PageSetupValue::Number(5.0)));

    let empty = prepared(0, &HashMap::new());
    assert_eq!(empty, PrintPrepared::Ready(super::super::PrintSetup::new(PrintSettings::default(), PageSetup::default())));
    assert_eq!(prepared(1, &results), PrintPrepared::Cancelled);
    // GTK's backend answers 2 to its Cancel button and to Escape.
    assert_eq!(prepared(2, &results), PrintPrepared::Cancelled);
    assert!(matches!(prepared(3, &results), PrintPrepared::Unavailable(_)));
}

#[test]
fn only_response_0_confirms_a_job() {
    assert_eq!(sent(0), PrintSent::Sent);
    assert_eq!(sent(1), PrintSent::NotConfirmed);
    assert_eq!(sent(2), PrintSent::NotConfirmed, "KDE answers 2 when lpr succeeded");
}

#[test]
fn the_pdf_goes_as_a_memory_file_read_from_its_start() {
    use std::io::Read;
    let mut file = std::fs::File::from(memfd(b"%PDF-1.7 ...").unwrap());
    let mut read = Vec::new();
    file.read_to_end(&mut read).unwrap();
    assert_eq!(read, b"%PDF-1.7 ...");
}

#[test]
fn no_portal_or_no_print_backend_is_no_service() {
    let dialog = Step::Prepare(Arc::new(PrintOptions::default()));
    let fdo = |e| zbus::Error::FDO(Box::new(e));
    let missing = [fdo::Error::ServiceUnknown("portal".into()), fdo::Error::UnknownInterface("Print".into()), fdo::Error::UnknownMethod("PreparePrint".into())];
    for e in missing {
        assert!(matches!(dbus_error(&dialog, fdo(e)), PrintAnswer::Prepared(PrintPrepared::NoService)));
    }
    assert!(matches!(dbus_error(&dialog, fdo(fdo::Error::AccessDenied("lockdown".into()))),
        PrintAnswer::Prepared(PrintPrepared::Unavailable(why)) if why.contains("lockdown")));
    let job = Step::Send(Arc::new(super::super::PrintJob {
        title: "a.pdf".into(), setup: super::super::PrintSetup::new(PrintSettings::default(), PageSetup::default()), bytes: vec![],
    }));
    assert!(matches!(dbus_error(&job, fdo(fdo::Error::ServiceUnknown("portal".into()))), PrintAnswer::Sent(PrintSent::NotConfirmed)));
}
