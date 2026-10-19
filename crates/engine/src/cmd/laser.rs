//! Parametric laser-cut boxes: assembled solids and deterministic 2D cutting plans.

use serde_json::{Value, json};
use solidcraft_geom::Vec2;
use solidcraft_io::Geom2;

use super::{CommandSpec, not_in_sketch};
use crate::params::{bad, num};
use crate::{Result, Session};

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new("laser.box", "Laser Box", laser_box)
        .at("SOLID", "CREATE")
        .enabled(not_in_sketch)
        .params("width, depth, height, thickness, kerf?: mm; fingers_x, fingers_y, fingers_z: odd integers 1..31; creates an open five-panel finger-jointed box"),
    CommandSpec::new("laser.nest", "Nest Laser Panels", nest_command)
        .noundo()
        .params("box dimensions plus stock_width, stock_height, margin?: 5, spacing?: 3; deterministic shelf nesting with 0/90 degree rotation"),
    CommandSpec::new("laser.export_dxf", "Export Laser DXF", export_dxf)
        .noundo()
        .params("same parameters as laser.nest; returns inline DXF in millimetres"),
    CommandSpec::new("laser.export_svg", "Export Laser SVG", export_svg)
        .noundo()
        .params("same parameters as laser.nest; returns inline SVG in millimetres with a CUT group"),
];

#[derive(Clone, Copy)]
struct BoxParams {
    width: f64,
    depth: f64,
    height: f64,
    thickness: f64,
    kerf: f64,
    fingers_x: usize,
    fingers_y: usize,
    fingers_z: usize,
}

#[derive(Clone)]
struct Panel {
    name: &'static str,
    outline: Vec<Vec2>,
}

#[derive(Clone, Copy)]
struct Placement {
    panel: usize,
    sheet: usize,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: u16,
}

fn positive(cmd: &str, p: &Value, key: &str) -> Result<f64> {
    let value = num(p, key).ok_or_else(|| bad(cmd, format!("`{key}` must be a finite number")))?;
    if value <= 0.0 || value > 1_000_000.0 {
        return Err(bad(cmd, format!("`{key}` must be positive and at most 1000000 mm")));
    }
    Ok(value)
}

fn non_negative(cmd: &str, p: &Value, key: &str, default: f64) -> Result<f64> {
    let value = match p.get(key) {
        None => default,
        Some(_) => num(p, key).ok_or_else(|| bad(cmd, format!("`{key}` must be a finite number")))?,
    };
    if !(0.0..=1_000_000.0).contains(&value) {
        return Err(bad(cmd, format!("`{key}` must be between 0 and 1000000 mm")));
    }
    Ok(value)
}

fn fingers(cmd: &str, p: &Value, key: &str) -> Result<usize> {
    let value = p.get(key).and_then(Value::as_u64).ok_or_else(|| bad(cmd, format!("`{key}` must be an odd integer from 1 to 31")))?;
    if value == 0 || value > 31 || value % 2 == 0 {
        return Err(bad(cmd, format!("`{key}` must be an odd integer from 1 to 31")));
    }
    usize::try_from(value).map_err(|_| bad(cmd, format!("`{key}` is out of range")))
}

fn box_params(cmd: &str, p: &Value) -> Result<BoxParams> {
    let out = BoxParams {
        width: positive(cmd, p, "width")?,
        depth: positive(cmd, p, "depth")?,
        height: positive(cmd, p, "height")?,
        thickness: positive(cmd, p, "thickness")?,
        kerf: non_negative(cmd, p, "kerf", 0.0)?,
        fingers_x: fingers(cmd, p, "fingers_x")?,
        fingers_y: fingers(cmd, p, "fingers_y")?,
        fingers_z: fingers(cmd, p, "fingers_z")?,
    };
    if out.thickness >= out.width.min(out.depth).min(out.height) {
        return Err(bad(cmd, "`thickness` must be smaller than width, depth and height"));
    }
    let shortest = (out.width / out.fingers_x as f64).min(out.depth / out.fingers_y as f64).min(out.height / out.fingers_z as f64);
    if out.kerf >= shortest {
        return Err(bad(cmd, "`kerf` must be smaller than every finger segment"));
    }
    Ok(out)
}

fn nested(s: &mut Session, id: &str, p: Value) -> Result<Value> {
    super::find_command(id).ok_or_else(|| bad("laser.box", format!("required command `{id}` is unavailable")))?.run_nested(s, &p)
}

fn add_box(s: &mut Session, corner: [f64; 3], size: [f64; 3], body: &str) -> Result<Value> {
    nested(s, "solid.box", json!({"corner": corner, "length": size[0], "width": size[1], "height": size[2], "body_name": body}))
}

