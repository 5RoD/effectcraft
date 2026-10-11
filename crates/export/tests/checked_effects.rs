//! A native checked effect must fail output visibly, without replacing completed files.
use std::sync::Mutex;

use effectcraft_color::Label;
use effectcraft_export::render_queue::*;
use effectcraft_export::{ExportError, Job, JobOptions, export};
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::{Comp, ItemKind, LayerSource, Project, Solid, build};
use effectcraft_render::{NoFootage, RenderOpts, Renderer};
use effectcraft_time::{FrameRate, Tick};

#[test]
fn checked_media_late_decode_failure_preserves_completed_export() {
    use effectcraft_project::{Footage, FootageKind, ItemId};
    use effectcraft_render::{FootageSource, Image};
    use std::sync::Arc;

    struct FailingMovie;
    impl FootageSource for FailingMovie {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
            None
        }
        fn frame_checked(&self, _: ItemId, f: &Footage, t: Tick) -> Result<Option<Arc<Image>>, String> {
            if t >= Tick::from_seconds_f64(1.0) {
                return Err("synthetic late decode failure".into());
            }
            Ok(Some(Arc::new(Image::filled(f.width, f.height, [0.5, 0.25, 1.0, 1.0]))))
        }
    }
    let mut project = Project::default();
    let mut comp = Comp::new(8, 8, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let fid = project.add_item(
        "Broken movie",
        Label::Aqua,
        None,
        ItemKind::Footage(Footage {
            path: "broken.mp4".into(),
            kind: FootageKind::Video,
            width: 8,
            height: 8,
            frame_rate: FrameRate::FPS_30,
            duration: Tick::from_seconds_f64(2.0),
            has_video: true,
            ..Default::default()
        }),
    );
    let layer = build::layer(&mut project, &comp, "Movie", LayerSource::Footage { item: fid }, (8, 8), None);
    comp.layers.push(layer);
    let cid = project.add_item("Media export", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    let before = project.clone();
    let settings = RenderSettings { time_span: TimeSpan::Custom { start: Tick::ZERO, end: Tick::from_seconds_f64(2.0) }, ..Default::default() };
    let output = OutputModule::for_format(OutputFormat::Gif);
    let dir = std::env::temp_dir().join(format!("ec-checked-media-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let destination = dir.join("completed.gif");
    std::fs::write(&destination, b"previous successful export").unwrap();
    let job = Job {
        project: &project,
        footage: &FailingMovie,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: &output,
        path: destination.to_str().unwrap(),
        sink: None,
        nested_switches: true,
        options: JobOptions::default(),
    };
    let mut rendered = 0;
    let error = export(&job, &mut |progress| {
        rendered = rendered.max(progress.done);
        true
    })
    .unwrap_err();
    assert!(matches!(error, ExportError::Render(_)));
    assert!(error.to_string().contains("footage"));
    assert!(error.to_string().contains("broken.mp4"));
    assert!(rendered > 0, "the error occurs after a completed earlier batch");
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous successful export");
    assert_eq!(project, before);
    assert!(std::fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().starts_with(".effectcraft-export-")));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn checked_raster_export_rejects_capacity_overflow_before_codec_setup_and_preserves_destination() {
    let mut project = Project::default();
    let comp = Comp::new(u32::MAX, u32::MAX, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
    let cid = project.add_item("Oversized", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    let before = project.clone();
    let settings = RenderSettings::default();
    let output = OutputModule::for_format(OutputFormat::Gif);
    let directory = std::env::temp_dir().join(format!("ec-checked-raster-export-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("completed.gif");
    std::fs::write(&destination, b"previous successful export").unwrap();
    let job = Job {
        project: &project,
        footage: &NoFootage,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: &output,
        path: destination.to_str().unwrap(),
        sink: None,
        nested_switches: true,
        options: JobOptions::default(),
    };
    let error = export(&job, &mut |_| true).unwrap_err();
    assert!(matches!(error, ExportError::Render(_)));
    assert!(error.to_string().contains("raster geometry before export"));
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous successful export");
    assert_eq!(project, before);
    let entries: Vec<_> = std::fs::read_dir(&directory).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert!(entries.iter().all(|path| !path.file_name().unwrap().to_string_lossy().starts_with(".effectcraft-export-")));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn checked_bloom_export_late_failure_preserves_destination_and_has_no_finished_sink_movie() {
    let mut project = Project::default();
    let mut comp = Comp::new(8, 8, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let solid = project.add_item("Emitter", Label::Red, None, ItemKind::Solid(Solid { color: [1.0; 3], width: 8, height: 8, pixel_aspect: 1.0 }));
    let mut layer = build::layer(&mut project, &comp, "Emitter", LayerSource::Solid { item: solid }, (8, 8), None);
    let spec = effectcraft_effects::find(effectcraft_effects::bloom::ID).unwrap();
    let mut effect = effectcraft_effects::instantiate(spec, &mut build::Ids(&mut project.next_id), spec.name, [8.0; 2]);
    // More than the largest encoding batch succeeds before the malformed animated value.
    effect.prop_mut("radius").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, Value::Scalar(0.0)).hold(), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Scalar(f64::MAX)).hold()];
    layer.props.sub_mut("effects").unwrap().children.push(effect.into());
    comp.layers.push(layer);
    let cid = project.add_item("Bloom export", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    let renderer = Renderer::new(&project, &NoFootage, RenderOpts::default());
    assert!(renderer.comp_frame_checked(cid, Tick::ZERO).is_ok());
    assert!(renderer.comp_frame_checked(cid, Tick::from_seconds_f64(1.0)).is_err());
    let settings = RenderSettings { time_span: TimeSpan::Custom { start: Tick::ZERO, end: Tick::from_seconds_f64(2.0) }, ..Default::default() };
    let output = OutputModule::for_format(OutputFormat::Gif);
    let files = std::sync::Arc::new(Mutex::new(Vec::new()));
    let received = files.clone();
    let sink = move |path: &str, data: Vec<u8>| received.lock().unwrap().push((path.to_string(), data));
    let mut job = Job {
        project: &project,
        footage: &NoFootage,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: &output,
        path: "bloom.gif",
        sink: Some(&sink),
        nested_switches: true,
        options: JobOptions::default(),
    };
    let mut rendered = 0;
    let error = export(&job, &mut |progress| {
        rendered = rendered.max(progress.done);
        true
    })
    .unwrap_err();
    assert!(matches!(error, ExportError::Render(_)));
    assert!(error.to_string().contains("ec.stylize.bloom"));
    assert!(rendered > 0, "a completed earlier batch proves the failure occurs mid-export");
    let completed = files.lock().unwrap();
    assert_eq!(completed.len(), 1, "the failure log is the only completed sink file");
    assert!(completed[0].0.ends_with(".txt"));
    assert!(String::from_utf8_lossy(&completed[0].1).contains("ec.stylize.bloom"));
    drop(completed);
    let dir = std::env::temp_dir().join(format!("ec-checked-bloom-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("bloom.gif");
    std::fs::write(&target, b"previous successful export").unwrap();
    job.sink = None;
    let path = target.to_str().unwrap();
    job.path = path;
    assert!(matches!(export(&job, &mut |_| true), Err(ExportError::Render(_))));
    assert_eq!(std::fs::read(&target).unwrap(), b"previous successful export");
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(entries.len(), 2, "previous movie and completed failure log remain");
    assert!(
        entries.iter().all(|path| !path.file_name().unwrap().to_string_lossy().starts_with(".effectcraft-export-")),
        "unfinished temporary file is removed"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn checked_registered_plugin_late_panic_fails_export_and_later_valid_output_recovers() {
    use effectcraft_effects::plugin::{EffectPlugin, PluginFrame, PluginManifest, PluginParams, register_plugin};
    struct Fault(PluginManifest);
    impl EffectPlugin for Fault {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn render(&self, frame: &mut PluginFrame, _params: &PluginParams, time: f64) -> Result<(), String> {
            if time >= 1.0 {
                frame.pixels[0] = [0.0; 4];
                panic!("synthetic late export plug-in panic");
            }
            Ok(())
        }
    }
    let spec = register_plugin(std::sync::Arc::new(Fault(PluginManifest {
        api: 1,
        id: "org.test.export-checked-plugin".into(),
        name: "Checked Export Plug-in".into(),
        category: "Test".into(),
        version: String::new(),
        author: String::new(),
        description: String::new(),
        params: Vec::new(),
    })))
    .unwrap();
    let mut project = Project::default();
    let mut comp = Comp::new(8, 8, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let solid = project.add_item("Emitter", Label::Red, None, ItemKind::Solid(Solid { color: [1.0; 3], width: 8, height: 8, pixel_aspect: 1.0 }));
    let mut layer = build::layer(&mut project, &comp, "Emitter", LayerSource::Solid { item: solid }, (8, 8), None);
    let effect = effectcraft_effects::instantiate(spec, &mut build::Ids(&mut project.next_id), spec.name, [8.0; 2]);
    layer.props.sub_mut("effects").unwrap().children.push(effect.into());
    comp.layers.push(layer);
    let cid = project.add_item("Plug-in export", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    let before = project.clone();
    let settings = RenderSettings { time_span: TimeSpan::Custom { start: Tick::ZERO, end: Tick::from_seconds_f64(2.0) }, ..Default::default() };
    let output = OutputModule::for_format(OutputFormat::Gif);
    let files = std::sync::Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let received = files.clone();
    let sink = move |path: &str, data: Vec<u8>| received.lock().unwrap().push((path.to_string(), data));
    let mut job = Job {
        project: &project,
        footage: &NoFootage,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: &output,
        path: "plugin.gif",
        sink: Some(&sink),
        nested_switches: true,
        options: JobOptions::default(),
    };
    let mut rendered = 0;
    let error = export(&job, &mut |progress| {
        rendered = rendered.max(progress.done);
        true
    })
    .unwrap_err();
    assert!(matches!(error, ExportError::Render(_)));
    assert!(error.to_string().contains("Rust plug-in panicked"));
    assert!(error.to_string().contains(spec.id));
    assert!(rendered > 0, "a completed batch precedes the failed frame");
    assert!(files.lock().unwrap().iter().all(|(name, _)| name.ends_with(".txt")), "no finished movie reaches the sink");
    let directory = std::env::temp_dir().join(format!("ec-checked-plugin-export-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("completed.gif");
    std::fs::write(&destination, b"previous successful export").unwrap();
    job.path = destination.to_str().unwrap();
    job.sink = None;
    assert!(matches!(export(&job, &mut |_| true), Err(ExportError::Render(_))));
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous successful export");
    assert_eq!(project, before);
    let valid_settings = RenderSettings { time_span: TimeSpan::Custom { start: Tick::ZERO, end: Tick::from_seconds_f64(0.5) }, ..Default::default() };
    job.settings = &valid_settings;
    job.sink = Some(&sink);
    job.path = "repaired.gif";
    export(&job, &mut |_| true).unwrap();
    assert!(files.lock().unwrap().iter().any(|(name, data)| name == "repaired.gif" && data.starts_with(b"GIF")));
    std::fs::remove_dir_all(directory).unwrap();
}
