//! Editors for EMTR child sections documented by EffectResearch.
//!
//! These blocks are little-endian in real files (and in EffectLibraryRust's parser). The
//! research converter uses native little-endian too, despite an older prose note saying big.

use egui::Ui;

use crate::effects::{EmitterDef, EmitterSubsectionDef};

#[derive(Clone, Copy)]
enum Kind {
    U8,
    U32,
    F32,
}

#[derive(Clone, Copy)]
struct Field {
    offset: usize,
    kind: Kind,
    label: &'static str,
}

macro_rules! fields {
    ($($offset:expr, $kind:ident, $label:expr);* $(;)?) => {
        &[$(Field { offset: $offset, kind: Kind::$kind, label: $label }),*]
    };
}

fn documented_fields(magic: &str) -> &'static [Field] {
    match magic {
        "FMAG" => fields![
            0x00,U8,"follow emitter"; 0x01,U8,"affect X"; 0x02,U8,"affect Y"; 0x03,U8,"affect Z";
            0x04,F32,"magnetic force"; 0x08,F32,"position X"; 0x0c,F32,"position Y";
            0x10,F32,"position Z"; 0x14,F32,"animation enabled"; 0x18,F32,"animation loop";
            0x1c,F32,"random start"; 0x20,U32,"key count"; 0x24,U32,"loop count"
        ],
        "FCOV" => fields![
            0x00,U8,"convergence type"; 0x04,F32,"position X"; 0x08,F32,"position Y";
            0x0c,F32,"position Z"; 0x10,F32,"ratio"; 0x14,F32,"animation enabled";
            0x18,F32,"animation loop"; 0x1c,F32,"random start"; 0x20,U32,"key count";
            0x24,U32,"loop count"
        ],
        "FCOL" => fields![
            0x00,U8,"collision type"; 0x01,U8,"process in world"; 0x02,U8,"common plane";
            0x04,F32,"coordinates"; 0x08,F32,"bounce rate"; 0x0c,U32,"collision count";
            0x10,F32,"friction"
        ],
        "FCLN" => fields![
            0x00,U8,"interpolation"; 0x01,U8,"random noise offset"; 0x02,U8,"world coordinates";
            0x04,F32,"animation speed X"; 0x08,F32,"animation speed Y"; 0x0c,F32,"animation speed Z";
            0x14,F32,"influence X"; 0x18,F32,"influence Y"; 0x1c,F32,"influence Z";
            0x20,F32,"table scale"; 0x24,F32,"noise offset"
        ],
        "FSPN" => fields![
            0x00,F32,"rotation force"; 0x04,U32,"axis"; 0x08,F32,"outer velocity";
            0x0c,F32,"rotation animation enabled"; 0x10,F32,"rotation loop";
            0x14,F32,"rotation random start"; 0x18,U32,"rotation key count";
            0x1c,U32,"rotation loop count"; 0xa0,F32,"diffusion animation enabled";
            0xa4,F32,"diffusion loop"; 0xa8,F32,"diffusion random start";
            0xac,U32,"diffusion key count"; 0xb0,U32,"diffusion loop count"
        ],
        "FRND" => fields![
            0x00,U8,"fixed random seed"; 0x01,U8,"detailed options"; 0x02,U8,"air resistance";
            0x04,F32,"velocity X"; 0x08,F32,"velocity Y"; 0x0c,F32,"velocity Z";
            0x10,U32,"apply timing"; 0x14,F32,"phase speed"; 0x18,F32,"phase width";
            0x1c,F32,"wave weight 0"; 0x20,F32,"wave weight 1"; 0x24,F32,"wave weight 2";
            0x28,F32,"wave weight 3"; 0x2c,F32,"frequency ratio 0"; 0x30,F32,"frequency ratio 1";
            0x34,F32,"frequency ratio 2"; 0x38,F32,"frequency ratio 3";
            0x3c,F32,"animation enabled"; 0x40,F32,"animation loop"; 0x44,F32,"random start";
            0x48,U32,"key count"; 0x4c,U32,"loop count"
        ],
        "FRN1" => fields![
            0x00,F32,"random width X"; 0x04,F32,"random width Y"; 0x08,F32,"random width Z";
            0x0c,U32,"interval"; 0x10,F32,"animation enabled"; 0x14,F32,"animation loop";
            0x18,F32,"random start"; 0x1c,U32,"key count"; 0x20,U32,"loop count"
        ],
        "FPAD" => fields![
            0x00,U8,"world coordinates"; 0x04,F32,"position X"; 0x08,F32,"position Y";
            0x0c,F32,"position Z"; 0x10,F32,"animation enabled"; 0x14,F32,"animation loop";
            0x18,F32,"random start"; 0x1c,U32,"key count"; 0x20,U32,"loop count"
        ],
        "FCSF" => fields![
            0x00,U32,"flag"; 0x04,F32,"value 0"; 0x08,F32,"value 1"; 0x0c,F32,"value 2";
            0x10,F32,"value 3"; 0x14,F32,"value 4"; 0x18,F32,"value 5"; 0x1c,F32,"value 6";
            0x20,F32,"value 7"; 0x24,F32,"value 8"; 0x28,F32,"value 9"; 0x2c,F32,"value 10";
            0x30,F32,"value 11"; 0x34,F32,"value 12"; 0x38,F32,"value 13";
            0x3c,F32,"value 14"; 0x40,F32,"value 15"
        ],
        "EP01" => fields![
            0x00,U32,"calculation type"; 0x04,U32,"follow emitter"; 0x08,U32,"option";
            0x0c,U32,"texturing"; 0x10,U32,"divisions"; 0x14,U32,"connection type";
            0x18,F32,"head alpha"; 0x1c,F32,"tail alpha"; 0x20,F32,"history interpolation";
            0x24,F32,"direction interpolation"
        ],
        "EP02" => fields![
            0x00,U32,"calculation type"; 0x04,U32,"follow emitter"; 0x08,U32,"option";
            0x0c,U32,"texturing"; 0x10,F32,"partitions"; 0x14,F32,"histories";
            0x18,F32,"sample interval"; 0x1c,F32,"head alpha"; 0x20,F32,"tail alpha";
            0x24,F32,"history interpolation"; 0x28,F32,"direction interpolation"
        ],
        "EP03" => fields![
            0x00,U32,"calculation type"; 0x04,U32,"follow emitter"; 0x08,U32,"option";
            0x0c,U32,"UV0 texturing"; 0x10,U32,"UV1 texturing"; 0x14,U32,"UV2 texturing";
            0x18,F32,"history entries"; 0x1c,U32,"connection type"; 0x20,F32,"head alpha";
            0x24,F32,"tail alpha"; 0x28,U32,"partitions"; 0x2c,F32,"history interpolation";
            0x30,F32,"direction interpolation"; 0x34,F32,"air resistance";
            0x38,F32,"acceleration X"; 0x3c,F32,"acceleration Y"; 0x40,F32,"acceleration Z";
            0x58,U32,"UV mapping type"; 0x5c,F32,"starting scale"; 0x60,F32,"ending scale"
        ],
        "EP04" => fields![
            0x00,F32,"repeat offset X"; 0x04,F32,"repeat offset Y"; 0x08,F32,"repeat offset Z";
            0x0c,F32,"repeat count"; 0x10,F32,"size X"; 0x14,F32,"size Y"; 0x18,F32,"size Z";
            0x1c,F32,"clipping plane height"; 0x20,F32,"position X"; 0x24,F32,"position Y";
            0x28,F32,"position Z"; 0x2c,U32,"clipping type"; 0x30,F32,"edge fade X";
            0x34,F32,"edge fade Y"; 0x38,F32,"edge fade Z"; 0x3c,F32,"fix before camera";
            0x40,F32,"rotation X"; 0x44,F32,"rotation Y"; 0x48,F32,"rotation Z"
        ],
        _ => &[],
    }
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) -> bool {
    let Some(dst) = data.get_mut(offset..offset + 4) else {
        return false;
    };
    dst.copy_from_slice(&value.to_le_bytes());
    true
}