fn alter_box(s: &mut Session, corner: [f64; 3], size: [f64; 3], body: &str, operation: &str) -> Result<Value> {
    nested(
        s,
        "solid.box",
        json!({"corner": corner, "length": size[0], "width": size[1], "height": size[2], "operation": operation, "targets": [body]}),
    )
}

fn active_spans(length: f64, count: usize) -> impl Iterator<Item = (f64, f64)> {
    let segment = length / count as f64;
    (0..count).step_by(2).map(move |i| (i as f64 * segment, segment))
}

fn laser_box(s: &mut Session, p: &Value) -> Result<Value> {
    let q = box_params("laser.box", p)?;
    let (w, d, h, t) = (q.width, q.depth, q.height, q.thickness);
    let names = ["Bottom", "Front", "Back", "Left", "Right"];
    if s.model.state().bodies.iter().any(|body| names.contains(&body.name.as_str())) {
        return Err(bad("laser.box", "remove or rename existing Bottom/Front/Back/Left/Right bodies first"));
    }

    add_box(s, [0.0, 0.0, 0.0], [w, d, t], "Bottom")?;
    add_box(s, [-t, -t, 0.0], [w + 2.0 * t, t, h], "Front")?;
    add_box(s, [-t, d, 0.0], [w + 2.0 * t, t, h], "Back")?;
    add_box(s, [-t, 0.0, 0.0], [t, d, h], "Left")?;
    add_box(s, [w, 0.0, 0.0], [t, d, h], "Right")?;

    // Bottom-to-wall fingers. Each tab is joined to Bottom and the identical volume is removed
    // from its wall: the assembled model therefore proves that the complementary joint fits.
    for (start, length) in active_spans(w, q.fingers_x) {
        for (y, wall) in [(-t, "Front"), (d, "Back")] {
            alter_box(s, [start, y, 0.0], [length, t, t], "Bottom", "join")?;
            alter_box(s, [start, y, 0.0], [length, t, t], wall, "cut")?;
        }
    }
    for (start, length) in active_spans(d, q.fingers_y) {
        for (x, wall) in [(-t, "Left"), (w, "Right")] {
            alter_box(s, [x, start, 0.0], [t, length, t], "Bottom", "join")?;
            alter_box(s, [x, start, 0.0], [t, length, t], wall, "cut")?;
        }
    }

    // The side walls carry the vertical tabs; front and back contain their matching slots.
    for (start, length) in active_spans(h, q.fingers_z) {
        for (x, side) in [(-t, "Left"), (w, "Right")] {
            for (y, wall) in [(-t, "Front"), (d, "Back")] {
                alter_box(s, [x, y, start], [t, t, length], side, "join")?;
                alter_box(s, [x, y, start], [t, t, length], wall, "cut")?;
            }
        }
    }

    let panels = panel_outlines(q);
    Ok(json!({
        "bodies": ["Bottom", "Front", "Back", "Left", "Right"],
        "panels": panels.iter().map(panel_json).collect::<Vec<_>>(),
        "width": w, "depth": d, "height": h, "thickness": t, "kerf": q.kerf
    }))
}

fn edge(
    out: &mut Vec<Vec2>,
    start: Vec2,
    direction: Vec2,
    outward: Vec2,
    length: f64,
    span_start: f64,
    span_length: f64,
    count: usize,
    depth: f64,
    kerf: f64,
) {
    out.push(start);
    let segment = span_length / count as f64;
    for i in (0..count).step_by(2) {
        // Laser centerlines: male tabs expand, female slots contract by half the beam.
        let compensation = if depth > 0.0 { -kerf / 2.0 } else { kerf / 2.0 };
        let raw_a = span_start + i as f64 * segment;
        let raw_b = span_start + (i + 1) as f64 * segment;
        let a = if i == 0 { raw_a } else { raw_a + compensation };
        let b = if i + 1 == count { raw_b } else { raw_b - compensation };
        if b <= a {
            continue;
        }
        let pa = start + direction * a;
        let pb = start + direction * b;
        out.push(pa);
        out.push(pa + outward * depth);
        out.push(pb + outward * depth);
        out.push(pb);
    }
    out.push(start + direction * length);
}

