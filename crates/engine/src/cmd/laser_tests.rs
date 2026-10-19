use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::find_command;
use crate::Session;

fn params() -> Value {
    json!({
        "width": 120.0,
        "depth": 80.0,
        "height": 60.0,
        "thickness": 3.0,
        "kerf": 0.15,
        "fingers_x": 3,
        "fingers_y": 3,
        "fingers_z": 3,
        "stock_width": 600.0,
        "stock_height": 400.0
    })
}

fn run(s: &mut Session, id: &str, p: &Value) -> Value {
    match s.execute(id, p) {
        Ok(value) => value,
        Err(error) => panic!("{id}: {error}"),
    }
}

#[test]
fn registry_exposes_laser_commands() {
    for id in ["laser.box", "laser.nest", "laser.export_dxf", "laser.export_svg"] {
        assert!(find_command(id).is_some(), "missing {id}");
    }
}

#[test]
fn laser_box_makes_five_named_jointed_panels() {
    let mut s = Session::default();
    let made = run(&mut s, "laser.box", &params());
    let state = s.model.state();
    assert_eq!(state.bodies.len(), 5);
    let names: BTreeSet<&str> = state.bodies.iter().map(|body| body.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["Back", "Bottom", "Front", "Left", "Right"]));
    assert_eq!(made["panels"].as_array().map_or(0, Vec::len), 5);
    for panel in made["panels"].as_array().into_iter().flatten() {
        assert!(panel["outline"].as_array().map_or(0, Vec::len) > 8);
    }

    let panels = made["panels"].as_array().cloned().unwrap_or_default();
    let xs = |name: &str| {
        panels
            .iter()
            .find(|panel| panel["name"] == name)
            .and_then(|panel| panel["outline"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|point| point.get(0).and_then(Value::as_f64))
            .collect::<Vec<_>>()
    };
    assert!(xs("Bottom").iter().any(|x| (*x - 40.075).abs() < 1e-9));
    assert!(xs("Front").iter().any(|x| (*x - 42.925).abs() < 1e-9));
}

#[test]
fn laser_box_rejects_reserved_body_names_without_mutation() {
    let mut s = Session::default();
    run(&mut s, "solid.box", &json!({"length": 10, "width": 10, "height": 10, "body_name": "Bottom"}));
    let command = find_command("laser.box").expect("laser.box");
    assert!((command.run)(&mut s, &params()).is_err());
    let state = s.model.state();
    assert_eq!(state.bodies.len(), 1);
    assert_eq!(state.bodies[0].name, "Bottom");
}

#[test]
fn hostile_laser_params_are_errors() {
    for patch in [
        json!({"width": -1}),
        json!({"thickness": 1000}),
        json!({"kerf": 1000}),
        json!({"fingers_x": 2}),
        json!({"fingers_y": 33}),
        json!({"height": "NaN"}),
    ] {
        let mut p = params();
        if let (Some(dst), Some(src)) = (p.as_object_mut(), patch.as_object()) {
            dst.extend(src.clone());
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Session::default().execute("laser.box", &p)));
        assert!(result.is_ok(), "laser.box panicked for {patch}");
        assert!(result.ok().is_some_and(|value| value.is_err()), "laser.box accepted {patch}");
    }
}

#[test]
fn nesting_and_exports_are_deterministic_and_valid() {
    let mut s = Session::default();
    let a = run(&mut s, "laser.nest", &params());
    let b = run(&mut s, "laser.nest", &params());
    assert_eq!(a, b);
    let placements = a["placements"].as_array().cloned().unwrap_or_default();
    assert_eq!(placements.len(), 5);
    for (i, p) in placements.iter().enumerate() {
        let (sheet, x, y, w, h) = (p["sheet"].as_u64(), p["x"].as_f64(), p["y"].as_f64(), p["width"].as_f64(), p["height"].as_f64());
        let (Some(sheet), Some(x), Some(y), Some(w), Some(h)) = (sheet, x, y, w, h) else { panic!("bad placement {p}") };
        assert!(x >= 0.0 && y >= 0.0 && x + w <= 600.0 && y + h <= 400.0);
        for q in placements.iter().skip(i + 1).filter(|q| q["sheet"].as_u64() == Some(sheet)) {
            let (Some(qx), Some(qy), Some(qw), Some(qh)) = (q["x"].as_f64(), q["y"].as_f64(), q["width"].as_f64(), q["height"].as_f64()) else {
                panic!("bad placement {q}")
            };
            assert!(x + w <= qx || qx + qw <= x || y + h <= qy || qy + qh <= y, "overlap: {p} {q}");
        }
    }

    let dxf = run(&mut s, "laser.export_dxf", &params())["dxf"].as_str().unwrap_or_default().to_string();
    assert!(dxf.contains("SECTION") && dxf.contains("ENTITIES") && dxf.ends_with("EOF\n"));
    let svg = run(&mut s, "laser.export_svg", &params())["svg"].as_str().unwrap_or_default().to_string();
    assert!(svg.contains("<svg") && svg.contains("viewBox=") && svg.contains("id=\"CUT\""));

    let mut small = params();
    small["stock_width"] = json!(150.0);
    small["stock_height"] = json!(100.0);
    let small_layout = run(&mut s, "laser.nest", &small);
    assert!(small_layout["sheets"].as_u64().is_some_and(|sheets| sheets > 1));
    let small_dxf = run(&mut s, "laser.export_dxf", &small)["dxf"].as_str().unwrap_or_default().to_string();
    let max_x = solidcraft_io::read_dxf(&small_dxf)
        .unwrap_or_default()
        .iter()
        .filter_map(|shape| match shape {
            solidcraft_io::Geom2::Line(a, b) => Some(a.x.max(b.x)),
            _ => None,
        })
        .fold(0.0_f64, f64::max);
    assert!(max_x > 150.0, "multiple DXF sheets must not overlap at the origin");
}
