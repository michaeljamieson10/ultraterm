use std::collections::HashMap;
use std::fs;

use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::screen::{CellFlags, CursorShape, Rgb, Screen, CURSOR_COLOR, DEFAULT_BG};

const ATLAS_WIDTH: u32 = 2048;
const ATLAS_HEIGHT: u32 = 2048;
const GRID_LEFT_PADDING: f32 = 2.0;
const GRID_TOP_OFFSET_WINDOWED: f32 = 26.0;
const GRID_TOP_OFFSET_FULLSCREEN: f32 = -2.0;
const DEFAULT_FONT_SIZE: f32 = 20.0;
const FONT_SIZE_STEP: f32 = 1.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 64.0;

#[derive(Clone, Copy, Debug)]
pub struct SelectionRange {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

#[derive(Clone, Copy, Debug)]
pub struct OverlayStats {
    pub fps: f32,
    pub frame_ms: f32,
    pub dirty_rows: usize,
    pub pty_bytes_per_sec: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadVertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen_size: [f32; 2],
    _pad: [f32; 2],
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct GlyphKey {
    ch: char,
    bold: bool,
    italic: bool,
}

#[derive(Clone, Copy, Debug)]
struct GlyphEntry {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    width: u32,
    height: u32,
    xmin: i32,
    ymin: i32,
    advance: f32,
}

struct GlyphAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    width: u32,
    height: u32,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    cache: HashMap<GlyphKey, GlyphEntry>,
}

impl GlyphAtlas {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph-atlas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let mut atlas = Self {
            texture,
            view,
            sampler,
            width,
            height,
            next_x: 1,
            next_y: 1,
            row_height: 0,
            cache: HashMap::new(),
        };
        atlas.clear(queue);
        atlas
    }

    fn clear(&mut self, queue: &wgpu::Queue) {
        self.cache.clear();
        self.next_x = 1;
        self.next_y = 1;
        self.row_height = 0;

        let mut clear = vec![0_u8; (self.width * self.height) as usize];
        clear[0] = 255;
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &clear,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.width),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn white_uv(&self) -> ([f32; 2], [f32; 2]) {
        let w = self.width as f32;
        let h = self.height as f32;
        ([0.0, 0.0], [1.0 / w, 1.0 / h])
    }

    fn get_or_insert(
        &mut self,
        font: &Font,
        font_size: f32,
        queue: &wgpu::Queue,
        key: GlyphKey,
    ) -> GlyphEntry {
        if let Some(entry) = self.cache.get(&key).copied() {
            return entry;
        }

        let (metrics, bitmap) = font.rasterize(key.ch, font_size);
        let glyph_width = metrics.width.max(1) as u32;
        let glyph_height = metrics.height.max(1) as u32;

        if self.next_x + glyph_width + 1 >= self.width {
            self.next_x = 1;
            self.next_y = self.next_y.saturating_add(self.row_height + 1);
            self.row_height = 0;
        }

        if self.next_y + glyph_height + 1 >= self.height {
            self.clear(queue);
        }

        let x = self.next_x;
        let y = self.next_y;
        self.next_x = self.next_x.saturating_add(glyph_width + 1);
        self.row_height = self.row_height.max(glyph_height);

        let mut upload = if metrics.width == 0 || metrics.height == 0 {
            vec![0_u8]
        } else {
            bitmap
        };

        if upload.is_empty() {
            upload.push(0);
        }

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &upload,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(glyph_width),
                rows_per_image: Some(glyph_height),
            },
            wgpu::Extent3d {
                width: glyph_width,
                height: glyph_height,
                depth_or_array_layers: 1,
            },
        );

        let entry = GlyphEntry {
            uv_min: [x as f32 / self.width as f32, y as f32 / self.height as f32],
            uv_max: [
                (x + glyph_width) as f32 / self.width as f32,
                (y + glyph_height) as f32 / self.height as f32,
            ],
            width: glyph_width,
            height: glyph_height,
            xmin: metrics.xmin,
            ymin: metrics.ymin,
            advance: metrics.advance_width,
        };

        self.cache.insert(key, entry);
        entry
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals_buffer: wgpu::Buffer,
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    quad_index_count: u32,
    atlas: GlyphAtlas,
    font: Font,
    font_size: f32,
    grid_top_offset: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    baseline: f32,
    row_buffers: Vec<wgpu::Buffer>,
    row_capacities: Vec<u64>,
    row_counts: Vec<u32>,
    extra_buffer: wgpu::Buffer,
    extra_capacity: u64,
    extra_count: u32,
}

