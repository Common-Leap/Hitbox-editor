pub mod bind_groups {
    #[derive(Debug, Clone)]
    pub struct BindGroup0(wgpu::BindGroup);
    #[derive(Debug)]
    pub struct BindGroupLayout0<'a> {
        pub camera: wgpu::BufferBinding<'a>,
        pub particles: wgpu::BufferBinding<'a>,
    }
    const LAYOUT_DESCRIPTOR0: wgpu::BindGroupLayoutDescriptor = wgpu::BindGroupLayoutDescriptor {
        label: Some("LayoutDescriptor0"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: true,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    };
    impl BindGroup0 {
        pub fn get_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
            device.create_bind_group_layout(&LAYOUT_DESCRIPTOR0)
        }
        pub fn from_bindings(device: &wgpu::Device, bindings: BindGroupLayout0) -> Self {
            let bind_group_layout = device.create_bind_group_layout(&LAYOUT_DESCRIPTOR0);
            let bind_group = device
                .create_bind_group(
                    &wgpu::BindGroupDescriptor {
                        layout: &bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(bindings.camera),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Buffer(bindings.particles),
                            },
                        ],
                        label: Some("BindGroup0"),
                    },
                );
            Self(bind_group)
        }
        pub fn set<P: super::SetBindGroup>(&self, pass: &mut P) {
            pass.set_bind_group(0, &self.0, &[]);
        }
        pub fn inner(&self) -> &wgpu::BindGroup {
            &self.0
        }
    }
    #[derive(Debug, Clone)]
    pub struct BindGroup1(wgpu::BindGroup);
    #[derive(Debug)]
    pub struct BindGroupLayout1<'a> {
        pub tex: &'a wgpu::TextureView,
        pub tex_sampler: &'a wgpu::Sampler,
        pub alpha_tex: &'a wgpu::TextureView,
        pub alpha_sampler: &'a wgpu::Sampler,
        pub indirect_tex: &'a wgpu::TextureView,
        pub indirect_sampler: &'a wgpu::Sampler,
        pub indirect_params: wgpu::BufferBinding<'a>,
    }
    const LAYOUT_DESCRIPTOR1: wgpu::BindGroupLayoutDescriptor = wgpu::BindGroupLayoutDescriptor {
        label: Some("LayoutDescriptor1"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    };
    impl BindGroup1 {
        pub fn get_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
            device.create_bind_group_layout(&LAYOUT_DESCRIPTOR1)
        }
        pub fn from_bindings(device: &wgpu::Device, bindings: BindGroupLayout1) -> Self {
            let bind_group_layout = device.create_bind_group_layout(&LAYOUT_DESCRIPTOR1);
            let bind_group = device
                .create_bind_group(
                    &wgpu::BindGroupDescriptor {
                        layout: &bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(bindings.tex),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.tex_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(
                                    bindings.alpha_tex,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.alpha_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: wgpu::BindingResource::TextureView(
                                    bindings.indirect_tex,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 5,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.indirect_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 6,
                                resource: wgpu::BindingResource::Buffer(
                                    bindings.indirect_params,
                                ),
                            },
                        ],
                        label: Some("BindGroup1"),
                    },
                );
            Self(bind_group)
        }
        pub fn set<P: super::SetBindGroup>(&self, pass: &mut P) {
            pass.set_bind_group(1, &self.0, &[]);
        }
        pub fn inner(&self) -> &wgpu::BindGroup {
            &self.0
        }
    }
    #[derive(Debug, Clone)]
    pub struct BindGroup2(wgpu::BindGroup);
    #[derive(Debug)]
    pub struct BindGroupLayout2<'a> {
        pub mat_tex_col: &'a wgpu::TextureView,
        pub mat_tex_col_sampler: &'a wgpu::Sampler,
        pub mat_tex_emi: &'a wgpu::TextureView,
        pub mat_tex_emi_sampler: &'a wgpu::Sampler,
        pub mat_tex_prm: &'a wgpu::TextureView,
        pub mat_tex_prm_sampler: &'a wgpu::Sampler,
        pub mat_tex_flags: wgpu::BufferBinding<'a>,
    }
    const LAYOUT_DESCRIPTOR2: wgpu::BindGroupLayoutDescriptor = wgpu::BindGroupLayoutDescriptor {
        label: Some("LayoutDescriptor2"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float {
                        filterable: true,
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    };
    impl BindGroup2 {
        pub fn get_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
            device.create_bind_group_layout(&LAYOUT_DESCRIPTOR2)
        }
        pub fn from_bindings(device: &wgpu::Device, bindings: BindGroupLayout2) -> Self {
            let bind_group_layout = device.create_bind_group_layout(&LAYOUT_DESCRIPTOR2);
            let bind_group = device
                .create_bind_group(
                    &wgpu::BindGroupDescriptor {
                        layout: &bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(
                                    bindings.mat_tex_col,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.mat_tex_col_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(
                                    bindings.mat_tex_emi,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.mat_tex_emi_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 4,
                                resource: wgpu::BindingResource::TextureView(
                                    bindings.mat_tex_prm,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 5,
                                resource: wgpu::BindingResource::Sampler(
                                    bindings.mat_tex_prm_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 6,
                                resource: wgpu::BindingResource::Buffer(
                                    bindings.mat_tex_flags,
                                ),
                            },
                        ],
                        label: Some("BindGroup2"),
                    },
                );
            Self(bind_group)
        }
        pub fn set<P: super::SetBindGroup>(&self, pass: &mut P) {
            pass.set_bind_group(2, &self.0, &[]);
        }
        pub fn inner(&self) -> &wgpu::BindGroup {
            &self.0
        }
    }
    #[derive(Debug, Copy, Clone)]
    pub struct BindGroups<'a> {
        pub bind_group0: &'a BindGroup0,
        pub bind_group1: &'a BindGroup1,
        pub bind_group2: &'a BindGroup2,
    }
    impl BindGroups<'_> {
        pub fn set<P: super::SetBindGroup>(&self, pass: &mut P) {
            self.bind_group0.set(pass);
            self.bind_group1.set(pass);
            self.bind_group2.set(pass);
        }
    }
}
pub fn set_bind_groups<P: SetBindGroup>(
    pass: &mut P,
    bind_group0: &bind_groups::BindGroup0,
    bind_group1: &bind_groups::BindGroup1,
    bind_group2: &bind_groups::BindGroup2,
) {
    bind_group0.set(pass);
    bind_group1.set(pass);
    bind_group2.set(pass);
}
pub fn fs_main_entry(targets: [Option<wgpu::ColorTargetState>; 1]) -> FragmentEntry<1> {
    FragmentEntry {
        entry_point: ENTRY_FS_MAIN,
        targets,
        constants: Default::default(),
    }
}
pub const SOURCE: &str = include_str!("src/particle.wgsl");
pub fn create_shader_module(device: &wgpu::Device) -> wgpu::ShaderModule {
    let source = std::borrow::Cow::Borrowed(SOURCE);
    device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(source),
        })
}
pub fn create_pipeline_layout(device: &wgpu::Device) -> wgpu::PipelineLayout {
    device
        .create_pipeline_layout(
            &wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[
                    Some(&bind_groups::BindGroup0::get_bind_group_layout(device)),
                    Some(&bind_groups::BindGroup1::get_bind_group_layout(device)),
                    Some(&bind_groups::BindGroup2::get_bind_group_layout(device)),
                ],
                immediate_size: 0,
            },
        )
}
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, encase::ShaderType)]
pub struct CameraUniforms {
    pub view_proj: glam::Mat4,
    pub cam_right: glam::Vec3,
    pub _pad0: f32,
    pub cam_up: glam::Vec3,
    pub _pad1: f32,
}
pub const ENTRY_FS_MAIN: &str = "fs_main";
pub const ENTRY_VS_MAIN: &str = "vs_main";
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, encase::ShaderType)]
pub struct IndirectParams {
    pub is_indirect: u32,
    pub distortion_strength: f32,
    pub indirect_scroll_u: f32,
    pub indirect_scroll_v: f32,
    pub indirect_scale_u: f32,
    pub indirect_scale_v: f32,
    pub indirect_offset_u: f32,
    pub indirect_offset_v: f32,
}
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, encase::ShaderType)]
pub struct ParticleInstance {
    pub position: glam::Vec4,
    pub color: glam::Vec4,
    pub rotation: f32,
    pub aspect_ratio: f32,
    pub size: f32,
    pub _pad: f32,
    pub tex_scale: glam::Vec2,
    pub tex_offset: glam::Vec2,
}
#[derive(Debug)]
pub struct VertexEntry<const N: usize> {
    pub entry_point: &'static str,
    pub buffers: [wgpu::VertexBufferLayout<'static>; N],
    pub constants: Vec<(&'static str, f64)>,
}
pub fn vertex_state<'a, const N: usize>(
    module: &'a wgpu::ShaderModule,
    entry: &'a VertexEntry<N>,
) -> wgpu::VertexState<'a> {
    wgpu::VertexState {
        module,
        entry_point: Some(entry.entry_point),
        buffers: &entry.buffers,
        compilation_options: wgpu::PipelineCompilationOptions {
            constants: &entry.constants,
            ..Default::default()
        },
    }
}
#[derive(Debug)]
pub struct FragmentEntry<const N: usize> {
    pub entry_point: &'static str,
    pub targets: [Option<wgpu::ColorTargetState>; N],
    pub constants: Vec<(&'static str, f64)>,
}
pub fn fragment_state<'a, const N: usize>(
    module: &'a wgpu::ShaderModule,
    entry: &'a FragmentEntry<N>,
) -> wgpu::FragmentState<'a> {
    wgpu::FragmentState {
        module,
        entry_point: Some(entry.entry_point),
        targets: &entry.targets,
        compilation_options: wgpu::PipelineCompilationOptions {
            constants: &entry.constants,
            ..Default::default()
        },
    }
}
pub trait SetBindGroup {
    fn set_bind_group(
        &mut self,
        index: u32,
        bind_group: &wgpu::BindGroup,
        offsets: &[wgpu::DynamicOffset],
    );
}
impl SetBindGroup for wgpu::ComputePass<'_> {
    fn set_bind_group(
        &mut self,
        index: u32,
        bind_group: &wgpu::BindGroup,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.set_bind_group(index, bind_group, offsets);
    }
}
impl SetBindGroup for wgpu::RenderPass<'_> {
    fn set_bind_group(
        &mut self,
        index: u32,
        bind_group: &wgpu::BindGroup,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.set_bind_group(index, bind_group, offsets);
    }
}
impl SetBindGroup for wgpu::RenderBundleEncoder<'_> {
    fn set_bind_group(
        &mut self,
        index: u32,
        bind_group: &wgpu::BindGroup,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.set_bind_group(index, bind_group, offsets);
    }
}
pub fn vs_main_entry() -> VertexEntry<0> {
    VertexEntry {
        entry_point: ENTRY_VS_MAIN,
        buffers: [],
        constants: Default::default(),
    }
}
