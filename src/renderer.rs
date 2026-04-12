/// ssbh_wgpu rendering integration for the hitbox editor.
/// Uses egui-wgpu paint callbacks to render directly into the egui surface.

use std::path::Path;
use glam::{Mat4, Vec3, Vec4};
use ssbh_wgpu::{
    CameraTransforms, ModelFolder, ModelRenderOptions, RenderModel, RenderSettings,
    SharedRenderData, SsbhRenderer,
};

pub struct Camera {
    pub translation: Vec3,
    pub rotation: Vec3,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            translation: Vec3::new(0.0, -8.0, -60.0),
            rotation: Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0),
            fov_y: 30f32.to_radians(),
            near: 1.0,
            far: 400_000.0,
        }
    }
}

impl Camera {
    /// Pan in the camera plane (left/right and up/down) while keeping rotation fixed.
    /// Moves in world X (left/right) and world Y (up/down) regardless of camera rotation.
    pub fn pan(&mut self, delta_x: f32, delta_y: f32) {
        let speed = self.translation.z.abs() * 0.001;
        self.translation.x -= delta_x * speed;
        self.translation.y += delta_y * speed;
    }

    /// Zoom: move along Z axis.
    pub fn zoom(&mut self, delta: f32) {
        let speed = self.translation.z.abs() * 0.1;
        self.translation.z += delta * speed;
        self.translation.z = self.translation.z.min(-1.0);
    }

    pub fn transforms(&self, width: f32, height: f32) -> CameraTransforms {
        let aspect = if height > 0.0 { width / height } else { 1.0 };
        let rotation = Mat4::from_euler(
            glam::EulerRot::XYZ,
            self.rotation.x,
            self.rotation.y,
            self.rotation.z,
        );
        let model_view = Mat4::from_translation(self.translation) * rotation;
        let projection = Mat4::perspective_rh(self.fov_y, aspect, self.near, self.far);
        let mvp = projection * model_view;
        CameraTransforms {
            model_view_matrix: model_view,
            mvp_matrix: mvp,
            projection_matrix: projection,
            mvp_inv_matrix: mvp.inverse(),
            camera_pos: model_view.inverse().col(3),
            screen_dimensions: Vec4::new(width, height, 1.0, 0.0),
        }
    }
}

/// All wgpu rendering resources stored in egui's callback_resources.
pub struct HitboxRenderState {
    pub renderer: SsbhRenderer,
    pub shared_data: SharedRenderData,
    pub render_models: Vec<RenderModel>,
    pub render_settings: RenderSettings,
    pub model_render_options: ModelRenderOptions,
    pub camera: Camera,
    pub current_width: u32,
    pub current_height: u32,
    /// Cached animation data — reloaded only when the path changes
    cached_anim: Option<(std::path::PathBuf, ssbh_data::anim_data::AnimData)>,
    /// Cached skeleton data — reloaded only when the path changes
    cached_skel: Option<(std::path::PathBuf, ssbh_data::skel_data::SkelData)>,
    /// Weapon skeletons: (weapon_name, skel, attach_bone_name)
    /// attach_bone_name is the character bone the weapon root attaches to (e.g. "haver")
    weapon_skels: Vec<(String, ssbh_data::skel_data::SkelData, String)>,
    /// Track last rendered state to skip redundant GPU work
    last_frame: f32,
    last_anim_path: Option<std::path::PathBuf>,
    last_skel_path: Option<std::path::PathBuf>,
    /// Particle + trail renderer
    pub particle_renderer: Option<crate::particle_renderer::ParticleRenderer>,
    /// Particle data to render this frame (set by update loop before prepare)
    pub pending_particles: Vec<crate::effects::Particle>,
    pub pending_trails: Vec<crate::effects::SwordTrail>,
    /// Offscreen texture for particle compositing (same size as viewport)
    particle_target: Option<(wgpu::Texture, wgpu::TextureView)>,
    particle_target_size: (u32, u32),
    pub surface_format: wgpu::TextureFormat,
}