impl Renderer {
    pub async fn new(window: &'static Window, rows: usize) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .context("failed to create surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("failed to request adapter"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("renderer-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .context("failed to request device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        let font = load_font()?;
        let line_metrics = font
            .horizontal_line_metrics(DEFAULT_FONT_SIZE)
            .ok_or_else(|| anyhow!("missing line metrics for loaded font"))?;
        let mono_metrics = font.metrics('W', DEFAULT_FONT_SIZE);
        let cell_width = mono_metrics.advance_width.max(1.0).ceil();
        let cell_height = line_metrics.new_line_size.max(1.0).ceil();
        let baseline = line_metrics.ascent.ceil();

        let atlas = GlyphAtlas::new(&device, &queue, ATLAS_WIDTH, ATLAS_HEIGHT);

        let quad_vertices = [
            QuadVertex {
                pos: [0.0, 0.0],
                uv: [0.0, 0.0],
            },
            QuadVertex {
                pos: [1.0, 0.0],
                uv: [1.0, 0.0],
            },
            QuadVertex {
                pos: [1.0, 1.0],
                uv: [1.0, 1.0],
            },
            QuadVertex {
                pos: [0.0, 1.0],
                uv: [0.0, 1.0],
            },
        ];
        let quad_indices: [u16; 6] = [0, 1, 2, 2, 3, 0];

        let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-vertex-buffer"),
            contents: bytemuck::cast_slice(&quad_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-index-buffer"),
            contents: bytemuck::cast_slice(&quad_indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let globals = Globals {
            screen_size: [config.width as f32, config.height as f32],
            _pad: [0.0, 0.0],
        };
        let globals_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals-buffer"),
            contents: bytemuck::bytes_of(&globals),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("renderer-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: globals_buffer.as_entire_binding(),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("renderer-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminal-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<QuadVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 8,
                                shader_location: 1,
                            },
                        ],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 2,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 8,
                                shader_location: 3,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 16,
                                shader_location: 4,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 24,
                                shader_location: 5,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 32,
                                shader_location: 6,
                            },
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let extra_capacity = 1024 * std::mem::size_of::<Instance>() as u64;
        let extra_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("extra-instance-buffer"),
            size: extra_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            globals_buffer,
            quad_vertex_buffer,
            quad_index_buffer,
            quad_index_count: quad_indices.len() as u32,
            atlas,
            font,
            font_size: DEFAULT_FONT_SIZE,
            grid_top_offset: if window.fullscreen().is_some() {
                GRID_TOP_OFFSET_FULLSCREEN
            } else {
                GRID_TOP_OFFSET_WINDOWED
            },
            cell_width,
            cell_height,
            baseline,
            row_buffers: Vec::new(),
            row_capacities: Vec::new(),
            row_counts: Vec::new(),
            extra_buffer,
            extra_capacity,
            extra_count: 0,
        };