fn read_f32(data: &[u8], offset: usize) -> Option<f32> {
    read_u32(data, offset).map(f32::from_bits)
}

fn write_f32(data: &mut [u8], offset: usize, value: f32) -> bool {
    write_u32(data, offset, value.to_bits())
}

fn draw_field(ui: &mut Ui, section: &mut EmitterSubsectionDef, field: Field) -> bool {
    match field.kind {
        Kind::U8 => {
            let Some(value) = section.data.get_mut(field.offset) else {
                return false;
            };
            let mut v = u32::from(*value);
            let changed = ui
                .add(egui::DragValue::new(&mut v).range(0..=255))
                .changed();
            if changed {
                *value = v as u8;
            }
            changed
        }
        Kind::U32 => {
            let Some(mut value) = read_u32(&section.data, field.offset) else {
                return false;
            };
            let changed = ui.add(egui::DragValue::new(&mut value)).changed();
            changed && write_u32(&mut section.data, field.offset, value)
        }
        Kind::F32 => {
            let Some(mut value) = read_f32(&section.data, field.offset) else {
                return false;
            };
            let changed = ui
                .add(egui::DragValue::new(&mut value).speed(0.01))
                .changed();
            changed && value.is_finite() && write_f32(&mut section.data, field.offset, value)
        }
    }
}

