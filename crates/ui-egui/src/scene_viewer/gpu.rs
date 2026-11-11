//! Worker-owned instanced renderer. No pipeline creation, uploads or draw encoding on a UI frame.
use super::{camera::Camera, draw_plan};
use crate::theme::Tokens;
use craft_anything_engine::CancelToken;
use craft_anything_formats::scene_bundle::{Scene, Visibility, math};
use eframe::wgpu as w;
use std::{
    future::Future,
    sync::{Arc, mpsc},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

fn bounded<F: Future>(future: F, device: &w::Device) -> Result<F::Output, String> {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    let start = Instant::now();
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return Ok(value);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err("GPU operation timed out; try another graphics adapter.".into());
        }
        let _ = device.poll(w::PollType::Poll);
        std::thread::sleep(Duration::from_millis(2));
    }
}
struct Draw {
    indices: std::ops::Range<u32>,
    base: i32,
}
pub struct FrameData {
    pub camera: Camera,
    pub visibility: Visibility,
    pub selected: Option<usize>,
    pub selected_resource: Option<usize>,
    pub size: [u32; 2],
    pub tokens: Tokens,
    pub phases: std::collections::BTreeMap<usize, u8>,
    pub source_colors: bool,
    pub object_colors: bool,
}
pub struct Gpu {
    device: w::Device,
    queue: w::Queue,
    pipeline: w::RenderPipeline,
    vertices: w::Buffer,
    indices: w::Buffer,
    instances: w::Buffer,
    uniform: w::Buffer,
    bind: w::BindGroup,
    draws: Vec<Draw>,
    order: Vec<usize>,
    errors: mpsc::Receiver<String>,
    pub view: w::TextureView,
    _color: w::Texture,
    depth: w::TextureView,
    _depth: w::Texture,
    size: [u32; 2],
    pub stats: Option<draw_plan::DrawStats>,
    pub elapsed_ms: u64,
}
fn target(device: &w::Device, size: [u32; 2], depth: bool) -> (w::Texture, w::TextureView) {
    let texture = device.create_texture(&w::TextureDescriptor {
        label: Some("Scene viewport"),
        size: w::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: w::TextureDimension::D2,
        format: if depth { w::TextureFormat::Depth32Float } else { w::TextureFormat::Rgba8Unorm },
        usage: if depth { w::TextureUsages::RENDER_ATTACHMENT } else { w::TextureUsages::RENDER_ATTACHMENT | w::TextureUsages::TEXTURE_BINDING },
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}
fn checked_u32(v: usize) -> Result<u32, String> {
    u32::try_from(v).map_err(|_| "Scene exceeds graphics address range.".into())
}
fn buffer(device: &w::Device, queue: &w::Queue, bytes: &[u8], usage: w::BufferUsages, cancel: &CancelToken) -> Result<w::Buffer, String> {
    let length = bytes.len().max(4) as u64;
    if length > device.limits().max_buffer_size {
        return Err("Scene exceeds this adapter's buffer limit.".into());
    }
    let out = device.create_buffer(&w::BufferDescriptor {
        label: Some("Scene data"),
        size: length,
        usage: usage | w::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for (i, block) in bytes.chunks(4 * 1024 * 1024).enumerate() {
        if cancel.is_cancelled() {
            return Err("Scene upload cancelled.".into());
        }
        queue.write_buffer(&out, (i * 4 * 1024 * 1024) as u64, block);
        queue.submit([]);
    }
    Ok(out)
}
impl Gpu {
    pub fn new(state: &eframe::egui_wgpu::RenderState, scene: &Scene, cancel: &CancelToken) -> Result<Self, String> {
        let device = state.device.clone();
        let queue = state.queue.clone();
        let (tx, errors) = mpsc::sync_channel(8);
        let lost = tx.clone();
        device.on_uncaptured_error(Arc::new(move |e| {
            let _ = tx.try_send(format!("Graphics error: {e}"));
        }));
        device.set_device_lost_callback(move |_, message| {
            let _ = lost.try_send(format!("Graphics device lost: {message}"));
        });
        let scope = device.push_error_scope(w::ErrorFilter::Validation);
        let shader = device
            .create_shader_module(w::ShaderModuleDescriptor { label: Some("Scene shading"), source: w::ShaderSource::Wgsl(include_str!("scene.wgsl").into()) });
        let uniform = device.create_buffer(&w::BufferDescriptor {
            label: Some("Scene camera"),
            size: 112,
            usage: w::BufferUsages::UNIFORM | w::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&w::BindGroupLayoutDescriptor {
            label: None,
            entries: &[w::BindGroupLayoutEntry {
                binding: 0,
                visibility: w::ShaderStages::VERTEX_FRAGMENT,
                ty: w::BindingType::Buffer { ty: w::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&w::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[w::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let pipeline_layout =
            device.create_pipeline_layout(&w::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let vertex_attrs = [w::VertexAttribute { format: w::VertexFormat::Float32x3, offset: 0, shader_location: 0 }];
        let instance_attrs = [
            w::VertexAttribute { format: w::VertexFormat::Float32x4, offset: 0, shader_location: 1 },
            w::VertexAttribute { format: w::VertexFormat::Float32x4, offset: 16, shader_location: 2 },
            w::VertexAttribute { format: w::VertexFormat::Float32x4, offset: 32, shader_location: 3 },
            w::VertexAttribute { format: w::VertexFormat::Float32x4, offset: 48, shader_location: 4 },
            w::VertexAttribute { format: w::VertexFormat::Uint32, offset: 64, shader_location: 5 },
            w::VertexAttribute { format: w::VertexFormat::Uint32, offset: 68, shader_location: 6 },
            w::VertexAttribute { format: w::VertexFormat::Uint32, offset: 72, shader_location: 7 },
        ];
        let pipeline = device.create_render_pipeline(&w::RenderPipelineDescriptor {
            label: Some("Scene instancing"),
            layout: Some(&pipeline_layout),
            vertex: w::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(w::VertexBufferLayout { array_stride: 12, step_mode: w::VertexStepMode::Vertex, attributes: &vertex_attrs }),
                    Some(w::VertexBufferLayout { array_stride: 80, step_mode: w::VertexStepMode::Instance, attributes: &instance_attrs }),
                ],
            },
            primitive: w::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(w::DepthStencilState {
                format: w::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(w::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(w::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(w::ColorTargetState { format: w::TextureFormat::Rgba8Unorm, blend: None, write_mask: w::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        if let Some(e) = bounded(scope.pop(), &device)? {
            return Err(format!("Cannot create scene renderer: {e}"));
        }
        let vertices = buffer(&device, &queue, &scene.gpu_positions, w::BufferUsages::VERTEX, cancel)?;
        let indices = buffer(&device, &queue, &scene.gpu_indices, w::BufferUsages::INDEX, cancel)?;
        let mut groups = vec![Vec::new(); scene.meshes.len()];
        for (id, i) in scene.instances.iter().enumerate() {
            groups.get_mut(i.mesh).ok_or("Invalid mesh group")?.push(id);
        }
        let mut order = Vec::new();
        let mut draws = Vec::new();
        for (id, group) in groups.into_iter().enumerate() {
            let mesh = scene.meshes.get(id).ok_or("Invalid draw mesh")?;
            order.extend(group);
            draws.push(Draw {
                indices: checked_u32(mesh.index_start)?..checked_u32(mesh.index_start.saturating_add(mesh.triangle_count.saturating_mul(3)))?,
                base: i32::try_from(mesh.vertex_start).map_err(|_| "Mesh base exceeds graphics range")?,
            });
        }
        let instances = device.create_buffer(&w::BufferDescriptor {
            label: Some("Scene transforms"),
            size: (order.len().max(1) * 80) as u64,
            usage: w::BufferUsages::VERTEX | w::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let size = [16, 16];
        let (color, view) = target(&device, size, false);
        let (depth, depth_view) = target(&device, size, true);
        Ok(Self {
            device,
            queue,
            pipeline,
            vertices,
            indices,
            instances,
            uniform,
            bind,
            draws,
            order,
            errors,
            view,
            _color: color,
            depth: depth_view,
            _depth: depth,
            size,
            stats: None,
            elapsed_ms: 0,
        })
    }
    pub fn render(&mut self, scene: &Scene, frame: &FrameData, cancel: &CancelToken) -> Result<(), String> {
        let started = Instant::now();
        let FrameData { camera, visibility, selected, selected_resource, size: requested, tokens, phases, source_colors, object_colors } = frame;
        if let Ok(e) = self.errors.try_recv() {
            return Err(e);
        }
        let cap = self.device.limits().max_texture_dimension_2d.min(1600);
        let size = [requested[0].clamp(1, cap), requested[1].clamp(1, cap)];
        let scope = self.device.push_error_scope(w::ErrorFilter::Validation);
        if size != self.size {
            (self._color, self.view) = target(&self.device, size, false);
            (self._depth, self.depth) = target(&self.device, size, true);
            self.size = size;
        }
        let plan = draw_plan::plan_with_cancel(scene, camera, visibility, f64::from(size[0]) / f64::from(size[1]), draw_plan::DEFAULT_TRIANGLE_BUDGET, || {
            cancel.is_cancelled()
        })?;
        let mut counts = vec![0u32; self.draws.len()];
        let mut bytes = Vec::with_capacity(self.order.len() * 80);
        for id in &self.order {
            if cancel.is_cancelled() {
                return Err("Scene render cancelled.".into());
            }
            if !plan.visible_instances.contains(id) {
                continue;
            }
            let i = scene.instances.get(*id).ok_or("Invalid draw instance")?;
            let count = counts.get_mut(i.mesh).ok_or("Invalid visible draw group")?;
            *count = count.checked_add(1).ok_or("Visible instance count overflows")?;
            let mesh = scene.meshes.get(i.mesh).ok_or("Invalid draw definition")?;
            let shift = math::sub(math::transform(&i.matrix, mesh.origin), scene.render_origin);
            let mut m = i.matrix;
            m[12] = shift[0];
            m[13] = shift[1];
            m[14] = shift[2];
            for v in m {
                let v = v as f32;
                if !v.is_finite() {
                    return Err("Transform exceeds graphics precision range.".into());
                }
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let picked_resource = selected_resource.is_some_and(|r| i.resources.contains(&r));
            bytes.extend_from_slice(&(if picked_resource { u32::MAX - 1 } else { checked_u32(*id)? }).to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            let phase = i.resources.iter().filter_map(|r| phases.get(r)).filter(|p| **p == 1 || **p == 2).max().copied();
            let color = if let Some(phase) = phase {
                Some(if phase == 2 { tokens.warning.to_normalized_gamma_f32() } else { tokens.success.to_normalized_gamma_f32() })
            } else if *object_colors {
                scene.object_colors.get(id).map(|c| [c[0], c[1], c[2], 1.])
            } else if *source_colors {
                scene.source_colors.get(id).map(|c| [c[0], c[1], c[2], 1.])
            } else {
                None
            };
            let packed = color
                .map(|c| {
                    c.iter().enumerate().fold(0u32, |out, (n, v)| out | ((v.clamp(0., 1.) * 255.).round() as u32).checked_shl((n * 8) as u32).unwrap_or(0))
                })
                .unwrap_or(0);
            bytes.extend_from_slice(&packed.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        self.queue.write_buffer(&self.instances, 0, &bytes);
        let matrix = camera.projection(scene.render_origin, f64::from(size[0]) / f64::from(size[1]), scene.bounds.radius())?;
        let mut uniform = Vec::with_capacity(112);
        for v in matrix {
            uniform.extend_from_slice(&(v as f32).to_le_bytes());
        }
        for color in [tokens.text_dim, tokens.accent] {
            for v in color.to_normalized_gamma_f32() {
                uniform.extend_from_slice(&v.to_le_bytes());
            }
        }
        uniform.extend_from_slice(
            &(if selected_resource.is_some() { u32::MAX - 1 } else { selected.map(checked_u32).transpose()?.unwrap_or(u32::MAX) }).to_le_bytes(),
        );
        uniform.extend_from_slice(&[0; 12]);
        self.queue.write_buffer(&self.uniform, 0, &uniform);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let color = tokens.canvas.to_normalized_gamma_f32();
        {
            let mut pass = encoder.begin_render_pass(&w::RenderPassDescriptor {
                label: Some("Scene viewport"),
                color_attachments: &[Some(w::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: w::Operations {
                        load: w::LoadOp::Clear(w::Color { r: f64::from(color[0]), g: f64::from(color[1]), b: f64::from(color[2]), a: 1. }),
                        store: w::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(w::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(w::Operations { load: w::LoadOp::Clear(1.), store: w::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_vertex_buffer(1, self.instances.slice(..));
            pass.set_index_buffer(self.indices.slice(..), w::IndexFormat::Uint32);
            let mut start = 0u32;
            for (d, count) in self.draws.iter().zip(counts) {
                let end = start.checked_add(count).ok_or("Draw instance range overflows")?;
                if count > 0 {
                    pass.draw_indexed(d.indices.clone(), d.base, start..end);
                }
                start = end;
            }
        }
        let submission = self.queue.submit([encoder.finish()]);
        // Backpressure: at most one scene render is in flight, including while dragging.
        self.device
            .poll(w::PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(10)) })
            .map_err(|e| format!("GPU frame did not complete: {e}"))?;
        if let Some(e) = bounded(scope.pop(), &self.device)? {
            return Err(format!("Scene rendering failed: {e}"));
        }
        self.stats = Some(plan.stats);
        self.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(())
    }
}