impl HitboxRenderState {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let mut renderer = SsbhRenderer::new(
            device,
            queue,
            1,
            1,
            1.0,
            [0.05, 0.05, 0.1, 1.0],
            surface_format,
        );
        let shared_data = SharedRenderData::new(device, queue);
        // Use Shaded mode for full PBR rendering with textures
        let render_settings = RenderSettings {
            render_bloom: false,
            render_shadows: false,
            ..RenderSettings::default()
        };
        renderer.update_render_settings(queue, &render_settings);
        Self {
            renderer,
            shared_data,
            render_models: Vec::new(),
            render_settings,
            model_render_options: ModelRenderOptions::default(),
            camera: Camera::default(),
            current_width: 0,
            current_height: 0,
            cached_anim: None,
            cached_skel: None,
            weapon_skels: Vec::new(),
            last_frame: -1.0,
            last_anim_path: None,
            last_skel_path: None,
            particle_renderer: Some(crate::particle_renderer::ParticleRenderer::new(device, queue, surface_format)),
            pending_particles: Vec::new(),
            pending_trails: Vec::new(),
            particle_target: None,
            particle_target_size: (0, 0),
            surface_format,
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 { return; }
        if width == self.current_width && height == self.current_height { return; }
        self.renderer.resize(device, width, height, 1.0);
        self.current_width = width;
        self.current_height = height;
        // Recreate particle offscreen target
        if self.particle_target_size != (width, height) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("particle_target"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.particle_target = Some((tex, view));
            self.particle_target_size = (width, height);
        }
    }

    /// Returns (view_proj, cam_right, cam_up) for particle rendering.
    pub fn camera_vectors(&self) -> (Mat4, Vec3, Vec3) {
        let transforms = self.camera.transforms(self.current_width as f32, self.current_height as f32);
        let mv_inv = transforms.model_view_matrix.inverse();
        let cam_right = mv_inv.col(0).truncate().normalize();
        let cam_up    = mv_inv.col(1).truncate().normalize();
        (transforms.mvp_matrix, cam_right, cam_up)
    }

