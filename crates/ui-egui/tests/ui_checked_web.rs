//! Native stand-ins for the browser's JSON frame/job worker boundaries.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use effectcraft_engine::offload::{Post, WorkerJob, WorkerReply, WorkerRequest};
use effectcraft_engine::remote::{FrameMsg, FrameReply, FrameServer, Mirror};
use effectcraft_engine::render::{Accelerator, RenderOpts};
use effectcraft_engine::{Session, keyframe::Value, project::LayerId};
use effectcraft_ui_egui::frames::{FrameKey, Frames, RemoteDone, RemoteFrame, RemoteFrames, RemoteJob, RenderSource};
use serde_json::json;

fn roundtrip<T: serde::Serialize + for<'de> serde::Deserialize<'de>>(value: &T) -> T {
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

struct BrowserFrames {
    state: Mutex<(Mirror, FrameServer)>,
    requests: AtomicUsize,
}
impl RemoteFrames for BrowserFrames {
    fn slots(&self) -> usize {
        1
    }
    fn start(&self, job: RemoteJob, done: RemoteDone) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        let (mirror, server) = &mut *state;
        if let Some(message) = mirror.sync(job.revision, &job.project) {
            server.handle(roundtrip(&message));
        }
        let reply = server
            .handle(roundtrip(&FrameMsg::Render {
                id: 19,
                revision: job.revision,
                comp: job.comp,
                time: job.t,
                opts: Box::new(job.opts),
                disk: None,
                layers: false,
                prefetch: vec![],
            }))
            .into_iter()
            .next()
            .unwrap();
        let result = match (roundtrip(&reply.0), reply.1) {
            (FrameReply::Failed { error, stale, .. }, pixels) => {
                assert!(!stale);
                assert!(pixels.is_none());
                // This is the browser's on_message branch, forwarding the error verbatim.
                Err(error)
            }
            (FrameReply::Frame { width, height, ms, .. }, Some(rgba)) => Ok(RemoteFrame { width, height, rgba, ms }),
            other => panic!("unexpected frame reply: {:?}", other.0),
        };
        drop(state);
        done(result);
    }
}

fn scene() -> (Session, effectcraft_engine::project::ItemId, u64) {
    let mut session = Session::default();
    let comp = effectcraft_engine::project::ItemId(
        session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 0.2, "frameRate": 10})).unwrap()["comp"].as_u64().unwrap(),
    );
    let layer = session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
    session.execute("effect.apply", json!({"layers": [layer], "effect": "Native Bloom"})).unwrap();
    Arc::make_mut(&mut session.project).comp_mut(comp).unwrap().layer_mut(LayerId(layer)).unwrap().props.prop_mut("effects/#1/radius").unwrap().value =
        Value::Scalar(f64::MAX);
    (session, comp, layer)
}
fn source(session: &Session) -> RenderSource {
    RenderSource {
        project: session.project.clone(),
        footage: session.footage.clone(),
        expr: session.expr.clone(),
        layer_cache: session.layer_cache.clone(),
        gpu: None,
        gpu_display: false,
        disk: None,
    }
}
fn key(session: &Session, comp: effectcraft_engine::project::ItemId) -> FrameKey {
    FrameKey {
        revision: session.revision,
        content: effectcraft_ui_egui::frames::comp_content(&session.project, comp),
        comp: comp.0,
        frame: 0,
        scale: 1000,
        view: 0,
        opts: 0,
    }
}

