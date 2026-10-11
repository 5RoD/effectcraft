//! Retiming failures use the checked export boundary and preserve completed destinations.
use effectcraft_export::{ExportError, Job, JobOptions, export, render_queue::*};
use effectcraft_keyframe::Value;
use effectcraft_project::{Comp, ItemKind, LayerSource, Project, Solid, build};
use effectcraft_render::{NoFootage, RenderOpts, Renderer};
use effectcraft_time::{FrameRate, Tick};

#[test]
fn checked_timewarp_export_rejects_hostile_workflow_and_preserves_project_and_destination() {
    let mut project = Project::default();
    let mut comp = Comp::new(8, 8, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
    let solid = project.add_item("Source", Default::default(), None, ItemKind::Solid(Solid { color: [1.0; 3], width: 8, height: 8, pixel_aspect: 1.0 }));
    let mut layer = build::layer(&mut project, &comp, "Source", LayerSource::Solid { item: solid }, (8, 8), None);
    let spec = effectcraft_effects::find(effectcraft_effects::timewarp::ID).unwrap();
    let mut effect = effectcraft_effects::instantiate(spec, &mut build::Ids(&mut project.next_id), spec.name, [8.0; 2]);
    effect.prop_mut("speed").unwrap().value = Value::Scalar(f64::MAX);
    layer.props.sub_mut("effects").unwrap().children.push(effect.into());
    comp.layers.push(layer);
    let cid = project.add_item("Timewarp export", Default::default(), None, ItemKind::Comp(comp.into()));
    let before = project.clone();
    let error = Renderer::new(&project, &NoFootage, RenderOpts::default()).comp_frame_checked(cid, Tick::ZERO).unwrap_err();
    assert_eq!(error.effect_id, effectcraft_effects::timewarp::ID);
    let directory = std::env::temp_dir().join(format!("ec-checked-timewarp-export-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("completed.gif");
    std::fs::write(&path, b"previous successful export").unwrap();
    let settings = RenderSettings::default();
    let output = OutputModule::for_format(OutputFormat::Gif);
    let job = Job {
        project: &project,
        footage: &NoFootage,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: &output,
        path: path.to_str().unwrap(),
        sink: None,
        nested_switches: true,
        options: JobOptions::default(),
    };
    let error = export(&job, &mut |_| true).unwrap_err();
    assert!(matches!(error, ExportError::Render(_)));
    assert!(error.to_string().contains(effectcraft_effects::timewarp::ID));
    assert_eq!(std::fs::read(&path).unwrap(), b"previous successful export");
    assert_eq!(project, before);
    std::fs::remove_dir_all(directory).unwrap();
}
