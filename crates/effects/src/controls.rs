//! Expression Controls: parameter-only effects whose values expressions read. They pass pixels
//! through unchanged.

use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;

use crate::{Buf, EffectCtx, EffectSpec, col, num, p, popup, slider};

/// Id of the Stereo 3D Controls effect (Layer ▸ Camera ▸ Create Stereo 3D Rig).
pub const STEREO_CONTROLS: &str = "ec.control.stereo3d";
/// Stereo 3D Controls ▸ Configuration: which eyes are offset from the master camera.
pub const STEREO_CONFIGURATIONS: [&str; 3] = ["Stereo Pair (Left & Right)", "Center & Right", "Center & Left"];
/// Stereo 3D Controls ▸ Convergence Of: the zero-parallax distance.
pub const STEREO_CONVERGENCE: [&str; 2] = ["Camera Point of Interest", "Camera Zoom Plane"];
/// Parameter-only control used by the original ordinary-text counter expression rig.
pub const TEXT_COUNTER: &str = "ec.control.textCounter";

fn passthrough(_: &EffectCtx, b: Buf) -> Buf {
    b
}

fn spec(id: &'static str, name: &'static str, params: Vec<crate::ParamSpec>) -> EffectSpec {
    EffectSpec { id, name, category: "Expression Controls", params, render: crate::EffectRender::Legacy(passthrough), gpu: true, float: true }
}

pub fn specs() -> Vec<EffectSpec> {
    vec![
        spec("ec.control.slider", "Slider Control", vec![p("slider", "Slider", num(0.0), slider(-1_000_000.0, 1_000_000.0, 0.0, 100.0, 2))]),
        spec("ec.control.angle", "Angle Control", vec![p("angle", "Angle", num(0.0), ParamUi::Angle)]),
        spec("ec.control.checkbox", "Checkbox Control", vec![p("checkbox", "Checkbox", Value::Bool(false), ParamUi::Checkbox)]),
        spec("ec.control.color", "Color Control", vec![p("color", "Color", col(1.0, 0.0, 0.0), ParamUi::Color)]),
        spec("ec.control.point", "Point Control", vec![p("point", "Point", Value::Vec2([0.5, 0.5]), ParamUi::Point)]),
        spec("ec.control.point3d", "3D Point Control", vec![p("point", "3D Point", Value::Vec3([0.0, 0.0, 0.0]), ParamUi::Point3)]),
        spec("ec.control.layer", "Layer Control", vec![p("layer", "Layer", Value::Layer(None), ParamUi::Layer)]),
        spec("ec.control.dropdown", "Dropdown Menu Control", vec![p("menu", "Menu", Value::Enum(0), popup(&["Item 1", "Item 2", "Item 3"]))]),
        spec(
            TEXT_COUNTER,
            "Native Text Counter",
            vec![
                p("value", "Value", num(0.0), slider(-1e15, 1e15, 0.0, 100.0, 4)),
                p("type", "Format", Value::Enum(0), popup(&["Number", "Time"])),
                p("decimals", "Decimal Places", num(0.0), slider(0.0, 10.0, 0.0, 6.0, 0)),
                p("padding", "Minimum Integer Digits", num(0.0), slider(0.0, 32.0, 0.0, 10.0, 0)),
                p("grouping", "Digit Grouping", Value::Enum(0), popup(&["None", "1,234.56", "1.234,56", "1 234.56"])),
                p("prefix", "Prefix", Value::Str(String::new()), ParamUi::Text),
                p("suffix", "Suffix", Value::Str(String::new()), ParamUi::Text),
                p("currentTime", "Use Layer Clock", Value::Bool(false), ParamUi::Checkbox),
                p("countDown", "Count Down", Value::Bool(false), ParamUi::Checkbox),
                p("clockRate", "Clock Rate", num(1.0), slider(-100.0, 100.0, -5.0, 5.0, 3)),
                p("clockStart", "Clock Start (layer seconds)", num(0.0), slider(-1_000_000.0, 1_000_000.0, 0.0, 10.0, 3)),
                p("timeStyle", "Time Style", Value::Enum(0), popup(&["HH:MM:SS", "MM:SS", "Seconds"])),
            ],
        ),
        // Layer ▸ Camera ▸ Create Stereo 3D Rig: the rig's eye cameras read these (expressions).
        spec(
            STEREO_CONTROLS,
            "Stereo 3D Controls",
            vec![
                p("configuration", "Configuration", Value::Enum(0), popup(&STEREO_CONFIGURATIONS)),
                p("sceneDepth", "Stereo Scene Depth", num(3.0), slider(0.0, 100.0, 0.0, 10.0, 2)),
                p("convergence", "Enable Convergence", Value::Bool(false), ParamUi::Checkbox),
                p("convergenceOf", "Convergence Of", Value::Enum(0), popup(&STEREO_CONVERGENCE)),
                p("zOffset", "Convergence Z Offset", num(0.0), slider(-100_000.0, 100_000.0, -2000.0, 2000.0, 1)),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, run_fx};
    use effectcraft_raster::Image;

    #[test]
    fn native_text_counter_is_a_parameter_only_passthrough() {
        let image = Image::filled(4, 3, [0.25, 0.125, 0.0, 0.5]);
        let out = run_fx(TEXT_COUNTER, &[("value", num(1234.5)), ("prefix", Value::Str("€".into()))], image.clone(), 0.0, EffectEnv::default());
        assert_eq!(out.img.data, image.data);
        assert_eq!(out.offset, [0.0, 0.0]);
        assert_eq!(out.scale, 1.0);
    }
}