#[test]
fn checked_browser_frame_failure_is_terminal_and_recovers_after_edit() {
    let (mut session, comp, layer) = scene();
    let remote = Arc::new(BrowserFrames {
        state: Mutex::new((Mirror::default(), FrameServer::new(session.footage.clone(), session.expr.clone()))),
        requests: AtomicUsize::new(0),
    });
    let mut frames = Frames::default();
    frames.set_remote(Some(remote.clone()));
    let failed = key(&session, comp);
    frames.request_urgent(&source(&session), failed, comp, Default::default(), RenderOpts::default());
    assert_eq!(frames.dispatch_remote(), 1);
    assert!(frames.failure(&failed).unwrap().starts_with("Render failed in comp "));
    assert_eq!(frames.inflight(), 0);
    assert_eq!(frames.remote_busy(), 0);
    assert!(!frames.urgent_pending());
    assert!(!frames.is_cached(&failed));
    for _ in 0..100 {
        frames.request_urgent(&source(&session), failed, comp, Default::default(), RenderOpts::default());
        assert_eq!(frames.dispatch_remote(), 0);
    }
    assert_eq!(remote.requests.load(Ordering::Relaxed), 1);
    session.execute("prop.set", json!({"layer": layer, "path": "effects/#1/radius", "value": 0.0})).unwrap();
    let repaired = key(&session, comp);
    assert_ne!(failed, repaired);
    frames.request_urgent(&source(&session), repaired, comp, Default::default(), RenderOpts::default());
    assert_eq!(frames.dispatch_remote(), 1);
    assert!(frames.failure(&repaired).is_none());
    assert!(frames.is_cached(&repaired));
    assert_eq!(remote.requests.load(Ordering::Relaxed), 2);
}

#[test]
fn checked_browser_render_job_reports_failure_without_delivering_finished_movie() {
    let (mut session, _, _) = scene();
    Arc::make_mut(&mut session.project).settings.gpu_acceleration = true;
    session.execute("renderQueue.add", json!({"format": "gif", "output": "/job/bloom.gif"})).unwrap();
    let item = session.project.render_queue.first().unwrap().clone();
    let request = WorkerRequest {
        id: 1,
        project: session.project.to_json(),
        files: vec![],
        job: WorkerJob::Render { items: vec![vec![(item, "/job/bloom.gif".into())]] },
    };
    let gpu = effectcraft_gpu::Gpu::headless_deferred();
    let required = std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1");
    assert!(!required || gpu.is_some(), "checked browser job acceptance requires a working deferred GPU");
    if let Some(gpu) = &gpu {
        eprintln!("checked browser job adapter: {}", gpu.name());
    }
    for accelerator in [None, gpu.as_ref()] {
        let files = Arc::new(Mutex::new(Vec::new()));
        let completed = files.clone();
        let sink = Arc::new(move |path: &str, bytes: Vec<u8>| completed.lock().unwrap().push((path.to_string(), bytes)));
        let mut worker = Session { exporter: Some(Arc::new(effectcraft_host::FileExporter { sink: Some(sink) })), ..Default::default() };
        worker.accel = accelerator.map(|gpu| Arc::new(gpu.clone()) as Arc<dyn Accelerator>);
        let replies = Arc::new(Mutex::new(Vec::new()));
        let delivered = replies.clone();
        let post: Post = std::rc::Rc::new(move |reply| delivered.lock().unwrap().push(roundtrip(&reply)));
        effectcraft_engine::offload::run_request(&mut worker, roundtrip(&request), &post);
        let replies = replies.lock().unwrap();
        let error = replies
            .iter()
            .find_map(|reply| match reply {
                WorkerReply::Item {
                    update:
                        effectcraft_engine::render_queue::ItemUpdate::Finished {
                            status: effectcraft_engine::project::render_queue::RenderStatus::Failed(error),
                            ..
                        },
                } => Some(error),
                _ => None,
            })
            .expect("the browser Render Queue receives a failed item");
        assert!(error.contains("ec.stylize.bloom"));
        assert!(matches!(replies.last(), Some(WorkerReply::Done)));
        let files = files.lock().unwrap();
        assert_eq!(files.len(), 1, "only the completed failure log reaches the browser download sink");
        assert!(files[0].0.ends_with(".txt"));
        assert!(String::from_utf8_lossy(&files[0].1).contains("ec.stylize.bloom"));
    }
}