        renderer.ensure_rows(rows.max(1));
        Ok(renderer)
    }

    pub fn grid_from_window_size(&self, size: PhysicalSize<u32>) -> (usize, usize) {
        let available_width = (size.width as f32 - GRID_LEFT_PADDING).max(1.0);
        let available_height = (size.height as f32 - self.grid_top_offset.max(0.0)).max(1.0);
        let cols = (available_width / self.cell_width).floor().max(1.0) as usize;
        let rows = (available_height / self.cell_height).floor().max(1.0) as usize;
        (cols, rows)
    }

    pub fn grid_left_padding(&self) -> f32 {
        GRID_LEFT_PADDING
    }

    pub fn grid_top_offset(&self) -> f32 {
        self.grid_top_offset
    }

    pub fn set_fullscreen_layout(&mut self, fullscreen: bool) -> bool {
        let next = if fullscreen {
            GRID_TOP_OFFSET_FULLSCREEN
        } else {
            GRID_TOP_OFFSET_WINDOWED
        };
        if (next - self.grid_top_offset).abs() < f32::EPSILON {
            return false;
        }
        self.grid_top_offset = next;
        true
    }

    pub fn increase_font_size(&mut self) -> bool {
        self.set_font_size(self.font_size + FONT_SIZE_STEP)
    }

    pub fn decrease_font_size(&mut self) -> bool {
        self.set_font_size(self.font_size - FONT_SIZE_STEP)
    }

    fn set_font_size(&mut self, font_size: f32) -> bool {
        let next = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if (next - self.font_size).abs() < f32::EPSILON {
            return false;
        }

        let Some(line_metrics) = self.font.horizontal_line_metrics(next) else {
            return false;
        };
        let mono_metrics = self.font.metrics('W', next);

        self.font_size = next;
        self.cell_width = mono_metrics.advance_width.max(1.0).ceil();
        self.cell_height = line_metrics.new_line_size.max(1.0).ceil();
        self.baseline = line_metrics.ascent.ceil();
        self.atlas.clear(&self.queue);
        true
    }

    pub fn resize_surface(&mut self, size: PhysicalSize<u32>, rows: usize) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        let globals = Globals {
            screen_size: [self.config.width as f32, self.config.height as f32],
            _pad: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));
        self.ensure_rows(rows.max(1));
    }

    pub fn ensure_rows(&mut self, rows: usize) {
        while self.row_buffers.len() < rows {
            self.row_buffers
                .push(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("row-instance-buffer"),
                    size: 16,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            self.row_capacities.push(16);
            self.row_counts.push(0);
        }
        self.row_counts.truncate(rows);
        self.row_buffers.truncate(rows);
        self.row_capacities.truncate(rows);
    }

    fn ensure_row_capacity(&mut self, row: usize, bytes_needed: u64) {
        if bytes_needed <= self.row_capacities[row] {
            return;
        }
        let capacity = bytes_needed.next_power_of_two().max(64);
        self.row_buffers[row] = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("row-instance-buffer"),
            size: capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.row_capacities[row] = capacity;
    }

    fn ensure_extra_capacity(&mut self, bytes_needed: u64) {
        if bytes_needed <= self.extra_capacity {
            return;
        }
        self.extra_capacity = bytes_needed.next_power_of_two().max(256);
        self.extra_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("extra-instance-buffer"),
            size: self.extra_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
    }

    pub fn update_rows(&mut self, screen: &Screen, dirty_rows: &[usize]) {
        if dirty_rows.is_empty() {
            return;
        }

        let cols = screen.cols();
        let cursor = screen.cursor();
        let (white_min, white_max) = self.atlas.white_uv();

        for &row in dirty_rows {
            if row >= screen.rows() || row >= self.row_buffers.len() {
                continue;
            }

            let line = screen.line(row);
            let mut instances = Vec::with_capacity(cols * 2 + 4);

            for (col, cell) in line.iter().enumerate() {
                let x = GRID_LEFT_PADDING + col as f32 * self.cell_width;
                let y = self.grid_top_offset + row as f32 * self.cell_height;

                if cell.bg != DEFAULT_BG {
                    instances.push(Instance {
                        pos: [x, y],
                        size: [self.cell_width, self.cell_height],
                        uv_min: white_min,
                        uv_max: white_max,
                        color: rgba(cell.bg, 1.0),
                    });
                }

                if cell.ch != ' ' && !cell.flags.contains(CellFlags::WIDE_CONT) {
                    if let Some(glyph_instance) =
                        self.glyph_instance(cell.ch, cell.flags, x, y, rgba(cell.fg, 1.0))
                    {
                        instances.push(glyph_instance);
                    }
                }
            }

            if cursor.visible && cursor.row == row {
                let cursor_x = GRID_LEFT_PADDING + cursor.col as f32 * self.cell_width;
                let (pos, size) = match cursor.shape {
                    CursorShape::Block => (
                        [
                            cursor_x,
                            self.grid_top_offset + row as f32 * self.cell_height,
                        ],
                        [self.cell_width, self.cell_height],
                    ),
                    CursorShape::Beam => (
                        [
                            cursor_x,
                            self.grid_top_offset + row as f32 * self.cell_height,
                        ],
                        [(self.cell_width * 0.12).max(2.0), self.cell_height],
                    ),
                    CursorShape::Underline => {
                        let h = (self.cell_height * 0.14).max(2.0);
                        (
                            [
                                cursor_x,
                                self.grid_top_offset
                                    + row as f32 * self.cell_height
                                    + (self.cell_height - h),
                            ],
                            [self.cell_width, h],
                        )
                    }
                };
                instances.push(Instance {
                    pos,
                    size,
                    uv_min: white_min,
                    uv_max: white_max,
                    color: rgba(CURSOR_COLOR, 0.75),
                });
            }

            let bytes = bytemuck::cast_slice::<Instance, u8>(&instances);
            self.ensure_row_capacity(row, bytes.len() as u64);
            if !bytes.is_empty() {
                self.queue.write_buffer(&self.row_buffers[row], 0, bytes);
            }
            self.row_counts[row] = instances.len() as u32;
        }
    }

    fn glyph_instance(
        &mut self,
        ch: char,
        flags: CellFlags,
        cell_x: f32,
        cell_y: f32,
        color: [f32; 4],
    ) -> Option<Instance> {
        let key = GlyphKey {
            ch,
            bold: flags.contains(CellFlags::BOLD),
            italic: flags.contains(CellFlags::ITALIC),
        };
        let glyph = self
            .atlas
            .get_or_insert(&self.font, self.font_size, &self.queue, key);

        if glyph.width == 0 || glyph.height == 0 {
            return None;
        }

        let x_offset = ((self.cell_width - glyph.advance).max(0.0)) * 0.5;
        let x = cell_x + x_offset + glyph.xmin as f32;
        let y = cell_y + self.baseline - glyph.ymin as f32 - glyph.height as f32;

        Some(Instance {
            pos: [x, y],
            size: [glyph.width as f32, glyph.height as f32],
            uv_min: glyph.uv_min,
            uv_max: glyph.uv_max,
            color,
        })
    }

    pub fn mark_all_dirty(&mut self, screen: &Screen) {
        let all_rows: Vec<usize> = (0..screen.rows()).collect();
        self.update_rows(screen, &all_rows);
    }

    pub fn render(
        &mut self,
        screen: &Screen,
        selection: Option<SelectionRange>,
        overlay: Option<OverlayStats>,
    ) -> std::result::Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut extra_instances = Vec::new();
        self.push_selection_instances(screen, selection, &mut extra_instances);
        self.push_overlay_instances(overlay, &mut extra_instances);

        if !extra_instances.is_empty() {
            let bytes = bytemuck::cast_slice::<Instance, u8>(&extra_instances);
            self.ensure_extra_capacity(bytes.len() as u64);
            self.queue.write_buffer(&self.extra_buffer, 0, bytes);
            self.extra_count = extra_instances.len() as u32;
        } else {
            self.extra_count = 0;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: DEFAULT_BG.r as f64 / 255.0,
                            g: DEFAULT_BG.g as f64 / 255.0,
                            b: DEFAULT_BG.b as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            pass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);

            for (row, count) in self.row_counts.iter().enumerate() {
                if *count == 0 || row >= screen.rows() {
                    continue;
                }
                pass.set_vertex_buffer(1, self.row_buffers[row].slice(..));
                pass.draw_indexed(0..self.quad_index_count, 0, 0..*count);
            }

            if self.extra_count > 0 {
                pass.set_vertex_buffer(1, self.extra_buffer.slice(..));
                pass.draw_indexed(0..self.quad_index_count, 0, 0..self.extra_count);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(())
    }

    fn push_selection_instances(
        &self,
        screen: &Screen,
        selection: Option<SelectionRange>,
        out: &mut Vec<Instance>,
    ) {
        let Some(sel) = selection else {
            return;
        };

        let ((start_row, start_col), (end_row, end_col)) = if sel.start <= sel.end {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };

        let (white_min, white_max) = self.atlas.white_uv();

        for row in start_row..=end_row {
            if row >= screen.rows() {
                continue;
            }
            let from =
                if row == start_row { start_col } else { 0 }.min(screen.cols().saturating_sub(1));
            let to = if row == end_row {
                end_col
            } else {
                screen.cols().saturating_sub(1)
            }
            .min(screen.cols().saturating_sub(1));
            if from > to {
                continue;
            }
            out.push(Instance {
                pos: [
                    GRID_LEFT_PADDING + from as f32 * self.cell_width,
                    self.grid_top_offset + row as f32 * self.cell_height,
                ],
                size: [(to - from + 1) as f32 * self.cell_width, self.cell_height],
                uv_min: white_min,
                uv_max: white_max,
                color: [0.36, 0.52, 0.95, 0.35],
            });
        }
    }

    fn push_overlay_instances(&mut self, overlay: Option<OverlayStats>, out: &mut Vec<Instance>) {
        let Some(stats) = overlay else {
            return;
        };

        let text = format!(
            "FPS {:.1}\nFrame {:.2} ms\nDirty {}\nPTY {} KB/s",
            stats.fps,
            stats.frame_ms,
            stats.dirty_rows,
            stats.pty_bytes_per_sec / 1024
        );

        let lines: Vec<&str> = text.lines().collect();
        let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);

        let (white_min, white_max) = self.atlas.white_uv();
        out.push(Instance {
            pos: [6.0, 6.0],
            size: [
                (max_chars as f32 + 1.6) * self.cell_width,
                (lines.len() as f32 + 0.8) * self.cell_height,
            ],
            uv_min: white_min,
            uv_max: white_max,
            color: [0.0, 0.0, 0.0, 0.62],
        });

        for (line_index, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let x = 12.0 + col as f32 * self.cell_width;
                let y = 10.0 + line_index as f32 * self.cell_height;
                if let Some(instance) =
                    self.glyph_instance(ch, CellFlags::empty(), x, y, [0.95, 0.95, 0.95, 1.0])
                {
                    out.push(instance);
                }
            }
        }
    }
}

fn rgba(color: Rgb, alpha: f32) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        alpha,
    ]
}

fn load_font() -> Result<Font> {
    // Keep this list explicit for predictable startup on macOS.
    let candidates = [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Supplemental/Menlo.ttc",
        "/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
    ];

    for path in candidates {
        let Ok(bytes) = fs::read(path) else {
            continue;
        };

        let settings = FontSettings {
            collection_index: 0,
            scale: 40.0,
            ..FontSettings::default()
        };

        if let Ok(font) = Font::from_bytes(bytes, settings) {
            return Ok(font);
        }
    }

    Err(anyhow!("failed to load a monospaced system font"))
}