fn draw_animation(ui: &mut Ui, section: &mut EmitterSubsectionDef) -> bool {
    if section.data.len() < 12 {
        return false;
    }
    let mut changed = false;
    egui::Grid::new(("ea_header", &section.magic))
        .num_columns(2)
        .show(ui, |ui| {
            for (offset, label) in [(0, "enabled"), (1, "loop"), (2, "random start")] {
                let mut on = section.data[offset] != 0;
                ui.label(label);
                if ui.checkbox(&mut on, "").changed() {
                    section.data[offset] = u8::from(on);
                    changed = true;
                }
                ui.end_row();
            }
            ui.label("loop count");
            let mut loops = read_u32(&section.data, 8).unwrap_or(0);
            if ui.add(egui::DragValue::new(&mut loops)).changed() {
                changed |= write_u32(&mut section.data, 8, loops);
            }
            ui.end_row();
        });
    let count = read_u32(&section.data, 4).unwrap_or(0) as usize;
    let available = section.data.len().saturating_sub(12) / 16;
    let count = count.min(available);
    ui.label(
        egui::RichText::new(format!("{count} keyframe(s)"))
            .small()
            .color(egui::Color32::GRAY),
    );
    egui::Grid::new(("ea_keys", &section.magic))
        .striped(true)
        .show(ui, |ui| {
            ui.label("key");
            ui.label("X");
            ui.label("Y");
            ui.label("Z");
            ui.label("frame");
            ui.end_row();
            for key in 0..count {
                ui.label(key.to_string());
                for component in 0..4 {
                    let offset = 12 + key * 16 + component * 4;
                    let mut value = read_f32(&section.data, offset).unwrap_or(0.0);
                    if ui
                        .add(egui::DragValue::new(&mut value).speed(0.01))
                        .changed()
                        && value.is_finite()
                    {
                        changed |= write_f32(&mut section.data, offset, value);
                    }
                }
                ui.end_row();
            }
        });
    changed
}