fn outline(
    width: f64,
    height: f64,
    bottom: Option<(f64, f64, usize, f64, f64)>,
    right: Option<(f64, f64, usize, f64, f64)>,
    top: Option<(f64, f64, usize, f64, f64)>,
    left: Option<(f64, f64, usize, f64, f64)>,
) -> Vec<Vec2> {
    let mut p = Vec::new();
    let b = bottom.unwrap_or((0.0, width, 1, 0.0, 0.0));
    edge(&mut p, Vec2::ZERO, Vec2::X, -Vec2::Y, width, b.0, b.1, b.2, b.3, b.4);
    let r = right.unwrap_or((0.0, height, 1, 0.0, 0.0));
    edge(&mut p, Vec2::new(width, 0.0), Vec2::Y, Vec2::X, height, r.0, r.1, r.2, r.3, r.4);
    let t = top.unwrap_or((0.0, width, 1, 0.0, 0.0));
    edge(&mut p, Vec2::new(width, height), -Vec2::X, Vec2::Y, width, t.0, t.1, t.2, t.3, t.4);
    let l = left.unwrap_or((0.0, height, 1, 0.0, 0.0));
    edge(&mut p, Vec2::new(0.0, height), -Vec2::Y, -Vec2::X, height, l.0, l.1, l.2, l.3, l.4);
    p.dedup_by(|a, b| a.dist(*b) < 1e-9);
    p
}

fn panel_outlines(q: BoxParams) -> Vec<Panel> {
    let (w, d, h, t, k) = (q.width, q.depth, q.height, q.thickness, q.kerf);
    let bottom = outline(
        w,
        d,
        Some((0.0, w, q.fingers_x, t, k)),
        Some((0.0, d, q.fingers_y, t, k)),
        Some((0.0, w, q.fingers_x, t, k)),
        Some((0.0, d, q.fingers_y, t, k)),
    );
    let front =
        outline(w + 2.0 * t, h, Some((t, w, q.fingers_x, -t, k)), Some((0.0, h, q.fingers_z, -t, k)), None, Some((0.0, h, q.fingers_z, -t, k)));
    let side = outline(d, h, Some((0.0, d, q.fingers_y, -t, k)), Some((0.0, h, q.fingers_z, t, k)), None, Some((0.0, h, q.fingers_z, t, k)));
    vec![
        Panel { name: "Bottom", outline: bottom },
        Panel { name: "Front", outline: front.clone() },
        Panel { name: "Back", outline: front },
        Panel { name: "Left", outline: side.clone() },
        Panel { name: "Right", outline: side },
    ]
}

fn bounds(points: &[Vec2]) -> Result<(Vec2, Vec2)> {
    let first = points.first().copied().ok_or_else(|| bad("laser.nest", "a panel has no outline"))?;
    let mut lo = first;
    let mut hi = first;
    for p in points.iter().copied() {
        lo.x = lo.x.min(p.x);
        lo.y = lo.y.min(p.y);
        hi.x = hi.x.max(p.x);
        hi.y = hi.y.max(p.y);
    }
    Ok((lo, hi))
}

fn panel_json(panel: &Panel) -> Value {
    json!({"name": panel.name, "outline": panel.outline.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>()})
}

fn nest(p: &Value, cmd: &str) -> Result<(Vec<Panel>, Vec<Placement>, f64, f64)> {
    #[derive(Clone, Copy)]
    struct Row {
        x: f64,
        y: f64,
        height: f64,
    }

    let q = box_params(cmd, p)?;
    let stock_width = positive(cmd, p, "stock_width")?;
    let stock_height = positive(cmd, p, "stock_height")?;
    let margin = non_negative(cmd, p, "margin", 5.0)?;
    let spacing = non_negative(cmd, p, "spacing", 3.0)?;
    if margin * 2.0 >= stock_width || margin * 2.0 >= stock_height {
        return Err(bad(cmd, "`margin` leaves no usable stock area"));
    }
    let panels = panel_outlines(q);
    let mut placements = Vec::new();
    let mut sheets: Vec<Vec<Row>> = Vec::new();

    for (panel, item) in panels.iter().enumerate() {
        let (lo, hi) = bounds(&item.outline)?;
        let natural = (hi.x - lo.x, hi.y - lo.y);
        let orientations = [(0, natural.0, natural.1), (90, natural.1, natural.0)];
        let mut placed = false;

        // ponytail: first-fit shelves are deterministic and sufficient for five panels; replace
        // with irregular nesting only when real jobs show that rectangular shelves waste stock.
        'existing: for (sheet, rows) in sheets.iter_mut().enumerate() {
            for row in rows {
                for (rotation, width, height) in orientations {
                    if height <= row.height && row.x + width <= stock_width - margin {
                        placements.push(Placement { panel, sheet, x: row.x, y: row.y, width, height, rotation });
                        row.x += width + spacing;
                        placed = true;
                        break 'existing;
                    }
                }
            }
        }
        if placed {
            continue;
        }

        'new_row: for (sheet, rows) in sheets.iter_mut().enumerate() {
            let y = rows.last().map_or(margin, |row| row.y + row.height + spacing);
            for (rotation, width, height) in orientations {
                if width <= stock_width - 2.0 * margin && y + height <= stock_height - margin {
                    placements.push(Placement { panel, sheet, x: margin, y, width, height, rotation });
                    rows.push(Row { x: margin + width + spacing, y, height });
                    placed = true;
                    break 'new_row;
                }
            }
        }
        if placed {
            continue;
        }

        let (rotation, width, height) = orientations
            .into_iter()
            .find(|(_, width, height)| *width <= stock_width - 2.0 * margin && *height <= stock_height - 2.0 * margin)
            .ok_or_else(|| bad(cmd, format!("panel `{}` does not fit on the stock", item.name)))?;
        let sheet = sheets.len();
        placements.push(Placement { panel, sheet, x: margin, y: margin, width, height, rotation });
        sheets.push(vec![Row { x: margin + width + spacing, y: margin, height }]);
    }
    Ok((panels, placements, stock_width, stock_height))
}

