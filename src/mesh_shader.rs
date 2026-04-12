pub mod bind_groups {
    #[derive(Debug, Clone)]
    pub struct BindGroup0(wgpu::BindGroup);
    #[derive(Debug)]
    pub struct BindGroupLayout0<'a> {
        pub camera: wgpu::BufferBinding<'a>,
        pub instances: wgpu::BufferBinding<'a>,
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
                                resource: wgpu::BindingResource::Buffer(bindings.instances),
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
    #[derive(Debug, Copy, Clone)]
    pub struct BindGroups<'a> {
        pub bind_group0: &'a BindGroup0,
        pub bind_group1: &'a BindGroup1,
    }
    impl BindGroups<'_> {
        pub fn set<P: super::SetBindGroup>(&self, pass: &mut P) {
            self.bind_group0.set(pass);
            self.bind_group1.set(pass);
        }
    }
}
pub fn set_bind_groups<P: SetBindGroup>(
    pass: &mut P,
    bind_group0: &bind_groups::BindGroup0,
    bind_group1: &bind_groups::BindGroup1,
) {
    bind_group0.set(pass);
    bind_group1.set(pass);
}
pub fn fs_main_entry(targets: [Option<wgpu::ColorTargetState>; 1]) -> FragmentEntry<1> {
    FragmentEntry {
        entry_point: ENTRY_FS_MAIN,
        targets,
        constants: Default::default(),
    }
}
pub const SOURCE: &str = include_str!("src/mesh.wgsl");
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
pub struct MeshInstance {
    pub world_pos: glam::Vec3,
    pub scale: f32,
    pub color: glam::Vec4,
    pub rotation_y: f32,
    pub _pad: glam::Vec2,
}
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VertexInput {
    pub position: glam::Vec3,
    pub uv: glam::Vec2,
    pub normal: glam::Vec3,
}
impl VertexInput {
    pub const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 3] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::offset_of!(VertexInput, position) as u64,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: std::mem::offset_of!(VertexInput, uv) as u64,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::offset_of!(VertexInput, normal) as u64,
            shader_location: 2,
        },
    ];
    pub const fn vertex_buffer_layout(
        step_mode: wgpu::VertexStepMode,
    ) -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<VertexInput>() as u64,
            step_mode,
            attributes: &VertexInput::VERTEX_ATTRIBUTES,
        }
    }
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
pub fn vs_main_entry(vert_step_mode: wgpu::VertexStepMode) -> VertexEntry<1> {
    VertexEntry {
        entry_point: ENTRY_VS_MAIN,
        buffers: [VertexInput::vertex_buffer_layout(vert_step_mode)],
        constants: Default::default(),
    }
}