fn raw_editor(ui: &mut Ui, section: &mut EmitterSubsectionDef) -> bool {
    let mut changed = false;
    egui::Grid::new(("subsection_raw", &section.magic))
        .striped(true)
        .show(ui, |ui| {
            for row in 0..section.data.len().div_ceil(8) {
                ui.label(format!("{:04X}", row * 8));
                for offset in row * 8..((row + 1) * 8).min(section.data.len()) {
                    let mut value = u32::from(section.data[offset]);
                    if ui
                        .add(
                            egui::DragValue::new(&mut value)
                                .range(0..=255)
                                .hexadecimal(2, false, true),
                        )
                        .changed()
                    {
                        section.data[offset] = value as u8;
                        changed = true;
                    }
                }
                ui.end_row();
            }
        });
    changed
}

/// Draw every subsection on an emitter. Known layouts get named controls; every block retains
/// an advanced byte editor so newly discovered and intentionally opaque fields remain editable.
pub fn draw(ui: &mut Ui, emitter: &mut EmitterDef, pristine: &EmitterDef) -> bool {
    if emitter.subsections.is_empty() {
        return false;
    }
    let mut changed = false;
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new("Emitter animations & fields").strong());
    ui.label(
        egui::RichText::new("EMTR child sections; edits export and ride the live carrier.")
            .small()
            .color(egui::Color32::GRAY),
    );
    for index in 0..emitter.subsections.len() {
        let magic = emitter.subsections[index].magic.clone();
        let was_edited = pristine
            .subsections
            .get(index)
            .is_some_and(|p| p.magic == magic && p.data != emitter.subsections[index].data);
        let heading = if was_edited {
            format!("{magic} •")
        } else {
            magic.clone()
        };
        egui::CollapsingHeader::new(heading)
            .id_salt(("subsection", index))
            .default_open(was_edited)
            .show(ui, |ui| {
                let section = &mut emitter.subsections[index];
                if magic.starts_with("EA") {
                    changed |= draw_animation(ui, section);
                } else {
                    let fields = documented_fields(&magic);
                    if !fields.is_empty() {
                        egui::Grid::new(("subsection_fields", index))
                            .num_columns(2)
                            .striped(true)
                            .show(ui, |ui| {
                                for &field in fields {
                                    if field.offset < section.data.len() {
                                        ui.label(field.label);
                                        changed |= draw_field(ui, section, field);
                                        ui.end_row();
                                    }
                                }
                            });
                    } else {
                        ui.label(
                            egui::RichText::new("This layout is not yet named by EffectResearch.")
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    }
                }
                egui::CollapsingHeader::new("Advanced raw bytes")
                    .id_salt(("raw", index))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Change only values whose binary meaning you know.",
                            )
                            .small()
                            .color(egui::Color32::YELLOW),
                        );
                        changed |= raw_editor(ui, section);
                    });
                if was_edited && ui.small_button("Reset section").clicked() {
                    if let Some(original) =
                        pristine.subsections.get(index).filter(|p| p.magic == magic)
                    {
                        section.data = original.data.clone();
                        changed = true;
                    }
                }
            });
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ea_layout_is_little_endian_and_round_trips() {
        let mut data = vec![1, 0, 1, 0];
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&7u32.to_le_bytes());
        for value in [1.0f32, -2.5, 3.25, 9.0] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(read_u32(&data, 4), Some(1));
        assert_eq!(read_u32(&data, 8), Some(7));
        assert_eq!(read_f32(&data, 16), Some(-2.5));
        assert!(write_f32(&mut data, 16, 6.5));
        assert_eq!(read_f32(&data, 16), Some(6.5));
    }

    #[test]
    fn documented_rows_never_run_past_their_storage_width() {
        for magic in [
            "FMAG", "FCOV", "FCOL", "FCLN", "FSPN", "FRND", "FRN1", "FPAD", "FCSF", "EP01", "EP02",
            "EP03", "EP04",
        ] {
            for field in documented_fields(magic) {
                let width = match field.kind {
                    Kind::U8 => 1,
                    Kind::U32 | Kind::F32 => 4,
                };
                assert!(field.offset + width <= 0x100, "{magic} {}", field.label);
            }
        }
    }
}