fn nest_json(panels: &[Panel], placements: &[Placement], stock_width: f64, stock_height: f64) -> Value {
    let sheets = placements.iter().map(|p| p.sheet).max().map_or(0, |n| n + 1);
    json!({
        "stock_width": stock_width,
        "stock_height": stock_height,
        "sheets": sheets,
        "placements": placements.iter().filter_map(|p| panels.get(p.panel).map(|panel| json!({
            "panel": panel.name, "sheet": p.sheet, "x": p.x, "y": p.y,
            "width": p.width, "height": p.height, "rotation": p.rotation
        }))).collect::<Vec<_>>()
    })
}

fn nest_command(_s: &mut Session, p: &Value) -> Result<Value> {
    let (panels, placements, w, h) = nest(p, "laser.nest")?;
    Ok(nest_json(&panels, &placements, w, h))
}

fn placed_points(panel: &Panel, placement: Placement) -> Result<Vec<Vec2>> {
    let (lo, hi) = bounds(&panel.outline)?;
    let height = hi.y - lo.y;
    Ok(panel
        .outline
        .iter()
        .map(|point| {
            let p = *point - lo;
            if placement.rotation == 90 {
                Vec2::new(placement.x + height - p.y, placement.y + p.x)
            } else {
                Vec2::new(placement.x + p.x, placement.y + p.y)
            }
        })
        .collect())
}

fn cutting_lines(panels: &[Panel], placements: &[Placement], stock_width: f64) -> Result<Vec<Geom2>> {
    let mut out = Vec::new();
    for placement in placements {
        let panel = panels.get(placement.panel).ok_or_else(|| bad("laser.export_dxf", "invalid panel placement"))?;
        let mut points = placed_points(panel, *placement)?;
        let shift = placement.sheet as f64 * stock_width;
        for point in &mut points {
            point.x += shift;
        }
        for pair in points.windows(2) {
            if let (Some(a), Some(b)) = (pair.first(), pair.get(1)) {
                out.push(Geom2::Line(*a, *b));
            }
        }
        if let (Some(last), Some(first)) = (points.last(), points.first()) {
            out.push(Geom2::Line(*last, *first));
        }
    }
    Ok(out)
}

fn export_dxf(_s: &mut Session, p: &Value) -> Result<Value> {
    let (panels, placements, stock_width, stock_height) = nest(p, "laser.export_dxf")?;
    let dxf = solidcraft_io::write_dxf(&cutting_lines(&panels, &placements, stock_width)?);
    Ok(json!({"dxf": dxf, "layout": nest_json(&panels, &placements, stock_width, stock_height)}))
}

fn export_svg(_s: &mut Session, p: &Value) -> Result<Value> {
    let (panels, placements, stock_width, stock_height) = nest(p, "laser.export_svg")?;
    let sheets = placements.iter().map(|placement| placement.sheet).max().map_or(0, |n| n + 1);
    let total_width = stock_width * sheets as f64;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_width}mm\" height=\"{stock_height}mm\" viewBox=\"0 0 {total_width} {stock_height}\"><g id=\"CUT\" fill=\"none\" stroke=\"#ff0000\" stroke-width=\"0.1\">"
    );
    for placement in &placements {
        let panel = panels.get(placement.panel).ok_or_else(|| bad("laser.export_svg", "invalid panel placement"))?;
        let mut points = placed_points(panel, *placement)?;
        let shift = placement.sheet as f64 * stock_width;
        for point in &mut points {
            point.x += shift;
        }
        svg.push_str("<path d=\"");
        for (i, point) in points.iter().enumerate() {
            svg.push_str(if i == 0 { "M " } else { " L " });
            svg.push_str(&format!("{:.6} {:.6}", point.x, stock_height - point.y));
        }
        svg.push_str(" Z\"/>");
    }
    svg.push_str("</g></svg>");
    Ok(json!({"svg": svg, "layout": nest_json(&panels, &placements, stock_width, stock_height)}))
}