    pub fn load_model(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, model_dir: &Path) {
        self.render_models.clear();
        self.weapon_skels.clear();
        let folder = ModelFolder::load_folder(model_dir);
        let render_model = RenderModel::from_folder(device, queue, &folder, &self.shared_data);
        self.render_models.push(render_model);

        // Scan sibling directories for weapon skeletons.
        // model_dir is e.g. fighter/link/model/body/c00
        // Weapon dirs are e.g. fighter/link/model/sword/c00
        if let Some(model_root) = model_dir.parent().and_then(|p| p.parent()) {
            if let Ok(entries) = std::fs::read_dir(model_root) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name();
                    let dir_name = dir_name.to_string_lossy();
                    if dir_name == "body" { continue; }
                    let weapon_skel = entry.path().join("c00").join("model.nusktb");
                    if !weapon_skel.exists() { continue; }
                    if let Ok(skel) = ssbh_data::skel_data::SkelData::from_file(&weapon_skel) {
                        // Determine the attach bone: prefer "haver" (right hand), then "havel" (left hand)
                        // This is the character body bone the weapon root is parented to at runtime.
                        let attach = weapon_attach_bone(&dir_name);
                        self.weapon_skels.push((dir_name.to_string(), skel, attach));
                    }
                }
            }
        }

        // Force re-skin on next frame
        self.last_frame = -1.0;
        self.last_anim_path = None;
        self.last_skel_path = None;
    }

    pub fn apply_animation(
        &mut self,
        queue: &wgpu::Queue,
        anim_path: Option<&Path>,
        skel_path: Option<&Path>,
        frame: f32,
    ) {
        // Reload anim only when path changes
        if let Some(path) = anim_path {
            let needs_load = self.cached_anim.as_ref()
                .map(|(p, _)| p != path)
                .unwrap_or(true);
            if needs_load {
                self.cached_anim = ssbh_data::anim_data::AnimData::from_file(path)
                    .ok()
                    .map(|a| (path.to_path_buf(), a));
            }
        } else {
            self.cached_anim = None;
        }

        // Reload skel only when path changes
        if let Some(path) = skel_path {
            let needs_load = self.cached_skel.as_ref()
                .map(|(p, _)| p != path)
                .unwrap_or(true);
            if needs_load {
                self.cached_skel = ssbh_data::skel_data::SkelData::from_file(path)
                    .ok()
                    .map(|s| (path.to_path_buf(), s));
            }
        } else {
            self.cached_skel = None;
        }

        let anim = self.cached_anim.as_ref().map(|(_, a)| a);
        let skel = self.cached_skel.as_ref().map(|(_, s)| s);

        for render_model in &mut self.render_models {
            render_model.apply_anims(
                queue,
                anim.into_iter(),
                skel,
                None,
                None,
                &self.shared_data,
                frame,
            );
        }
    }

    pub fn update_camera(&mut self, queue: &wgpu::Queue, width: f32, height: f32) {
        let transforms = self.camera.transforms(width, height);
        self.renderer.update_camera(queue, transforms);
    }

    /// Returns all bone names from the cached skeleton.
    pub fn bone_names(&self) -> Vec<String> {
        self.cached_skel.as_ref()
            .map(|(_, s)| s.bones.iter().map(|b| b.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn weapon_skel_count(&self) -> usize {
        self.weapon_skels.len()
    }

    /// Returns a map of bone name -> world matrix for the current frame.
    /// Includes both body skeleton bones and weapon skeleton bones.
    /// The offset from ACMD should be transformed by this matrix (not just added).
    pub fn bone_world_matrices(&self) -> std::collections::HashMap<String, glam::Mat4> {
            let mut result = std::collections::HashMap::new();
            let skel = match self.cached_skel.as_ref() {
                Some((_, s)) => s,
                None => return result,
            };
            let anim = self.cached_anim.as_ref().map(|(_, a)| a);
            let frame = self.last_frame.max(0.0);
            let bone_count = skel.bones.len();

            struct BoneState {
                translation: glam::Vec3,
                rotation: glam::Quat,
                scale: glam::Vec3,
                compensate_scale: bool,
            }

            let mut states: Vec<BoneState> = skel.bones.iter().map(|b| {
                let m = glam::Mat4::from_cols_array_2d(&b.transform);
                let (scale, rotation, translation) = m.to_scale_rotation_translation();
                BoneState { translation, rotation, scale, compensate_scale: false }
            }).collect();

            if let Some(anim_data) = anim {
                for group in &anim_data.groups {
                    use ssbh_data::anim_data::GroupType;
                    if group.group_type != GroupType::Transform { continue; }
                    for node in &group.nodes {
                        let Some(idx) = skel.bones.iter().position(|b| b.name == node.name) else { continue };
                        let Some(track) = node.tracks.first() else { continue };
                        use ssbh_data::anim_data::TrackValues;
                        if let TrackValues::Transform(values) = &track.values {
                            if values.is_empty() { continue; }
                            let cur = (frame.floor() as usize).clamp(0, values.len() - 1);
                            let nxt = (frame.ceil()  as usize).clamp(0, values.len() - 1);
                            let f   = frame.fract();
                            let a = &values[cur]; let b = &values[nxt];
                            states[idx] = BoneState {
                                translation: glam::Vec3::from(a.translation.to_array()).lerp(glam::Vec3::from(b.translation.to_array()), f),
                                rotation: glam::Quat::from_array(a.rotation.to_array()).slerp(glam::Quat::from_array(b.rotation.to_array()), f),
                                scale: glam::Vec3::from(a.scale.to_array()).lerp(glam::Vec3::from(b.scale.to_array()), f),
                                compensate_scale: track.compensate_scale,
                            };
                        }
                    }
                }
            }

            let mut world: Vec<glam::Mat4> = vec![glam::Mat4::IDENTITY; bone_count];
            for (i, bone) in skel.bones.iter().enumerate() {
                let st = &states[i];
                let comp = if st.compensate_scale {
                    bone.parent_index.map(|p| glam::Vec3::ONE / states[p].scale).unwrap_or(glam::Vec3::ONE)
                } else { glam::Vec3::ONE };
                let local = glam::Mat4::from_translation(st.translation)
                    * glam::Mat4::from_scale(comp)
                    * glam::Mat4::from_quat(st.rotation)
                    * glam::Mat4::from_scale(st.scale);
                let parent = bone.parent_index.map(|p| world[p]).unwrap_or(glam::Mat4::IDENTITY);
                world[i] = parent * local;
                result.insert(bone.name.clone(), world[i]);
                result.insert(bone.name.to_lowercase(), world[i]);
            }

            // `top` = character root (feet), identity matrix
            result.entry("top".to_string()).or_insert(glam::Mat4::IDENTITY);
            result.entry("Top".to_string()).or_insert(glam::Mat4::IDENTITY);

            // ── Weapon skeletons ──────────────────────────────────────────────────
            for (_, weapon_skel, attach_bone) in &self.weapon_skels {
                let attach_world = skel.bones.iter().enumerate()
                    .find(|(_, b)| b.name.eq_ignore_ascii_case(attach_bone))
                    .map(|(i, _)| world[i])
                    .unwrap_or(glam::Mat4::IDENTITY);

                for bone in &weapon_skel.bones {
                    let Ok(bind_world) = weapon_skel.calculate_world_transform(bone) else { continue };
                    let bind_mat = glam::Mat4::from_cols_array_2d(&bind_world);
                    let final_mat = attach_world * bind_mat;
                    // Only insert if this bone name isn't already in the body skeleton —
                    // body skeleton bones always take priority over weapon skeleton bones.
                    result.entry(bone.name.clone()).or_insert(final_mat);
                    result.entry(bone.name.to_lowercase()).or_insert(final_mat);
                }
            }

            result
        }

    /// Returns a map of bone name -> world position (convenience wrapper).
    pub fn bone_world_positions(&self) -> std::collections::HashMap<String, glam::Vec3> {
        self.bone_world_matrices()
            .into_iter()
            .map(|(k, m)| (k, m.col(3).truncate()))
            .collect()
    }

    /// Projects a 3D world position to normalized device coordinates (NDC),
    /// then to pixel coordinates within the given viewport rect.
    pub fn world_to_screen(&self, world_pos: glam::Vec3, viewport: egui::Rect) -> Option<egui::Pos2> {
        let transforms = self.camera.transforms(viewport.width(), viewport.height());
        let clip = transforms.mvp_matrix * glam::Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
        if clip.w <= 0.0 { return None; }
        let ndc = clip.truncate() / clip.w;
        // Allow a small margin outside NDC so hitboxes near viewport edges still show
        if ndc.x < -1.5 || ndc.x > 1.5 || ndc.y < -1.5 || ndc.y > 1.5 { return None; }
        let sx = (ndc.x * 0.5 + 0.5) * viewport.width() + viewport.left();
        let sy = (-ndc.y * 0.5 + 0.5) * viewport.height() + viewport.top();
        Some(egui::pos2(sx, sy))
    }

    /// Computes the screen-space radius for a sphere of `world_radius` centered at `world_pos`.
    /// Uses the camera-right vector so it's correct regardless of camera orientation.
    pub fn world_radius_to_screen(&self, world_pos: glam::Vec3, world_radius: f32, viewport: egui::Rect) -> Option<f32> {
        let transforms = self.camera.transforms(viewport.width(), viewport.height());
        let cam_right = transforms.model_view_matrix.inverse().col(0).truncate().normalize();
        let edge = world_pos + cam_right * world_radius;
        let center_screen = self.world_to_screen(world_pos, viewport)?;
        let edge_screen = self.world_to_screen(edge, viewport)?;
        Some((edge_screen - center_screen).length())
    }
}

/// Determine which character body bone a weapon model attaches to at runtime.
/// This is based on the weapon folder name convention used in Smash Ultimate.
fn weapon_attach_bone(weapon_dir: &str) -> String {
    // Most right-hand weapons attach to "haver" (right hand helper bone).
    // Left-hand weapons (shields, off-hand items) attach to "havel".
    // Hammers, bats, and other two-handed weapons also use "haver".
    match weapon_dir {
        "shield" | "shieldl" => "havel".to_string(),
        _ => "haver".to_string(),
    }
}
///
/// In `prepare`: runs all ssbh_wgpu internal passes (skinning, shadow, bloom, etc.)
///               into intermediate textures via `begin_render_models`.
/// In `paint`:   calls `end_render_models` on the egui surface render pass,
///               which composites the final result onto the surface.
pub struct ViewportCallback {
    pub width: f32,
    pub height: f32,
    pub current_frame: f32,
    pub anim_path: Option<std::path::PathBuf>,
    pub skel_path: Option<std::path::PathBuf>,
    pub particles: Vec<crate::effects::Particle>,
    pub trails: Vec<crate::effects::SwordTrail>,
    pub emitter_sets: Vec<crate::effects::EmitterSet>,
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(state) = resources.get_mut::<HitboxRenderState>() {
            let w = self.width as u32;
            let h = self.height as u32;

            let frame_changed = (self.current_frame - state.last_frame).abs() > f32::EPSILON;
            let anim_changed = self.anim_path != state.last_anim_path;
            let skel_changed = self.skel_path != state.last_skel_path;

            state.resize(device, w, h);
            state.update_camera(queue, self.width, self.height);

            // Re-skin when frame, animation, or skeleton changes
            if frame_changed || anim_changed || skel_changed {
                state.apply_animation(
                    queue,
                    self.anim_path.as_deref(),
                    self.skel_path.as_deref(),
                    self.current_frame,
                );
                state.last_frame = self.current_frame;
                state.last_anim_path = self.anim_path.clone();
                state.last_skel_path = self.skel_path.clone();
            }

            // Always re-render (camera may have changed, or egui needs a fresh frame)
            state.renderer.begin_render_models(
                egui_encoder,
                &state.render_models,
                state.shared_data.database(),
                &state.model_render_options,
            );

            // Render particles and trails into the particle target texture
            if !self.particles.is_empty() || !self.trails.is_empty() {
                eprintln!("[RENDER] {} particles, {} trails, target={}", self.particles.len(), self.trails.len(), state.particle_target.is_some());
                if state.particle_target.is_none() {
                    eprintln!("[RENDER] WARNING: particle_target is None! viewport size={}x{}", w, h);
                }
                if let Some((_, ref target_view)) = state.particle_target {
                    let (view_proj, cam_right, cam_up) = state.camera_vectors();
                    if let Some(pr) = state.particle_renderer.as_mut() {
                        // Clear the particle target first, then render into it
                        {
                            let _ = egui_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("particle_target_clear"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: target_view,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                        store: wgpu::StoreOp::Store,
                                    },
                                    depth_slice: None,
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                            });
                        }
                        pr.render(
                            device,
                            queue,
                            egui_encoder,
                            target_view,
                            view_proj,
                            cam_right,
                            cam_up,
                            &self.particles,
                            &self.trails,
                            &self.emitter_sets,
                        );
                        pr.prepare_composite(device, target_view);
                    }
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        // Composite the rendered scene onto the egui surface via the overlay pass.
        if let Some(state) = resources.get::<HitboxRenderState>() {
            state.renderer.end_render_models(render_pass);
            // Composite the particle/trail offscreen texture on top.
            if !self.particles.is_empty() || !self.trails.is_empty() {
                if let Some(pr) = state.particle_renderer.as_ref() {
                    pr.composite(render_pass);
                }
            }
        }
    }
}
