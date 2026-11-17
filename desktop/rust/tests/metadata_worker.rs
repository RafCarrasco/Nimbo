//! Real thread/queue test with a labeled metadata provider fixture, not OS-enumeration evidence.
use nimbo_desktop::{
    picker::{PICKER_URL, PickerRequest, PickerSession},
    policy::{Config, NativeRequest},
    sources::worker::MetadataWorker,
    sources::{MetadataProvider, NativeMonitor, NativeProcess, NativeWindow},
};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
fn ticket() -> nimbo_desktop::picker::RefreshTicket {
    let main = NativeRequest {
        window: "main",
        source: "https://golive-ad118.web.app/",
        frame_id: Some(1),
        main_frame_id: Some(1),
        parent_frame_id: None,
        document_generation: 1,
    };
    let cfg = Config::release();
    let mut session = PickerSession::new(&cfg, &main, 50, false).unwrap();
    session.bind_document(2).unwrap();
    session
        .begin_refresh(
            &cfg,
            &main,
            &PickerRequest {
                window: 50,
                source: PICKER_URL,
                frame_id: 2,
                parent_frame_id: None,
                document_generation: 1,
            },
        )
        .unwrap()
}
struct Fixture(mpsc::Sender<thread::ThreadId>);
impl MetadataProvider for Fixture {
    type Error = &'static str;
    fn monitors(&self) -> Result<Vec<NativeMonitor>, Self::Error> {
        self.0.send(thread::current().id()).unwrap();
        Ok(vec![])
    }
    fn windows(&self, _own: &[u32]) -> Result<Vec<NativeWindow>, Self::Error> {
        Ok(vec![NativeWindow {
            hwnd: "22".into(),
            pid: 3,
            title: "Worker fixture".into(),
            path: "".into(),
            class_name: "Fixture".into(),
            visible: true,
            owned: false,
            tool: false,
            cloaked: false,
            minimized: false,
            fullscreen: false,
        }])
    }
    fn processes(&self) -> Result<Vec<NativeProcess>, Self::Error> {
        Ok(vec![])
    }
}
#[test]
fn shutdown_cancels_queued_jobs_and_stops_between_provider_queries() {
    struct Blocking {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        calls: mpsc::Sender<&'static str>,
    }
    impl MetadataProvider for Blocking {
        type Error = &'static str;
        fn monitors(&self) -> Result<Vec<NativeMonitor>, Self::Error> {
            self.calls.send("monitors").unwrap();
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(vec![])
        }
        fn windows(&self, _own: &[u32]) -> Result<Vec<NativeWindow>, Self::Error> {
            self.calls.send("windows").unwrap();
            Ok(vec![])
        }
        fn processes(&self) -> Result<Vec<NativeProcess>, Self::Error> {
            self.calls.send("processes").unwrap();
            Ok(vec![])
        }
    }
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (calls_tx, calls) = mpsc::channel();
    let mut worker = MetadataWorker::spawn(Blocking {
        entered: entered_tx,
        release,
        calls: calls_tx,
    })
    .unwrap();
    worker.submit(ticket(), vec![]).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.submit(ticket(), vec![]).unwrap();
    assert!(
        worker.submit(ticket(), vec![]).is_err(),
        "queue must be bounded"
    );
    assert!(
        worker.shutdown(Duration::ZERO).is_err(),
        "blocked provider must not claim a successful join"
    );
    release_tx.send(()).unwrap();
    let _ = release_tx.send(());
    worker.shutdown(Duration::from_secs(2)).unwrap();
    assert_eq!(calls.try_iter().collect::<Vec<_>>(), vec!["monitors"]);
    assert!(worker.take().unwrap().is_none());
    assert!(worker.submit(ticket(), vec![]).is_err());
}
#[test]
fn metadata_queries_run_on_an_owned_worker_and_return_the_original_ticket() {
    let (tx, rx) = mpsc::channel();
    let mut worker = MetadataWorker::spawn(Fixture(tx)).unwrap();
    let t = ticket();
    worker.submit(t, vec![]).unwrap();
    assert_ne!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        thread::current().id()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let (returned, result) = loop {
        if let Some(result) = worker.take().unwrap() {
            break result;
        }
        assert!(Instant::now() < deadline, "worker did not return catalog");
        thread::sleep(Duration::from_millis(1));
    };
    assert!(returned == t);
    assert_eq!(result.unwrap().list()[0]["name"], "Worker fixture");
    worker.shutdown(Duration::from_secs(2)).unwrap();
}
